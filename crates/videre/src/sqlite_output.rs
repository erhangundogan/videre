use crate::types::FileRecord;
use rusqlite::{params, Result};
use std::path::{Path, PathBuf};

pub fn write_records(records: &[FileRecord], db_path: &Path) -> Result<()> {
    let conn = videre_core::db::open_wal(db_path)?;

    // One implementation of the scan schema, in core: this used to own a
    // duplicate of the DDL plus the swallowed-ALTER migrations, which is
    // exactly the shape that drifts from the real schema. The core version
    // inspects PRAGMA table_info and adds only the columns that are missing.
    videre_core::library_db::ensure_scan_schema(&conn)?;

    write_records_to(&conn, records)
}

pub fn write_records_in(
    conn: &rusqlite::Connection,
    ctx: &videre_core::library::LibraryContext,
    records: &[FileRecord],
) -> anyhow::Result<()> {
    let paths: Vec<PathBuf> = records
        .iter()
        .map(|record| PathBuf::from(&record.path))
        .collect();
    videre_core::library_guard::validate_paths(ctx, &paths)?;
    write_records_to(conn, records)?;
    Ok(())
}

fn write_records_to(conn: &rusqlite::Connection, records: &[FileRecord]) -> Result<()> {
    videre_core::library_db::ensure_scan_schema(conn)?;
    let tx = conn.unchecked_transaction()?;

    {
        let mut stmt = tx.prepare(
            "INSERT INTO file_hashes
                (path, hash, size_bytes, created_at, modified_at, ext, mime,
                 phash, exif_date, gps_lat, gps_lon, width, height,
                 duration_secs, codec)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                     ?14, ?15)
             ON CONFLICT(path) DO UPDATE SET
                hash = excluded.hash,
                size_bytes = excluded.size_bytes,
                created_at = excluded.created_at,
                modified_at = excluded.modified_at,
                ext = excluded.ext,
                mime = excluded.mime,
                phash = excluded.phash,
                exif_date = excluded.exif_date,
                gps_lat = excluded.gps_lat,
                gps_lon = excluded.gps_lon,
                width = excluded.width,
                height = excluded.height,
                duration_secs = excluded.duration_secs,
                codec = excluded.codec",
        )?;

        for r in records {
            stmt.execute(params![
                r.path,
                r.hash,
                r.size_bytes as i64,
                r.created_at,
                r.modified_at,
                r.ext,
                r.mime,
                r.phash.map(|p| p as i64),
                r.exif_date,
                r.gps_lat,
                r.gps_lon,
                r.width,
                r.height,
                r.duration_secs,
                r.codec,
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
        "SELECT path, hash, size_bytes, created_at, modified_at, ext, mime,
                phash, exif_date, gps_lat, gps_lon, width, height,
                duration_secs, codec
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
            mime: row.get(6)?,
            phash: row.get::<_, Option<i64>>(7)?.map(|p| p as u64),
            exif_date: row.get(8)?,
            gps_lat: row.get(9)?,
            gps_lon: row.get(10)?,
            width: row.get(11)?,
            height: row.get(12)?,
            duration_secs: row.get(13)?,
            codec: row.get(14)?,
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
            mime: None,
            phash: Some(u64::MAX), // exercises the i64 sign-cast roundtrip
            exif_date: Some("2019-06-01T10:00:00".to_string()),
            gps_lat: Some(48.85),
            gps_lon: Some(2.35),
            width: Some(100),
            height: Some(80),
            duration_secs: None,
            codec: None,
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

    #[test]
    fn mime_round_trips_through_the_database() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut r = rec("/a/x.png", "h1");
        r.mime = Some("image/jpeg".to_string());
        write_records(&[r], &db).unwrap();

        let back = load_records(&db).unwrap();
        assert_eq!(back[0].mime.as_deref(), Some("image/jpeg"));
    }

    #[test]
    fn a_database_without_the_mime_column_gains_it() {
        // Existing libraries predate the column. The migration must add it
        // rather than failing, and existing rows keep NULL until re-scanned.
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("old.db");
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE file_hashes (
                path TEXT PRIMARY KEY, hash TEXT NOT NULL, size_bytes INTEGER,
                created_at TEXT, modified_at TEXT, ext TEXT, phash INTEGER,
                exif_date TEXT, gps_lat REAL, gps_lon REAL, width INTEGER,
                height INTEGER
            );
            INSERT INTO file_hashes (path, hash, size_bytes, ext)
             VALUES ('/old.jpg', 'h0', 123, 'jpg');",
        )
        .unwrap();
        drop(conn);

        write_records(&[rec("/new.jpg", "h1")], &db).unwrap();
        let back = load_records(&db).unwrap();
        assert_eq!(back.len(), 2);
        assert!(back.iter().all(|r| r.mime.is_none()));
    }

    #[test]
    fn rescanning_preserves_location_fields_owned_by_location_processing() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("hashes.db");
        let first = rec("/a.jpg", "h1");
        write_records(&[first.clone()], &db).unwrap();
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute(
            "UPDATE file_hashes SET location_name = 'Üsküdar', location_cluster_id = 17 WHERE path = '/a.jpg'",
            [],
        )
        .unwrap();
        drop(conn);

        let mut rescanned = first;
        rescanned.modified_at = Some("2026-09-06T12:00:00+00:00".into());
        write_records(&[rescanned], &db).unwrap();

        let conn = rusqlite::Connection::open(&db).unwrap();
        let location: (String, i64) = conn
            .query_row(
                "SELECT location_name, location_cluster_id FROM file_hashes WHERE path = '/a.jpg'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(location, ("Üsküdar".into(), 17));
    }
}
