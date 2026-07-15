use crate::types::FileRecord;
use rusqlite::{Result, params};
use std::path::Path;

pub fn write_records(records: &[FileRecord], db_path: &Path) -> Result<()> {
    let conn = videre_core::db::open_wal(db_path)?;

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

/// Read every file_hashes row back as FileRecords (the inverse of write_records;
/// used by consumers that need records without re-scanning the filesystem).
pub fn load_records(db_path: &Path) -> Result<Vec<FileRecord>> {
    let conn = videre_core::db::open_wal(db_path)?;
    let mut stmt = conn.prepare(
        "SELECT path, hash, size_bytes, created_at, modified_at, ext,
                phash, exif_date, gps_lat, gps_lon, width, height
         FROM file_hashes",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(FileRecord {
            path: row.get(0)?,
            hash: row.get(1)?,
            size_bytes: row.get::<_, i64>(2)? as u64,
            created_at: row.get(3)?,
            modified_at: row.get(4)?,
            ext: row.get(5)?,
            phash: row.get::<_, Option<i64>>(6)?.map(|p| p as u64),
            exif_date: row.get(7)?,
            gps_lat: row.get(8)?,
            gps_lon: row.get(9)?,
            width: row.get(10)?,
            height: row.get(11)?,
        })
    })?;
    rows.collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(path: &str, hash: &str) -> FileRecord {
        FileRecord {
            path: path.to_string(),
            hash: hash.to_string(),
            size_bytes: 10,
            created_at: Some("2020-01-01T00:00:00+00:00".to_string()),
            modified_at: Some("2021-01-01T00:00:00+00:00".to_string()),
            ext: "jpg".to_string(),
            phash: Some(u64::MAX), // exercises the i64 sign-cast roundtrip
            exif_date: Some("2019-06-01T10:00:00".to_string()),
            gps_lat: Some(48.85),
            gps_lon: Some(2.35),
            width: Some(100),
            height: Some(80),
        }
    }

    #[test]
    fn load_records_roundtrips_write_records() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let written = vec![rec("/a.jpg", "h1"), rec("/b.jpg", "h2")];
        write_records(&written, &db).unwrap();

        let mut loaded = load_records(&db).unwrap();
        loaded.sort_by(|a, b| a.path.cmp(&b.path));
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].path, "/a.jpg");
        assert_eq!(loaded[0].hash, "h1");
        assert_eq!(loaded[0].size_bytes, 10);
        assert_eq!(loaded[0].phash, Some(u64::MAX));
        assert_eq!(loaded[0].exif_date.as_deref(), Some("2019-06-01T10:00:00"));
        assert_eq!(loaded[0].gps_lat, Some(48.85));
        assert_eq!(loaded[0].width, Some(100));
    }

    #[test]
    fn load_records_empty_table_yields_empty_vec() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        write_records(&[], &db).unwrap(); // creates the table, writes nothing
        assert!(load_records(&db).unwrap().is_empty());
    }
}
