// Recursive search with metadata filters.
//
// Results stream through the `emit` callback as they are found, so the UI
// can display them progressively; returning `false` from `emit` aborts the
// walk (cancel / newer search started).

use crate::{scan, Entry, EntryKind};
use std::path::Path;

#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct SearchFilter {
    /// Lowercase substring of the file name; empty matches everything.
    #[serde(default)]
    pub query: String,
    /// Restrict to a media kind ("image" matches Image+Raw); empty = all media.
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub min_size: u64,
    /// 0 = no upper bound.
    #[serde(default)]
    pub max_size: u64,
    #[serde(default)]
    pub min_mtime: i64,
    /// 0 = no upper bound.
    #[serde(default)]
    pub max_mtime: i64,
    #[serde(default)]
    pub include_hidden: bool,
}

impl SearchFilter {
    fn matches(&self, e: &Entry) -> bool {
        if e.hidden && !self.include_hidden {
            return false;
        }
        match self.kind.as_str() {
            "image" => {
                if e.kind != EntryKind::Image && e.kind != EntryKind::Raw {
                    return false;
                }
            }
            "video" => {
                if e.kind != EntryKind::Video {
                    return false;
                }
            }
            _ => {
                if e.kind == EntryKind::Other {
                    return false;
                }
            }
        }
        if !self.query.is_empty() && !e.name.to_lowercase().contains(&self.query) {
            return false;
        }
        if e.size < self.min_size {
            return false;
        }
        if self.max_size > 0 && e.size > self.max_size {
            return false;
        }
        if e.mtime < self.min_mtime {
            return false;
        }
        if self.max_mtime > 0 && e.mtime > self.max_mtime {
            return false;
        }
        true
    }
}

/// Depth-first recursive search under `root`.
pub fn search<F: FnMut(Entry) -> bool>(root: &Path, filter: &SearchFilter, emit: &mut F) -> bool {
    let Ok(entries) = scan::list_dir(root) else {
        return true;
    };
    for e in entries {
        if e.kind == EntryKind::Dir {
            if e.hidden && !filter.include_hidden {
                continue;
            }
            if !search(Path::new(&e.path), filter, emit) {
                return false;
            }
        } else if filter.matches(&e) {
            if !emit(e) {
                return false;
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;

    #[test]
    fn recursive_and_filtered() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("sub")).unwrap();
        File::create(tmp.path().join("top.jpg")).unwrap();
        File::create(tmp.path().join("sub").join("deep.mkv")).unwrap();
        File::create(tmp.path().join("sub").join("notes.txt")).unwrap();

        let mut found = Vec::new();
        let f = SearchFilter::default();
        search(tmp.path(), &f, &mut |e| {
            found.push(e.name);
            true
        });
        found.sort();
        assert_eq!(found, ["deep.mkv", "top.jpg"]);

        let mut vids = Vec::new();
        let f = SearchFilter { kind: "video".into(), ..Default::default() };
        search(tmp.path(), &f, &mut |e| {
            vids.push(e.name);
            true
        });
        assert_eq!(vids, ["deep.mkv"]);
    }
}
