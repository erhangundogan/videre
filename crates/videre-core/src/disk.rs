//! What videre is using on disk, and which part of it is what.
//!
//! "videre is using 40GB" is not an answer anyone can act on. "of which 38GB is
//! the thumbnail cache" is, because one of those is deletable and the rest is
//! not. Everything here exists to make that distinction reportable.

use std::path::{Path, PathBuf};

/// One thing taking up space, and whether losing it would cost anything.
pub struct Usage {
    pub label: &'static str,
    pub path: PathBuf,
    pub bytes: u64,
    pub files: u64,
    /// True when deleting it costs only the time to rebuild. Thumbnails and
    /// HEIC conversions regenerate from the originals; embeddings take hours
    /// and the database cannot be rebuilt at all without a rescan.
    pub rebuildable: bool,
}

/// Total bytes and file count under `path`, following no symlinks.
///
/// Returns `(0, 0)` for a missing path rather than erroring: every caller is
/// reporting, and "not there" and "empty" mean the same thing to a reader.
pub fn dir_size(path: &Path) -> (u64, u64) {
    let Ok(root_meta) = std::fs::symlink_metadata(path) else {
        return (0, 0);
    };
    if !root_meta.is_dir() {
        return (root_meta.len(), 1);
    }
    let (mut bytes, mut files) = (0u64, 0u64);
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            // symlink_metadata, so a link into the library is counted as the
            // link it is rather than the gigabytes it points at.
            let Ok(md) = entry.path().symlink_metadata() else {
                continue;
            };
            if md.is_dir() {
                stack.push(entry.path());
            } else {
                bytes += md.len();
                files += 1;
            }
        }
    }
    (bytes, files)
}

/// Everything stored for one selected library, largest first.
pub fn usage_in(ctx: &crate::library::LibraryContext) -> Vec<Usage> {
    if ctx.ensure_root_identity().is_err() {
        return Vec::new();
    }

    fn row(label: &'static str, path: PathBuf, rebuildable: bool) -> Option<Usage> {
        let (bytes, files) = dir_size(&path);
        (bytes > 0).then_some(Usage {
            label,
            path,
            bytes,
            files,
            rebuildable,
        })
    }

    let mut out = Vec::new();
    out.extend(row("database", ctx.paths.db.clone(), false));
    let (mut journal_bytes, mut journal_files) = (0, 0);
    for suffix in ["-wal", "-shm"] {
        let mut path = ctx.paths.db.as_os_str().to_owned();
        path.push(suffix);
        let (bytes, files) = dir_size(Path::new(&path));
        journal_bytes += bytes;
        journal_files += files;
    }
    if journal_bytes > 0 {
        let mut path = ctx.paths.db.as_os_str().to_owned();
        path.push("-wal");
        out.push(Usage {
            label: "database journal",
            path: PathBuf::from(path),
            bytes: journal_bytes,
            files: journal_files,
            rebuildable: true,
        });
    }
    out.extend(row("embeddings", ctx.paths.embeddings.clone(), false));
    out.extend(row("thumbnails", ctx.cache.thumbnails.clone(), true));
    out.extend(row("place names (shared)", ctx.cache.geo.clone(), true));
    out.extend(row("locks", ctx.paths.locks.clone(), true));
    out.sort_by(|a, b| b.bytes.cmp(&a.bytes));
    out
}

/// Bytes as a person would say them: `1.4 GB`, `812 MB`, `4.0 KB`.
pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut v = bytes as f64;
    let mut unit = 0;
    while v >= 1024.0 && unit < UNITS.len() - 1 {
        v /= 1024.0;
        unit += 1;
    }
    format!("{v:.1} {}", UNITS[unit])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_path_is_zero_not_an_error() {
        assert_eq!(dir_size(Path::new("/definitely/not/here")), (0, 0));
    }

    #[test]
    fn sizes_a_tree_including_nested_files() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("a"), b"12345").unwrap();
        std::fs::create_dir(d.path().join("sub")).unwrap();
        std::fs::write(d.path().join("sub/b"), b"123").unwrap();
        assert_eq!(dir_size(d.path()), (8, 2));
    }

    #[test]
    fn a_single_file_sizes_as_itself() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("x");
        std::fs::write(&f, b"1234").unwrap();
        assert_eq!(dir_size(&f), (4, 1));
    }

    #[test]
    fn bytes_read_the_way_a_person_says_them() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1024), "1.0 KB");
        assert_eq!(human_bytes(1_048_576), "1.0 MB");
        assert_eq!(human_bytes(1_503_238_553), "1.4 GB");
    }

    #[test]
    fn explicit_usage_reports_only_the_selected_library_and_labels_shared_geo() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("library");
        let cache = temp.path().join("cache");
        std::fs::create_dir(&root).unwrap();
        let ctx = crate::library::LibraryContext::new(&root, &cache).unwrap();
        std::fs::create_dir_all(&ctx.paths.embeddings).unwrap();
        std::fs::create_dir_all(&ctx.cache.thumbnails).unwrap();
        std::fs::create_dir_all(&ctx.cache.geo).unwrap();
        std::fs::write(&ctx.paths.db, vec![0u8; 16]).unwrap();
        std::fs::write(ctx.paths.embeddings.join("model.db"), vec![0u8; 32]).unwrap();
        std::fs::write(ctx.cache.thumbnails.join("thumb.jpg"), vec![0u8; 64]).unwrap();
        std::fs::write(ctx.cache.geo.join("cities.csv"), vec![0u8; 8]).unwrap();

        let usage = usage_in(&ctx);
        let labels: Vec<_> = usage.iter().map(|item| item.label).collect();
        assert!(labels.contains(&"database"));
        assert!(labels.contains(&"embeddings"));
        assert!(labels.contains(&"thumbnails"));
        assert!(labels.contains(&"place names (shared)"));
        assert!(!labels.contains(&"embeddings (other libraries)"));
    }

    #[cfg(unix)]
    #[test]
    fn directory_size_does_not_follow_symlinks() {
        let temp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("large"), vec![0u8; 4096]).unwrap();
        std::os::unix::fs::symlink(outside.path(), temp.path().join("link")).unwrap();
        let (bytes, files) = dir_size(temp.path());
        assert_eq!(files, 1);
        assert!(bytes < 4096);
    }
}
