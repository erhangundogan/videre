//! Per-model embedding databases: one SQLite file per (library, model) pair,
//! attached to the main connection as `emb`.
//!
//! Embeddings used to live in an `embeddings` table inside the main library
//! database, tagged with a `model_id` column. That allowed exactly one model
//! to be usable at a time (every read filters on `model_id`, so switching
//! models made the whole library look unembedded) and left the main database
//! roughly three-quarters vectors. See
//! docs/superpowers/specs/2026-08-05-multi-model-embeddings-split-design.md.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Schema alias the model database is attached under.
pub const ATTACH_ALIAS: &str = "emb";

/// Filename extension for a model database.
const DB_EXT: &str = "db";

/// `google/siglip2-base-patch16-384` -> `google--siglip2-base-patch16-384`.
///
/// Mirrors the Hugging Face cache convention already on disk at
/// `~/.cache/huggingface/hub/models--google--siglip2-base-patch16-384`, so the
/// two directories read the same way.
pub fn model_slug(model_id: &str) -> String {
    model_id.replace('/', "--")
}

/// Inverse of `model_slug`. Only the first separator is restored: HF ids are
/// `owner/name` with exactly one `/`, and a name may legitimately contain `--`.
pub fn model_from_slug(slug: &str) -> String {
    slug.replacen("--", "/", 1)
}

/// Directory holding every model database for one library:
/// `<home>/embeddings/<db stem>-<hash16>`.
///
/// The hash of the canonical path is load-bearing, not decoration, exactly as
/// in `pipeline_runs::lock_path_for`: two libraries can both be named
/// `photos.db` in different directories, and keying on the stem alone would
/// silently merge their embeddings. Canonicalizing first also collapses a
/// symlink and a relative path to one directory.
///
/// Path only; creating the directory is the caller's job, so readers never
/// bring videre's home into existence just by looking.
pub fn library_dir(db_path: &Path) -> Result<PathBuf> {
    use std::hash::{Hash, Hasher};
    let canonical = db_path
        .canonicalize()
        .with_context(|| format!("canonicalize {}", db_path.display()))?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    canonical.hash(&mut hasher);
    let stem = canonical
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "db".to_string());
    Ok(crate::home::videre_home()?
        .join("embeddings")
        .join(format!("{stem}-{:016x}", hasher.finish())))
}

/// Full path to one model's database for one library. Path only; creates
/// nothing.
pub fn db_path(db_path: &Path, model_id: &str) -> Result<PathBuf> {
    Ok(library_dir(db_path)?.join(format!("{}.{DB_EXT}", model_slug(model_id))))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Points VIDERE_HOME at a temp dir for the duration of one test.
    /// Tests share a process, so this uses a mutex rather than bare set_var.
    fn with_home<T>(tag: &str, f: impl FnOnce(&Path) -> T) -> T {
        use std::sync::Mutex;
        static LOCK: Mutex<()> = Mutex::new(());
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("videre_embdb_{}_{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("VIDERE_HOME", &dir);
        let out = f(&dir);
        let _ = std::fs::remove_dir_all(&dir);
        out
    }

    fn touch_db(dir: &Path, name: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, b"").unwrap();
        p
    }

    #[test]
    fn model_slug_replaces_the_owner_separator() {
        assert_eq!(
            model_slug("google/siglip2-base-patch16-384"),
            "google--siglip2-base-patch16-384"
        );
    }

    #[test]
    fn model_slug_round_trips_through_model_from_slug() {
        for id in [
            "google/siglip2-base-patch16-384",
            "google/siglip-so400m-patch14-384",
            "google/siglip-base-patch16-224",
        ] {
            assert_eq!(model_from_slug(&model_slug(id)), id);
        }
    }

    #[test]
    fn model_slug_contains_no_path_separator() {
        // The slug becomes a filename; a surviving '/' would silently create
        // a nested directory instead of the intended file.
        assert!(!model_slug("google/siglip2-base-patch16-384").contains('/'));
    }

    #[test]
    fn two_libraries_sharing_a_stem_get_different_directories() {
        with_home("stem", |home| {
            let a_dir = home.join("a");
            let b_dir = home.join("b");
            std::fs::create_dir_all(&a_dir).unwrap();
            std::fs::create_dir_all(&b_dir).unwrap();
            let a = touch_db(&a_dir, "photos.db");
            let b = touch_db(&b_dir, "photos.db");

            let da = library_dir(&a).unwrap();
            let db_ = library_dir(&b).unwrap();
            assert_ne!(da, db_, "same stem in different dirs must not collide");
            assert!(da
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("photos-"));
        });
    }

    #[test]
    fn relative_and_absolute_paths_resolve_to_one_directory() {
        with_home("canon", |home| {
            let abs = touch_db(home, "hashes.db");
            let canonical_home = home.canonicalize().unwrap();
            let rel = canonical_home.join(".").join("hashes.db");
            assert_eq!(library_dir(&abs).unwrap(), library_dir(&rel).unwrap());
        });
    }

    #[test]
    fn db_path_joins_library_dir_and_model_slug() {
        with_home("dbpath", |home| {
            let lib = touch_db(home, "hashes.db");
            let p = db_path(&lib, "google/siglip2-base-patch16-384").unwrap();
            assert_eq!(p.file_name().unwrap(), "google--siglip2-base-patch16-384.db");
            assert_eq!(p.parent().unwrap(), library_dir(&lib).unwrap());
        });
    }

    #[test]
    fn path_computation_creates_nothing() {
        // Readers must be able to ask "which models exist" without bringing
        // videre's home into existence, same rule locks_dir follows.
        with_home("nocreate", |home| {
            let lib = touch_db(home, "hashes.db");
            let dir = library_dir(&lib).unwrap();
            let _ = db_path(&lib, "google/siglip2-base-patch16-384").unwrap();
            assert!(!dir.exists(), "path computation must not create {dir:?}");
        });
    }
}
