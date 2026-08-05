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

/// Model id used by `videre embed` / `search` / `report`. Single source of
/// truth so the report binary can query embeddings without depending on
/// videre-ml.
///
/// Changed 2026-08-04 from `google/siglip-so400m-patch14-384`. Measured on
/// 2,080 real photos: this model embeds at 131ms/photo against the old one's
/// 479ms, **3.6x faster**, taking a full 70k-photo library from ~9.4 hours to
/// ~2.6 hours, at 768 dimensions instead of 1152. In a blind side-by-side of
/// 14 searches the old model showed no visible advantage, and this one beat
/// the same-size/same-resolution previous-generation `siglip-base-patch16-384`
/// outright.
///
/// `VIDERE_EMBED_MODEL` overrides this (see `videre_ml::model`).
/// `google/siglip-base-patch16-224` is the tested fast option: 63ms/photo,
/// 7.7x faster, at the cost of seeing each photo at 224px rather than 384px.
///
/// **Changing this invalidates every stored embedding**, since `embeddings`
/// rows are tagged with the model id and `pending_images` filters on it. That
/// is intentional, vectors from different models are not comparable, but it
/// means a one-time full re-embed. `videre embed` warns when it sees rows from
/// a different model rather than silently reprocessing the whole library.
pub const DEFAULT_MODEL_ID: &str = "google/siglip2-base-patch16-384";

#[derive(Debug, Clone)]
pub struct PendingImage {
    pub hash: String,
    pub path: String,
}

pub fn ensure_embeddings_table(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS embeddings (
            hash        TEXT PRIMARY KEY NOT NULL,
            model_id    TEXT NOT NULL,
            embedding   BLOB NOT NULL,
            embedded_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_file_hashes_hash ON file_hashes(hash);",
    )
}

/// Counts stored embeddings that came from a *different* model than
/// `model_id`, i.e. rows that `pending_images` will treat as unembedded.
///
/// Exists so `videre embed` can say "your 70,000 embeddings were made with
/// another model and are about to be redone" instead of silently spending
/// hours reprocessing a library the user believed was already done. Returns
/// `Ok(0)` when the table doesn't exist yet.
pub fn embeddings_from_other_models(conn: &Connection, model_id: &str) -> Result<(usize, Vec<String>)> {
    if !crate::db::table_exists(conn, "embeddings")? {
        return Ok((0, vec![]));
    }
    // rusqlite 0.40 dropped the `FromSql` impl for `usize`; read the count as
    // the i64 SQLite actually returns and narrow afterwards.
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM embeddings WHERE model_id != ?1",
        [model_id],
        |r| r.get(0),
    )?;
    let count = count.max(0) as usize;
    let mut stmt =
        conn.prepare("SELECT DISTINCT model_id FROM embeddings WHERE model_id != ?1")?;
    let ids = stmt
        .query_map([model_id], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok((count, ids))
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
           AND NOT EXISTS (SELECT 1 FROM embeddings e
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
            "INSERT OR REPLACE INTO embeddings (hash, model_id, embedding, embedded_at)
             VALUES (?1, ?2, ?3, datetime('now'))",
        )?;
        for (hash, blob) in items {
            stmt.execute(params![hash, model_id, blob])?;
        }
    }
    tx.commit()
}

/// Returns an empty vec (rather than a raw SQLite error) when the
/// `embeddings` table doesn't exist yet, a db that's been scanned but never
/// embedded has no such table, and callers rely on "empty" to mean "run
/// videre embed first" (see `videre search`'s `load_corpus`).
pub fn load_embeddings(conn: &Connection, model_id: &str) -> Result<Vec<(String, Vec<u8>)>> {
    let table_exists: bool = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='embeddings'",
        [],
        |r| r.get::<_, i64>(0),
    )? > 0;
    if !table_exists {
        return Ok(Vec::new());
    }
    let mut stmt =
        conn.prepare("SELECT hash, embedding FROM embeddings WHERE model_id = ?1")?;
    let rows = stmt.query_map(params![model_id], |row| Ok((row.get(0)?, row.get(1)?)))?;
    rows.collect()
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

    fn test_db() -> Connection {
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
        ensure_embeddings_table(&conn).unwrap();
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
        let conn = test_db();
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
        let conn = test_db();
        insert_file(&conn, "/a/1.jpg", "h1", "jpg");
        insert_file(&conn, "/a/raw.dng", "h2", "dng");

        let pending = pending_images(&conn, "test-model").unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].hash, "h1");
    }

    #[test]
    fn pending_images_excludes_already_embedded() {
        let conn = test_db();
        insert_file(&conn, "/a/1.jpg", "h1", "jpg");
        insert_file(&conn, "/a/2.jpg", "h2", "jpg");
        insert_embeddings(&conn, "test-model", &[("h1".to_string(), vec![0u8; 4])]).unwrap();

        let pending = pending_images(&conn, "test-model").unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].hash, "h2");
    }

    #[test]
    fn pending_images_is_model_aware() {
        let conn = test_db();
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
        let conn = test_db();
        insert_embeddings(&conn, "test-model", &[]).unwrap();
        assert!(load_embeddings(&conn, "test-model").unwrap().is_empty());
    }

    #[test]
    fn insert_and_load_round_trip() {
        let conn = test_db();
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
    fn paths_for_hash_returns_all_duplicates() {
        let conn = test_db();
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

    #[test]
    fn is_video_ext_matches_mov_and_mp4_case_insensitively() {
        assert!(is_video_ext("mov"));
        assert!(is_video_ext("MP4"));
        assert!(is_video_ext("Mov"));
        assert!(!is_video_ext("jpg"));
        assert!(!is_video_ext(""));
    }
}
