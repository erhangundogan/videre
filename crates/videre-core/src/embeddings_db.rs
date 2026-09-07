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

fn validate_context_storage(ctx: &crate::library::LibraryContext) -> Result<()> {
    ctx.ensure_root_identity()?;
    crate::library_locks::reject_dir_redirect(&ctx.paths.embeddings, "the embeddings directory")?;
    Ok(())
}

/// Full path to one model database inside the selected library.
///
/// This is a path-only lookup. It validates the root and any existing state
/// redirection, but creates no directory or database.
pub fn db_path_in(ctx: &crate::library::LibraryContext, model_id: &str) -> Result<PathBuf> {
    crate::embeddings::validate_model_id(model_id)?;
    validate_context_storage(ctx)?;
    let path = ctx
        .paths
        .embeddings
        .join(format!("{}.{DB_EXT}", model_slug(model_id)));
    crate::library_locks::reject_redirect(&path, "the model database")?;
    Ok(path)
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

/// Attach one model database from the selected library as `emb`.
pub fn attach_in(
    conn: &Connection,
    ctx: &crate::library::LibraryContext,
    model_id: &str,
    create: bool,
) -> Result<()> {
    crate::library_locks::verify_state(ctx)?;
    let path = db_path_in(ctx, model_id)?;
    if !path.exists() {
        if !create {
            let available = list_models_in(ctx).unwrap_or_default();
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
        std::fs::create_dir_all(&ctx.paths.embeddings)
            .with_context(|| format!("create {}", ctx.paths.embeddings.display()))?;
        validate_context_storage(ctx)?;
        init_model_db(&path)?;
    }
    crate::library_locks::reject_redirect(&path, "the model database")?;
    let meta =
        std::fs::symlink_metadata(&path).with_context(|| format!("inspect {}", path.display()))?;
    anyhow::ensure!(meta.is_file(), "{} is not a regular file", path.display());
    conn.execute(
        &format!("ATTACH DATABASE ?1 AS {ATTACH_ALIAS}"),
        [path.to_string_lossy().as_ref()],
    )
    .with_context(|| format!("attach {}", path.display()))?;
    if create {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS emb.embeddings (
                hash        TEXT PRIMARY KEY NOT NULL,
                model_id    TEXT NOT NULL,
                embedding   BLOB NOT NULL,
                embedded_at TEXT NOT NULL
            );",
        )
        .with_context(|| format!("ensure schema in {}", path.display()))?;
    }
    Ok(())
}

/// Model ids with a database in the selected library, sorted.
pub fn list_models_in(ctx: &crate::library::LibraryContext) -> Result<Vec<String>> {
    validate_context_storage(ctx)?;
    let entries = match std::fs::read_dir(&ctx.paths.embeddings) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(error).with_context(|| format!("read {}", ctx.paths.embeddings.display()))
        }
    };
    let mut models = Vec::new();
    for entry in entries {
        let entry = entry.with_context(|| format!("read {}", ctx.paths.embeddings.display()))?;
        let path = entry.path();
        if path.extension().is_none_or(|extension| extension != DB_EXT) {
            continue;
        }
        crate::library_locks::reject_redirect(&path, "the model database")?;
        let meta = std::fs::symlink_metadata(&path)
            .with_context(|| format!("inspect {}", path.display()))?;
        anyhow::ensure!(meta.is_file(), "{} is not a regular file", path.display());
        if let Some(stem) = path.file_stem() {
            models.push(model_from_slug(&stem.to_string_lossy()));
        }
    }
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

/// Row count, dimensions and file size for each model in the selected library.
pub fn counts_by_model_in(
    ctx: &crate::library::LibraryContext,
) -> Result<Vec<ModelEmbeddingCount>> {
    let mut out = Vec::new();
    for model_id in list_models_in(ctx)? {
        let path = db_path_in(ctx, &model_id)?;
        let size_bytes = std::fs::metadata(&path)
            .with_context(|| format!("inspect {}", path.display()))?
            .len() as i64;
        let conn = Connection::open(&path).with_context(|| format!("open {}", path.display()))?;
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM embeddings", [], |row| row.get(0))
            .unwrap_or(0);
        let dims: i64 = conn
            .query_row(
                "SELECT LENGTH(embedding) / 2 FROM embeddings LIMIT 1",
                [],
                |row| row.get(0),
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

/// Attach for a reader: a missing model database is an error naming the models
/// that do exist.
///
/// Attach a selected library's existing model database for reading. Kept as a
/// distinct name from `attach_in(.., false)` so call sites read as intent
/// rather than as a boolean.
pub fn attach_for_read_in(
    conn: &Connection,
    ctx: &crate::library::LibraryContext,
    model_id: &str,
) -> Result<()> {
    attach_in(conn, ctx, model_id, false)
}

/// DETACH the model database. Needed before attaching a different model on
/// the same connection, since the alias may bind only one file at a time.
pub fn detach(conn: &Connection) -> Result<()> {
    conn.execute(&format!("DETACH DATABASE {ATTACH_ALIAS}"), [])
        .context("detach embeddings database")?;
    Ok(())
}

/// Test-only: a fully initialized [`LibraryContext`](crate::library::LibraryContext)
/// under a fresh per-tag directory, ready to hand to the `_in` model-store
/// helpers. No process-global environment is touched: each library's stores
/// live under its own root, so parallel tests never collide.
///
/// **`tag` must be unique across the whole test binary.** This wipes its
/// directory on entry, so two tests sharing a tag delete each other's store
/// mid-run and fail intermittently.
#[cfg(test)]
pub(crate) fn test_context(tag: &str) -> crate::library::LibraryContext {
    let base = std::env::temp_dir().join(format!("videre-embdb-{}-{}", tag, std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let root = base.join("lib");
    std::fs::create_dir_all(&root).unwrap();
    let ctx = crate::library::LibraryContext::new(&root, &base.join("cache")).unwrap();
    // The model-store helpers verify the state directory exists before
    // attaching, the one thing `library_db::initialize` would otherwise set up.
    std::fs::create_dir_all(&ctx.paths.state).unwrap();
    ctx
}

#[cfg(test)]
mod tests {
    use super::*;

    fn explicit_context(parent: &Path, name: &str) -> crate::library::LibraryContext {
        let root = parent.join(name);
        std::fs::create_dir(&root).unwrap();
        crate::library::LibraryContext::new(&root, &parent.join("cache")).unwrap()
    }

    #[test]
    fn explicit_model_stores_are_isolated_and_keep_f16_dimensions() {
        let temp = tempfile::tempdir().unwrap();
        let a = explicit_context(temp.path(), "a");
        let b = explicit_context(temp.path(), "b");
        let a_conn = crate::library_db::initialize(&a).unwrap();
        let b_conn = crate::library_db::initialize(&b).unwrap();
        for model in ["owner/model-a", "owner/model-b"] {
            attach_in(&a_conn, &a, model, true).unwrap();
            a_conn
                .execute(
                    "INSERT INTO emb.embeddings (hash, model_id, embedding, embedded_at)
                     VALUES ('shared', ?1, zeroblob(1536), 'now')",
                    [model],
                )
                .unwrap();
            detach(&a_conn).unwrap();
        }
        attach_in(&b_conn, &b, "owner/model-a", true).unwrap();
        b_conn
            .execute(
                "INSERT INTO emb.embeddings (hash, model_id, embedding, embedded_at)
                 VALUES ('shared', ?1, zeroblob(8), 'now')",
                ["owner/model-a"],
            )
            .unwrap();
        detach(&b_conn).unwrap();

        assert_eq!(
            list_models_in(&a).unwrap(),
            vec!["owner/model-a", "owner/model-b"]
        );
        let a_counts = counts_by_model_in(&a).unwrap();
        assert_eq!(a_counts.len(), 2);
        assert!(a_counts
            .iter()
            .all(|count| count.count == 1 && count.dims == 768));
        let b_counts = counts_by_model_in(&b).unwrap();
        assert_eq!(b_counts[0].count, 1);
        assert_eq!(b_counts[0].dims, 4);
        assert_ne!(
            db_path_in(&a, "owner/model-a").unwrap(),
            db_path_in(&b, "owner/model-a").unwrap()
        );
    }

    #[test]
    fn explicit_read_attachment_does_not_create_a_missing_store() {
        let temp = tempfile::tempdir().unwrap();
        let ctx = explicit_context(temp.path(), "library");
        let conn = crate::library_db::initialize(&ctx).unwrap();
        let expected = db_path_in(&ctx, "owner/missing").unwrap();
        let error = attach_for_read_in(&conn, &ctx, "owner/missing").unwrap_err();
        assert!(format!("{error:#}").contains("no embeddings for owner/missing"));
        assert!(!expected.exists());
        assert!(!ctx.paths.embeddings.exists());
    }

    /// Gives one test its own directory under the shared per-binary
    /// `VIDERE_HOME`. See `test_home` for why the home is not set per test.
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
    fn db_path_in_joins_the_embeddings_dir_and_model_slug() {
        let ctx = test_context("dbpath");
        let p = db_path_in(&ctx, "google/siglip2-base-patch16-384").unwrap();
        assert_eq!(
            p.file_name().unwrap(),
            "google--siglip2-base-patch16-384.db"
        );
        assert_eq!(p.parent().unwrap(), ctx.paths.embeddings);
    }

    #[test]
    fn attach_with_create_makes_a_database_with_the_chosen_page_size() {
        let ctx = test_context("create");
        let conn = Connection::open_in_memory().unwrap();
        attach_in(&conn, &ctx, "google/siglip2-base-patch16-384", true).unwrap();

        // Read the pragma back rather than assuming the write took: a
        // page_size set after the file has content is silently ignored.
        let ps: i64 = conn
            .query_row("PRAGMA emb.page_size", [], |r| r.get(0))
            .unwrap();
        assert_eq!(ps, PAGE_SIZE);
    }

    #[test]
    fn attach_with_create_is_idempotent_and_preserves_rows() {
        let ctx = test_context("idem");
        let model = "google/siglip2-base-patch16-384";

        let c1 = Connection::open_in_memory().unwrap();
        attach_in(&c1, &ctx, model, true).unwrap();
        c1.execute(
            "INSERT INTO emb.embeddings (hash, model_id, embedding, embedded_at)
             VALUES ('h1', ?1, X'0102', '2026-08-05T00:00:00')",
            [model],
        )
        .unwrap();
        detach(&c1).unwrap();
        drop(c1);

        let c2 = Connection::open_in_memory().unwrap();
        attach_in(&c2, &ctx, model, true).unwrap();
        let n: i64 = c2
            .query_row("SELECT COUNT(*) FROM emb.embeddings", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1, "re-attaching must not clobber existing rows");
    }

    #[test]
    fn attach_with_create_repairs_a_file_that_exists_without_the_table() {
        // `path.exists()` alone is not enough: initialisation can be cut short
        // by a crash or a full disk, and attaching the resulting file leaves it
        // broken forever because every later call sees the file and skips init.
        // Surfaced as an intermittent "no such table: emb.embeddings" in tests.
        let ctx = test_context("repair");
        let model = "google/siglip2-base-patch16-384";
        let path = db_path_in(&ctx, model).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"").unwrap(); // exists, but empty

        let conn = Connection::open_in_memory().unwrap();
        attach_in(&conn, &ctx, model, true).unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM emb.embeddings", [], |r| r.get(0))
            .expect("the table must exist after attach_in(create: true)");
        assert_eq!(n, 0);
    }

    #[test]
    fn attach_without_create_errors_and_names_available_models() {
        let ctx = test_context("missing");
        let conn = Connection::open_in_memory().unwrap();
        attach_in(&conn, &ctx, "google/siglip2-base-patch16-384", true).unwrap();
        detach(&conn).unwrap();

        let err = attach_in(&conn, &ctx, "google/siglip-base-patch16-224", false).unwrap_err();
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
    }

    #[test]
    fn two_models_do_not_see_each_others_rows() {
        let ctx = test_context("isolate");
        let a = "google/siglip2-base-patch16-384";
        let b = "google/siglip-base-patch16-224";

        let conn = Connection::open_in_memory().unwrap();
        attach_in(&conn, &ctx, a, true).unwrap();
        conn.execute(
            "INSERT INTO emb.embeddings (hash, model_id, embedding, embedded_at)
             VALUES ('h1', ?1, X'0102', 'now')",
            [a],
        )
        .unwrap();
        detach(&conn).unwrap();

        attach_in(&conn, &ctx, b, true).unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM emb.embeddings", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0, "model b must not see model a's rows");
    }

    #[test]
    fn attached_table_is_visible_through_emb_sqlite_master() {
        // Regression guard. `sqlite_master` is per-database: the unqualified
        // form returns 0 once the table is attached, and every caller treats
        // 0 as "not embedded yet" rather than as an error, so the failure is
        // silent. This test fails against the unqualified query.
        let ctx = test_context("master");
        let conn = Connection::open_in_memory().unwrap();
        attach_in(&conn, &ctx, "google/siglip2-base-patch16-384", true).unwrap();

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
    }

    #[test]
    fn detach_allows_attaching_a_different_model_on_the_same_connection() {
        let ctx = test_context("reattach");
        let conn = Connection::open_in_memory().unwrap();
        attach_in(&conn, &ctx, "google/siglip2-base-patch16-384", true).unwrap();
        detach(&conn).unwrap();
        attach_in(&conn, &ctx, "google/siglip-base-patch16-224", true).unwrap();
        detach(&conn).unwrap();
    }

    #[test]
    fn list_models_returns_sorted_ids_and_ignores_unrelated_files() {
        let ctx = test_context("list");
        let conn = Connection::open_in_memory().unwrap();
        for m in [
            "google/siglip2-base-patch16-384",
            "google/siglip-base-patch16-224",
        ] {
            attach_in(&conn, &ctx, m, true).unwrap();
            detach(&conn).unwrap();
        }
        // WAL sidecars and stray files must not be mistaken for models.
        std::fs::write(ctx.paths.embeddings.join("notes.txt"), b"x").unwrap();

        let models = list_models_in(&ctx).unwrap();
        assert_eq!(
            models,
            vec![
                "google/siglip-base-patch16-224".to_string(),
                "google/siglip2-base-patch16-384".to_string(),
            ]
        );
    }

    #[test]
    fn list_models_on_a_library_with_no_embeddings_is_empty_not_an_error() {
        let ctx = test_context("listempty");
        assert!(list_models_in(&ctx).unwrap().is_empty());
    }

    #[test]
    fn counts_by_model_reports_rows_dims_and_size() {
        let ctx = test_context("counts");
        let model = "google/siglip2-base-patch16-384";
        let conn = Connection::open_in_memory().unwrap();
        attach_in(&conn, &ctx, model, true).unwrap();
        // 768 dims f16 = 1536 bytes
        conn.execute(
            "INSERT INTO emb.embeddings (hash, model_id, embedding, embedded_at)
             VALUES ('h1', ?1, zeroblob(1536), 'now')",
            [model],
        )
        .unwrap();
        detach(&conn).unwrap();

        let counts = counts_by_model_in(&ctx).unwrap();
        assert_eq!(counts.len(), 1);
        assert_eq!(counts[0].model_id, model);
        assert_eq!(counts[0].count, 1);
        assert_eq!(
            counts[0].dims, 768,
            "dims derive from blob length, not a table"
        );
        assert!(counts[0].size_bytes > 0);
    }

    #[test]
    fn counts_by_model_reports_zero_dims_for_an_empty_model_database() {
        let ctx = test_context("countsempty");
        let conn = Connection::open_in_memory().unwrap();
        attach_in(&conn, &ctx, "google/siglip2-base-patch16-384", true).unwrap();
        detach(&conn).unwrap();

        let counts = counts_by_model_in(&ctx).unwrap();
        assert_eq!(counts.len(), 1);
        assert_eq!(counts[0].count, 0);
        assert_eq!(counts[0].dims, 0);
    }

    #[test]
    fn path_computation_creates_nothing() {
        // Readers must be able to ask "which model database would this be"
        // without bringing the store directory into existence.
        let ctx = test_context("nocreate");
        let _ = db_path_in(&ctx, "google/siglip2-base-patch16-384").unwrap();
        assert!(
            !ctx.paths.embeddings.exists(),
            "path computation must not create {:?}",
            ctx.paths.embeddings
        );
    }
}
