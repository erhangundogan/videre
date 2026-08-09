//! Classifications table: one row per unique content hash (photo/screenshot/
//! document/meme/unknown), keyed to embeddings.hash. Zero-shot classification
//! reuses embeddings `videre embed` already computed. See
//! docs/superpowers/specs/2026-07-29-screenshot-document-classification-design.md.

use rusqlite::{Connection, Result, params};

/// Create `classifications`, keyed by `(model_id, hash)`.
///
/// A pre-existing table without `model_id` is dropped and recreated rather
/// than migrated. Classifications are pure vector arithmetic over embeddings
/// that already exist, with no image decoding, so rebuilding costs minutes.
/// Guessing which model produced the legacy rows would instead produce data
/// that looks valid and is not.
pub fn ensure_classifications_table(conn: &Connection) -> Result<()> {
    if crate::db::table_exists(conn, "classifications")? {
        let has_model_id = conn
            .prepare("SELECT model_id FROM classifications LIMIT 0")
            .is_ok();
        if !has_model_id {
            eprintln!(
                "note: the classifications table predates multi-model support and has been \
                 reset. Re-run 'videre classify' to rebuild it (minutes, no image decoding)."
            );
            conn.execute_batch("DROP TABLE classifications;")?;
        }
    }
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS classifications (
            model_id      TEXT NOT NULL,
            hash          TEXT NOT NULL,
            category      TEXT NOT NULL,
            confidence    REAL NOT NULL,
            classified_at TEXT NOT NULL,
            PRIMARY KEY (model_id, hash)
        );",
    )
}

/// Hashes that have an embedding under `model_id` but no classification yet.
/// Excludes video hashes (`.mov`/`.mp4`), none of the four zero-shot
/// categories (photo/screenshot/document/meme) fit a video frame well, so
/// videos are never classified, per the video-embedding design's decision.
pub fn pending_hashes(conn: &Connection, model_id: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT hash FROM emb.embeddings
         WHERE model_id = ?1
           AND NOT EXISTS (
               SELECT 1 FROM classifications c
               WHERE c.hash = emb.embeddings.hash AND c.model_id = ?1
           )
           AND NOT EXISTS (
               SELECT 1 FROM file_hashes fh
               WHERE fh.hash = emb.embeddings.hash
                 AND (fh.mime IN ('video/quicktime', 'video/mp4')
                      OR (fh.mime IS NULL AND lower(fh.ext) IN ('mov', 'mp4')))
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
        conn.prepare("SELECT hash, lower(COALESCE(ext, '')), mime FROM file_hashes")?;
    // (hash, ext, mime) so the decision uses content when known.
    let rows: Vec<(String, String, Option<String>)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let ext_by_hash: std::collections::HashMap<String, (String, Option<String>)> =
        rows.into_iter().map(|(h, e, m)| (h, (e, m))).collect();

    Ok(hashes
        .iter()
        .filter(|hash| {
            !ext_by_hash.get(*hash).is_some_and(|(ext, mime)| {
                crate::mime_probe::effective_mime(mime.as_deref(), ext)
                    .is_some_and(crate::mime_probe::is_video_mime)
            })
        })
        .cloned()
        .collect())
}

/// Upsert a batch of (hash, category, confidence) rows inside one transaction.
pub fn insert_classifications(
    conn: &Connection,
    model_id: &str,
    items: &[(String, &str, f32)],
) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    {
        let mut stmt = tx.prepare(
            "INSERT OR REPLACE INTO classifications
                (model_id, hash, category, confidence, classified_at)
             VALUES (?1, ?2, ?3, ?4, datetime('now'))",
        )?;
        for (hash, category, confidence) in items {
            stmt.execute(params![model_id, hash, category, confidence])?;
        }
    }
    tx.commit()
}

/// (path, hash) pairs for every file classified as `category`, one entry per
/// on-disk path of a matched hash (same duplicate-path convention as
/// `embeddings::paths_for_hash`).
pub fn paths_for_category(
    conn: &Connection,
    model_id: &str,
    category: &str,
) -> Result<Vec<(String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT file_hashes.path, file_hashes.hash FROM file_hashes
         JOIN classifications ON file_hashes.hash = classifications.hash
         WHERE classifications.model_id = ?1 AND classifications.category = ?2
         ORDER BY file_hashes.path",
    )?;
    let rows = stmt.query_map(params![model_id, category], |row| {
        Ok((row.get(0)?, row.get(1)?))
    })?;
    rows.collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Main database with `file_hashes` and `classifications`, plus a real
    /// attached model database holding `embeddings`. Faking the split with a
    /// plain local table would hide the very thing under test.
    fn test_db_attached(tag: &str) -> Connection {
        let lib = crate::embeddings_db::test_library(tag);
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE file_hashes (
                path TEXT PRIMARY KEY,
                hash TEXT NOT NULL,
                ext  TEXT
            );",
        )
        .unwrap();
        crate::db::ensure_file_hashes_columns(&conn);
        ensure_classifications_table(&conn).unwrap();
        crate::embeddings_db::attach(&conn, &lib, "test-model", true).unwrap();
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
            "INSERT INTO emb.embeddings (hash, model_id, embedding, embedded_at)
             VALUES (?1, ?2, X'00', datetime('now'))",
            rusqlite::params![hash, model_id],
        )
        .unwrap();
    }

    #[test]
    fn pending_hashes_excludes_video_extensions() {
        let conn = test_db_attached("cls_video");
        insert_file_with_ext(&conn, "/a/1.jpg", "h1", "jpg");
        insert_file_with_ext(&conn, "/a/clip.mp4", "h2", "mp4");
        insert_file_with_ext(&conn, "/a/clip.mov", "h3", "mov");
        insert_embedding(&conn, "h1", "test-model");
        insert_embedding(&conn, "h2", "test-model");
        insert_embedding(&conn, "h3", "test-model");

        let pending = pending_hashes(&conn, "test-model").unwrap();
        assert_eq!(pending, vec!["h1".to_string()]);
    }

    #[test]
    fn exclude_video_hashes_filters_out_mov_and_mp4() {
        let conn = test_db_attached("cls_filter");
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
        let conn = test_db_attached("cls_orphan");
        let all = vec!["orphan-hash".to_string()];
        let filtered = exclude_video_hashes(&conn, &all).unwrap();
        assert_eq!(filtered, vec!["orphan-hash".to_string()]);
    }

    #[test]
    fn pending_hashes_returns_work_for_a_second_model() {
        // The bug this fixes: with a single-column primary key, any hash
        // already classified by model A was excluded for every model, so a
        // second model silently found zero pending work and classified
        // nothing while reporting success.
        //
        // Each model owns a separate database, so this genuinely swaps the
        // attached one rather than putting two model_ids in one table, which
        // the hash primary key would not allow anyway.
        let lib = crate::embeddings_db::test_library("cls_secondmodel");
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE file_hashes (path TEXT PRIMARY KEY, hash TEXT NOT NULL, ext TEXT);",
        )
        .unwrap();
        crate::db::ensure_file_hashes_columns(&conn);
        ensure_classifications_table(&conn).unwrap();

        crate::embeddings_db::attach(&conn, &lib, "model-a", true).unwrap();
        insert_embedding(&conn, "h1", "model-a");
        insert_classifications(&conn, "model-a", &[("h1".to_string(), "photo", 0.9)]).unwrap();
        assert!(pending_hashes(&conn, "model-a").unwrap().is_empty());
        crate::embeddings_db::detach(&conn).unwrap();

        crate::embeddings_db::attach(&conn, &lib, "model-b", true).unwrap();
        insert_embedding(&conn, "h1", "model-b");
        assert_eq!(
            pending_hashes(&conn, "model-b").unwrap(),
            vec!["h1".to_string()],
            "model-b must still have work to do"
        );
    }

    #[test]
    fn the_same_hash_can_hold_one_row_per_model() {
        let conn = test_db_attached("cls_perlmodel");
        insert_classifications(&conn, "model-a", &[("h1".to_string(), "photo", 0.9)]).unwrap();
        insert_classifications(&conn, "model-b", &[("h1".to_string(), "meme", 0.7)]).unwrap();

        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM classifications", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 2);
    }

    #[test]
    fn paths_for_category_is_scoped_to_one_model() {
        let conn = test_db_attached("cls_catmodel");
        insert_file(&conn, "/a/1.jpg", "h1");
        insert_classifications(&conn, "model-a", &[("h1".to_string(), "screenshot", 0.8)]).unwrap();
        insert_classifications(&conn, "model-b", &[("h1".to_string(), "photo", 0.8)]).unwrap();

        assert_eq!(
            paths_for_category(&conn, "model-a", "screenshot").unwrap().len(),
            1
        );
        assert!(paths_for_category(&conn, "model-b", "screenshot")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn a_legacy_table_without_model_id_is_dropped_and_recreated() {
        // Rebuilding costs minutes (pure vector arithmetic, no image
        // decoding). Guessing a model_id to stamp would produce data that
        // looks valid and is not.
        let conn = test_db_attached("cls_legacy");
        conn.execute_batch("DROP TABLE classifications;").unwrap();
        conn.execute_batch(
            "CREATE TABLE classifications (
                hash TEXT PRIMARY KEY NOT NULL, category TEXT NOT NULL,
                confidence REAL NOT NULL, classified_at TEXT NOT NULL
            );
            INSERT INTO classifications VALUES ('h1', 'photo', 0.9, 'now');",
        )
        .unwrap();

        ensure_classifications_table(&conn).unwrap();

        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM classifications", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0, "legacy rows are dropped, not silently mislabeled");
        conn.execute(
            "INSERT INTO classifications VALUES ('m', 'h1', 'photo', 0.9, 'now')",
            [],
        )
        .expect("recreated table must have the 5-column shape");
    }

    #[test]
    fn ensure_classifications_table_is_idempotent() {
        let conn = test_db_attached("cls_idem");
        ensure_classifications_table(&conn).unwrap();
        ensure_classifications_table(&conn).unwrap();
    }

    #[test]
    fn pending_hashes_returns_embedded_but_unclassified() {
        let conn = test_db_attached("cls_pending");
        insert_embedding(&conn, "h1", "test-model");
        insert_embedding(&conn, "h2", "test-model");
        insert_classifications(&conn, "test-model", &[("h1".to_string(), "photo", 0.9)]).unwrap();

        let pending = pending_hashes(&conn, "test-model").unwrap();
        assert_eq!(pending, vec!["h2".to_string()]);
    }

    #[test]
    fn pending_hashes_is_model_aware() {
        let conn = test_db_attached("cls_modelaware");
        insert_embedding(&conn, "h1", "model-a");

        assert_eq!(pending_hashes(&conn, "model-a").unwrap(), vec!["h1".to_string()]);
        assert!(pending_hashes(&conn, "model-b").unwrap().is_empty());
    }

    #[test]
    fn insert_classifications_upserts_on_conflict() {
        let conn = test_db_attached("cls_upsert");
        insert_classifications(&conn, "test-model", &[("h1".to_string(), "screenshot", 0.4)]).unwrap();
        insert_classifications(&conn, "test-model", &[("h1".to_string(), "photo", 0.9)]).unwrap();

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
        let conn = test_db_attached("cls_paths");
        insert_file(&conn, "/a/1.jpg", "h1");
        insert_file(&conn, "/b/1-copy.jpg", "h1");
        insert_file(&conn, "/a/2.png", "h2");
        insert_classifications(
            &conn,
            "test-model",
            &[
                ("h1".to_string(), "screenshot", 0.8),
                ("h2".to_string(), "photo", 0.9),
            ],
        )
        .unwrap();

        let hits = paths_for_category(&conn, "test-model", "screenshot").unwrap();
        assert_eq!(hits.len(), 2);
        assert!(hits.iter().all(|(_, hash)| hash == "h1"));
    }

    #[test]
    fn paths_for_category_returns_empty_for_unmatched_category() {
        let conn = test_db_attached("cls_empty");
        insert_file(&conn, "/a/1.jpg", "h1");
        insert_classifications(&conn, "test-model", &[("h1".to_string(), "photo", 0.9)]).unwrap();

        assert!(paths_for_category(&conn, "test-model", "meme").unwrap().is_empty());
    }
}
