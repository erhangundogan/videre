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
    if !path.exists() {
        return (0, 0);
    }
    if path.is_file() {
        return (path.metadata().map(|m| m.len()).unwrap_or(0), 1);
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
            let Ok(md) = entry.metadata() else { continue };
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

/// Everything videre stores, largest first.
///
/// `db` is passed in because the database is the one piece that does not have
/// to live under the home directory: `--db` puts it anywhere.
///
/// :warning: **Embeddings are per library, not per home.** They live in
/// `<home>/embeddings/<db stem>-<hash16>`, so summing the whole `embeddings`
/// directory attributes every library's vectors to whichever one is being
/// reported. `lib_embeddings` is this library's directory; anything else under
/// there is reported separately and labelled as belonging to other libraries,
/// because it is real disk use but not this library's.
///
/// :warning: The thumbnail cache is **not** always under the home directory.
/// With `VIDERE_HOME` unset it lives in the platform cache directory, so a
/// report that only walked the home would silently omit the largest deletable
/// thing videre owns. That is why it is a parameter rather than being looked up
/// here: `thumb_cache::cache_dir()` reads the environment, and a function whose
/// result depends on an env var it never mentions cannot be tested or reasoned
/// about. Callers pass it in.
pub fn usage(
    home: &Path,
    db: Option<&Path>,
    thumbs: &Path,
    lib_embeddings: Option<&Path>,
) -> Vec<Usage> {
    /// Sizes `path` and returns a row for it, or `None` when there is nothing
    /// there.
    ///
    /// Zero-byte entries are omitted rather than listed as `0 B`: an empty WAL
    /// or an unused locks directory is a row to skip past, not information.
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

    let mut out: Vec<Usage> = Vec::new();

    if let Some(db) = db {
        out.extend(row("database", db.to_path_buf(), false));

        // WAL and shm are one thing to a reader, and can be a meaningful
        // fraction of the total mid-run. Summed into a single row, because two
        // rows the reader has to add up is not a report.
        let (mut bytes, mut files) = (0u64, 0u64);
        let mut wal = db.as_os_str().to_owned();
        for suffix in ["-wal", "-shm"] {
            let mut p = db.as_os_str().to_owned();
            p.push(suffix);
            let (b, f) = dir_size(Path::new(&p));
            bytes += b;
            files += f;
        }
        if bytes > 0 {
            wal.push("-wal");
            out.push(Usage {
                label: "database journal",
                path: PathBuf::from(wal),
                bytes,
                files,
                rebuildable: true,
            });
        }
    }
    // This library's vectors, then everything else under `embeddings/` as its
    // own row: the difference matters because `stats` is reporting one library
    // while the disk is shared by all of them.
    let all_embeddings = dir_size(&home.join("embeddings")).0;
    let mine = match lib_embeddings {
        Some(dir) => {
            let r = row("embeddings", dir.to_path_buf(), false);
            let n = r.as_ref().map(|u| u.bytes).unwrap_or(0);
            out.extend(r);
            n
        }
        None => {
            out.extend(row("embeddings", home.join("embeddings"), false));
            all_embeddings
        }
    };
    if all_embeddings > mine {
        out.push(Usage {
            label: "embeddings (other libraries)",
            path: home.join("embeddings"),
            bytes: all_embeddings - mine,
            files: 0,
            rebuildable: false,
        });
    }
    out.extend(row("thumbnails", thumbs.to_path_buf(), true));
    out.extend(row("place names", home.join("geo"), true));
    out.extend(row("locks", home.join("locks"), true));

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
    fn usage_reports_the_database_and_marks_what_is_rebuildable() {
        let d = tempfile::tempdir().unwrap();
        let db = d.path().join("t.db");
        std::fs::write(&db, vec![0u8; 4096]).unwrap();
        std::fs::create_dir_all(d.path().join("embeddings")).unwrap();
        std::fs::write(d.path().join("embeddings/m.db"), vec![0u8; 8192]).unwrap();

        let u = usage(d.path(), Some(&db), &d.path().join("no-thumbs"), None);
        let by = |l: &str| u.iter().find(|x| x.label == l);

        // Largest first, so embeddings outranks the smaller database.
        assert_eq!(u[0].label, "embeddings");
        assert_eq!(by("embeddings").unwrap().bytes, 8192);
        assert_eq!(by("database").unwrap().bytes, 4096);
        assert!(!by("database").unwrap().rebuildable, "a database is not");
        assert!(
            !by("embeddings").unwrap().rebuildable,
            "embeddings cost hours; losing them is not free"
        );
    }

    #[test]
    fn another_librarys_embeddings_are_not_counted_as_this_ones() {
        // Embeddings live at <home>/embeddings/<stem>-<hash>, so the directory
        // is shared by every library on the machine. Reporting the whole thing
        // against one `--db` would inflate it by however many other libraries
        // exist, which is exactly the kind of number nobody notices is wrong.
        let d = tempfile::tempdir().unwrap();
        let db = d.path().join("t.db");
        std::fs::write(&db, vec![0u8; 16]).unwrap();
        let mine = d.path().join("embeddings/mine-0000000000000001");
        let theirs = d.path().join("embeddings/theirs-0000000000000002");
        std::fs::create_dir_all(&mine).unwrap();
        std::fs::create_dir_all(&theirs).unwrap();
        std::fs::write(mine.join("m.db"), vec![0u8; 1000]).unwrap();
        std::fs::write(theirs.join("m.db"), vec![0u8; 7000]).unwrap();

        let u = usage(
            d.path(),
            Some(&db),
            &d.path().join("no-thumbs"),
            Some(&mine),
        );
        let by = |l: &str| u.iter().find(|x| x.label == l).map(|x| x.bytes);
        assert_eq!(by("embeddings"), Some(1000), "only this library's vectors");
        assert_eq!(
            by("embeddings (other libraries)"),
            Some(7000),
            "the rest is still disk use, but it is not this library's"
        );
    }

    #[test]
    fn nothing_present_reports_nothing_rather_than_a_row_of_zeroes() {
        let d = tempfile::tempdir().unwrap();
        let u = usage(d.path(), None, &d.path().join("no-thumbs"), None);
        assert!(
            u.iter().all(|x| x.bytes > 0 || x.files > 0),
            "empty locations must not be listed"
        );
    }
}
