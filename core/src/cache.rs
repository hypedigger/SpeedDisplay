// Persistent thumbnail cache.
//
// Thumbnails are stored as individual files (JPEG/PNG) in the cache folder,
// indexed by a SQLite database keyed on (path, mtime, size). A thumbnail is
// valid as long as the source file has not changed.

use rusqlite::{params, Connection};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub struct ThumbCache {
    conn: Mutex<Connection>,
    pub dir: PathBuf,
}

#[derive(Debug, Clone)]
pub struct ThumbRecord {
    pub thumb_path: String,
    pub width: u32,
    pub height: u32,
}

impl ThumbCache {
    /// Open (or create) the cache in the given directory.
    pub fn open(dir: &Path) -> rusqlite::Result<Self> {
        std::fs::create_dir_all(dir).ok();
        std::fs::create_dir_all(dir.join("thumbs")).ok();
        std::fs::create_dir_all(dir.join("previews")).ok();
        std::fs::create_dir_all(dir.join("play")).ok();
        let conn = Connection::open(dir.join("index.db"))?;
        // Another instance or a WAL checkpoint may hold the database for a
        // moment: wait for it rather than fail - most writes here ignore
        // their result, so a failure would simply vanish.
        conn.busy_timeout(std::time::Duration::from_secs(10))?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             CREATE TABLE IF NOT EXISTS thumbs (
                 path   TEXT NOT NULL,
                 mtime  INTEGER NOT NULL,
                 size   INTEGER NOT NULL,
                 tsize  INTEGER NOT NULL,
                 thumb  TEXT NOT NULL,
                 w      INTEGER NOT NULL,
                 h      INTEGER NOT NULL,
                 PRIMARY KEY (path, tsize)
             );
             CREATE TABLE IF NOT EXISTS meta (
                 path   TEXT PRIMARY KEY,
                 parent TEXT NOT NULL,
                 rating INTEGER NOT NULL DEFAULT 0,
                 color  TEXT NOT NULL DEFAULT '',
                 flag   TEXT NOT NULL DEFAULT '',
                 tags   TEXT NOT NULL DEFAULT '',
                 rot    INTEGER NOT NULL DEFAULT 0,
                 taken  INTEGER NOT NULL DEFAULT 0,
                 pos    REAL NOT NULL DEFAULT 0,
                 dur    REAL NOT NULL DEFAULT 0
             );
             CREATE INDEX IF NOT EXISTS meta_parent ON meta(parent);
             CREATE TABLE IF NOT EXISTS files (
                 file   TEXT PRIMARY KEY,
                 source TEXT NOT NULL
             );",
        )?;
        // Older caches lack the rotation column; ignore the error if present.
        let _ = conn.execute("ALTER TABLE meta ADD COLUMN rot INTEGER NOT NULL DEFAULT 0", []);
        let _ = conn.execute("ALTER TABLE meta ADD COLUMN taken INTEGER NOT NULL DEFAULT 0", []);
        // Playback position of a video, in seconds; 0 = start from the top.
        let _ = conn.execute("ALTER TABLE meta ADD COLUMN pos REAL NOT NULL DEFAULT 0", []);
        // Total length of a video, in seconds; 0 = never probed.
        let _ = conn.execute("ALTER TABLE meta ADD COLUMN dur REAL NOT NULL DEFAULT 0", []);
        Ok(Self {
            conn: Mutex::new(conn),
            dir: dir.to_path_buf(),
        })
    }

    /// Mark a cached file as just used. Eviction sorts on this timestamp, so
    /// stamping it on every hit turns the plain FIFO into a real LRU: a file
    /// you keep coming back to stops being the next one thrown away.
    ///
    /// Throttled to one write per hour and per file - browsing a folder of
    /// 500 thumbnails must not cost 500 metadata writes.
    pub fn touch_used(path: &Path) {
        const STALE: std::time::Duration = std::time::Duration::from_secs(3600);
        let Ok(meta) = std::fs::metadata(path) else { return };
        let now = std::time::SystemTime::now();
        if meta
            .modified()
            .ok()
            .and_then(|m| now.duration_since(m).ok())
            .is_none_or(|age| age < STALE)
        {
            return;
        }
        if let Ok(f) = std::fs::OpenOptions::new().write(true).open(path) {
            let _ = f.set_times(std::fs::FileTimes::new().set_modified(now).set_accessed(now));
        }
    }

    /// Look up a still-valid thumbnail for (path, mtime, size, thumb size).
    pub fn get(&self, path: &str, mtime: i64, size: u64, tsize: u32) -> Option<ThumbRecord> {
        let conn = self.conn.lock().ok()?;
        let mut stmt = conn
            .prepare_cached(
                "SELECT thumb, w, h FROM thumbs
                 WHERE path=?1 AND tsize=?2 AND mtime=?3 AND size=?4",
            )
            .ok()?;
        let rec = stmt
            .query_row(params![path, tsize, mtime, size as i64], |r| {
                Ok(ThumbRecord {
                    thumb_path: r.get(0)?,
                    width: r.get(1)?,
                    height: r.get(2)?,
                })
            })
            .ok()?;
        Self::touch_used(Path::new(&rec.thumb_path));
        if Path::new(&rec.thumb_path).exists() {
            Some(rec)
        } else {
            None
        }
    }

    /// Any cached thumbnail file for `path`, whatever its size or freshness.
    /// Used as a cheap decode source (duplicate detection).
    pub fn any_thumb(&self, path: &str) -> Option<String> {
        let conn = self.conn.lock().ok()?;
        let mut stmt = conn
            .prepare_cached("SELECT thumb FROM thumbs WHERE path=?1 LIMIT 1")
            .ok()?;
        let thumb: String = stmt.query_row(params![path], |r| r.get(0)).ok()?;
        Path::new(&thumb).exists().then_some(thumb)
    }

    /// Insert or refresh a thumbnail record.
    pub fn put(
        &self,
        path: &str,
        mtime: i64,
        size: u64,
        tsize: u32,
        thumb: &str,
        w: u32,
        h: u32,
    ) {
        if let Ok(conn) = self.conn.lock() {
            // The original changed: its new thumbnail gets a new name, and the
            // old file used to stay behind, unreachable, forever. That is how a
            // cache ends up with 280 000 files its database knows nothing about.
            if let Ok(old) = conn.query_row(
                "SELECT thumb FROM thumbs WHERE path=?1 AND tsize=?2",
                params![path, tsize],
                |r| r.get::<_, String>(0),
            ) {
                if old != thumb {
                    let _ = std::fs::remove_file(&old);
                }
            }
            let _ = conn.execute(
                "INSERT OR REPLACE INTO thumbs (path, mtime, size, tsize, thumb, w, h)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![path, mtime, size as i64, tsize, thumb, w, h],
            );
        }
    }

    /// Forget every thumbnail of `path` (rows + files). Needed when a file
    /// changes without its timestamps moving, e.g. an in-place EXIF patch.
    pub fn invalidate(&self, path: &str) {
        let Ok(conn) = self.conn.lock() else { return };
        if let Ok(mut stmt) = conn.prepare("SELECT thumb FROM thumbs WHERE path=?1") {
            if let Ok(rows) = stmt.query_map(params![path], |r| r.get::<_, String>(0)) {
                for thumb in rows.flatten() {
                    let _ = std::fs::remove_file(&thumb);
                }
            }
        }
        let _ = conn.execute("DELETE FROM thumbs WHERE path=?1", params![path]);
    }

    /// Cached capture time (EXIF), 0 when unknown.
    pub fn taken(&self, path: &str) -> i64 {
        let Ok(conn) = self.conn.lock() else { return 0 };
        conn.prepare_cached("SELECT taken FROM meta WHERE path=?1")
            .and_then(|mut st| st.query_row(params![path], |r| r.get(0)))
            .unwrap_or(0)
    }

    /// Remember a capture time so the EXIF is only read once per file.
    pub fn set_taken(&self, path: &str, parent: &str, taken: i64) {
        if let Ok(conn) = self.conn.lock() {
            let _ = conn.execute(
                "INSERT INTO meta (path, parent, taken) VALUES (?1, ?2, ?3)
                 ON CONFLICT(path) DO UPDATE SET taken = ?3",
                params![path, parent, taken],
            );
        }
    }

    /// Where playback of this video stopped, in seconds (0 = beginning).
    pub fn pos(&self, path: &str) -> f64 {
        let Ok(conn) = self.conn.lock() else { return 0.0 };
        conn.prepare_cached("SELECT pos FROM meta WHERE path=?1")
            .and_then(|mut st| st.query_row(params![path], |r| r.get(0)))
            .unwrap_or(0.0)
    }

    /// Length of a video in seconds, 0 when ffprobe has never been run on it.
    pub fn dur(&self, path: &str) -> f64 {
        let Ok(conn) = self.conn.lock() else { return 0.0 };
        conn.prepare_cached("SELECT dur FROM meta WHERE path=?1")
            .and_then(|mut st| st.query_row(params![path], |r| r.get(0)))
            .unwrap_or(0.0)
    }

    /// Memoise a probed duration: ffprobe costs tens of milliseconds a file.
    pub fn set_dur(&self, path: &str, parent: &str, dur: f64) {
        if let Ok(conn) = self.conn.lock() {
            let _ = conn.execute(
                "INSERT INTO meta (path, parent, dur) VALUES (?1, ?2, ?3)
                 ON CONFLICT(path) DO UPDATE SET dur = ?3",
                params![path, parent, dur],
            );
        }
    }

    /// Remember where playback stopped so the video resumes there.
    pub fn set_pos(&self, path: &str, parent: &str, pos: f64) {
        if let Ok(conn) = self.conn.lock() {
            let _ = conn.execute(
                "INSERT INTO meta (path, parent, pos) VALUES (?1, ?2, ?3)
                 ON CONFLICT(path) DO UPDATE SET pos = ?3",
                params![path, parent, pos],
            );
        }
    }

    /// Delete the converted media only: remuxed videos and full-size
    /// previews. These are copies of the originals, the heaviest and the most
    /// private part of the cache, and they rebuild on demand.
    pub fn clear_play(&self) -> u64 {
        let mut freed = 0u64;
        if let Ok(rd) = std::fs::read_dir(self.dir.join("play")) {
            for f in rd.flatten() {
                let len = f.metadata().map(|m| m.len()).unwrap_or(0);
                if std::fs::remove_file(f.path()).is_ok() {
                    freed += len;
                }
            }
        }
        freed
    }

    /// Remember which original a cache file was made from. Thumbnails have
    /// their own table; the files ffmpeg produces (video thumbnails, previews,
    /// converted videos) only had a hash for a name - this is their link back.
    pub fn record_file(&self, file: &str, source: &str) {
        if let Ok(conn) = self.conn.lock() {
            let _ = conn.execute(
                "INSERT OR REPLACE INTO files (file, source) VALUES (?1, ?2)",
                params![file, source],
            );
        }
    }

    /// Drop what belongs to originals that cannot be found: thumbnails,
    /// ffmpeg products, ratings and playback positions.
    ///
    /// Built to be watched and to fail loudly:
    ///  * the database lock is held only to read the rows and, at the end, to
    ///    delete them in ONE transaction - never during the file work, which
    ///    used to stall every other cache access for the whole run;
    ///  * a database error is returned, not turned into an empty list (which
    ///    reported "0 files" as if there had been nothing to do);
    ///  * `progress` hears about every phase.
    pub fn purge_orphans(
        &self,
        progress: &(dyn Fn(PurgeProgress) + Sync),
    ) -> Result<PurgeReport, String> {
        use rayon::prelude::*;
        let mut rep = PurgeReport::default();
        let tell = |phase: &'static str, done: u64, total: u64, rep: &PurgeReport| {
            progress(PurgeProgress { phase, done, total, files: rep.files, bytes: rep.bytes, records: rep.records })
        };

        // 1. Read what the cache knows - briefly under the lock.
        tell("read", 0, 0, &rep);
        let (thumb_rows, file_rows, meta_paths) = {
            let conn = self.conn.lock().map_err(|_| "cache database unavailable".to_string())?;
            let pairs = |sql: &str| -> Result<Vec<(String, String)>, String> {
                let mut st = conn.prepare(sql).map_err(|e| e.to_string())?;
                let rows = st
                    .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
                    .map_err(|e| e.to_string())?;
                rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
            };
            let thumbs = pairs("SELECT path, thumb FROM thumbs")?;
            let files = pairs("SELECT file, source FROM files")?;
            let mut st = conn.prepare("SELECT path FROM meta").map_err(|e| e.to_string())?;
            let meta = st
                .query_map([], |r| r.get::<_, String>(0))
                .map_err(|e| e.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?;
            (thumbs, files, meta)
        };

        // Where to look for re-attachable strays later, and at which sizes:
        // decided NOW, before rows are deleted. A folder can outlive the files
        // the database knew in it (photos sorted out of it, new ones in), and
        // dropping it from the search would delete thumbnails that belong to
        // files sitting right there.
        let mut search_dirs: std::collections::HashSet<PathBuf> = thumb_rows
            .iter()
            .map(|(p, _)| p.as_str())
            .chain(file_rows.iter().map(|(_, s)| s.as_str()))
            .chain(meta_paths.iter().map(|p| p.as_str()))
            .filter_map(|p| Path::new(p).parent().map(Path::to_path_buf))
            .collect();
        search_dirs.retain(|d| d.is_dir());
        let search_sizes: Vec<u32> = {
            let conn = self.conn.lock().map_err(|_| "cache database unavailable".to_string())?;
            let mut st = conn.prepare("SELECT DISTINCT tsize FROM thumbs").map_err(|e| e.to_string())?;
            let v = st
                .query_map([], |r| r.get::<_, u32>(0))
                .map_err(|e| e.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?;
            v
        };

        // 2. Which originals are gone - checked in parallel, lock-free.
        let mut originals: Vec<&str> = thumb_rows
            .iter()
            .map(|(p, _)| p.as_str())
            .chain(file_rows.iter().map(|(_, s)| s.as_str()))
            .chain(meta_paths.iter().map(|p| p.as_str()))
            .collect();
        originals.sort_unstable();
        originals.dedup();
        let total = originals.len() as u64;
        let checked = std::sync::atomic::AtomicU64::new(0);
        let gone: std::collections::HashSet<&str> = originals
            .par_iter()
            .filter(|p| {
                let n = checked.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                if n % 2000 == 0 {
                    tell("check", n, total, &PurgeReport::default());
                }
                !Path::new(p).exists()
            })
            .copied()
            .collect();
        tell("check", total, total, &rep);

        // 3. Delete their cache files, still without the lock.
        let doomed: Vec<&str> = thumb_rows
            .iter()
            .filter(|(p, _)| gone.contains(p.as_str()))
            .map(|(_, t)| t.as_str())
            .chain(
                file_rows
                    .iter()
                    .filter(|(_, s)| gone.contains(s.as_str()))
                    .map(|(f, _)| f.as_str()),
            )
            .collect();
        let n_doomed = doomed.len() as u64;
        for (i, f) in doomed.iter().enumerate() {
            let len = std::fs::metadata(f).map(|m| m.len()).unwrap_or(0);
            if std::fs::remove_file(f).is_ok() {
                rep.files += 1;
                rep.bytes += len;
            }
            if i % 500 == 0 {
                tell("files", i as u64, n_doomed, &rep);
            }
        }
        tell("files", n_doomed, n_doomed, &rep);

        // 4. Forget them, in one transaction.
        tell("records", 0, gone.len() as u64, &rep);
        {
            let mut conn = self.conn.lock().map_err(|_| "cache database unavailable".to_string())?;
            let tx = conn.transaction().map_err(|e| e.to_string())?;
            {
                let mut del_thumb = tx.prepare("DELETE FROM thumbs WHERE path=?1").map_err(|e| e.to_string())?;
                let mut del_file = tx.prepare("DELETE FROM files WHERE source=?1").map_err(|e| e.to_string())?;
                let mut del_meta = tx.prepare("DELETE FROM meta WHERE path=?1").map_err(|e| e.to_string())?;
                for p in &gone {
                    del_thumb.execute(params![p]).map_err(|e| e.to_string())?;
                    del_file.execute(params![p]).map_err(|e| e.to_string())?;
                    rep.records += del_meta.execute(params![p]).map_err(|e| e.to_string())? as u64;
                }
            }
            tx.commit().map_err(|e| e.to_string())?;
        }
        rep.originals = gone.len() as u64;

        // 5. Thumbnail files the database does not know.
        self.reclaim_strays(&search_dirs, &search_sizes, &mut rep, &|done, total, rep| {
            tell("strays", done, total, rep)
        })?;

        tell("done", total, total, &rep);
        Ok(rep)
    }

    /// Thumbnail files no row points to. Each one is either re-attached to
    /// its original - when that original sits, unchanged, in a folder the
    /// cache knows - or deleted. Finding the others would mean scanning every
    /// drive for files whose thumbnail is most likely stale anyway.
    fn reclaim_strays(
        &self,
        folders: &std::collections::HashSet<PathBuf>,
        tsizes: &[u32],
        rep: &mut PurgeReport,
        tell: &dyn Fn(u64, u64, &PurgeReport),
    ) -> Result<(), String> {
        let lower = |s: &str| s.to_lowercase();
        // What the database points to now.
        let referenced = {
            let conn = self.conn.lock().map_err(|_| "cache database unavailable".to_string())?;
            let col = |sql: &str| -> Result<Vec<String>, String> {
                let mut st = conn.prepare(sql).map_err(|e| e.to_string())?;
                let rows = st.query_map([], |r| r.get::<_, String>(0)).map_err(|e| e.to_string())?;
                rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
            };
            let mut referenced = std::collections::HashSet::new();
            for f in col("SELECT thumb FROM thumbs")?.into_iter().chain(col("SELECT file FROM files")?) {
                if let Some(n) = Path::new(&f).file_name() {
                    referenced.insert(lower(&n.to_string_lossy()));
                }
            }
            referenced
        };

        // Only our own files: 16 hex digits + .jpg/.png, and not brand new (a
        // thumbnail being written right now has no row YET).
        let fresh = std::time::SystemTime::now() - std::time::Duration::from_secs(300);
        let ours = |name: &str| {
            let (stem, ext) = name.rsplit_once('.').unwrap_or(("", ""));
            stem.len() == 16
                && stem.chars().all(|c| c.is_ascii_hexdigit())
                && (ext.eq_ignore_ascii_case("jpg") || ext.eq_ignore_ascii_case("png"))
        };
        let mut strays: std::collections::HashMap<String, PathBuf> = Default::default();
        if let Ok(rd) = std::fs::read_dir(self.dir.join("thumbs")) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                let key = lower(&name);
                if !ours(&name) || referenced.contains(&key) {
                    continue;
                }
                let recent = e.metadata().and_then(|m| m.modified()).map(|t| t > fresh).unwrap_or(true);
                if !recent {
                    strays.insert(key, e.path());
                }
            }
        }
        let n_strays = strays.len() as u64;
        tell(0, n_strays, rep);

        // Recompute the names the originals in known folders would produce.
        let mut native_rows = Vec::new(); // (src, mtime, size, tsize, thumb)
        let mut ffmpeg_rows = Vec::new(); // (file, src)
        for dir in folders {
            let Ok(rd) = std::fs::read_dir(dir) else { continue };
            for e in rd.flatten() {
                let Ok(m) = e.metadata() else { continue };
                if !m.is_file() {
                    continue;
                }
                let src = e.path();
                let src_str = src.to_string_lossy().to_string();
                let mtime = m
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_millis())
                    .unwrap_or(0);
                let size = m.len();
                for &ts in tsizes {
                    let native = format!("{:016x}", crate::fnv1a64(format!("{src_str}|{mtime}|{size}|{ts}").as_bytes()));
                    for ext in ["jpg", "png"] {
                        if let Some(f) = strays.remove(&format!("{native}.{ext}")) {
                            native_rows.push((src_str.clone(), mtime as i64, size, ts, f));
                        }
                    }
                    for tag in [format!("vt{ts}"), format!("it{ts}")] {
                        let k = format!("{:016x}", crate::fnv1a64(format!("{}|{}|{}|{}", src.display(), mtime, size, tag).as_bytes()));
                        if let Some(f) = strays.remove(&format!("{k}.jpg")) {
                            ffmpeg_rows.push((f, src_str.clone()));
                        }
                    }
                }
            }
        }

        // Re-attach, in one transaction.
        {
            let mut conn = self.conn.lock().map_err(|_| "cache database unavailable".to_string())?;
            let tx = conn.transaction().map_err(|e| e.to_string())?;
            for (src, mtime, size, ts, f) in &native_rows {
                let (w, h) = image::image_dimensions(f).unwrap_or((0, 0));
                tx.execute(
                    "INSERT OR REPLACE INTO thumbs (path, mtime, size, tsize, thumb, w, h) VALUES (?1,?2,?3,?4,?5,?6,?7)",
                    params![src, mtime, *size as i64, ts, f.to_string_lossy(), w, h],
                )
                .map_err(|e| e.to_string())?;
            }
            for (f, src) in &ffmpeg_rows {
                tx.execute(
                    "INSERT OR REPLACE INTO files (file, source) VALUES (?1, ?2)",
                    params![f.to_string_lossy(), src],
                )
                .map_err(|e| e.to_string())?;
            }
            tx.commit().map_err(|e| e.to_string())?;
        }
        rep.reattached = (native_rows.len() + ffmpeg_rows.len()) as u64;

        // The rest has no known original: delete it.
        for (i, f) in strays.values().enumerate() {
            let len = std::fs::metadata(f).map(|m| m.len()).unwrap_or(0);
            if std::fs::remove_file(f).is_ok() {
                rep.strays += 1;
                rep.stray_bytes += len;
            }
            if i % 1000 == 0 {
                tell(rep.reattached + i as u64, n_strays, rep);
            }
        }
        tell(n_strays, n_strays, rep);
        Ok(())
    }

    /// Wipe the whole cache (records + files). Returns freed byte count.
    pub fn clear(&self) -> u64 {
        let mut freed = 0u64;
        for sub in ["thumbs", "previews", "play"] {
            let d = self.dir.join(sub);
            if let Ok(rd) = std::fs::read_dir(&d) {
                for f in rd.flatten() {
                    if let Ok(m) = f.metadata() {
                        freed += m.len();
                    }
                    let _ = std::fs::remove_file(f.path());
                }
            }
        }
        if let Ok(conn) = self.conn.lock() {
            let _ = conn.execute("DELETE FROM thumbs", []);
        }
        freed
    }

    /// Total size of cached files in bytes.
    pub fn size_on_disk(&self) -> u64 {
        let s = self.stats();
        s.thumbs_bytes + s.previews_bytes + s.play_bytes
    }

    /// Per-category size and file count.
    pub fn stats(&self) -> CacheStats {
        let scan = |sub: &str| -> (u64, u64) {
            let mut bytes = 0u64;
            let mut count = 0u64;
            if let Ok(rd) = std::fs::read_dir(self.dir.join(sub)) {
                for f in rd.flatten() {
                    if let Ok(m) = f.metadata() {
                        bytes += m.len();
                        count += 1;
                    }
                }
            }
            (bytes, count)
        };
        let (thumbs_bytes, thumbs_count) = scan("thumbs");
        let (previews_bytes, previews_count) = scan("previews");
        let (play_bytes, play_count) = scan("play");
        CacheStats {
            thumbs_bytes,
            thumbs_count,
            previews_bytes,
            previews_count,
            play_bytes,
            play_count,
        }
    }

    /// Delete the oldest cached files (all categories mixed) until the total
    /// size fits under `max_bytes`. Returns the number of bytes freed.
    pub fn enforce_limit(&self, max_bytes: u64) -> u64 {
        let mut files: Vec<(std::time::SystemTime, u64, std::path::PathBuf)> = Vec::new();
        let mut total = 0u64;
        for sub in ["thumbs", "previews", "play"] {
            if let Ok(rd) = std::fs::read_dir(self.dir.join(sub)) {
                for f in rd.flatten() {
                    if let Ok(m) = f.metadata() {
                        let t = m.modified().unwrap_or(std::time::UNIX_EPOCH);
                        total += m.len();
                        files.push((t, m.len(), f.path()));
                    }
                }
            }
        }
        if total <= max_bytes {
            return 0;
        }
        files.sort_by_key(|(t, _, _)| *t);
        let mut freed = 0u64;
        for (_, len, path) in files {
            if total - freed <= max_bytes {
                break;
            }
            if std::fs::remove_file(&path).is_ok() {
                freed += len;
            }
        }
        // Stale DB rows self-heal: get() checks that the file still exists.
        freed
    }
}

/// What a purge removed.
#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct PurgeReport {
    /// Originals found missing.
    pub originals: u64,
    /// Cache files deleted and the space they took.
    pub files: u64,
    pub bytes: u64,
    /// Metadata rows (ratings, positions...) of originals that are gone.
    pub records: u64,
    /// Thumbnail files no row pointed to: put back in the database, or
    /// deleted (with the space that freed).
    pub reattached: u64,
    pub strays: u64,
    pub stray_bytes: u64,
}

/// One step of a running purge, for a live display.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PurgeProgress {
    /// "read", "check", "files", "records", "strays" or "done".
    pub phase: &'static str,
    pub done: u64,
    pub total: u64,
    pub files: u64,
    pub bytes: u64,
    pub records: u64,
}

/// User metadata attached to a file (rating, colour label, pick/reject, tags).
#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct MetaRow {
    pub path: String,
    pub rating: i64,
    pub color: String,
    pub flag: String,
    pub tags: String,
    /// Non-destructive display rotation in degrees (0, 90, 180, 270).
    pub rot: i64,
    /// Playback position in seconds (videos).
    pub pos: f64,
    /// Total length in seconds (videos), 0 when unknown.
    pub dur: f64,
}

impl ThumbCache {
    /// All metadata rows for files directly inside `parent`.
    pub fn dir_meta(&self, parent: &str) -> Vec<MetaRow> {
        let Ok(conn) = self.conn.lock() else { return Vec::new() };
        let Ok(mut stmt) = conn.prepare_cached(
            "SELECT path, rating, color, flag, tags, rot, pos, dur FROM meta WHERE parent = ?1",
        ) else {
            return Vec::new();
        };
        stmt.query_map(params![parent], |r| {
            Ok(MetaRow {
                path: r.get(0)?,
                rating: r.get(1)?,
                color: r.get(2)?,
                flag: r.get(3)?,
                tags: r.get(4)?,
                rot: r.get(5)?,
                pos: r.get(6)?,
                dur: r.get(7)?,
            })
        })
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default()
    }

    /// Upsert metadata; `None` fields keep their current value.
    pub fn set_meta(
        &self,
        path: &str,
        parent: &str,
        rating: Option<i64>,
        color: Option<String>,
        flag: Option<String>,
        tags: Option<String>,
        rot: Option<i64>,
    ) {
        if let Ok(conn) = self.conn.lock() {
            let _ = conn.execute(
                "INSERT INTO meta (path, parent, rating, color, flag, tags, rot)
                 VALUES (?1, ?2, COALESCE(?3, 0), COALESCE(?4, ''), COALESCE(?5, ''),
                         COALESCE(?6, ''), COALESCE(?7, 0))
                 ON CONFLICT(path) DO UPDATE SET
                   rating = COALESCE(?3, rating),
                   color  = COALESCE(?4, color),
                   flag   = COALESCE(?5, flag),
                   tags   = COALESCE(?6, tags),
                   rot    = COALESCE(?7, rot)",
                params![path, parent, rating, color, flag, tags, rot],
            );
        }
    }
}

#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct CacheStats {
    pub thumbs_bytes: u64,
    pub thumbs_count: u64,
    pub previews_bytes: u64,
    pub previews_count: u64,
    pub play_bytes: u64,
    pub play_count: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A resume point survives a reopen, and clearing it works.
    #[test]
    fn playback_position_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let c = ThumbCache::open(tmp.path()).unwrap();
        assert_eq!(c.pos("E:/clip.mkv"), 0.0);
        c.set_pos("E:/clip.mkv", "E:/", 123.5);
        assert_eq!(c.pos("E:/clip.mkv"), 123.5);
        // Ratings must not wipe it, nor it wipe the ratings.
        c.set_meta("E:/clip.mkv", "E:/", Some(4), None, None, None, None);
        assert_eq!(c.pos("E:/clip.mkv"), 123.5);
        assert_eq!(c.dir_meta("E:/")[0].rating, 4);
        assert_eq!(c.dir_meta("E:/")[0].pos, 123.5);
        c.set_pos("E:/clip.mkv", "E:/", 0.0);
        assert_eq!(c.pos("E:/clip.mkv"), 0.0);
    }

    /// Purging removes everything whose original cannot be found, including
    /// files on a drive that is not connected, and keeps the rest.
    #[test]
    fn purge_removes_every_missing_original() {
        let tmp = tempfile::tempdir().unwrap();
        let c = ThumbCache::open(&tmp.path().join("cache")).unwrap();

        let alive = tmp.path().join("alive.jpg");
        std::fs::write(&alive, b"x").unwrap();
        let deleted = tmp.path().join("deleted.jpg");
        let parent = tmp.path().to_string_lossy().to_string();
        for (src, name) in [(&alive, "a.jpg"), (&deleted, "d.jpg")] {
            let thumb = c.dir.join("thumbs").join(name);
            std::fs::write(&thumb, b"thumbnail").unwrap();
            c.put(&src.to_string_lossy(), 1, 1, 256, &thumb.to_string_lossy(), 10, 10);
            c.set_meta(&src.to_string_lossy(), &parent, Some(5), None, None, None, None);
        }
        // A drive that is not plugged in: unreachable means gone.
        c.set_meta("Q:\\NotPluggedIn\\holiday.jpg", "Q:\\NotPluggedIn", Some(4), None, None, None, None);

        let rep = c.purge_orphans(&|_| {}).unwrap();

        assert_eq!(rep.files, 1, "the deleted original's thumbnail goes");
        assert_eq!(rep.records, 2, "its rating goes, and so does the unplugged one");
        assert_eq!(rep.originals, 2, "two originals were missing");
        assert!(c.dir.join("thumbs").join("a.jpg").exists(), "the live file is kept");
        assert!(!c.dir.join("thumbs").join("d.jpg").exists());
        assert!(c.dir_meta("Q:\\NotPluggedIn").is_empty());
        assert_eq!(c.dir_meta(&parent).len(), 1, "the live file keeps its rating");
    }

    /// The purge at the size of a real cache: 50 000 thumbnails, 40 000 of
    /// them orphaned. Run: cargo test --release --lib purge_at_scale -- --ignored --nocapture
    #[test]
    #[ignore]
    fn purge_at_scale() {
        let tmp = tempfile::tempdir().unwrap();
        let c = ThumbCache::open(&tmp.path().join("cache")).unwrap();
        let originals = tmp.path().join("originals");
        std::fs::create_dir_all(&originals).unwrap();
        let parent = originals.to_string_lossy().to_string();
        let t = std::time::Instant::now();
        {
            let mut conn = c.conn.lock().unwrap();
            let tx = conn.transaction().unwrap();
            for i in 0..50_000u32 {
                let src = originals.join(format!("p{i}.jpg"));
                if i % 5 == 0 {
                    std::fs::write(&src, b"x").unwrap(); // 1 in 5 still exists
                }
                let thumb = c.dir.join("thumbs").join(format!("t{i}.jpg"));
                std::fs::write(&thumb, b"thumbnail-bytes").unwrap();
                tx.execute(
                    "INSERT INTO thumbs (path, mtime, size, tsize, thumb, w, h) VALUES (?1,1,1,256,?2,10,10)",
                    params![src.to_string_lossy(), thumb.to_string_lossy()],
                )
                .unwrap();
            }
            tx.commit().unwrap();
        }
        c.set_meta(&originals.join("p1.jpg").to_string_lossy(), &parent, Some(3), None, None, None, None);
        println!("fixture built in {:?}", t.elapsed());

        let phases = std::sync::Mutex::new(Vec::<&'static str>::new());
        let t = std::time::Instant::now();
        let rep = c
            .purge_orphans(&|p| {
                let mut v = phases.lock().unwrap();
                if v.last() != Some(&p.phase) {
                    v.push(p.phase);
                }
            })
            .unwrap();
        let took = t.elapsed();
        println!("purge took {:?}: {:?}", took, rep);
        println!("phases seen: {:?}", phases.lock().unwrap());
        assert_eq!(rep.originals, 40_000);
        assert_eq!(rep.files, 40_000);
        assert_eq!(rep.records, 1, "the rating of a vanished file goes too");
        let left: i64 = c.conn.lock().unwrap().query_row("SELECT count(*) FROM thumbs", [], |r| r.get(0)).unwrap();
        assert_eq!(left, 10_000, "only the live originals keep their thumbnail");
        assert_eq!(*phases.lock().unwrap(), vec!["read", "check", "files", "records", "strays", "done"]);
    }

    /// A thumbnail file the database forgot is put back when its original is
    /// still there unchanged, and deleted when nothing can claim it. A file
    /// that is not one of ours is never touched.
    #[test]
    fn strays_are_reattached_or_removed() {
        let tmp = tempfile::tempdir().unwrap();
        let c = ThumbCache::open(&tmp.path().join("cache")).unwrap();
        let folder = tmp.path().join("photos");
        std::fs::create_dir_all(&folder).unwrap();

        // A known file, so the folder is known.
        let known = folder.join("known.jpg");
        std::fs::write(&known, b"k").unwrap();
        let kthumb = c.dir.join("thumbs").join("00000000000000aa.jpg");
        std::fs::write(&kthumb, b"k").unwrap();
        c.put(&known.to_string_lossy(), 1, 1, 330, &kthumb.to_string_lossy(), 1, 1);

        // An original whose row was lost: its thumbnail is recomputable.
        let lost = folder.join("lost.jpg");
        std::fs::write(&lost, b"lost-original").unwrap();
        let m = std::fs::metadata(&lost).unwrap();
        let mtime = m.modified().unwrap().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis();
        let name = format!(
            "{:016x}.jpg",
            crate::fnv1a64(format!("{}|{}|{}|{}", lost.to_string_lossy(), mtime, m.len(), 330).as_bytes())
        );
        let rescued = c.dir.join("thumbs").join(&name);
        image::RgbImage::new(4, 3).save(&rescued).unwrap();

        // A stale thumbnail nobody can claim, and a foreign file.
        let stale = c.dir.join("thumbs").join("0123456789abcdef.jpg");
        std::fs::write(&stale, b"old").unwrap();
        let foreign = c.dir.join("thumbs").join("notes.txt");
        std::fs::write(&foreign, b"keep me").unwrap();
        // Age them past the "being written right now" window.
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        for f in [&rescued, &stale] {
            std::fs::File::options().write(true).open(f).unwrap()
                .set_times(std::fs::FileTimes::new().set_modified(old)).unwrap();
        }

        let rep = c.purge_orphans(&|_| {}).unwrap();

        assert_eq!(rep.reattached, 1, "the lost row is rebuilt");
        assert!(rescued.exists(), "and its thumbnail kept");
        let row: (u32, u32) = c.conn.lock().unwrap()
            .query_row("SELECT w, h FROM thumbs WHERE thumb=?1", params![rescued.to_string_lossy()], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        assert_eq!(row, (4, 3), "with the thumbnail's real size");
        assert_eq!(rep.strays, 1, "the unclaimable one is deleted");
        assert!(!stale.exists());
        assert!(foreign.exists(), "a file that is not ours stays");
    }

    /// The case met on a real cache: the only file the database knew in a
    /// folder was moved away, but the folder lives on with another picture
    /// whose row was lost. Its thumbnail must be re-attached, not deleted.
    #[test]
    fn a_folder_known_only_through_a_vanished_file_is_still_searched() {
        let tmp = tempfile::tempdir().unwrap();
        let c = ThumbCache::open(&tmp.path().join("cache")).unwrap();
        let folder = tmp.path().join("to_sort");
        std::fs::create_dir_all(&folder).unwrap();

        // Known to the database, then moved away: the folder's only row.
        let moved = folder.join("moved.jpg");
        let mthumb = c.dir.join("thumbs").join("00000000000000bb.jpg");
        std::fs::write(&mthumb, b"m").unwrap();
        c.put(&moved.to_string_lossy(), 1, 1, 330, &mthumb.to_string_lossy(), 1, 1);

        // Still in the folder, row lost.
        let stays = folder.join("stays.jpg");
        std::fs::write(&stays, b"still-here").unwrap();
        let m = std::fs::metadata(&stays).unwrap();
        let mtime = m.modified().unwrap().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis();
        let name = format!(
            "{:016x}.jpg",
            crate::fnv1a64(format!("{}|{}|{}|{}", stays.to_string_lossy(), mtime, m.len(), 330).as_bytes())
        );
        let thumb = c.dir.join("thumbs").join(&name);
        image::RgbImage::new(2, 2).save(&thumb).unwrap();
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        std::fs::File::options().write(true).open(&thumb).unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(old)).unwrap();

        let rep = c.purge_orphans(&|_| {}).unwrap();

        assert_eq!(rep.originals, 1, "the moved file is gone");
        assert_eq!(rep.reattached, 1, "the picture still in the folder gets its row back");
        assert!(thumb.exists(), "and keeps its thumbnail");
    }

    /// A changed original replaces its thumbnail AND removes the old file.
    #[test]
    fn replacing_a_thumbnail_removes_the_old_file() {
        let tmp = tempfile::tempdir().unwrap();
        let c = ThumbCache::open(&tmp.path().join("cache")).unwrap();
        let a = c.dir.join("thumbs").join("1111111111111111.jpg");
        let b = c.dir.join("thumbs").join("2222222222222222.jpg");
        std::fs::write(&a, b"v1").unwrap();
        std::fs::write(&b, b"v2").unwrap();
        c.put("E:/p.jpg", 1, 1, 330, &a.to_string_lossy(), 1, 1);
        c.put("E:/p.jpg", 2, 2, 330, &b.to_string_lossy(), 1, 1);
        assert!(!a.exists(), "the superseded thumbnail is gone");
        assert!(b.exists());
    }

    #[test]
    fn roundtrip_and_invalidation() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = ThumbCache::open(tmp.path()).unwrap();

        // Create a fake thumb file so existence check passes.
        let tp = tmp.path().join("thumbs").join("x.jpg");
        std::fs::write(&tp, b"fake").unwrap();
        let tp = tp.to_string_lossy().to_string();

        cache.put("C:/pic.jpg", 111, 999, 384, &tp, 384, 256);
        assert!(cache.get("C:/pic.jpg", 111, 999, 384).is_some());
        // Different mtime -> stale -> miss.
        assert!(cache.get("C:/pic.jpg", 222, 999, 384).is_none());
        // Different requested size -> miss.
        assert!(cache.get("C:/pic.jpg", 111, 999, 256).is_none());

        assert!(cache.size_on_disk() > 0);
        cache.clear();
        assert!(cache.get("C:/pic.jpg", 111, 999, 384).is_none());
    }
}
