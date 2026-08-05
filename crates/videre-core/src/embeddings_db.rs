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
use rusqlite::Connection;
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

/// Page size for model databases, overriding SQLite's 4096 default.
///
/// Measured 2026-08-05 over 20,000 synthetic rows, extrapolated to 70,587:
///
/// | page_size | 1152-dim | 768-dim |
/// |-----------|----------|---------|
/// | 4096      | 282 MB   | 143 MB  |
/// | 8192      | 189 MB   | 143 MB  |
/// | 16384     | 189 MB   | 128 MB  |
/// | 32768     | 175 MB   | 122 MB  |
///
/// 8192 recovers a third of a 1152-dimension model's footprint but does
/// nothing at all for 768-dimension models, which is where future data goes.
/// 16384 is the first size that improves both. 32768 buys a further 5% at the
/// cost of reading 32KB to touch one vector.
pub const PAGE_SIZE: i64 = 16384;

/// Initialise a new model database at `path`: page size, WAL, schema.
///
/// Done on a standalone connection, before any ATTACH, because `page_size`
/// only takes effect on an empty database and must be set before
/// `journal_mode = WAL` and before any table exists. Setting it later is
/// silently ignored and needs a full VACUUM to apply.
fn init_model_db(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let conn = Connection::open(path).with_context(|| format!("create {}", path.display()))?;
    conn.pragma_update(None, "page_size", PAGE_SIZE)
        .context("set page_size")?;
    conn.pragma_update(None, "journal_mode", "WAL")
        .context("set journal_mode")?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS embeddings (
            hash        TEXT PRIMARY KEY NOT NULL,
            model_id    TEXT NOT NULL,
            embedding   BLOB NOT NULL,
            embedded_at TEXT NOT NULL
        );",
    )
    .with_context(|| format!("create embeddings table in {}", path.display()))?;
    Ok(())
}

/// ATTACH the model database for `(db_path, model_id)` as `emb`.
///
/// With `create`, a missing file is initialised first; this is reached only
/// from `videre embed`. Without, a missing file is an error naming the models
/// that do exist, so the user is never left guessing why search is empty.
pub fn attach(conn: &Connection, db_path: &Path, model_id: &str, create: bool) -> Result<()> {
    let path = self::db_path(db_path, model_id)?;
    if !path.exists() {
        if !create {
            let available = list_models(db_path).unwrap_or_default();
            let available = if available.is_empty() {
                "(none)".to_string()
            } else {
                available.join(", ")
            };
            anyhow::bail!(
                "no embeddings for {model_id} in this library\n  \
                 expected: {}\n  available: {available}\n  \
                 run: videre embed --model {model_id}",
                path.display()
            );
        }
        init_model_db(&path)?;
    }
    conn.execute(
        &format!("ATTACH DATABASE ?1 AS {ATTACH_ALIAS}"),
        [path.to_string_lossy().as_ref()],
    )
    .with_context(|| format!("attach {}", path.display()))?;
    Ok(())
}

/// Model ids with an existing database for this library, sorted.
///
/// A missing directory is an empty list, not an error: a library that has
/// never been embedded is a normal state, not a fault.
pub fn list_models(db_path: &Path) -> Result<Vec<String>> {
    let dir = library_dir(db_path)?;
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("read {}", dir.display())),
    };
    let mut models: Vec<String> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == DB_EXT))
        .filter_map(|p| p.file_stem().map(|s| model_from_slug(&s.to_string_lossy())))
        .collect();
    models.sort();
    Ok(models)
}

/// One model's embedding inventory, for `videre stats`.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ModelEmbeddingCount {
    pub model_id: String,
    pub count: i64,
    /// Vector dimensions, derived from stored blob length rather than a
    /// hardcoded per-model table, so an unfamiliar model still reports
    /// honestly. 0 when the database holds no rows yet.
    pub dims: i64,
    pub size_bytes: i64,
}

/// Row count, dimensions, and file size for every model in this library.
pub fn counts_by_model(db_path: &Path) -> Result<Vec<ModelEmbeddingCount>> {
    let mut out = Vec::new();
    for model_id in list_models(db_path)? {
        let path = self::db_path(db_path, &model_id)?;
        let size_bytes = std::fs::metadata(&path).map(|m| m.len() as i64).unwrap_or(0);
        let conn = Connection::open(&path).with_context(|| format!("open {}", path.display()))?;
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM embeddings", [], |r| r.get(0))
            .unwrap_or(0);
        let dims: i64 = conn
            .query_row(
                "SELECT LENGTH(embedding) / 2 FROM embeddings LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        out.push(ModelEmbeddingCount {
            model_id,
            count,
            dims,
            size_bytes,
        });
    }
    Ok(out)
}

/// DETACH the model database. Needed before attaching a different model on
/// the same connection, since the alias may bind only one file at a time.
pub fn detach(conn: &Connection) -> Result<()> {
    conn.execute(&format!("DETACH DATABASE {ATTACH_ALIAS}"), [])
        .context("detach embeddings database")?;
    Ok(())
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
    fn attach_with_create_makes_a_database_with_the_chosen_page_size() {
        with_home("create", |home| {
            let lib = touch_db(home, "hashes.db");
            let conn = Connection::open_in_memory().unwrap();
            attach(&conn, &lib, "google/siglip2-base-patch16-384", true).unwrap();

            // Read the pragma back rather than assuming the write took: a
            // page_size set after the file has content is silently ignored.
            let ps: i64 = conn
                .query_row("PRAGMA emb.page_size", [], |r| r.get(0))
                .unwrap();
            assert_eq!(ps, PAGE_SIZE);
        });
    }

    #[test]
    fn attach_with_create_is_idempotent_and_preserves_rows() {
        with_home("idem", |home| {
            let lib = touch_db(home, "hashes.db");
            let model = "google/siglip2-base-patch16-384";

            let c1 = Connection::open_in_memory().unwrap();
            attach(&c1, &lib, model, true).unwrap();
            c1.execute(
                "INSERT INTO emb.embeddings (hash, model_id, embedding, embedded_at)
                 VALUES ('h1', ?1, X'0102', '2026-08-05T00:00:00')",
                [model],
            )
            .unwrap();
            detach(&c1).unwrap();
            drop(c1);

            let c2 = Connection::open_in_memory().unwrap();
            attach(&c2, &lib, model, true).unwrap();
            let n: i64 = c2
                .query_row("SELECT COUNT(*) FROM emb.embeddings", [], |r| r.get(0))
                .unwrap();
            assert_eq!(n, 1, "re-attaching must not clobber existing rows");
        });
    }

    #[test]
    fn attach_without_create_errors_and_names_available_models() {
        with_home("missing", |home| {
            let lib = touch_db(home, "hashes.db");
            let conn = Connection::open_in_memory().unwrap();
            attach(&conn, &lib, "google/siglip2-base-patch16-384", true).unwrap();
            detach(&conn).unwrap();

            let err = attach(&conn, &lib, "google/siglip-base-patch16-224", false).unwrap_err();
            let msg = format!("{err:#}");
            assert!(
                msg.contains("no embeddings for google/siglip-base-patch16-224"),
                "{msg}"
            );
            assert!(
                msg.contains("google/siglip2-base-patch16-384"),
                "error must list what IS available: {msg}"
            );
            assert!(msg.contains("videre embed --model"), "{msg}");
        });
    }

    #[test]
    fn two_models_do_not_see_each_others_rows() {
        with_home("isolate", |home| {
            let lib = touch_db(home, "hashes.db");
            let a = "google/siglip2-base-patch16-384";
            let b = "google/siglip-base-patch16-224";

            let conn = Connection::open_in_memory().unwrap();
            attach(&conn, &lib, a, true).unwrap();
            conn.execute(
                "INSERT INTO emb.embeddings (hash, model_id, embedding, embedded_at)
                 VALUES ('h1', ?1, X'0102', 'now')",
                [a],
            )
            .unwrap();
            detach(&conn).unwrap();

            attach(&conn, &lib, b, true).unwrap();
            let n: i64 = conn
                .query_row("SELECT COUNT(*) FROM emb.embeddings", [], |r| r.get(0))
                .unwrap();
            assert_eq!(n, 0, "model b must not see model a's rows");
        });
    }

    #[test]
    fn attached_table_is_visible_through_emb_sqlite_master() {
        // Regression guard. `sqlite_master` is per-database: the unqualified
        // form returns 0 once the table is attached, and every caller treats
        // 0 as "not embedded yet" rather than as an error, so the failure is
        // silent. This test fails against the unqualified query.
        with_home("master", |home| {
            let lib = touch_db(home, "hashes.db");
            let conn = Connection::open_in_memory().unwrap();
            attach(&conn, &lib, "google/siglip2-base-patch16-384", true).unwrap();

            let found: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM emb.sqlite_master
                     WHERE type='table' AND name='embeddings'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(found, 1);

            let unqualified: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master
                     WHERE type='table' AND name='embeddings'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(unqualified, 0, "documents exactly why emb. is required");
        });
    }

    #[test]
    fn detach_allows_attaching_a_different_model_on_the_same_connection() {
        with_home("reattach", |home| {
            let lib = touch_db(home, "hashes.db");
            let conn = Connection::open_in_memory().unwrap();
            attach(&conn, &lib, "google/siglip2-base-patch16-384", true).unwrap();
            detach(&conn).unwrap();
            attach(&conn, &lib, "google/siglip-base-patch16-224", true).unwrap();
            detach(&conn).unwrap();
        });
    }

    #[test]
    fn list_models_returns_sorted_ids_and_ignores_unrelated_files() {
        with_home("list", |home| {
            let lib = touch_db(home, "hashes.db");
            let conn = Connection::open_in_memory().unwrap();
            for m in [
                "google/siglip2-base-patch16-384",
                "google/siglip-base-patch16-224",
            ] {
                attach(&conn, &lib, m, true).unwrap();
                detach(&conn).unwrap();
            }
            // WAL sidecars and stray files must not be mistaken for models.
            let dir = library_dir(&lib).unwrap();
            std::fs::write(dir.join("notes.txt"), b"x").unwrap();

            let models = list_models(&lib).unwrap();
            assert_eq!(
                models,
                vec![
                    "google/siglip-base-patch16-224".to_string(),
                    "google/siglip2-base-patch16-384".to_string(),
                ]
            );
        });
    }

    #[test]
    fn list_models_on_a_library_with_no_embeddings_is_empty_not_an_error() {
        with_home("listempty", |home| {
            let lib = touch_db(home, "hashes.db");
            assert!(list_models(&lib).unwrap().is_empty());
        });
    }

    #[test]
    fn counts_by_model_reports_rows_dims_and_size() {
        with_home("counts", |home| {
            let lib = touch_db(home, "hashes.db");
            let model = "google/siglip2-base-patch16-384";
            let conn = Connection::open_in_memory().unwrap();
            attach(&conn, &lib, model, true).unwrap();
            // 768 dims f16 = 1536 bytes
            conn.execute(
                "INSERT INTO emb.embeddings (hash, model_id, embedding, embedded_at)
                 VALUES ('h1', ?1, zeroblob(1536), 'now')",
                [model],
            )
            .unwrap();
            detach(&conn).unwrap();

            let counts = counts_by_model(&lib).unwrap();
            assert_eq!(counts.len(), 1);
            assert_eq!(counts[0].model_id, model);
            assert_eq!(counts[0].count, 1);
            assert_eq!(
                counts[0].dims, 768,
                "dims derive from blob length, not a table"
            );
            assert!(counts[0].size_bytes > 0);
        });
    }

    #[test]
    fn counts_by_model_reports_zero_dims_for_an_empty_model_database() {
        with_home("countsempty", |home| {
            let lib = touch_db(home, "hashes.db");
            let conn = Connection::open_in_memory().unwrap();
            attach(&conn, &lib, "google/siglip2-base-patch16-384", true).unwrap();
            detach(&conn).unwrap();

            let counts = counts_by_model(&lib).unwrap();
            assert_eq!(counts.len(), 1);
            assert_eq!(counts[0].count, 0);
            assert_eq!(counts[0].dims, 0);
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
