// Lumen - Tauri layer.
// Thin glue over lumen-core (which holds all the tested engine logic).

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(windows)]
mod winclip;

use lumen_core::cache::ThumbCache;
use lumen_core::fileops::{conflict_side, same_entry, tree_size, unique_dest, Conflict, PasteOutcome};
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
            let cache_w = cache.clone();
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
                if let Some(t) = &thumb {
                    cache_w.record_file(t, &r.path); // so a purge can find its way back
                }
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
            let cache_p = cache.clone();
            let tick = tick.clone();
            workers.push(std::thread::spawn(move || loop {
                if gen_ref.load(Ordering::SeqCst) != generation {
                    break;
                }
                let Some(r) = queue.lock().unwrap().pop_front() else { break };
                let made = match r.kind {
                    EntryKind::Video => ffmpeg.thumb(Path::new(&r.path), tsize),
                    _ => ffmpeg.image_thumb(Path::new(&r.path), tsize),
                };
                if let Some(t) = &made {
                    cache_p.record_file(t, &r.path);
                }
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
    let cache = state.cache.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let out = ffmpeg.preview(Path::new(&path)).ok_or_else(|| "preview failed".to_string())?;
        cache.record_file(&out, &path);
        Ok(out)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn prepare_video(state: State<'_, AppState>, path: String) -> Result<PlayableOut, String> {
    let ffmpeg = state.ffmpeg.clone();
    let cache = state.cache.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let out = ffmpeg.playable(Path::new(&path)).ok_or_else(|| "unplayable".to_string())?;
        // A direct play is the original itself; only real conversions are ours.
        if out.path != path {
            cache.record_file(&out.path, &path);
        }
        Ok(out)
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
    let app2 = app.clone();
    let cache = state.cache.clone();
    tauri::async_runtime::spawn_blocking(move || {
        // The engine lives in lumen_core, where it is tested headless; this
        // layer only lends it a progress channel, a cancel flag and a bin.
        let last_emit = std::sync::Mutex::new(std::time::Instant::now());
        let outcome = lumen_core::fileops::paste(
            &paths,
            Path::new(&dest),
            cut,
            &policy.unwrap_or_else(|| "rename".into()),
            &|done, total, current| {
                // The engine reports every file; the window needs a few
                // updates a second, so the throttle sits here.
                let now = std::time::Instant::now();
                let mut last = last_emit.lock().unwrap();
                if now.duration_since(*last) < std::time::Duration::from_millis(120) {
                    return;
                }
                *last = now;
                let _ = app.emit(
                    "paste-progress",
                    serde_json::json!({
                        "generation": generation,
                        "done": done,
                        "total": total,
                        "current": current.file_name().map(|n| n.to_string_lossy().to_string()),
                    }),
                );
            },
            &|| gen_ref.load(Ordering::SeqCst) != generation,
            // Overwriting always goes through the recycle bin.
            &|p| trash::delete(p).map_err(|e| std::io::Error::other(e.to_string())),
        );
        // Ratings, rotations and thumbnails go where the pictures went - and
        // a copy gets its own record. Also covers what was done before a
        // cancellation: those files did move.
        if let Ok(o) = &outcome {
            cache.follow_files(&o.pairs, cut);
        }
        let cancelled = outcome.as_ref().map(|o| o.cancelled).unwrap_or(false);
        let _ = app2.emit(
            "paste-done",
            serde_json::json!({ "generation": generation, "cancelled": cancelled }),
        );
        outcome
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
fn rename_file(state: State<'_, AppState>, path: String, new_name: String) -> Result<String, String> {
    let p = PathBuf::from(&path);
    let dest = p.with_file_name(&new_name);
    if dest.exists() {
        return Err("exists".into());
    }
    std::fs::rename(&p, &dest).map_err(|e| e.to_string())?;
    let dest = dest.to_string_lossy().to_string();
    // A renamed picture (or folder) keeps what is attached to it.
    state.cache.follow_files(&[(path, dest.clone())], true);
    Ok(dest)
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

/// Remove from the cache what belongs to originals that no longer exist.
#[tauri::command]
async fn purge_orphans(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<lumen_core::cache::PurgeReport, String> {
    let cache = state.cache.clone();
    tauri::async_runtime::spawn_blocking(move || {
        lumen_core::watchdog::log("purge: started");
        let last = std::sync::Mutex::new(std::time::Instant::now());
        let result = cache.purge_orphans(&|p| {
            // Phase changes always go through; within a phase, ten a second.
            let mut l = last.lock().unwrap();
            if p.phase == "done" || p.done == 0 || l.elapsed() >= std::time::Duration::from_millis(100) {
                *l = std::time::Instant::now();
                let _ = app.emit("purge-progress", &p);
            }
        });
        match &result {
            Ok(r) => lumen_core::watchdog::log(&format!(
                "purge: done - {} originals missing, {} files ({} bytes), {} records",
                r.originals, r.files, r.bytes, r.records
            )),
            Err(e) => lumen_core::watchdog::log(&format!("purge: FAILED - {e}")),
        }
        result
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

/// Lengths of a batch of videos, probed in parallel and memoised. The grid
/// shows them on the thumbnails, so this runs on the folder being looked at.
#[tauri::command]
async fn video_durations(
    state: State<'_, AppState>,
    paths: Vec<String>,
    parent: String,
) -> Result<Vec<(String, f64)>, String> {
    let cache = state.cache.clone();
    let ffmpeg = state.ffmpeg.clone();
    tauri::async_runtime::spawn_blocking(move || {
        use rayon::prelude::*;
        Ok(paths
            .par_iter()
            .map(|p| {
                let known = cache.dur(p);
                if known != 0.0 {
                    return (p.clone(), known);
                }
                let d = ffmpeg.duration(Path::new(p)).unwrap_or(0.0);
                cache.set_dur(p, &parent, d);
                (p.clone(), d)
            })
            .collect())
    })
    .await
    .map_err(|e| e.to_string())?
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
fn rename_batch(state: State<'_, AppState>, pairs: Vec<(String, String)>) -> Result<u32, String> {
    let mut ok = 0u32;
    let mut renamed = Vec::new();
    for (src, new_name) in pairs {
        let p = PathBuf::from(&src);
        let dest = p.with_file_name(&new_name);
        if dest.exists() {
            continue;
        }
        if std::fs::rename(&p, &dest).is_ok() {
            ok += 1;
            renamed.push((src, dest.to_string_lossy().to_string()));
        }
    }
    state.cache.follow_files(&renamed, true);
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
            // Stuck or crashing decodes get named here, so a recurrence of
            // "no more thumbnails" can be traced to the file behind it.
            lumen_core::watchdog::set_log_file(cache_dir.join("speeddisplay.log"));
            lumen_core::watchdog::log("started");
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
            video_durations,
            read_exif,
            image_dims,
            image_sizes,
            photo_info,
            rotate_exif,
            cancel_rotate,
            clear_play,
            purge_orphans,
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
