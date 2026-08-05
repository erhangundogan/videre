//! Classifications table: one row per unique content hash (photo/screenshot/
//! document/meme/unknown), keyed to embeddings.hash. Zero-shot classification
//! reuses embeddings `videre embed` already computed. See
//! docs/superpowers/specs/2026-07-29-screenshot-document-classification-design.md.

use rusqlite::{Connection, Result, params};

pub fn ensure_classifications_table(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS classifications (
            hash          TEXT PRIMARY KEY NOT NULL,
            category      TEXT NOT NULL,
            confidence    REAL NOT NULL,
            classified_at TEXT NOT NULL
        );",
    )
}

/// Hashes that have an embedding under `model_id` but no classification yet.
/// Excludes video hashes (`.mov`/`.mp4`), none of the four zero-shot
/// categories (photo/screenshot/document/meme) fit a video frame well, so
/// videos are never classified, per the video-embedding design's decision.
pub fn pending_hashes(conn: &Connection, model_id: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT hash FROM embeddings
         WHERE model_id = ?1
           AND NOT EXISTS (SELECT 1 FROM classifications c WHERE c.hash = embeddings.hash)
           AND NOT EXISTS (
               SELECT 1 FROM file_hashes fh
               WHERE fh.hash = embeddings.hash AND lower(fh.ext) IN ('mov', 'mp4')
           )
         ORDER BY hash",
    )?;
    let rows = stmt.query_map(params![model_id], |row| row.get(0))?;
    rows.collect()
}

/// Filters `hashes` down to non-video ones, for callers (like `--reprocess`)
/// that build their hash list independently of `pending_hashes` and need the
/// same video exclusion applied so the two paths can't drift apart. A hash
/// with no matching `file_hashes` row (nothing known about its extension) is
/// kept, not excluded, only a *confirmed* video extension is filtered out.
/// Loads the full hash->ext mapping in one query rather than one query per
/// hash, since callers can pass every embedded hash in the library (tens of
/// thousands). See `pending_hashes` above for the equivalent single-query
/// exclusion used when the caller list comes from `embeddings` directly
/// rather than being pre-built like it is here.
pub fn exclude_video_hashes(conn: &Connection, hashes: &[String]) -> Result<Vec<String>> {
    let mut stmt =
        conn.prepare("SELECT hash, lower(ext) FROM file_hashes WHERE ext IS NOT NULL")?;
    let ext_by_hash: std::collections::HashMap<String, String> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;

    Ok(hashes
        .iter()
        .filter(|hash| {
            !ext_by_hash
                .get(*hash)
                .is_some_and(|ext| crate::embeddings::is_video_ext(ext))
        })
        .cloned()
        .collect())
}

/// Upsert a batch of (hash, category, confidence) rows inside one transaction.
pub fn insert_classifications(conn: &Connection, items: &[(String, &str, f32)]) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    {
        let mut stmt = tx.prepare(
            "INSERT OR REPLACE INTO classifications (hash, category, confidence, classified_at)
             VALUES (?1, ?2, ?3, datetime('now'))",
        )?;
        for (hash, category, confidence) in items {
            stmt.execute(params![hash, category, confidence])?;
        }
    }
    tx.commit()
}

/// (path, hash) pairs for every file classified as `category`, one entry per
/// on-disk path of a matched hash (same duplicate-path convention as
/// `embeddings::paths_for_hash`).
pub fn paths_for_category(conn: &Connection, category: &str) -> Result<Vec<(String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT file_hashes.path, file_hashes.hash FROM file_hashes
         JOIN classifications ON file_hashes.hash = classifications.hash
         WHERE classifications.category = ?1
         ORDER BY file_hashes.path",
    )?;
    let rows = stmt.query_map(params![category], |row| Ok((row.get(0)?, row.get(1)?)))?;
    rows.collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE file_hashes (
                path TEXT PRIMARY KEY,
                hash TEXT NOT NULL,
                ext  TEXT
            );
            CREATE TABLE embeddings (
                hash        TEXT PRIMARY KEY,
                model_id    TEXT NOT NULL,
                embedding   BLOB NOT NULL,
                embedded_at TEXT NOT NULL
            );",
        )
        .unwrap();
        ensure_classifications_table(&conn).unwrap();
        conn
    }

    fn insert_file(conn: &Connection, path: &str, hash: &str) {
        conn.execute(
            "INSERT INTO file_hashes (path, hash, ext) VALUES (?1, ?2, 'jpg')",
            rusqlite::params![path, hash],
        )
        .unwrap();
    }

    fn insert_file_with_ext(conn: &Connection, path: &str, hash: &str, ext: &str) {
        conn.execute(
            "INSERT INTO file_hashes (path, hash, ext) VALUES (?1, ?2, ?3)",
            rusqlite::params![path, hash, ext],
        )
        .unwrap();
    }

    fn insert_embedding(conn: &Connection, hash: &str, model_id: &str) {
        conn.execute(
            "INSERT INTO embeddings (hash, model_id, embedding, embedded_at)
             VALUES (?1, ?2, X'00', datetime('now'))",
            rusqlite::params![hash, model_id],
        )
        .unwrap();
    }

    #[test]
    fn pending_hashes_excludes_video_extensions() {
        let conn = test_db();
        insert_file_with_ext(&conn, "/a/1.jpg", "h1", "jpg");
        insert_file_with_ext(&conn, "/a/clip.mp4", "h2", "mp4");
        insert_file_with_ext(&conn, "/a/clip.mov", "h3", "mov");
        insert_embedding(&conn, "h1", "m");
        insert_embedding(&conn, "h2", "m");
        insert_embedding(&conn, "h3", "m");

        let pending = pending_hashes(&conn, "m").unwrap();
        assert_eq!(pending, vec!["h1".to_string()]);
    }

    #[test]
    fn exclude_video_hashes_filters_out_mov_and_mp4() {
        let conn = test_db();
        insert_file_with_ext(&conn, "/a/1.jpg", "h1", "jpg");
        insert_file_with_ext(&conn, "/a/clip.mp4", "h2", "mp4");
        insert_file_with_ext(&conn, "/a/clip.mov", "h3", "mov");

        let all = vec!["h1".to_string(), "h2".to_string(), "h3".to_string()];
        let filtered = exclude_video_hashes(&conn, &all).unwrap();
        assert_eq!(filtered, vec!["h1".to_string()]);
    }

    #[test]
    fn exclude_video_hashes_keeps_hashes_with_no_file_hashes_row() {
        // A hash present in `embeddings` but with no matching `file_hashes` row
        // (e.g. the file was pruned) has no ext to check. Keep it rather than
        // silently dropping it, since it isn't known to be a video.
        let conn = test_db();
        let all = vec!["orphan-hash".to_string()];
        let filtered = exclude_video_hashes(&conn, &all).unwrap();
        assert_eq!(filtered, vec!["orphan-hash".to_string()]);
    }

    #[test]
    fn ensure_classifications_table_is_idempotent() {
        let conn = test_db();
        ensure_classifications_table(&conn).unwrap();
        ensure_classifications_table(&conn).unwrap();
    }

    #[test]
    fn pending_hashes_returns_embedded_but_unclassified() {
        let conn = test_db();
        insert_embedding(&conn, "h1", "m");
        insert_embedding(&conn, "h2", "m");
        insert_classifications(&conn, &[("h1".to_string(), "photo", 0.9)]).unwrap();

        let pending = pending_hashes(&conn, "m").unwrap();
        assert_eq!(pending, vec!["h2".to_string()]);
    }

    #[test]
    fn pending_hashes_is_model_aware() {
        let conn = test_db();
        insert_embedding(&conn, "h1", "model-a");

        assert_eq!(pending_hashes(&conn, "model-a").unwrap(), vec!["h1".to_string()]);
        assert!(pending_hashes(&conn, "model-b").unwrap().is_empty());
    }

    #[test]
    fn insert_classifications_upserts_on_conflict() {
        let conn = test_db();
        insert_classifications(&conn, &[("h1".to_string(), "screenshot", 0.4)]).unwrap();
        insert_classifications(&conn, &[("h1".to_string(), "photo", 0.9)]).unwrap();

        let category: String = conn
            .query_row("SELECT category FROM classifications WHERE hash = 'h1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(category, "photo");
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM classifications", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1); // upsert, not a second row
    }

    #[test]
    fn paths_for_category_returns_matching_paths_with_hash() {
        let conn = test_db();
        insert_file(&conn, "/a/1.jpg", "h1");
        insert_file(&conn, "/b/1-copy.jpg", "h1");
        insert_file(&conn, "/a/2.png", "h2");
        insert_classifications(
            &conn,
            &[
                ("h1".to_string(), "screenshot", 0.8),
                ("h2".to_string(), "photo", 0.9),
            ],
        )
        .unwrap();

        let hits = paths_for_category(&conn, "screenshot").unwrap();
        assert_eq!(hits.len(), 2);
        assert!(hits.iter().all(|(_, hash)| hash == "h1"));
    }

    #[test]
    fn paths_for_category_returns_empty_for_unmatched_category() {
        let conn = test_db();
        insert_file(&conn, "/a/1.jpg", "h1");
        insert_classifications(&conn, &[("h1".to_string(), "photo", 0.9)]).unwrap();

        assert!(paths_for_category(&conn, "meme").unwrap().is_empty());
    }
}
