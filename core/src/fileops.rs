// File operations: copy, move, merge, conflict arbitration.
//
// This is the most dangerous code in the application - it deletes things - so
// it lives here, in the pure-Rust crate, where it can be tested headless. The
// Tauri layer only supplies the progress and cancellation closures.
//
// Two rules hold the whole design together:
//   * paths are compared through `canonicalize`, never as strings, because
//     Windows says "Beach" and "beach" are the same folder;
//   * a move deletes the source only once every byte is verifiably at the
//     destination, and a source that refuses to go keeps BOTH copies.

use std::path::{Path, PathBuf};

/// Progress and cancellation, supplied by the caller.
pub struct PasteCtx<'a> {
    pub total: u64,
    pub done: u64,
    /// Called with (done, total, current file) - the caller throttles.
    pub progress: &'a (dyn Fn(u64, u64, &Path) + Sync),
    /// Checked between files and between buffers.
    pub cancelled: &'a (dyn Fn() -> bool + Sync),
    /// How an overwritten item is disposed of. The application sends it to the
    /// recycle bin; tests pass their own so nothing reaches the real bin, and
    /// assert that this - not a hard delete - is what gets called.
    pub recycle: &'a (dyn Fn(&Path) -> std::io::Result<()> + Sync),
}

impl PasteCtx<'_> {
    pub fn cancelled(&self) -> bool {
        (self.cancelled)()
    }
    fn advance(&mut self, bytes: u64, current: &Path) {
        self.done += bytes;
        (self.progress)(self.done, self.total, current);
    }
    pub fn emit(&self, current: &Path) {
        (self.progress)(self.done, self.total, current);
    }
}

/// What a paste did, detailed enough to be reported and undone.
#[derive(Debug, Default, serde::Serialize)]
pub struct PasteOutcome {
    /// (source, destination) of every item actually transferred.
    pub pairs: Vec<(String, String)>,
    /// Items overwritten: they went to the recycle bin.
    pub replaced: u32,
    /// Folders merged into an existing one.
    pub merged: u32,
    /// Copied, but the source could not be removed: both copies remain.
    pub kept: u32,
    /// Human-readable reasons for `kept`.
    pub notes: Vec<String>,
    pub skipped: u32,
    pub cancelled: bool,
}

/// Next free "name", "name (2)", "name (3)"… inside `dir`.
/// `whole` keeps the name in one piece: a folder called "Show.Name.S01" must
/// not become "Show.Name (2).S01".
pub fn unique_dest(dir: &Path, name: &str, whole: bool) -> PathBuf {
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
pub fn tree_size(p: &Path) -> u64 {
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

pub fn copy_recursive(src: &Path, dst: &Path, ctx: &mut PasteCtx) -> std::io::Result<()> {
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
pub fn same_entry(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

/// True when `inner` is `outer` itself or lives somewhere inside it.
pub fn is_within(inner: &Path, outer: &Path) -> bool {
    match (inner.canonicalize(), outer.canonicalize()) {
        (Ok(i), Ok(o)) => i.starts_with(&o),
        _ => false,
    }
}

pub fn remove_tree(p: &Path) -> std::io::Result<()> {
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
pub enum PlaceErr {
    /// Nothing usable was written at the destination.
    Copy(std::io::Error),
    /// The destination is complete; the source is still there, in whole or in
    /// part (a file locked by a thumbnail job, an antivirus, another app...).
    SourceKept(std::io::Error),
}

impl PlaceErr {
    pub fn io(&self) -> &std::io::Error {
        match self {
            PlaceErr::Copy(e) | PlaceErr::SourceKept(e) => e,
        }
    }
}

/// A directory whose files are still open cannot be renamed on Windows, and
/// the lock is usually a passing thing (our own ffmpeg worker finishing a
/// thumbnail). Give it a few chances before falling back to a full copy.
pub fn rename_retry(src: &Path, dst: &Path) -> bool {
    for attempt in 0..5u64 {
        if std::fs::rename(src, dst).is_ok() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(60 * (attempt + 1)));
    }
    false
}

pub fn remove_tree_retry(p: &Path) -> std::io::Result<()> {
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

pub fn place(src: &Path, dst: &Path, cut: bool, ctx: &mut PasteCtx) -> Result<(), PlaceErr> {
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
pub fn place_in_merge(
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
pub fn merge_into(
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
                    if (ctx.recycle)(&to).is_err() {
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



/// Copy or move `paths` into `dest_dir`.
///
/// `policy` decides a name clash: "rename" (keep both), "replace" (the old one
/// goes through `recycle`), "skip". Two folders of the same name are merged,
/// never swapped. Returns what happened, detailed enough to report and undo.
#[allow(clippy::too_many_arguments)]
pub fn paste(
    paths: &[String],
    dest_dir: &Path,
    cut: bool,
    policy: &str,
    progress: &(dyn Fn(u64, u64, &Path) + Sync),
    cancelled: &(dyn Fn() -> bool + Sync),
    recycle: &(dyn Fn(&Path) -> std::io::Result<()> + Sync),
) -> Result<PasteOutcome, String> {
    let total: u64 = paths.iter().map(|p| tree_size(Path::new(p))).sum();
    let mut ctx = PasteCtx { total, done: 0, progress, cancelled, recycle };
    let mut out = PasteOutcome::default();

    for p in paths {
        if ctx.cancelled() {
            out.cancelled = true;
            break;
        }
        let src = PathBuf::from(p);
        let Some(name) = src.file_name().map(|n| n.to_string_lossy().to_string()) else {
            continue;
        };
        // A folder can never receive itself or one of its own ancestors.
        if is_within(dest_dir, &src) {
            out.skipped += 1;
            ctx.done += tree_size(&src);
            continue;
        }
        if cut && src.parent().is_some_and(|q| same_entry(q, dest_dir)) {
            // Already where it is being dropped: nothing to do, and the
            // report says so rather than staying silent.
            out.skipped += 1;
            ctx.done += tree_size(&src);
            continue;
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
                if (ctx.recycle)(&direct).is_err() {
                    out.skipped += 1;
                    continue;
                }
                out.replaced += 1;
                direct
            } else {
                unique_dest(dest_dir, &name, src.is_dir())
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


    Ok(out)
}

/// One side of a clash, with everything the dialog needs to compare them.
#[derive(serde::Serialize)]
pub struct ConflictSide {
    pub path: String,
    pub size: u64,
    pub mtime: i64,
    /// Pixel dimensions, 0 when unknown (folder, video, unreadable header).
    pub w: u32,
    pub h: u32,
}

pub fn conflict_side(p: &Path) -> ConflictSide {
    let md = std::fs::metadata(p).ok();
    let is_dir = md.as_ref().is_some_and(|m| m.is_dir());
    let (w, h) = if is_dir {
        (0, 0)
    } else {
        crate::image_dims(p).unwrap_or((0, 0))
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
#[derive(serde::Serialize)]
pub struct Conflict {
    pub name: String,
    pub dir: bool,
    /// The item being pasted, and the one already in the destination.
    pub src: ConflictSide,
    pub dst: ConflictSide,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    /// A paste rig: progress is counted, cancellation is a flag, and the
    /// recycler records what it was asked to throw away instead of reaching
    /// the real recycle bin. Tests can then assert that an overwrite goes
    /// through the bin rather than through a hard delete.
    struct Harness {
        recycled: std::sync::Mutex<Vec<PathBuf>>,
        stop: AtomicBool,
        calls: AtomicUsize,
    }

    impl Harness {
        fn new() -> Self {
            Self {
                recycled: std::sync::Mutex::new(Vec::new()),
                stop: AtomicBool::new(false),
                calls: AtomicUsize::new(0),
            }
        }
        fn run(&self, paths: &[String], dest: &Path, cut: bool, policy: &str) -> PasteOutcome {
            paste(
                paths,
                dest,
                cut,
                policy,
                &|_, _, _| {
                    self.calls.fetch_add(1, Ordering::SeqCst);
                },
                &|| self.stop.load(Ordering::SeqCst),
                &|p| {
                    self.recycled.lock().unwrap().push(p.to_path_buf());
                    remove_tree(p)
                },
            )
            .expect("paste returned an error")
        }
    }

    fn write(p: &Path, bytes: &[u8]) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, bytes).unwrap();
    }
    fn read(p: &Path) -> Vec<u8> {
        std::fs::read(p).unwrap_or_default()
    }

    #[test]
    fn plain_move_between_folders() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("A/photo.jpg");
        write(&src, b"pixels");
        let dest = tmp.path().join("B");
        std::fs::create_dir_all(&dest).unwrap();

        let h = Harness::new();
        let out = h.run(&[src.to_string_lossy().to_string()], &dest, true, "rename");

        assert_eq!(out.pairs.len(), 1);
        assert!(!src.exists(), "the source should be gone");
        assert_eq!(read(&dest.join("photo.jpg")), b"pixels");
        assert!(h.recycled.lock().unwrap().is_empty(), "nothing to recycle here");
    }

    /// The bug that cost real folders: on Windows "Beach" and "beach" are one
    /// directory, so moving one onto the other must do nothing whatsoever.
    #[test]
    fn a_folder_cannot_be_moved_onto_itself() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("Dest");
        let beach = dest.join("Beach");
        write(&beach.join("a.jpg"), b"one");
        write(&beach.join("sub/b.jpg"), b"two");

        // The very same folder, spelled with the other case.
        let other_case = dest.join("beach").to_string_lossy().to_string();
        let h = Harness::new();
        let out = h.run(&[other_case.clone()], &dest, true, "replace");

        assert_eq!(out.skipped, 1, "it must be skipped, not processed");

        // A copy takes the other road to the same guard: the destination name
        // exists and resolves to the source itself.
        let out = h.run(&[other_case], &dest, false, "replace");
        assert_eq!(out.skipped, 1, "copying a folder onto itself is a no-op too");
        assert!(h.recycled.lock().unwrap().is_empty(), "nothing may be thrown away");
        assert_eq!(read(&beach.join("a.jpg")), b"one", "the folder must be untouched");
        assert_eq!(read(&beach.join("sub/b.jpg")), b"two");
    }

    #[test]
    fn a_folder_cannot_swallow_its_own_parent() {
        let tmp = tempfile::tempdir().unwrap();
        let outer = tmp.path().join("Outer");
        let inner = outer.join("Inner");
        write(&inner.join("x.txt"), b"x");

        let h = Harness::new();
        let out = h.run(&[outer.to_string_lossy().to_string()], &inner, true, "rename");

        assert_eq!(out.skipped, 1);
        assert_eq!(read(&inner.join("x.txt")), b"x");
    }

    /// Two folders of the same name merge: the destination keeps what it had,
    /// the source adds what is new, and only the clashes are arbitrated.
    #[test]
    fn same_name_folders_are_merged_not_swapped() {
        let tmp = tempfile::tempdir().unwrap();
        let from = tmp.path().join("From/Album");
        write(&from.join("new.jpg"), b"new");
        write(&from.join("shared.jpg"), b"source version");
        let dest = tmp.path().join("To");
        write(&dest.join("Album/old.jpg"), b"old");
        write(&dest.join("Album/shared.jpg"), b"destination version");

        let h = Harness::new();
        let out = h.run(&[from.to_string_lossy().to_string()], &dest, true, "replace");

        let album = dest.join("Album");
        assert_eq!(out.merged, 1);
        assert_eq!(read(&album.join("old.jpg")), b"old", "the destination's own file survives");
        assert_eq!(read(&album.join("new.jpg")), b"new", "the source's file arrives");
        assert_eq!(read(&album.join("shared.jpg")), b"source version", "the clash is replaced");
        assert_eq!(
            h.recycled.lock().unwrap().len(),
            1,
            "the overwritten file goes to the bin, it is never unlinked"
        );
    }

    #[test]
    fn skip_policy_leaves_both_sides_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let from = tmp.path().join("From/doc.txt");
        write(&from, b"source");
        let dest = tmp.path().join("To");
        write(&dest.join("doc.txt"), b"destination");

        let h = Harness::new();
        let out = h.run(&[from.to_string_lossy().to_string()], &dest, true, "skip");

        assert_eq!(out.skipped, 1);
        assert_eq!(read(&dest.join("doc.txt")), b"destination");
        assert!(from.exists(), "a skipped move keeps its source");
    }

    #[test]
    fn rename_policy_keeps_both_and_never_splits_a_folder_name() {
        let tmp = tempfile::tempdir().unwrap();
        let from = tmp.path().join("From/Show.Name.S01");
        write(&from.join("e01.mkv"), b"episode");
        let dest = tmp.path().join("To");
        write(&dest.join("Show.Name.S01/e01.mkv"), b"other episode");

        let h = Harness::new();
        h.run(&[from.to_string_lossy().to_string()], &dest, true, "rename");

        assert!(
            dest.join("Show.Name.S01 (2)").is_dir(),
            "a folder name stays whole instead of becoming Show.Name (2).S01"
        );
        assert_eq!(read(&dest.join("Show.Name.S01/e01.mkv")), b"other episode");
    }

    #[test]
    fn a_cancelled_copy_leaves_no_half_file() {
        let tmp = tempfile::tempdir().unwrap();
        let from = tmp.path().join("From/big.bin");
        write(&from, &vec![7u8; 4 << 20]); // 4 MiB: several copy buffers
        let dest = tmp.path().join("To");
        std::fs::create_dir_all(&dest).unwrap();

        let h = Harness::new();
        h.stop.store(true, Ordering::SeqCst);
        let out = h.run(&[from.to_string_lossy().to_string()], &dest, false, "rename");

        assert!(out.cancelled);
        assert!(!dest.join("big.bin").exists(), "no leftover at the destination");
        assert!(from.exists(), "and the source is intact");
    }

    /// A copy, as opposed to a move, must never touch the source.
    #[test]
    fn copy_keeps_the_source() {
        let tmp = tempfile::tempdir().unwrap();
        let from = tmp.path().join("From/tree/deep/pic.jpg");
        write(&from, b"data");
        let dest = tmp.path().join("To");
        std::fs::create_dir_all(&dest).unwrap();

        let h = Harness::new();
        let folder = tmp.path().join("From/tree");
        h.run(&[folder.to_string_lossy().to_string()], &dest, false, "rename");

        assert_eq!(read(&from), b"data", "the original stays put");
        assert_eq!(read(&dest.join("tree/deep/pic.jpg")), b"data");
    }

    /// Windows refuses to delete a file another program holds open without
    /// sharing deletion. The copy is complete, so BOTH copies must survive
    /// and the caller must be told: this is the second bug that lost data.
    #[cfg(windows)]
    #[test]
    fn a_locked_source_keeps_both_copies() {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_READ: u32 = 1;

        let tmp = tempfile::tempdir().unwrap();
        let folder = tmp.path().join("From/Clips");
        write(&folder.join("held.mp4"), b"frames");
        let dest = tmp.path().join("To");
        std::fs::create_dir_all(&dest).unwrap();

        let _held = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .open(folder.join("held.mp4"))
            .unwrap();

        let h = Harness::new();
        let out = h.run(&[folder.to_string_lossy().to_string()], &dest, true, "rename");

        assert_eq!(out.kept, 1, "the caller must learn the source stayed behind");
        assert!(!out.notes.is_empty(), "with a reason to show");
        assert_eq!(read(&dest.join("Clips/held.mp4")), b"frames", "the copy is complete");
        assert!(folder.join("held.mp4").exists(), "and the locked source is still there");
    }

    #[test]
    fn progress_is_reported() {
        let tmp = tempfile::tempdir().unwrap();
        let from = tmp.path().join("From/a.bin");
        write(&from, &vec![1u8; 2 << 20]);
        let dest = tmp.path().join("To");
        std::fs::create_dir_all(&dest).unwrap();

        let h = Harness::new();
        h.run(&[from.to_string_lossy().to_string()], &dest, false, "rename");
        assert!(h.calls.load(Ordering::SeqCst) > 0, "the progress channel was never used");
    }
}
