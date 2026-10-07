// Lumen - Tauri layer.
// Thin glue over lumen-core (which holds all the tested engine logic).

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(windows)]
mod winclip;

use lumen_core::cache::ThumbCache;
use lumen_core::video::{Ffmpeg, PlayableOut};
use lumen_core::{scan, thumbs, Entry, EntryKind};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tauri::{Emitter, Manager, State};

/// Image extensions the pure-Rust decoder handles; everything else that is
/// still an image (AVIF, HEIC, JXL, JP2, PSD, SVG...) goes through ffmpeg.
const NATIVE_IMG: &[&str] = &[
    "jpg", "jpeg", "jpe", "jfif", "png", "gif", "bmp", "webp", "tif", "tiff",
    "ico", "tga", "pbm", "pgm", "ppm", "pnm",
];

struct AppState {
    cache: Arc<ThumbCache>,
    ffmpeg: Arc<Ffmpeg>,
    /// Monotonic id: bumping it makes in-flight thumbnail batches stale.
    generation: Arc<AtomicU64>,
    /// Monotonic id for recursive searches (cancellation).
    search_gen: Arc<AtomicU64>,
    /// Monotonic id for recursive thumbnail pre-generation (cancellation).
    pregen_gen: Arc<AtomicU64>,
    /// Monotonic id for copy/move jobs (cancellation).
    paste_gen: Arc<AtomicU64>,
    /// Current rotation batch; bumped to cancel it.
    rot_gen: Arc<AtomicU64>,
    /// Wipe the converted-media folder when the window closes.
    clear_play_on_exit: Arc<std::sync::atomic::AtomicBool>,
}

/// Active folder watcher (auto-refresh); replaced on every navigation.
struct WatchState(std::sync::Mutex<Option<notify::RecommendedWatcher>>);

/// File passed on the command line (double-click via file association).
struct StartupFile(Option<String>);

#[derive(Debug, Clone, Deserialize)]
struct ThumbReq {
    path: String,
    kind: EntryKind,
    ext: String,
}

#[derive(Debug, Clone, Serialize)]
struct ThumbEvent {
    src: String,
    thumb: String,
    ok: bool,
    generation: u64,
}

#[tauri::command]
fn list_roots() -> Vec<String> {
    scan::list_roots()
}

#[tauri::command]
fn home_dirs() -> Vec<(String, String)> {
    // (label, path) pairs for the sidebar shortcuts.
    let mut out = Vec::new();
    if let Some(home) = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")) {
        let home = PathBuf::from(home);
        for sub in ["Pictures", "Videos", "Downloads", "Desktop", "Images", "Vidéos", "Téléchargements", "Bureau"] {
            let p = home.join(sub);
            if p.is_dir() {
                out.push((sub.to_string(), p.to_string_lossy().to_string()));
            }
        }
        out.push(("~".to_string(), home.to_string_lossy().to_string()));
    }
    out
}

#[tauri::command]
fn list_dir(path: String) -> Result<Vec<Entry>, String> {
    scan::list_dir(Path::new(&path)).map_err(|e| e.to_string())
}

/// Kick a parallel thumbnail batch. Results stream back as "thumb" events.
#[tauri::command]
fn request_thumbs(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    items: Vec<ThumbReq>,
    tsize: u32,
) -> u64 {
    let generation = state.generation.fetch_add(1, Ordering::SeqCst) + 1;
    let cache = state.cache.clone();
    let ffmpeg = state.ffmpeg.clone();
    let gen_ref = state.generation.clone();

    std::thread::spawn(move || {
        // Split: native images go to the rayon pipeline, the rest to ffmpeg.
        // Both queues keep the submission order, i.e. the UI's sort order,
        // so what the user sees first is generated first.
        let (native, other): (Vec<_>, Vec<_>) = items.into_iter().partition(|r| {
            r.kind == EntryKind::Image && NATIVE_IMG.contains(&r.ext.as_str())
                || r.kind == EntryKind::Raw
        });

        // ffmpeg-backed thumbs (videos, exotic images): a small worker pool
        // running CONCURRENTLY with the image pipeline — previously they ran
        // one by one, after every image, which crawled on video-heavy folders.
        let queue = Arc::new(std::sync::Mutex::new(
            other.into_iter().collect::<std::collections::VecDeque<_>>(),
        ));
        // The native pipeline hands its failures over to this same pool, so
        // the workers must wait for it instead of stopping on an empty queue.
        let natives_done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut workers = Vec::new();
        for _ in 0..4 {
            let queue = queue.clone();
            let ffmpeg = ffmpeg.clone();
            let app = app.clone();
            let gen_ref = gen_ref.clone();
            let natives_done = natives_done.clone();
            workers.push(std::thread::spawn(move || loop {
                if gen_ref.load(Ordering::SeqCst) != generation {
                    break; // newer folder opened: stop wasting ffmpeg runs
                }
                let next = queue.lock().unwrap().pop_front();
                let Some(r) = next else {
                    if natives_done.load(Ordering::SeqCst) {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(25));
                    continue;
                };
                let thumb = match r.kind {
                    EntryKind::Video => ffmpeg.thumb(Path::new(&r.path), tsize),
                    _ => ffmpeg.image_thumb(Path::new(&r.path), tsize),
                };
                let _ = app.emit("thumb", ThumbEvent {
                    src: r.path,
                    thumb: thumb.clone().unwrap_or_default(),
                    ok: thumb.is_some(),
                    generation,
                });
            }));
        }

        let batch: Vec<(String, EntryKind)> =
            native.iter().map(|r| (r.path.clone(), r.kind)).collect();
        let gen_ref2 = gen_ref.clone();
        let gen_ref3 = gen_ref.clone();
        let app2 = app.clone();
        let retry = queue.clone();
        thumbs::batch(
            &cache,
            &batch,
            tsize,
            move |out| {
            if gen_ref2.load(Ordering::SeqCst) != generation {
                return; // A newer folder was opened: drop stale results.
            }
            if !out.ok {
                // Truncated or damaged file (a recovery, an interrupted
                // download): the strict Rust decoder refuses it whole, while
                // ffmpeg still returns the part of the picture that is there.
                // Queued rather than run here: this closure is on a rayon
                // worker, and a folder of broken files would fork ffmpeg on
                // every core at once.
                retry.lock().unwrap().push_back(ThumbReq {
                    path: out.src,
                    kind: EntryKind::Image,
                    ext: String::new(),
                });
                return;
            }
                let _ = app2.emit("thumb", ThumbEvent {
                    src: out.src,
                    thumb: out.thumb,
                    ok: out.ok,
                    generation,
                });
            },
            // Checked before every decode: opening another folder abandons
            // the rest of this one instead of hogging all the cores.
            move || gen_ref3.load(Ordering::SeqCst) != generation,
        );
        natives_done.store(true, Ordering::SeqCst);

        for w in workers {
            let _ = w.join();
        }
    });
    generation
}

/// Walk `root` recursively and build every missing thumbnail, so the whole
/// tree browses instantly afterwards. Progress streams back as events and
/// the walk stops as soon as a newer generation is requested.
#[tauri::command]
fn pregen_thumbs(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    root: String,
    tsize: u32,
) -> u64 {
    let generation = state.pregen_gen.fetch_add(1, Ordering::SeqCst) + 1;
    let cache = state.cache.clone();
    let ffmpeg = state.ffmpeg.clone();
    let gen_ref = state.pregen_gen.clone();

    std::thread::spawn(move || {
        let cancelled = |g: &Arc<AtomicU64>| g.load(Ordering::SeqCst) != generation;

        // 1. Collect the whole subtree first, so the total is known.
        let mut items: Vec<ThumbReq> = Vec::new();
        let filter = lumen_core::search::SearchFilter::default();
        let walk_gen = gen_ref.clone();
        lumen_core::search::search(Path::new(&root), &filter, &mut |e| {
            if cancelled(&walk_gen) {
                return false;
            }
            items.push(ThumbReq { path: e.path, kind: e.kind, ext: e.ext });
            true
        });
        let total = items.len();
        let _ = app.emit(
            "pregen-progress",
            serde_json::json!({ "generation": generation, "done": 0, "total": total }),
        );
        if total == 0 || cancelled(&gen_ref) {
            let _ = app.emit(
                "pregen-done",
                serde_json::json!({ "generation": generation, "total": total }),
            );
            return;
        }

        // 2. Same split as the on-screen batch: rayon for native images,
        //    a small ffmpeg pool for videos and exotic formats.
        let done = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let tick = {
            let app = app.clone();
            let done = done.clone();
            move || {
                let n = done.fetch_add(1, Ordering::SeqCst) + 1;
                if n % 5 == 0 || n == total {
                    let _ = app.emit(
                        "pregen-progress",
                        serde_json::json!({ "generation": generation, "done": n, "total": total }),
                    );
                }
            }
        };

        let (native, other): (Vec<_>, Vec<_>) = items.into_iter().partition(|r| {
            r.kind == EntryKind::Image && NATIVE_IMG.contains(&r.ext.as_str())
                || r.kind == EntryKind::Raw
        });

        let queue = Arc::new(std::sync::Mutex::new(
            other.into_iter().collect::<std::collections::VecDeque<_>>(),
        ));
        let mut workers = Vec::new();
        for _ in 0..4 {
            let queue = queue.clone();
            let ffmpeg = ffmpeg.clone();
            let gen_ref = gen_ref.clone();
            let tick = tick.clone();
            workers.push(std::thread::spawn(move || loop {
                if gen_ref.load(Ordering::SeqCst) != generation {
                    break;
                }
                let Some(r) = queue.lock().unwrap().pop_front() else { break };
                match r.kind {
                    EntryKind::Video => ffmpeg.thumb(Path::new(&r.path), tsize),
                    _ => ffmpeg.image_thumb(Path::new(&r.path), tsize),
                };
                tick();
            }));
        }

        let batch: Vec<(String, EntryKind)> =
            native.iter().map(|r| (r.path.clone(), r.kind)).collect();
        let gen_ref2 = gen_ref.clone();
        let retry: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
        let retry2 = retry.clone();
        let gen_ref3 = gen_ref.clone();
        thumbs::batch(
            &cache,
            &batch,
            tsize,
            move |out| {
                if gen_ref2.load(Ordering::SeqCst) == generation {
                    if !out.ok {
                        retry2.lock().unwrap().push(out.src);
                    }
                    tick();
                }
            },
            move || gen_ref3.load(Ordering::SeqCst) != generation,
        );
        // Damaged files get a second chance through ffmpeg, sequentially:
        // they are rare, and the pool is already busy with the videos.
        for path in std::mem::take(&mut *retry.lock().unwrap()) {
            if cancelled(&gen_ref) {
                break;
            }
            ffmpeg.image_thumb(Path::new(&path), tsize);
        }
        for w in workers {
            let _ = w.join();
        }

        let _ = app.emit(
            "pregen-done",
            serde_json::json!({
                "generation": generation,
                "total": total,
                "cancelled": cancelled(&gen_ref),
            }),
        );
    });
    generation
}

#[tauri::command]
fn cancel_pregen(state: State<'_, AppState>) {
    state.pregen_gen.fetch_add(1, Ordering::SeqCst);
}

/// Stop every thumbnail job, on screen and recursive alike. Called before a
/// move: a folder whose files are open by an ffmpeg worker cannot be renamed
/// on Windows, which used to force a slow copy-then-delete instead.
#[tauri::command]
fn cancel_thumb_work(state: State<'_, AppState>) {
    state.generation.fetch_add(1, Ordering::SeqCst);
    state.pregen_gen.fetch_add(1, Ordering::SeqCst);
}

/// Full-size decode for the viewer when the browser cannot display the
/// original (RAW preview extraction, exotic image conversion).
#[tauri::command]
async fn full_image(state: State<'_, AppState>, path: String, kind: EntryKind, ext: String) -> Result<String, String> {
    let cache = state.cache.clone();
    let ffmpeg = state.ffmpeg.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let src = Path::new(&path);
        match kind {
            EntryKind::Raw => {
                let jpeg = lumen_core::raw::extract_preview(src).ok_or("no preview")?;
                let key = lumen_core::fnv1a64(path.as_bytes());
                let out = cache.dir.join("play").join(format!("{key:016x}_full.jpg"));
                std::fs::write(&out, jpeg).map_err(|e| e.to_string())?;
                lumen_core::cache::ThumbCache::touch_used(&out);
                Ok(out.to_string_lossy().to_string())
            }
            EntryKind::Image if !NATIVE_IMG.contains(&ext.as_str()) => {
                // Exotic format: convert at full resolution through ffmpeg.
                ffmpeg.image_thumb(src, 16384).ok_or("convert failed".into())
            }
            _ => Ok(path),
        }
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn video_preview(state: State<'_, AppState>, path: String) -> Result<String, String> {
    let ffmpeg = state.ffmpeg.clone();
    tauri::async_runtime::spawn_blocking(move || {
        ffmpeg.preview(Path::new(&path)).ok_or_else(|| "preview failed".to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn prepare_video(state: State<'_, AppState>, path: String) -> Result<PlayableOut, String> {
    let ffmpeg = state.ffmpeg.clone();
    tauri::async_runtime::spawn_blocking(move || {
        ffmpeg.playable(Path::new(&path)).ok_or_else(|| "unplayable".to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
fn cache_size(state: State<'_, AppState>) -> u64 {
    state.cache.size_on_disk()
}

#[tauri::command]
fn cache_stats(state: State<'_, AppState>) -> lumen_core::cache::CacheStats {
    state.cache.stats()
}

/// Evict oldest cache files until the cache fits under `max_bytes`.
#[tauri::command]
async fn enforce_cache_limit(state: State<'_, AppState>, max_bytes: u64) -> Result<u64, String> {
    let cache = state.cache.clone();
    tauri::async_runtime::spawn_blocking(move || cache.enforce_limit(max_bytes))
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn clear_cache(state: State<'_, AppState>) -> u64 {
    state.cache.clear()
}

#[tauri::command]
fn delete_file(path: String) -> Result<(), String> {
    // Goes to the recycle bin, never a hard delete.
    trash::delete(&path).map_err(|e| e.to_string())
}

/// Next free "name", "name (2)", "name (3)"… inside `dir`.
/// `whole` keeps the name in one piece: a folder called "South.Park.S27" must
/// not become "South.Park (2).S27".
fn unique_dest(dir: &Path, name: &str, whole: bool) -> PathBuf {
    let cand = dir.join(name);
    if !cand.exists() {
        return cand;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() && !whole => (s.to_string(), format!(".{e}")),
        _ => (name.to_string(), String::new()),
    };
    (2..)
        .map(|i| dir.join(format!("{stem} ({i}){ext}")))
        .find(|p| !p.exists())
        .unwrap()
}

/// Total byte size of a file or of a whole folder.
fn tree_size(p: &Path) -> u64 {
    if p.is_dir() {
        std::fs::read_dir(p)
            .map(|rd| rd.flatten().map(|e| tree_size(&e.path())).sum())
            .unwrap_or(0)
    } else {
        std::fs::metadata(p).map(|m| m.len()).unwrap_or(0)
    }
}

/// Copy one file, reporting progress and honouring cancellation.
fn copy_file_progress(
    src: &Path,
    dst: &Path,
    ctx: &mut PasteCtx,
) -> std::io::Result<()> {
    use std::io::{Read, Write};
    let mut fin = std::fs::File::open(src)?;
    let mut fout = std::fs::File::create(dst)?;
    let mut buf = vec![0u8; 1 << 20]; // 1 MiB
    loop {
        if ctx.cancelled() {
            return Err(std::io::Error::new(std::io::ErrorKind::Interrupted, "cancelled"));
        }
        let n = fin.read(&mut buf)?;
        if n == 0 {
            break;
        }
        fout.write_all(&buf[..n])?;
        ctx.advance(n as u64, src);
    }
    fout.flush()?;
    // Keep the original timestamps on the copy, like Explorer does.
    if let Ok(meta) = std::fs::metadata(src) {
        let times = std::fs::FileTimes::new()
            .set_modified(meta.modified().unwrap_or_else(|_| std::time::SystemTime::now()));
        let _ = fout.set_times(times);
    }
    Ok(())
}

fn copy_recursive(src: &Path, dst: &Path, ctx: &mut PasteCtx) -> std::io::Result<()> {
    if src.is_dir() {
        std::fs::create_dir_all(dst)?;
        for e in std::fs::read_dir(src)? {
            let e = e?;
            copy_recursive(&e.path(), &dst.join(e.file_name()), ctx)?;
        }
        Ok(())
    } else {
        copy_file_progress(src, dst, ctx)
    }
}

/// True when both paths designate the SAME entry on disk. Windows is
/// case-insensitive, so "Beach" and "beach" are one and the same folder:
/// comparing strings would have us delete the very thing we are moving.
fn same_entry(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

/// True when `inner` is `outer` itself or lives somewhere inside it.
fn is_within(inner: &Path, outer: &Path) -> bool {
    match (inner.canonicalize(), outer.canonicalize()) {
        (Ok(i), Ok(o)) => i.starts_with(&o),
        _ => false,
    }
}

/// Overwriting sends the old entry to the recycle bin - never a hard delete,
/// so a wrong answer in the conflict dialog stays recoverable.
fn trash_path(p: &Path) -> std::io::Result<()> {
    trash::delete(p).map_err(|e| std::io::Error::other(e.to_string()))
}

fn remove_tree(p: &Path) -> std::io::Result<()> {
    if p.is_dir() {
        std::fs::remove_dir_all(p)
    } else {
        std::fs::remove_file(p)
    }
}

/// Move (or copy) one entry to a target that does not exist yet.
/// Why a move could not be completed. The distinction is what keeps data
/// safe: only a FAILED COPY leaves rubbish at the destination worth cleaning
/// up. When the copy went through and merely the source could not be removed,
/// the destination holds the only complete copy and must never be touched.
enum PlaceErr {
    /// Nothing usable was written at the destination.
    Copy(std::io::Error),
    /// The destination is complete; the source is still there, in whole or in
    /// part (a file locked by a thumbnail job, an antivirus, another app...).
    SourceKept(std::io::Error),
}

impl PlaceErr {
    fn io(&self) -> &std::io::Error {
        match self {
            PlaceErr::Copy(e) | PlaceErr::SourceKept(e) => e,
        }
    }
}

/// A directory whose files are still open cannot be renamed on Windows, and
/// the lock is usually a passing thing (our own ffmpeg worker finishing a
/// thumbnail). Give it a few chances before falling back to a full copy.
fn rename_retry(src: &Path, dst: &Path) -> bool {
    for attempt in 0..5u64 {
        if std::fs::rename(src, dst).is_ok() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(60 * (attempt + 1)));
    }
    false
}

fn remove_tree_retry(p: &Path) -> std::io::Result<()> {
    let mut last = remove_tree(p);
    for attempt in 0..4u64 {
        if last.is_ok() || !p.exists() {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(80 * (attempt + 1)));
        last = remove_tree(p);
    }
    last
}

fn place(src: &Path, dst: &Path, cut: bool, ctx: &mut PasteCtx) -> Result<(), PlaceErr> {
    if cut && rename_retry(src, dst) {
        ctx.done += tree_size(dst);
        return Ok(());
    }
    let expected = tree_size(src);
    copy_recursive(src, dst, ctx).map_err(PlaceErr::Copy)?;
    if cut {
        // Deleting the source is the point of no return: only take it once
        // every byte is verifiably at the destination.
        let copied = tree_size(dst);
        if copied < expected {
            return Err(PlaceErr::Copy(std::io::Error::other(format!(
                "incomplete copy: {copied} of {expected} bytes"
            ))));
        }
        remove_tree_retry(src).map_err(PlaceErr::SourceKept)?;
    }
    Ok(())
}

/// `place` inside a merge: a source that could not be deleted is recorded and
/// stepped over, never a reason to undo a good copy.
fn place_in_merge(
    src: &Path,
    dst: &Path,
    cut: bool,
    ctx: &mut PasteCtx,
    out: &mut PasteOutcome,
) -> std::io::Result<()> {
    match place(src, dst, cut, ctx) {
        Ok(()) => Ok(()),
        Err(PlaceErr::SourceKept(e)) => {
            out.kept += 1;
            out.notes.push(format!(
                "{}: {e}",
                src.file_name().unwrap_or_default().to_string_lossy()
            ));
            Ok(())
        }
        Err(PlaceErr::Copy(e)) => {
            let _ = remove_tree(dst);
            Err(e)
        }
    }
}

/// Merge `src` into the existing folder `dst`, the way Explorer does: the
/// destination is never wiped, only the colliding leaves are arbitrated by
/// `policy`. Anything skipped simply stays where it is.
fn merge_into(
    src: &Path,
    dst: &Path,
    cut: bool,
    policy: &str,
    ctx: &mut PasteCtx,
    out: &mut PasteOutcome,
) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for e in std::fs::read_dir(src)? {
        if ctx.cancelled() {
            return Err(std::io::Error::new(std::io::ErrorKind::Interrupted, "cancelled"));
        }
        let e = e?;
        let from = e.path();
        let name = e.file_name();
        let to = dst.join(&name);
        if to.exists() {
            if same_entry(&from, &to) {
                continue; // same file seen from both sides
            }
            if from.is_dir() && to.is_dir() {
                merge_into(&from, &to, cut, policy, ctx, out)?;
                continue;
            }
            match policy {
                "skip" => {
                    out.skipped += 1;
                    ctx.done += tree_size(&from);
                    continue;
                }
                "replace" => {
                    if trash_path(&to).is_err() {
                        out.skipped += 1;
                        continue;
                    }
                    out.replaced += 1;
                }
                _ => {
                    let uniq = unique_dest(dst, &name.to_string_lossy(), from.is_dir());
                    place_in_merge(&from, &uniq, cut, ctx, out)?;
                    continue;
                }
            }
        }
        place_in_merge(&from, &to, cut, ctx, out)?;
    }
    if cut {
        // Only succeeds once everything really moved: whatever was skipped
        // keeps the source folder alive, which is exactly what we want.
        let _ = std::fs::remove_dir(src);
    }
    Ok(())
}

/// Shared state of a running copy/move: progress emission + cancellation.
struct PasteCtx {
    app: tauri::AppHandle,
    generation: u64,
    gen_ref: Arc<AtomicU64>,
    total: u64,
    done: u64,
    last_emit: std::time::Instant,
}

impl PasteCtx {
    fn cancelled(&self) -> bool {
        self.gen_ref.load(Ordering::SeqCst) != self.generation
    }
    fn advance(&mut self, bytes: u64, current: &Path) {
        self.done += bytes;
        // Throttle: the UI only needs a few updates per second.
        if self.last_emit.elapsed() >= std::time::Duration::from_millis(120) {
            self.last_emit = std::time::Instant::now();
            self.emit(current);
        }
    }
    fn emit(&self, current: &Path) {
        let _ = self.app.emit(
            "paste-progress",
            serde_json::json!({
                "generation": self.generation,
                "done": self.done,
                "total": self.total,
                "current": current.file_name().map(|n| n.to_string_lossy().to_string()),
            }),
        );
    }
}

/// Names inside `dest` that already exist among the pasted items.
#[tauri::command]
async fn check_conflicts(paths: Vec<String>, dest: String) -> Result<Vec<Conflict>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let dest_dir = PathBuf::from(&dest);
        let mut out = Vec::new();
        for p in paths {
            let src = PathBuf::from(&p);
            let Some(name) = src.file_name() else { continue };
            let direct = dest_dir.join(name);
            if !direct.exists() || same_entry(&src, &direct) {
                continue; // free slot, or the item is already exactly there
            }
            if src.parent().is_some_and(|q| same_entry(q, &dest_dir)) {
                continue;
            }
            out.push(Conflict {
                name: name.to_string_lossy().to_string(),
                dir: src.is_dir() && direct.is_dir(),
                src: conflict_side(&src),
                dst: conflict_side(&direct),
            });
        }
        Ok(out)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// One side of a clash, with everything the dialog needs to compare them.
#[derive(Serialize)]
struct ConflictSide {
    path: String,
    size: u64,
    mtime: i64,
    /// Pixel dimensions, 0 when unknown (folder, video, unreadable header).
    w: u32,
    h: u32,
}

fn conflict_side(p: &Path) -> ConflictSide {
    let md = std::fs::metadata(p).ok();
    let is_dir = md.as_ref().is_some_and(|m| m.is_dir());
    let (w, h) = if is_dir {
        (0, 0)
    } else {
        lumen_core::image_dims(p).unwrap_or((0, 0))
    };
    ConflictSide {
        path: p.to_string_lossy().to_string(),
        size: if is_dir {
            tree_size(p)
        } else {
            md.as_ref().map(|m| m.len()).unwrap_or(0)
        },
        mtime: md
            .as_ref()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0),
        w,
        h,
    }
}

/// One name already taken in the destination. `dir` marks a folder-on-folder
/// clash, which is merged rather than replaced.
#[derive(Serialize)]
struct Conflict {
    name: String,
    dir: bool,
    /// The item being pasted, and the one already in the destination.
    src: ConflictSide,
    dst: ConflictSide,
}

/// Result of a paste, detailed enough to be undone.
#[derive(Debug, Default, Serialize)]
struct PasteOutcome {
    /// (source, destination) of every item actually transferred.
    pairs: Vec<(String, String)>,
    /// Items overwritten: they cannot be restored by an undo.
    replaced: u32,
    /// Folders merged into an existing one: moving them back would drag the
    /// destination's own files along, so an undo is not offered.
    merged: u32,
    /// Copied to the destination but the source could not be removed: both
    /// copies are still on disk, on purpose.
    kept: u32,
    /// Human-readable reasons for `kept`, shown to the user.
    notes: Vec<String>,
    skipped: u32,
    cancelled: bool,
}

/// Paste files/folders into `dest`. `cut` moves, otherwise copies.
/// `policy` decides what happens on a name clash: "rename" (default),
/// "replace" or "skip".
#[tauri::command]
async fn paste_files(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    paths: Vec<String>,
    dest: String,
    cut: bool,
    policy: Option<String>,
) -> Result<PasteOutcome, String> {
    let generation = state.paste_gen.fetch_add(1, Ordering::SeqCst) + 1;
    let gen_ref = state.paste_gen.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let dest_dir = PathBuf::from(&dest);
        let policy = policy.unwrap_or_else(|| "rename".into());
        let total: u64 = paths.iter().map(|p| tree_size(Path::new(p))).sum();
        let mut ctx = PasteCtx {
            app,
            generation,
            gen_ref,
            total,
            done: 0,
            last_emit: std::time::Instant::now(),
        };
        let mut out = PasteOutcome::default();

        for p in &paths {
            if ctx.cancelled() {
                out.cancelled = true;
                break;
            }
            let src = PathBuf::from(p);
            let Some(name) = src.file_name().map(|n| n.to_string_lossy().to_string()) else {
                continue;
            };
            // A folder can never receive itself or one of its own ancestors.
            if is_within(&dest_dir, &src) {
                out.skipped += 1;
                ctx.done += tree_size(&src);
                continue;
            }
            if cut && src.parent().is_some_and(|q| same_entry(q, &dest_dir)) {
                continue; // moving inside its own folder: nothing to do
            }

            let direct = dest_dir.join(&name);
            let target = if direct.exists() {
                // "Beach" landing on "beach" may be the very same folder:
                // touching it would destroy the source AND the destination.
                if same_entry(&src, &direct) {
                    out.skipped += 1;
                    ctx.done += tree_size(&src);
                    continue;
                }
                if policy == "skip" {
                    out.skipped += 1;
                    ctx.done += tree_size(&src);
                    continue;
                }
                // Two folders of the same name are merged, never swapped.
                if src.is_dir() && direct.is_dir() && policy != "rename" {
                    match merge_into(&src, &direct, cut, &policy, &mut ctx, &mut out) {
                        Ok(()) => {}
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                            out.cancelled = true;
                            break;
                        }
                        Err(e) => return Err(e.to_string()),
                    }
                    out.merged += 1;
                    out.pairs.push((p.clone(), direct.to_string_lossy().to_string()));
                    ctx.emit(&src);
                    continue;
                }
                if policy == "replace" {
                    // To the recycle bin, so the choice remains reversible.
                    if trash_path(&direct).is_err() {
                        out.skipped += 1;
                        continue;
                    }
                    out.replaced += 1;
                    direct
                } else {
                    unique_dest(&dest_dir, &name, src.is_dir())
                }
            } else {
                direct
            };

            // Past this point the target never pre-existed, so cleaning it up
            // after a FAILED COPY can only remove what we ourselves wrote.
            match place(&src, &target, cut, &mut ctx) {
                Ok(()) => {}
                Err(PlaceErr::Copy(e)) => {
                    let _ = remove_tree(&target); // don't leave half a copy
                    if e.kind() == std::io::ErrorKind::Interrupted {
                        out.cancelled = true;
                        break;
                    }
                    return Err(e.to_string());
                }
                Err(err @ PlaceErr::SourceKept(_)) => {
                    // The destination is complete. Keep BOTH copies and say
                    // so: erasing either one here is how data gets lost.
                    out.kept += 1;
                    out.notes.push(format!(
                        "{}: {}",
                        src.file_name().unwrap_or_default().to_string_lossy(),
                        err.io()
                    ));
                    out.pairs.push((p.clone(), target.to_string_lossy().to_string()));
                    ctx.emit(&src);
                    continue;
                }
            }
            out.pairs.push((p.clone(), target.to_string_lossy().to_string()));
            ctx.emit(&src);
        }

        let _ = ctx.app.emit(
            "paste-done",
            serde_json::json!({ "generation": generation, "cancelled": out.cancelled }),
        );
        Ok(out)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
fn cancel_paste(state: State<'_, AppState>) {
    state.paste_gen.fetch_add(1, Ordering::SeqCst);
}

/// Put back files that were sent to the recycle bin (undo of a delete).
#[tauri::command]
async fn restore_trashed(paths: Vec<String>) -> Result<u32, String> {
    tauri::async_runtime::spawn_blocking(move || {
        #[cfg(windows)]
        {
            let wanted: std::collections::HashSet<String> =
                paths.iter().map(|p| p.to_lowercase()).collect();
            let items = trash::os_limited::list().map_err(|e| e.to_string())?;
            let mine: Vec<_> = items
                .into_iter()
                .filter(|it| {
                    let full = it.original_parent.join(&it.name);
                    wanted.contains(&full.to_string_lossy().to_lowercase())
                })
                .collect();
            let n = mine.len() as u32;
            trash::os_limited::restore_all(mine).map_err(|e| e.to_string())?;
            Ok(n)
        }
        #[cfg(not(windows))]
        {
            let _ = paths;
            Err("Windows only".to_string())
        }
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Details shown by the properties dialog.
#[derive(Debug, Default, Serialize)]
struct FileProps {
    path: String,
    size: u64,
    is_dir: bool,
    files: u64,
    folders: u64,
    created: i64,
    modified: i64,
    readonly: bool,
    hidden: bool,
}

#[tauri::command]
async fn file_props(path: String) -> Result<FileProps, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let p = PathBuf::from(&path);
        let meta = std::fs::metadata(&p).map_err(|e| e.to_string())?;
        let ms = |t: std::io::Result<std::time::SystemTime>| {
            t.ok()
                .and_then(|x| x.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0)
        };
        let mut out = FileProps {
            path: path.clone(),
            is_dir: meta.is_dir(),
            size: if meta.is_dir() { tree_size(&p) } else { meta.len() },
            created: ms(meta.created()),
            modified: ms(meta.modified()),
            readonly: meta.permissions().readonly(),
            ..Default::default()
        };
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            out.hidden = meta.file_attributes() & 0x2 != 0;
        }
        if meta.is_dir() {
            fn count(p: &Path, files: &mut u64, folders: &mut u64) {
                if let Ok(rd) = std::fs::read_dir(p) {
                    for e in rd.flatten() {
                        if e.path().is_dir() {
                            *folders += 1;
                            count(&e.path(), files, folders);
                        } else {
                            *files += 1;
                        }
                    }
                }
            }
            count(&p, &mut out.files, &mut out.folders);
        }
        Ok(out)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
fn set_attributes(path: String, readonly: bool, hidden: bool) -> Result<(), String> {
    let p = PathBuf::from(&path);
    let meta = std::fs::metadata(&p).map_err(|e| e.to_string())?;
    let mut perms = meta.permissions();
    perms.set_readonly(readonly);
    std::fs::set_permissions(&p, perms).map_err(|e| e.to_string())?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        use windows_sys::Win32::Storage::FileSystem::SetFileAttributesW;
        const HIDDEN: u32 = 0x2;
        let cur = std::fs::metadata(&p).map_err(|e| e.to_string())?.file_attributes();
        let next = if hidden { cur | HIDDEN } else { cur & !HIDDEN };
        let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
        if unsafe { SetFileAttributesW(wide.as_ptr(), next) } == 0 {
            return Err("SetFileAttributes failed".into());
        }
    }
    Ok(())
}

/// Files on the clipboard, as Explorer and the desktop understand them.
#[derive(Serialize)]
struct ClipFiles {
    paths: Vec<String>,
    cut: bool,
}

/// Publish a file selection on the Windows clipboard (Ctrl+C / Ctrl+X).
#[tauri::command]
fn clip_set_files(paths: Vec<String>, cut: bool) -> Result<(), String> {
    #[cfg(windows)]
    {
        winclip::set_files(&paths, cut)
    }
    #[cfg(not(windows))]
    {
        let _ = (paths, cut);
        Ok(())
    }
}

/// Read the files Explorer (or we) put on the clipboard.
#[tauri::command]
fn clip_get_files() -> Option<ClipFiles> {
    #[cfg(windows)]
    {
        winclip::get_files().map(|(paths, cut)| ClipFiles { paths, cut })
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// Free and total bytes of the volume holding `path`.
#[tauri::command]
fn disk_space(path: String) -> Option<(u64, u64)> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
        let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
        let (mut free, mut total) = (0u64, 0u64);
        let ok = unsafe {
            GetDiskFreeSpaceExW(wide.as_ptr(), std::ptr::null_mut(), &mut total, &mut free)
        };
        if ok == 0 {
            return None;
        }
        Some((free, total))
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        None
    }
}

// ---------- XMP sidecars ----------
#[tauri::command]
fn write_xmp(path: String, rating: i64, label: String, tags: String) -> Result<(), String> {
    let media = Path::new(&path);
    // Keep whatever orientation the sidecar already carries: ratings and
    // keywords must not undo a rotation recorded for Lightroom & co.
    let orientation = lumen_core::xmp::read(media).map(|m| m.orientation).unwrap_or(0);
    lumen_core::xmp::write(
        media,
        &lumen_core::xmp::XmpMeta { rating, label, tags, orientation },
    )
    .map_err(|e| e.to_string())
}

/// Read the sidecars of a folder, so ratings written by Lightroom & co.
/// show up here too.
#[tauri::command]
async fn read_xmp(paths: Vec<String>) -> Result<Vec<(String, lumen_core::xmp::XmpMeta)>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        use rayon::prelude::*;
        Ok(paths
            .par_iter()
            .filter_map(|p| lumen_core::xmp::read(Path::new(p)).map(|m| (p.clone(), m)))
            .collect())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Create a folder inside `parent` ("New folder", "New folder (2)"...).
#[tauri::command]
fn create_folder(parent: String, name: String) -> Result<String, String> {
    let dest = unique_dest(Path::new(&parent), &name, true);
    std::fs::create_dir(&dest).map_err(|e| e.to_string())?;
    Ok(dest.to_string_lossy().to_string())
}

#[tauri::command]
fn rename_file(path: String, new_name: String) -> Result<String, String> {
    let p = PathBuf::from(&path);
    let dest = p.with_file_name(&new_name);
    if dest.exists() {
        return Err("exists".into());
    }
    std::fs::rename(&p, &dest).map_err(|e| e.to_string())?;
    Ok(dest.to_string_lossy().to_string())
}

#[tauri::command]
fn reveal(path: String) {
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("explorer")
            .arg("/select,")
            .arg(&path)
            .spawn();
    }
    #[cfg(not(windows))]
    {
        if let Some(parent) = Path::new(&path).parent() {
            let _ = std::process::Command::new("xdg-open").arg(parent).spawn();
        }
    }
}

#[tauri::command]
fn get_settings(app: tauri::AppHandle) -> serde_json::Value {
    let p = settings_path(&app);
    std::fs::read_to_string(p)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(serde_json::json!({}))
}

#[tauri::command]
fn save_settings(app: tauri::AppHandle, value: serde_json::Value) {
    let p = settings_path(&app);
    if let Some(parent) = p.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(p, serde_json::to_string_pretty(&value).unwrap_or_default());
}

// ---------- Metadata (rating / colour / pick-reject / tags) ----------
#[tauri::command]
fn dir_meta(state: State<'_, AppState>, parent: String) -> Vec<lumen_core::cache::MetaRow> {
    state.cache.dir_meta(&parent)
}

#[tauri::command]
fn set_meta(
    state: State<'_, AppState>,
    path: String,
    parent: String,
    rating: Option<i64>,
    color: Option<String>,
    flag: Option<String>,
    tags: Option<String>,
    rot: Option<i64>,
) {
    state.cache.set_meta(&path, &parent, rating, color, flag, tags, rot);
}

/// Rotate by writing the EXIF Orientation tag in place (2 bytes, pixels
/// untouched). Returns the paths that actually carried a tag to patch.
#[tauri::command]
async fn rotate_exif(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    paths: Vec<String>,
    quarters: i32,
    token: u64,
) -> Result<Vec<String>, String> {
    let cache = state.cache.clone();
    let gen_ref = state.rot_gen.clone();
    // Claim this run: a newer one (or the Cancel button) bumps the counter.
    gen_ref.store(token, Ordering::SeqCst);
    tauri::async_runtime::spawn_blocking(move || {
        let total = paths.len();
        let mut done = Vec::new();
        let mut last = std::time::Instant::now();
        for (i, p) in paths.into_iter().enumerate() {
            if gen_ref.load(Ordering::SeqCst) != token {
                break; // cancelled
            }
            let path = PathBuf::from(&p);
            let current = lumen_core::exifwrite::get_orientation(&path);
            let next = lumen_core::exifwrite::rotate_orientation(current, quarters);
            let patched = lumen_core::exifwrite::set_orientation(&path, next).unwrap_or(false);
            if patched {
                // The timestamps are deliberately preserved, so the cache
                // cannot notice the change on its own: drop it explicitly.
                cache.invalidate(&p);
                done.push(p.clone());
            }
            // No sidecar is written for raws: Lightroom and Bridge would need
            // one to see the rotation, but an extra file next to every picture
            // is not wanted. The user is told instead.
            // Throttled so a thousand files do not flood the event channel.
            if last.elapsed() >= std::time::Duration::from_millis(120) || i + 1 == total {
                last = std::time::Instant::now();
                let _ = app.emit(
                    "rot-progress",
                    serde_json::json!({
                        "token": token, "done": i + 1, "total": total,
                        "current": std::path::Path::new(&p)
                            .file_name().unwrap_or_default().to_string_lossy(),
                    }),
                );
            }
        }
        let cancelled = gen_ref.load(Ordering::SeqCst) != token;
        let _ = app.emit(
            "rot-done",
            serde_json::json!({ "token": token, "count": done.len(), "cancelled": cancelled }),
        );
        Ok(done)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Delete everything in the `play` folder: the converted videos and the
/// full-size previews, which are copies of the user's own media.
#[tauri::command]
fn clear_play(state: State<'_, AppState>) -> u64 {
    state.cache.clear_play()
}

/// Remember whether `play` must be wiped when the window closes.
#[tauri::command]
fn set_clear_play_on_exit(state: State<'_, AppState>, on: bool) {
    state.clear_play_on_exit.store(on, Ordering::SeqCst);
}

/// Stop a running rotation batch.
#[tauri::command]
fn cancel_rotate(state: State<'_, AppState>) {
    state.rot_gen.fetch_add(1_000_000, Ordering::SeqCst);
}

/// Remember where a video was left, so it resumes there next time.
#[tauri::command]
fn set_video_pos(state: State<'_, AppState>, path: String, parent: String, pos: f64) {
    state.cache.set_pos(&path, &parent, pos);
}

/// Capture times (EXIF DateTimeOriginal) in UNIX milliseconds, read in
/// parallel and memoised in the cache database. 0 = no EXIF date.
#[tauri::command]
async fn capture_dates(
    state: State<'_, AppState>,
    paths: Vec<String>,
    parent: String,
) -> Result<Vec<(String, i64)>, String> {
    let cache = state.cache.clone();
    tauri::async_runtime::spawn_blocking(move || {
        use rayon::prelude::*;
        let out: Vec<(String, i64)> = paths
            .par_iter()
            .map(|p| {
                let known = cache.taken(p);
                if known != 0 {
                    return (p.clone(), known);
                }
                let taken = lumen_core::exifwrite::capture_time(Path::new(p));
                // Fall back to the file date so sorting never collapses.
                let value = if taken != 0 {
                    taken
                } else {
                    std::fs::metadata(p)
                        .ok()
                        .and_then(|m| m.modified().ok())
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_millis() as i64)
                        .unwrap_or(0)
                };
                cache.set_taken(p, &parent, value);
                (p.clone(), value)
            })
            .collect();
        Ok(out)
    })
    .await
    .map_err(|e| e.to_string())?
}

// ---------- EXIF / dimensions ----------
/// Header-only pixel dimensions — fast enough to call before display.
#[tauri::command]
fn image_dims(path: String) -> Option<(u32, u32)> {
    lumen_core::image_dims(Path::new(&path))
}

/// What the camera recorded about a shot: capture time and gear.
#[derive(Serialize, Default)]
struct PhotoInfo {
    /// Capture time in UNIX milliseconds; 0 when the file carries none.
    taken: i64,
    /// "SONY ILCE-7M3" - make and model, without the usual duplication.
    camera: String,
    lens: String,
}

#[tauri::command]
async fn photo_info(path: String) -> PhotoInfo {
    tauri::async_runtime::spawn_blocking(move || {
        let p = Path::new(&path);
        let mut info = PhotoInfo {
            taken: lumen_core::exifwrite::capture_time(p),
            ..Default::default()
        };
        let fields: std::collections::HashMap<String, String> =
            lumen_core::exif_summary(p).into_iter().collect();
        let make = fields.get("Make").cloned().unwrap_or_default();
        let model = fields.get("Model").cloned().unwrap_or_default();
        let make = make.trim().trim_matches('"').to_string();
        let model = model.trim().trim_matches('"').to_string();
        // Canon writes "Canon" + "Canon EOS 5D": don't say it twice.
        info.camera = if model.to_lowercase().starts_with(&make.to_lowercase()) || make.is_empty() {
            model
        } else if model.is_empty() {
            make
        } else {
            format!("{make} {model}")
        };
        info.lens = fields
            .get("LensModel")
            .map(|v| v.trim().trim_matches('"').to_string())
            .unwrap_or_default();
        info
    })
    .await
    .unwrap_or_default()
}

/// Pixel dimensions of a batch of images, read from the headers only.
#[tauri::command]
async fn image_sizes(paths: Vec<String>) -> Result<Vec<(String, u32, u32)>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        use rayon::prelude::*;
        Ok(paths
            .par_iter()
            .filter_map(|p| {
                lumen_core::image_dims(Path::new(p)).map(|(w, h)| (p.clone(), w, h))
            })
            .collect())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn read_exif(path: String) -> Result<Vec<(String, String)>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let p = PathBuf::from(&path);
        let mut out = Vec::new();
        if let Some((w, h)) = lumen_core::image_dims(&p) {
            out.push(("Dimensions".to_string(), format!("{w} × {h}")));
        }
        out.extend(lumen_core::exif_summary(&p));
        Ok(out)
    })
    .await
    .map_err(|e| e.to_string())?
}

// ---------- Folder watcher (auto-refresh) ----------
#[tauri::command]
fn watch_dir(app: tauri::AppHandle, watch: State<'_, WatchState>, path: String) {
    use notify::Watcher;
    let mut guard = watch.0.lock().unwrap();
    *guard = None; // drop the previous watcher first
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let watcher = notify::recommended_watcher(move |res: Result<notify::Event, notify::Error>| {
        if res.is_ok() {
            let _ = tx.send(());
        }
    })
    .ok()
    .and_then(|mut w| {
        w.watch(Path::new(&path), notify::RecursiveMode::NonRecursive)
            .ok()?;
        Some(w)
    });
    *guard = watcher;
    // Debounce bursts (file copies emit many events) into one UI refresh.
    std::thread::spawn(move || {
        while rx.recv().is_ok() {
            while rx
                .recv_timeout(std::time::Duration::from_millis(400))
                .is_ok()
            {}
            let _ = app.emit("dir-changed", ());
        }
    });
}

// ---------- Recursive search (streaming) ----------
#[tauri::command]
fn search_dir(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    root: String,
    filter: lumen_core::search::SearchFilter,
    token: u64,
) -> u64 {
    // The events carry the CALLER's token, not our internal counter: the walk
    // starts emitting before this command's reply reaches the front end, which
    // would otherwise have no way to recognise the first batches.
    let generation = state.search_gen.fetch_add(1, Ordering::SeqCst) + 1;
    let gen_ref = state.search_gen.clone();
    std::thread::spawn(move || {
        let mut batch: Vec<Entry> = Vec::new();
        lumen_core::search::search(Path::new(&root), &filter, &mut |e| {
            if gen_ref.load(Ordering::SeqCst) != generation {
                return false;
            }
            batch.push(e);
            if batch.len() >= 50 {
                let _ = app.emit(
                    "search-hit",
                    serde_json::json!({ "generation": token, "items": batch }),
                );
                batch.clear();
            }
            true
        });
        if gen_ref.load(Ordering::SeqCst) == generation {
            let _ = app.emit(
                "search-hit",
                serde_json::json!({ "generation": token, "items": batch }),
            );
            let _ = app.emit("search-done", serde_json::json!({ "generation": token }));
        }
    });
    generation
}

#[tauri::command]
fn cancel_search(state: State<'_, AppState>) {
    state.search_gen.fetch_add(1, Ordering::SeqCst);
}

// ---------- Duplicates ----------
#[tauri::command]
async fn find_duplicates(
    state: State<'_, AppState>,
    items: Vec<ThumbReq>,
    near: bool,
) -> Result<Vec<lumen_core::duplicates::DupGroup>, String> {
    let cache = state.cache.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let paths: Vec<String> = items.iter().map(|r| r.path.clone()).collect();
        let mut groups = lumen_core::duplicates::exact_duplicates(&paths);
        if near {
            let pairs: Vec<(String, String)> = items
                .iter()
                .filter(|r| matches!(r.kind, EntryKind::Image | EntryKind::Raw))
                .map(|r| {
                    let small = cache.any_thumb(&r.path).unwrap_or_else(|| r.path.clone());
                    (r.path.clone(), small)
                })
                .collect();
            groups.extend(lumen_core::duplicates::near_duplicates(&pairs));
        }
        Ok(groups)
    })
    .await
    .map_err(|e| e.to_string())?
}

// ---------- Video tools ----------
#[tauri::command]
async fn cut_video(
    state: State<'_, AppState>,
    path: String,
    start: f64,
    end: f64,
) -> Result<String, String> {
    let ffmpeg = state.ffmpeg.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let src = PathBuf::from(&path);
        let stem = src.file_stem().unwrap_or_default().to_string_lossy().to_string();
        let ext = src.extension().unwrap_or_default().to_string_lossy().to_string();
        let dir = src.parent().unwrap_or(Path::new("."));
        let out = unique_dest(dir, &format!("{stem}_coupe.{ext}"), false);
        ffmpeg.cut_video(&src, start, end, &out)?;
        Ok(out.to_string_lossy().to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

// ---------- Batch conversion / rename ----------
#[tauri::command]
async fn convert_batch(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    paths: Vec<String>,
    format: String,
    max_dim: u32,
    quality: u32,
) -> Result<u32, String> {
    let ffmpeg = state.ffmpeg.clone();
    tauri::async_runtime::spawn_blocking(move || {
        use rayon::prelude::*;
        let total = paths.len();
        let done = std::sync::atomic::AtomicUsize::new(0);
        let ok: u32 = paths
            .par_iter()
            .map(|p| {
                let src = PathBuf::from(p);
                let stem = src.file_stem().unwrap_or_default().to_string_lossy().to_string();
                let dir = src.parent().unwrap_or(Path::new("."));
                let out = unique_dest(dir, &format!("{stem}.{format}"), false);
                let r = ffmpeg.convert_image(&src, &out, max_dim, quality);
                let n = done.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                let _ = app.emit("convert-progress", serde_json::json!({ "done": n, "total": total }));
                u32::from(r.is_ok())
            })
            .sum();
        Ok(ok)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Rename `pairs` of (absolute source path, new file name). Skips collisions.
#[tauri::command]
fn rename_batch(pairs: Vec<(String, String)>) -> Result<u32, String> {
    let mut ok = 0u32;
    for (src, new_name) in pairs {
        let p = PathBuf::from(&src);
        let dest = p.with_file_name(&new_name);
        if dest.exists() {
            continue;
        }
        if std::fs::rename(&p, &dest).is_ok() {
            ok += 1;
        }
    }
    Ok(ok)
}

// ---------- Windows integration ----------
/// Extract the Windows shell icon for a file type as a 32x32 PNG,
/// cached per extension under the app cache dir.
#[tauri::command]
fn file_icon(state: State<'_, AppState>, path: String) -> Option<String> {
    #[cfg(windows)]
    {
        let p = Path::new(&path);
        let ext = p
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        // exe/lnk/ico carry per-file icons; everything else is per-extension.
        let per_file = matches!(ext.as_str(), "exe" | "lnk" | "ico");
        let key = if per_file {
            format!("{:016x}", lumen_core::fnv1a64(path.as_bytes()))
        } else if ext.is_empty() {
            "_none".to_string()
        } else {
            ext.clone()
        };
        let dir = state.cache.dir.join("icons");
        let out = dir.join(format!("{key}.png"));
        if out.exists() {
            return Some(out.to_string_lossy().to_string());
        }
        std::fs::create_dir_all(&dir).ok();

        unsafe {
            use windows_sys::Win32::Graphics::Gdi::{
                CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, SelectObject,
                BITMAPINFO, BI_RGB, DIB_RGB_COLORS,
            };
            use windows_sys::Win32::UI::Shell::{
                SHGetFileInfoW, SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON,
                SHGFI_USEFILEATTRIBUTES,
            };
            use windows_sys::Win32::UI::WindowsAndMessaging::{DestroyIcon, DrawIconEx, DI_NORMAL};

            let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
            let mut sfi: SHFILEINFOW = std::mem::zeroed();
            let flags = if per_file {
                SHGFI_ICON | SHGFI_LARGEICON
            } else {
                SHGFI_ICON | SHGFI_LARGEICON | SHGFI_USEFILEATTRIBUTES
            };
            let ok = SHGetFileInfoW(
                wide.as_ptr(),
                0x80, // FILE_ATTRIBUTE_NORMAL
                &mut sfi,
                std::mem::size_of::<SHFILEINFOW>() as u32,
                flags,
            );
            if ok == 0 || sfi.hIcon.is_null() {
                return None;
            }

            const SIZE: i32 = 32;
            let hdc = CreateCompatibleDC(std::ptr::null_mut());
            let mut bmi: BITMAPINFO = std::mem::zeroed();
            bmi.bmiHeader.biSize = std::mem::size_of_val(&bmi.bmiHeader) as u32;
            bmi.bmiHeader.biWidth = SIZE;
            bmi.bmiHeader.biHeight = -SIZE; // top-down
            bmi.bmiHeader.biPlanes = 1;
            bmi.bmiHeader.biBitCount = 32;
            bmi.bmiHeader.biCompression = BI_RGB as u32;
            let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
            let hbmp = CreateDIBSection(hdc, &bmi, DIB_RGB_COLORS, &mut bits, std::ptr::null_mut(), 0);
            if hbmp.is_null() {
                DeleteDC(hdc);
                DestroyIcon(sfi.hIcon);
                return None;
            }
            let old = SelectObject(hdc, hbmp as _);
            DrawIconEx(hdc, 0, 0, sfi.hIcon, SIZE, SIZE, 0, std::ptr::null_mut(), DI_NORMAL);
            let mut rgba =
                std::slice::from_raw_parts(bits as *const u8, (SIZE * SIZE * 4) as usize).to_vec();
            SelectObject(hdc, old);
            DeleteObject(hbmp as _);
            DeleteDC(hdc);
            DestroyIcon(sfi.hIcon);

            // BGRA -> RGBA; legacy icons without alpha get opaque non-black pixels.
            let has_alpha = rgba.chunks_exact(4).any(|c| c[3] != 0);
            for c in rgba.chunks_exact_mut(4) {
                c.swap(0, 2);
                if !has_alpha && (c[0] != 0 || c[1] != 0 || c[2] != 0) {
                    c[3] = 255;
                }
            }
            if lumen_core::save_rgba_png(SIZE as u32, SIZE as u32, rgba, &out) {
                return Some(out.to_string_lossy().to_string());
            }
        }
        None
    }
    #[cfg(not(windows))]
    {
        let _ = (state, path);
        None
    }
}

#[tauri::command]
fn set_wallpaper(path: String) -> Result<(), String> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            SystemParametersInfoW, SPIF_SENDCHANGE, SPIF_UPDATEINIFILE, SPI_SETDESKWALLPAPER,
        };
        let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
        let ok = unsafe {
            SystemParametersInfoW(
                SPI_SETDESKWALLPAPER,
                0,
                wide.as_ptr() as *mut _,
                SPIF_UPDATEINIFILE | SPIF_SENDCHANGE,
            )
        };
        if ok == 0 {
            return Err("SystemParametersInfo failed".into());
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        Err("Windows only".into())
    }
}

/// Register HKCU file associations for the given extensions.
/// (For types already owned by another app, Windows still asks the user
/// the first time — that hash-protected choice can't be forced.)
#[tauri::command]
fn associate_files(exts: Vec<String>) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let exe = exe.to_string_lossy().to_string();
    let hkcu = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER);
    let classes = hkcu
        .open_subkey_with_flags("Software\\Classes", winreg::enums::KEY_ALL_ACCESS)
        .map_err(|e| e.to_string())?;

    let (progid, _) = classes.create_subkey("Lumen.Media").map_err(|e| e.to_string())?;
    progid.set_value("", &"Fichier multimédia SpeedDisplay").map_err(|e| e.to_string())?;
    let (icon, _) = progid.create_subkey("DefaultIcon").map_err(|e| e.to_string())?;
    icon.set_value("", &format!("\"{exe}\",0")).map_err(|e| e.to_string())?;
    let (cmd, _) = progid
        .create_subkey("shell\\open\\command")
        .map_err(|e| e.to_string())?;
    cmd.set_value("", &format!("\"{exe}\" \"%1\"")).map_err(|e| e.to_string())?;

    for ext in exts {
        let ext = if ext.starts_with('.') { ext } else { format!(".{ext}") };
        if let Ok((k, _)) = classes.create_subkey(&ext) {
            let _ = k.set_value("", &"Lumen.Media");
        }
        // Also add to the "Open with" list.
        if let Ok((k, _)) = classes.create_subkey(format!("{ext}\\OpenWithProgids")) {
            let _ = k.set_value("Lumen.Media", &"");
        }
    }
    Ok(())
}

#[tauri::command]
fn open_with(path: String, exe: String) -> Result<(), String> {
    std::process::Command::new(&exe)
        .arg(&path)
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn get_startup_file(startup: State<'_, StartupFile>) -> Option<String> {
    startup.0.clone()
}

fn settings_path(app: &tauri::AppHandle) -> PathBuf {
    app.path()
        .app_config_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("settings.json")
}

fn main() {
    // A file argument means "opened via double-click / file association".
    let startup_file = std::env::args()
        .nth(1)
        .filter(|a| Path::new(a).is_file());

    tauri::Builder::default()
        .manage(WatchState(std::sync::Mutex::new(None)))
        .manage(StartupFile(startup_file))
        // The converted videos and full-size previews are copies of the
        // user's own media: wipe them on the way out when asked to.
        .on_window_event(|window, event| {
            if matches!(event, tauri::WindowEvent::Destroyed) {
                let state = window.state::<AppState>();
                if state.clear_play_on_exit.load(Ordering::SeqCst) {
                    state.cache.clear_play();
                }
            }
        })
        .setup(|app| {
            let cache_dir = app
                .path()
                .app_cache_dir()
                .unwrap_or_else(|_| PathBuf::from("./lumen-cache"));
            let cache = Arc::new(ThumbCache::open(&cache_dir).expect("cache"));
            let ffmpeg = Arc::new(Ffmpeg::new(&cache_dir));
            app.manage(AppState {
                cache,
                ffmpeg,
                generation: Arc::new(AtomicU64::new(0)),
                search_gen: Arc::new(AtomicU64::new(0)),
                pregen_gen: Arc::new(AtomicU64::new(0)),
                paste_gen: Arc::new(AtomicU64::new(0)),
                rot_gen: Arc::new(AtomicU64::new(0)),
                clear_play_on_exit: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_roots,
            home_dirs,
            list_dir,
            request_thumbs,
            pregen_thumbs,
            cancel_pregen,
            cancel_thumb_work,
            full_image,
            video_preview,
            prepare_video,
            cache_size,
            cache_stats,
            enforce_cache_limit,
            clear_cache,
            delete_file,
            rename_file,
            create_folder,
            paste_files,
            check_conflicts,
            cancel_paste,
            restore_trashed,
            file_props,
            set_attributes,
            disk_space,
            clip_set_files,
            clip_get_files,
            write_xmp,
            read_xmp,
            dir_meta,
            set_meta,
            set_video_pos,
            read_exif,
            image_dims,
            image_sizes,
            photo_info,
            rotate_exif,
            cancel_rotate,
            clear_play,
            set_clear_play_on_exit,
            capture_dates,
            watch_dir,
            search_dir,
            cancel_search,
            find_duplicates,
            cut_video,
            convert_batch,
            rename_batch,
            set_wallpaper,
            associate_files,
            file_icon,
            open_with,
            get_startup_file,
            reveal,
            get_settings,
            save_settings
        ])
        .run(tauri::generate_context!())
        .expect("error while running Lumen");
}
