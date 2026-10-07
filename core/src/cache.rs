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
                 pos    REAL NOT NULL DEFAULT 0
             );
             CREATE INDEX IF NOT EXISTS meta_parent ON meta(parent);",
        )?;
        // Older caches lack the rotation column; ignore the error if present.
        let _ = conn.execute("ALTER TABLE meta ADD COLUMN rot INTEGER NOT NULL DEFAULT 0", []);
        let _ = conn.execute("ALTER TABLE meta ADD COLUMN taken INTEGER NOT NULL DEFAULT 0", []);
        // Playback position of a video, in seconds; 0 = start from the top.
        let _ = conn.execute("ALTER TABLE meta ADD COLUMN pos REAL NOT NULL DEFAULT 0", []);
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
}

impl ThumbCache {
    /// All metadata rows for files directly inside `parent`.
    pub fn dir_meta(&self, parent: &str) -> Vec<MetaRow> {
        let Ok(conn) = self.conn.lock() else { return Vec::new() };
        let Ok(mut stmt) = conn.prepare_cached(
            "SELECT path, rating, color, flag, tags, rot, pos FROM meta WHERE parent = ?1",
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
