//! Embeddings table: one row per unique content hash, keyed to file_hashes.hash.

use rusqlite::{Connection, Result, params};

/// Extensions the embedding pipeline can decode. Video is out of scope in v1.
pub const EMBEDDABLE_EXTS: &[&str] = &[
    "jpg", "jpeg", "png", "gif", "webp", "bmp", "tiff", "heic", "dng",
];

pub struct PendingImage {
    pub hash: String,
    pub path: String,
}

pub fn ensure_embeddings_table(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS embeddings (
            hash        TEXT PRIMARY KEY,
            model_id    TEXT NOT NULL,
            embedding   BLOB NOT NULL,
            embedded_at TEXT NOT NULL
        );",
    )
}

/// Unique hashes that are embeddable but not yet embedded; one representative
/// path per hash (MIN(path) keeps it deterministic).
pub fn pending_images(conn: &Connection) -> Result<Vec<PendingImage>> {
    let placeholders = EMBEDDABLE_EXTS
        .iter()
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT hash, MIN(path) FROM file_hashes
         WHERE lower(ext) IN ({placeholders})
           AND hash NOT IN (SELECT hash FROM embeddings)
         GROUP BY hash
         ORDER BY hash"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(
        rusqlite::params_from_iter(EMBEDDABLE_EXTS.iter()),
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

pub fn load_embeddings(conn: &Connection, model_id: &str) -> Result<Vec<(String, Vec<u8>)>> {
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
    fn pending_images_dedupes_by_hash_and_filters_ext() {
        let conn = test_db();
        insert_file(&conn, "/a/1.jpg", "h1", "jpg");
        insert_file(&conn, "/b/1-copy.jpg", "h1", "jpg"); // same hash, second path
        insert_file(&conn, "/a/2.png", "h2", "png");
        insert_file(&conn, "/a/clip.mp4", "h3", "mp4");   // unsupported for embedding

        let pending = pending_images(&conn).unwrap();
        assert_eq!(pending.len(), 2); // h1 once, h2 once, h3 excluded
        assert!(pending.iter().any(|p| p.hash == "h1"));
        assert!(pending.iter().any(|p| p.hash == "h2"));
    }

    #[test]
    fn pending_images_excludes_already_embedded() {
        let conn = test_db();
        insert_file(&conn, "/a/1.jpg", "h1", "jpg");
        insert_file(&conn, "/a/2.jpg", "h2", "jpg");
        insert_embeddings(&conn, "test-model", &[("h1".to_string(), vec![0u8; 4])]).unwrap();

        let pending = pending_images(&conn).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].hash, "h2");
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
}
