use crate::types::FileRecord;
use rusqlite::{Connection, Result, params};
use std::path::Path;

pub fn write_records(records: &[FileRecord], db_path: &Path) -> Result<()> {
    let conn = Connection::open(db_path)?;

    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS file_hashes (
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
    )?;

    let tx = conn.unchecked_transaction()?;

    {
        let mut stmt = tx.prepare(
            "INSERT OR REPLACE INTO file_hashes
                (path, hash, size_bytes, created_at, modified_at, ext,
                 phash, exif_date, gps_lat, gps_lon, width, height)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        )?;

        for r in records {
            stmt.execute(params![
                r.path,
                r.hash,
                r.size_bytes as i64,
                r.created_at,
                r.modified_at,
                r.ext,
                r.phash.map(|p| p as i64),
                r.exif_date,
                r.gps_lat,
                r.gps_lon,
                r.width,
                r.height,
            ])?;
        }
    }

    tx.commit()?;
    Ok(())
}
