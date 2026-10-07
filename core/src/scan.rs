// Directory scanning: fast, non-recursive listing of a folder.

use crate::{classify, Entry, EntryKind};
use std::fs;
use std::path::Path;
use std::time::UNIX_EPOCH;

/// List a directory (non-recursive). Hidden/system entries are skipped.
/// Returns folders first (alphabetical), then files (alphabetical).
pub fn list_dir(dir: &Path) -> std::io::Result<Vec<Entry>> {
    let mut dirs: Vec<Entry> = Vec::new();
    let mut files: Vec<Entry> = Vec::new();

    for item in fs::read_dir(dir)? {
        let item = match item {
            Ok(i) => i,
            Err(_) => continue,
        };
        let name = item.file_name().to_string_lossy().to_string();
        if name.eq_ignore_ascii_case("$RECYCLE.BIN")
            || name.eq_ignore_ascii_case("System Volume Information")
        {
            continue;
        }
        let meta = match item.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let hidden = name.starts_with('.') || {
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                meta.file_attributes() & 0x2 != 0 // FILE_ATTRIBUTE_HIDDEN
            }
            #[cfg(not(windows))]
            {
                false
            }
        };
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let path = item.path().to_string_lossy().to_string();

        if meta.is_dir() {
            dirs.push(Entry {
                name,
                path,
                kind: EntryKind::Dir,
                size: 0,
                mtime,
                ext: String::new(),
                hidden,
            });
        } else {
            let (kind, ext) = classify(&name);
            files.push(Entry {
                name,
                path,
                kind,
                size: meta.len(),
                mtime,
                ext,
                hidden,
            });
        }
    }

    let by_name = |a: &Entry, b: &Entry| {
        a.name.to_lowercase().cmp(&b.name.to_lowercase())
    };
    dirs.sort_by(by_name);
    files.sort_by(by_name);
    dirs.extend(files);
    Ok(dirs)
}

/// List Windows drive roots (C:\, D:\ ...). On other systems, returns "/".
pub fn list_roots() -> Vec<String> {
    #[cfg(windows)]
    {
        let mut roots = Vec::new();
        for letter in b'A'..=b'Z' {
            let root = format!("{}:\\", letter as char);
            if std::path::Path::new(&root).exists() {
                roots.push(root);
            }
        }
        roots
    }
    #[cfg(not(windows))]
    {
        vec!["/".to_string()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;

    #[test]
    fn lists_and_sorts() {
        let tmp = tempfile::tempdir().unwrap();
        File::create(tmp.path().join("b.jpg")).unwrap();
        File::create(tmp.path().join("a.mkv")).unwrap();
        fs::create_dir(tmp.path().join("zfolder")).unwrap();
        File::create(tmp.path().join(".hidden")).unwrap();

        let entries = list_dir(tmp.path()).unwrap();
        // Hidden entries are returned flagged; the UI decides to show them.
        assert_eq!(entries.len(), 4);
        // Folder first, then files alphabetically.
        assert_eq!(entries[0].name, "zfolder");
        assert_eq!(entries[0].kind, EntryKind::Dir);
        assert_eq!(entries[1].name, ".hidden");
        assert!(entries[1].hidden);
        assert_eq!(entries[2].name, "a.mkv");
        assert_eq!(entries[2].kind, EntryKind::Video);
        assert_eq!(entries[3].name, "b.jpg");
        assert_eq!(entries[3].kind, EntryKind::Image);
    }
}
