//! Shared change-detection for the walk: given the stored row signatures, decide
//! whether a walked file still needs processing. Used by `scan` and by `watch`'s
//! scan stage, so the "skip unchanged files" rule has one definition.

use std::collections::HashMap;
use std::path::Path;
use videre_core::db::{is_current, mtime_iso, RowSig};

/// Whether a walked path needs processing: a new file (no row), a changed file
/// (size or mtime differs), a file that cannot be stat'd (let the hash path deal
/// with it), or a row incomplete for the requested work (missing mime, or no
/// phash when `--similar`). Returns false only when the row is already current.
pub fn needs_processing(sigs: &HashMap<String, RowSig>, path: &Path, want_similar: bool) -> bool {
    let key = path.to_string_lossy();
    match sigs.get(key.as_ref()) {
        None => true,
        Some(sig) => match std::fs::metadata(path) {
            Err(_) => true,
            Ok(meta) => {
                let cur_mtime = meta.modified().ok().map(mtime_iso);
                !is_current(sig, meta.len(), cur_mtime.as_deref(), want_similar)
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::SystemTime;

    fn sig_for(path: &Path, has_mime: bool, has_phash: bool) -> RowSig {
        let meta = fs::metadata(path).unwrap();
        RowSig {
            size_bytes: meta.len(),
            modified_at: meta.modified().ok().map(mtime_iso),
            has_mime,
            has_phash,
        }
    }

    #[test]
    fn current_file_is_skipped_changed_and_new_are_not() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.jpg");
        fs::write(&p, b"hello").unwrap();
        let mut sigs = HashMap::new();
        sigs.insert(p.to_string_lossy().into_owned(), sig_for(&p, true, false));

        // unchanged + complete -> skip
        assert!(!needs_processing(&sigs, &p, false));
        // --similar with no phash -> process (backfill)
        assert!(needs_processing(&sigs, &p, true));

        // size change -> process
        fs::write(&p, b"hello world, now bigger").unwrap();
        assert!(needs_processing(&sigs, &p, false));

        // a path with no stored row -> process
        let q = dir.path().join("b.jpg");
        fs::write(&q, b"x").unwrap();
        assert!(needs_processing(&sigs, &q, false));
    }
}
