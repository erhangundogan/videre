//! Embeddings table: one row per unique content hash, keyed to file_hashes.hash.

use rusqlite::{Connection, Result, params};

/// Extensions the embedding pipeline can decode. `.mov`/`.mp4` are handled by
/// extracting one representative frame via QuickLook (macOS only, degrades to
/// a per-file decode error on other platforms, same pattern already
/// accepted for `.heic`). See
/// docs/superpowers/specs/2026-07-31-video-embedding-design.md.
///
/// `.dng` is deliberately NOT included: the `image` crate has no DNG decoder,
/// so including it here would make `videre embed` query DNG hashes as
/// pending and fail to decode every single one, forever, on every run -
/// scanning/EXIF extraction for `.dng` still work fine elsewhere (see
/// `scanner.rs`/`hasher.rs`), only embedding is unsupported.
pub const EMBEDDABLE_EXTS: &[&str] = &[
    "jpg", "jpeg", "png", "gif", "webp", "bmp", "tiff", "heic", "mov", "mp4",
];

/// True if `ext` (any case) is a video extension handled by single-frame
/// QuickLook extraction. Shared by every "is this a video" check in
/// videre-core so the extension list can't drift between call sites.
pub fn is_video_ext(ext: &str) -> bool {
    matches!(ext.to_lowercase().as_str(), "mov" | "mp4")
}

/// Model id used by `videre embed` / `search` / `report` when neither
/// `--model` nor `config.toml` names one. Single source of truth so the report
/// binary can query embeddings without depending on videre-ml.
///
/// Changed 2026-08-06 from `google/siglip2-base-patch16-384`. Measured on a
/// real 70,587 photo library: 63ms per photo against 131ms, taking a full
/// re-embed from roughly 2.6 hours to 1.2. All three candidate models were
/// embedded in full and compared side by side on real photos; every one
/// returned correct results, and pairwise agreement rose from 33% at k=6 to
/// about 68% at k=200, showing they draw from the same pool of correct answers
/// and differ mainly in ordering. With quality indistinguishable by
/// inspection, speed decides.
///
/// It sees each photo at 224px rather than 384px, which is where the speed
/// comes from and the first place to look if fine-detail queries disappoint.
///
/// Changing this invalidates nothing: each model owns a separate database
/// under `~/.videre/embeddings/` (see `crate::embeddings_db`), so switching
/// leaves previous vectors intact and queryable via `--model`. The new model
/// simply starts from zero and needs its own `videre embed` run.
pub const DEFAULT_MODEL_ID: &str = "google/siglip-base-patch16-224";

/// The model to use, given an explicit home: `--model` > `config.toml` > the
/// built-in default.
///
/// Split from `resolve_model_id` the same way `home::resolve_db_in` is split
/// from `home::resolve_db`, so tests can pass a home directly instead of
/// mutating `VIDERE_HOME`. Tests share a process and run in parallel, so a
/// per-test `set_var` races every concurrent `getenv`.
///
/// Note the return type is `anyhow::Result`, not this module's `Result`, which
/// is `rusqlite::Result`. It returns a Result at all so a malformed
/// `config.toml` stays a hard error; silently falling back to the default
/// would mask a typo in the one file the user edits by hand.
pub fn resolve_model_id_in(
    home: &std::path::Path,
    explicit: Option<&str>,
) -> anyhow::Result<String> {
    if let Some(id) = explicit {
        return Ok(id.to_string());
    }
    Ok(crate::home::load_config(home)?
        .default_model
        .unwrap_or_else(|| DEFAULT_MODEL_ID.to_string()))
}

/// `resolve_model_id_in` against the resolved videre home.
///
/// `VIDERE_EMBED_MODEL` was removed rather than demoted. Two ways to set one
/// thing is confusing, and an export made months ago silently outranking the
/// config file is a bad failure mode. `--model` covers the one-off case.
pub fn resolve_model_id(explicit: Option<&str>) -> anyhow::Result<String> {
    resolve_model_id_in(&crate::home::videre_home()?, explicit)
}

#[derive(Debug, Clone)]
pub struct PendingImage {
    pub hash: String,
    pub path: String,
}

/// Create the index the embedding joins depend on.
///
/// Only the index: the `embeddings` table itself now lives in a per-model
/// database created by `embeddings_db::attach`. This index belongs to
/// `file_hashes` and stays in the main database, where the joins actually
/// run; moving it along with the table would be a silent performance
/// regression on every one of them.
pub fn ensure_embeddings_index(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_file_hashes_hash ON file_hashes(hash);",
    )
}

/// Unique hashes that are embeddable but not yet embedded under `model_id`;
/// one representative path per hash (MIN(path) keeps it deterministic).
pub fn pending_images(conn: &Connection, model_id: &str) -> Result<Vec<PendingImage>> {
    let placeholders = EMBEDDABLE_EXTS
        .iter()
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(",");
    let model_param = EMBEDDABLE_EXTS.len() + 1;
    let sql = format!(
        "SELECT hash, MIN(path) FROM file_hashes
         WHERE lower(ext) IN ({placeholders})
           AND NOT EXISTS (SELECT 1 FROM emb.embeddings e
                           WHERE e.hash = file_hashes.hash AND e.model_id = ?{model_param})
         GROUP BY hash
         ORDER BY hash"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(
        rusqlite::params_from_iter(EMBEDDABLE_EXTS.iter().copied().chain(std::iter::once(model_id))),
        |row| {
            Ok(PendingImage {
                hash: row.get(0)?,
                path: row.get(1)?,
            })
        },
    )?;
    rows.collect()
}

/// Upsert a batch of (hash, f16 blob) rows inside one transaction.
pub fn insert_embeddings(
    conn: &Connection,
    model_id: &str,
    items: &[(String, Vec<u8>)],
) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    {
        let mut stmt = tx.prepare(
            "INSERT OR REPLACE INTO emb.embeddings (hash, model_id, embedding, embedded_at)
             VALUES (?1, ?2, ?3, datetime('now'))",
        )?;
        for (hash, blob) in items {
            stmt.execute(params![hash, model_id, blob])?;
        }
    }
    tx.commit()
}

/// Version that drops the legacy read path below. Greppable on purpose, so
/// removing it is a scheduled change rather than a promise in a commit
/// message nobody rereads.
pub const LEGACY_FALLBACK_REMOVE_IN: &str = "0.11.0";

/// Rows in a pre-split `embeddings` table still sitting in the main database.
///
/// Detection only: nothing is read, moved, or deleted.
pub fn legacy_main_db_embedding_count(conn: &Connection) -> Result<i64> {
    if !crate::db::table_exists(conn, "embeddings")? {
        return Ok(0);
    }
    conn.query_row("SELECT COUNT(*) FROM main.embeddings", [], |r| r.get(0))
}

/// Print the legacy-embeddings note at most once per process.
pub fn warn_legacy_embeddings_once(conn: &Connection) {
    static WARNED: std::sync::Once = std::sync::Once::new();
    let count = legacy_main_db_embedding_count(conn).unwrap_or(0);
    if count == 0 {
        return;
    }
    WARNED.call_once(|| {
        eprintln!(
            "note: {count} embedding(s) are still in the main database. videre reads them for \
             now, but new embeddings go to ~/.videre/embeddings/. Run 'videre embed' to move \
             to the new location; this fallback is removed in {LEGACY_FALLBACK_REMOVE_IN}."
        );
    });
}

/// Returns an empty vec (rather than a raw SQLite error) when no embeddings
/// exist yet, since callers rely on "empty" to mean "run videre embed first"
/// (see `videre search`'s `load_corpus`).
///
/// Falls back to a pre-split `embeddings` table in the main database when the
/// attached one has nothing for this model. Without that, upgrading would
/// strand every existing user's vectors: the new code would not look where
/// they are, and search would return zero hits while reporting success. The
/// attached database always wins, so a re-embed is never shadowed by a stale
/// copy. Remove in `LEGACY_FALLBACK_REMOVE_IN`.
pub fn load_embeddings(conn: &Connection, model_id: &str) -> Result<Vec<(String, Vec<u8>)>> {
    let attached: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM emb.sqlite_master WHERE type='table' AND name='embeddings'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .unwrap_or(0)
        > 0;

    let mut rows: Vec<(String, Vec<u8>)> = Vec::new();
    if attached {
        let mut stmt =
            conn.prepare("SELECT hash, embedding FROM emb.embeddings WHERE model_id = ?1")?;
        rows = stmt
            .query_map(params![model_id], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<Vec<_>>>()?;
    }

    // Named main.embeddings explicitly: with emb attached, an unqualified
    // table name could resolve to either schema depending on attach order.
    if crate::db::table_exists(conn, "embeddings")? {
        let have: std::collections::HashSet<String> = rows.iter().map(|(h, _)| h.clone()).collect();
        let mut stmt =
            conn.prepare("SELECT hash, embedding FROM main.embeddings WHERE model_id = ?1")?;
        for row in stmt.query_map(params![model_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?))
        })? {
            let (hash, blob) = row?;
            if !have.contains(&hash) {
                rows.push((hash, blob));
            }
        }
    }

    Ok(rows)
}

pub fn paths_for_hash(conn: &Connection, hash: &str) -> Result<Vec<String>> {
    let mut stmt =
        conn.prepare("SELECT path FROM file_hashes WHERE hash = ?1 ORDER BY path")?;
    let rows = stmt.query_map(params![hash], |row| row.get(0))?;
    rows.collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    /// A main database with `file_hashes`, plus a real attached model
    /// database. In-memory main with an on-disk `emb` mirrors production: the
    /// split is the thing under test, so faking it with a plain local table
    /// would test nothing and would hide the `sqlite_master` trap entirely.
    fn test_db_attached(tag: &str) -> Connection {
        let lib = crate::embeddings_db::test_library(tag);
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE file_hashes (
                path        TEXT PRIMARY KEY,
                hash        TEXT NOT NULL,
                size_bytes  INTEGER,
                created_at  TEXT,
                modified_at TEXT,
                ext         TEXT,
                phash       INTEGER,
                exif_date   TEXT,
                gps_lat     REAL,
                gps_lon     REAL,
                width       INTEGER,
                height      INTEGER
            );",
        )
        .unwrap();
        ensure_embeddings_index(&conn).unwrap();
        crate::embeddings_db::attach(&conn, &lib, "test-model", true).unwrap();
        conn
    }

    fn insert_file(conn: &Connection, path: &str, hash: &str, ext: &str) {
        conn.execute(
            "INSERT INTO file_hashes (path, hash, ext) VALUES (?1, ?2, ?3)",
            rusqlite::params![path, hash, ext],
        )
        .unwrap();
    }

    #[test]
    fn pending_images_dedupes_by_hash_and_includes_video() {
        let conn = test_db_attached("emb_dedupe");
        insert_file(&conn, "/a/1.jpg", "h1", "jpg");
        insert_file(&conn, "/b/1-copy.jpg", "h1", "jpg"); // same hash, second path
        insert_file(&conn, "/a/2.png", "h2", "png");
        insert_file(&conn, "/a/clip.mp4", "h3", "mp4");   // now embeddable
        insert_file(&conn, "/a/other.xyz", "h4", "xyz");  // still unsupported

        let pending = pending_images(&conn, "test-model").unwrap();
        assert_eq!(pending.len(), 3); // h1 once, h2 once, h3 (video) included, h4 excluded
        assert!(pending.iter().any(|p| p.hash == "h1"));
        assert!(pending.iter().any(|p| p.hash == "h2"));
        assert!(pending.iter().any(|p| p.hash == "h3"));
    }

    #[test]
    fn pending_images_excludes_dng_since_it_cannot_be_decoded() {
        let conn = test_db_attached("emb_dng");
        insert_file(&conn, "/a/1.jpg", "h1", "jpg");
        insert_file(&conn, "/a/raw.dng", "h2", "dng");

        let pending = pending_images(&conn, "test-model").unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].hash, "h1");
    }

    #[test]
    fn pending_images_excludes_already_embedded() {
        let conn = test_db_attached("emb_already");
        insert_file(&conn, "/a/1.jpg", "h1", "jpg");
        insert_file(&conn, "/a/2.jpg", "h2", "jpg");
        insert_embeddings(&conn, "test-model", &[("h1".to_string(), vec![0u8; 4])]).unwrap();

        let pending = pending_images(&conn, "test-model").unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].hash, "h2");
    }

    #[test]
    fn pending_images_is_model_aware() {
        let conn = test_db_attached("emb_modelaware");
        insert_file(&conn, "/a/1.jpg", "h1", "jpg");
        insert_embeddings(&conn, "a", &[("h1".to_string(), vec![0u8; 4])]).unwrap();

        // Embedded under model "a": nothing pending for "a" ...
        assert!(pending_images(&conn, "a").unwrap().is_empty());

        // ... but still pending for model "b" (re-embedding with a new model).
        let pending = pending_images(&conn, "b").unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].hash, "h1");
    }

    #[test]
    fn insert_embeddings_empty_slice_succeeds() {
        let conn = test_db_attached("emb_empty");
        insert_embeddings(&conn, "test-model", &[]).unwrap();
        assert!(load_embeddings(&conn, "test-model").unwrap().is_empty());
    }

    #[test]
    fn insert_and_load_round_trip() {
        let conn = test_db_attached("emb_roundtrip");
        insert_embeddings(
            &conn,
            "test-model",
            &[("h1".to_string(), vec![1u8, 2, 3, 4]), ("h2".to_string(), vec![5u8, 6])],
        )
        .unwrap();

        let rows = load_embeddings(&conn, "test-model").unwrap();
        assert_eq!(rows.len(), 2);
        let h1 = rows.iter().find(|(h, _)| h == "h1").unwrap();
        assert_eq!(h1.1, vec![1u8, 2, 3, 4]);

        // different model_id loads nothing
        assert!(load_embeddings(&conn, "other").unwrap().is_empty());
    }

    #[test]
    fn load_embeddings_finds_the_table_in_the_attached_database() {
        // Guards the sqlite_master trap: that view is per-database, so the
        // unqualified probe returns 0 for an attached table, and
        // load_embeddings reads 0 as "nothing embedded yet". The failure is
        // silent, so only a test like this catches it.
        let conn = test_db_attached("emb_attachedprobe");
        insert_embeddings(&conn, "test-model", &[("h1".to_string(), vec![1u8, 2])]).unwrap();

        let rows = load_embeddings(&conn, "test-model").unwrap();
        assert_eq!(rows.len(), 1, "must read through emb., not main");
    }

    #[test]
    fn ensure_embeddings_index_creates_the_index_in_the_main_database() {
        // The index belongs to file_hashes and must stay in main; moving it
        // with the table would silently regress every join.
        let conn = test_db_attached("emb_indexmain");
        let found: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM main.sqlite_master
                 WHERE type='index' AND name='idx_file_hashes_hash'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(found, 1);
    }

    #[test]
    fn load_embeddings_falls_back_to_the_main_database() {
        // The upgrade path. Without this an existing user's vectors sit in
        // the main database, the new code never looks there, and search
        // returns zero hits while reporting success.
        let conn = test_db_attached("emb_fallback");
        conn.execute_batch(
            "CREATE TABLE main.embeddings (
                hash TEXT PRIMARY KEY, model_id TEXT NOT NULL,
                embedding BLOB NOT NULL, embedded_at TEXT NOT NULL
            );
            INSERT INTO main.embeddings VALUES ('h1', 'test-model', X'0102', 'now');",
        )
        .unwrap();

        let rows = load_embeddings(&conn, "test-model").unwrap();
        assert_eq!(rows.len(), 1, "must fall back to main");
        assert_eq!(rows[0].1, vec![1u8, 2]);
    }

    #[test]
    fn the_attached_database_wins_over_the_legacy_one() {
        // Once a hash is embedded in the new location, the stale main-db copy
        // must never shadow it, or a re-embed would appear to do nothing.
        let conn = test_db_attached("emb_precedence");
        conn.execute_batch(
            "CREATE TABLE main.embeddings (
                hash TEXT PRIMARY KEY, model_id TEXT NOT NULL,
                embedding BLOB NOT NULL, embedded_at TEXT NOT NULL
            );
            INSERT INTO main.embeddings VALUES ('h1', 'test-model', X'FFFF', 'now');",
        )
        .unwrap();
        insert_embeddings(&conn, "test-model", &[("h1".to_string(), vec![1u8, 2])]).unwrap();

        let rows = load_embeddings(&conn, "test-model").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].1, vec![1u8, 2], "the new location must win");
    }

    #[test]
    fn fallback_respects_the_model_filter() {
        let conn = test_db_attached("emb_fallbackmodel");
        conn.execute_batch(
            "CREATE TABLE main.embeddings (
                hash TEXT PRIMARY KEY, model_id TEXT NOT NULL,
                embedding BLOB NOT NULL, embedded_at TEXT NOT NULL
            );
            INSERT INTO main.embeddings VALUES ('h1', 'other/model', X'0102', 'now');",
        )
        .unwrap();

        assert!(load_embeddings(&conn, "test-model").unwrap().is_empty());
    }

    #[test]
    fn legacy_detection_counts_without_touching_anything() {
        let conn = test_db_attached("emb_legacycount");
        conn.execute_batch(
            "CREATE TABLE main.embeddings (
                hash TEXT PRIMARY KEY, model_id TEXT NOT NULL,
                embedding BLOB NOT NULL, embedded_at TEXT NOT NULL
            );
            INSERT INTO main.embeddings VALUES ('h1', 'old/model', X'01', 'now');",
        )
        .unwrap();

        assert_eq!(legacy_main_db_embedding_count(&conn).unwrap(), 1);
        let still_there: i64 = conn
            .query_row("SELECT COUNT(*) FROM main.embeddings", [], |r| r.get(0))
            .unwrap();
        assert_eq!(still_there, 1, "detection must never delete anything");
    }

    #[test]
    fn legacy_detection_returns_zero_when_there_is_no_such_table() {
        let conn = test_db_attached("emb_nolegacy");
        assert_eq!(legacy_main_db_embedding_count(&conn).unwrap(), 0);
    }

    #[test]
    fn paths_for_hash_returns_all_duplicates() {
        let conn = test_db_attached("emb_paths");
        insert_file(&conn, "/a/1.jpg", "h1", "jpg");
        insert_file(&conn, "/b/1-copy.jpg", "h1", "jpg");
        let paths = paths_for_hash(&conn, "h1").unwrap();
        assert_eq!(paths.len(), 2);
    }

    #[test]
    fn default_model_id_is_a_siglip_checkpoint() {
        // Deliberately not pinned to one exact id: the default model has
        // changed once already (2026-08-04, so400m-384 -> siglip2-base-384 for
        // a measured 3.6x speedup) and pinning it only made this test fail as
        // a formality. What actually matters is that it stays a real HF
        // owner/name SigLIP id, since `Embedder::load` splits on '/' and the
        // whole embeddings table is keyed by this string.
        assert!(DEFAULT_MODEL_ID.starts_with("google/"), "{DEFAULT_MODEL_ID}");
        assert!(DEFAULT_MODEL_ID.contains("siglip"), "{DEFAULT_MODEL_ID}");
        assert_eq!(DEFAULT_MODEL_ID.matches('/').count(), 1, "{DEFAULT_MODEL_ID}");
    }

    fn cfg_home(tag: &str, toml_text: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("videre_rmi_{}_{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        if !toml_text.is_empty() {
            std::fs::write(dir.join("config.toml"), toml_text).unwrap();
        }
        dir
    }

    #[test]
    fn resolve_model_id_prefers_the_explicit_argument() {
        let home = cfg_home("explicit", "default_model = \"owner/from-config\"\n");
        assert_eq!(
            resolve_model_id_in(&home, Some("owner/explicit")).unwrap(),
            "owner/explicit"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn resolve_model_id_uses_config_when_there_is_no_flag() {
        let home = cfg_home("fromconfig", "default_model = \"owner/from-config\"\n");
        assert_eq!(resolve_model_id_in(&home, None).unwrap(), "owner/from-config");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn resolve_model_id_falls_back_to_the_builtin_default() {
        let home = cfg_home("builtin", "");
        assert_eq!(resolve_model_id_in(&home, None).unwrap(), DEFAULT_MODEL_ID);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn videre_embed_model_env_var_has_no_effect() {
        // The env var is gone. Written to fail against the old implementation,
        // since deleting a branch is exactly the change that gets half-done.
        // Safe to set: after this change nothing reads it.
        let home = cfg_home("noenv", "");
        std::env::set_var("VIDERE_EMBED_MODEL", "owner/should-be-ignored");
        let got = resolve_model_id_in(&home, None).unwrap();
        std::env::remove_var("VIDERE_EMBED_MODEL");
        assert_eq!(got, DEFAULT_MODEL_ID, "VIDERE_EMBED_MODEL must be ignored");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_malformed_config_is_an_error_not_a_silent_default() {
        let home = cfg_home("malformed", "not = = toml\n");
        assert!(resolve_model_id_in(&home, None).is_err());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn is_video_ext_matches_mov_and_mp4_case_insensitively() {
        assert!(is_video_ext("mov"));
        assert!(is_video_ext("MP4"));
        assert!(is_video_ext("Mov"));
        assert!(!is_video_ext("jpg"));
        assert!(!is_video_ext(""));
    }
}
