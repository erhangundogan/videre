use crate::types::FileRecord;
use rusqlite::{params, Result};
use std::path::PathBuf;

/// Write records to a database at a path, opening its own connection, for
/// tests. Commands write through [`write_records_in`].
#[cfg(test)]
fn write_records(records: &[FileRecord], db_path: &std::path::Path) -> Result<()> {
    let conn = videre_core::db::open_wal(db_path)?;
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

/// A record's resolved capture date and place: what the file says, or else
/// its Google Takeout sidecar, or else its own time.
struct Resolved {
    capture_date: Option<String>,
    date_source: Option<&'static str>,
    gps: Option<(f64, f64)>,
    gps_source: Option<&'static str>,
}

/// The Takeout sidecar facts for those of `paths` that have a sidecar,
/// matched by the same rules `videre import` uses. Folders with no sidecar
/// cost one directory read.
pub fn sidecar_facts(
    paths: &[PathBuf],
) -> std::collections::HashMap<PathBuf, crate::takeout_sidecar::SidecarMeta> {
    crate::takeout_sidecar::survey(paths)
        .matched
        .into_iter()
        .filter_map(|m| {
            let text = std::fs::read_to_string(&m.sidecar).ok()?;
            let meta = crate::takeout_sidecar::parse_sidecar(&text).ok()?;
            Some((m.file, meta))
        })
        .collect()
}

fn resolve_record(
    r: &FileRecord,
    sidecar: Option<&crate::takeout_sidecar::SidecarMeta>,
) -> Resolved {
    use videre_core::capture_date::{resolve, DateSource, Inputs};
    let source = r.date_source.as_deref().and_then(DateSource::parse);
    let date = r.exif_date.as_deref();
    let inputs = Inputs {
        exif: date.filter(|_| source == Some(DateSource::Exif)),
        video_apple: date.filter(|_| source == Some(DateSource::Video)),
        sidecar_unix: sidecar.and_then(|s| s.taken_unix),
        mvhd_unix: r.mvhd_unix,
        mtime: r.modified_at.as_deref(),
    };
    let resolved = resolve(&inputs);
    let (gps, gps_source) = match (r.gps_lat.zip(r.gps_lon), sidecar.and_then(|s| s.gps)) {
        (Some(g), _) => (Some(g), Some("file")),
        (None, Some(g)) => (Some(g), Some("sidecar")),
        (None, None) => (None, None),
    };
    Resolved {
        capture_date: resolved.as_ref().map(|(d, _)| d.clone()),
        date_source: resolved.map(|(_, s)| s.as_str()),
        gps,
        gps_source,
    }
}

fn write_records_to(conn: &rusqlite::Connection, records: &[FileRecord]) -> Result<()> {
    videre_core::library_db::ensure_scan_schema(conn)?;
    // Only a record the file itself leaves short of a date or a place needs
    // its sidecar.
    let wanting: Vec<PathBuf> = records
        .iter()
        .filter(|r| {
            !matches!(r.date_source.as_deref(), Some("exif" | "video")) || r.gps_lat.is_none()
        })
        .map(|r| PathBuf::from(&r.path))
        .collect();
    let sidecars = sidecar_facts(&wanting);
    let tx = conn.unchecked_transaction()?;

    {
        let mut stmt = tx.prepare(
            "INSERT INTO file_hashes
                (path, hash, size_bytes, created_at, modified_at, ext, mime,
                 phash, exif_date, gps_lat, gps_lon, width, height,
                 duration_secs, codec, meta_hash, capture_date, date_source,
                 gps_source)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                     ?14, ?15, ?16, ?17, ?18, ?19)
             ON CONFLICT(path) DO UPDATE SET
                hash = excluded.hash,
                meta_hash = excluded.meta_hash,
                size_bytes = excluded.size_bytes,
                created_at = excluded.created_at,
                modified_at = excluded.modified_at,
                ext = excluded.ext,
                mime = excluded.mime,
                phash = CASE WHEN file_hashes.hash = excluded.hash
                             THEN COALESCE(excluded.phash, file_hashes.phash)
                             ELSE excluded.phash END,
                exif_date = excluded.exif_date,
                gps_lat = excluded.gps_lat,
                gps_lon = excluded.gps_lon,
                width = excluded.width,
                height = excluded.height,
                duration_secs = excluded.duration_secs,
                codec = excluded.codec,
                capture_date = excluded.capture_date,
                date_source = excluded.date_source,
                gps_source = excluded.gps_source",
        )?;

        for r in records {
            let resolved = resolve_record(r, sidecars.get(&PathBuf::from(&r.path)));
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
                resolved.gps.map(|g| g.0),
                resolved.gps.map(|g| g.1),
                r.width,
                r.height,
                r.duration_secs,
                r.codec,
                r.meta_hash,
                resolved.capture_date,
                resolved.date_source,
                resolved.gps_source,
            ])?;
        }
    }

    tx.commit()?;
    Ok(())
}

/// Load every file record from a path, opening its own connection: the
/// inverse of `write_records`, for tests.
#[cfg(test)]
fn load_records(db_path: &std::path::Path) -> Result<Vec<FileRecord>> {
    let conn = videre_core::db::open_wal(db_path)?;
    load_records_from(&conn)
}

/// Load every file record from an already-open, validated connection.
///
/// The directory-local reader path: the caller has opened the selected
/// library's database through `library_db::open_existing`, so this must not
/// open or create anything of its own.
pub fn load_records_from(conn: &rusqlite::Connection) -> Result<Vec<FileRecord>> {
    let mut stmt = conn.prepare(&format!("SELECT {FILE_RECORD_COLUMNS} FROM file_hashes"))?;
    let rows = stmt.query_map([], file_record_from_row)?;
    rows.collect()
}

/// The `file_hashes` columns a [`FileRecord`] reads, in the order
/// [`file_record_from_row`] expects. Shared by every reader (dedupe, the JSONL
/// snapshot) so the projection and the mapping cannot drift apart.
pub const FILE_RECORD_COLUMNS: &str =
    "path, hash, size_bytes, created_at, modified_at, ext, mime, phash, \
     exif_date, gps_lat, gps_lon, width, height, duration_secs, codec, meta_hash";

/// Map one `file_hashes` row, projected as [`FILE_RECORD_COLUMNS`], into a
/// [`FileRecord`]. The JSONL snapshot streams rows straight through this.
pub fn file_record_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<FileRecord> {
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
        date_source: None,
        mvhd_unix: None,
        meta_hash: row.get(15)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(path: &str, hash: &str) -> FileRecord {
        FileRecord {
            path: path.to_string(),
            hash: hash.to_string(),
            meta_hash: None,
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
            date_source: None,
            mvhd_unix: None,
        }
    }

    #[test]
    fn load_records_roundtrips_write_records() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut written = vec![rec("/a.jpg", "h1"), rec("/b.jpg", "h2")];
        written[0].meta_hash = Some("m1".to_string());
        write_records(&written, &db).unwrap();

        let mut loaded = load_records(&db).unwrap();
        loaded.sort_by(|a, b| a.path.cmp(&b.path));
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].path, "/a.jpg");
        assert_eq!(loaded[0].hash, "h1");
        assert_eq!(loaded[0].meta_hash.as_deref(), Some("m1"));
        assert_eq!(loaded[1].meta_hash, None);
        assert_eq!(loaded[0].size_bytes, 10);
        assert_eq!(loaded[0].phash, Some(u64::MAX));
        assert_eq!(loaded[0].exif_date.as_deref(), Some("2019-06-01T10:00:00"));
        assert_eq!(loaded[0].gps_lat, Some(48.85));
        assert_eq!(loaded[0].width, Some(100));
    }

    #[test]
    fn rescanning_keeps_the_fingerprint_until_the_content_changes() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("hashes.db");
        let mut first = rec("/a.jpg", "h1");
        first.phash = Some(42);
        write_records(std::slice::from_ref(&first), &db).unwrap();

        let mut same = rec("/a.jpg", "h1");
        same.phash = None;
        write_records(std::slice::from_ref(&same), &db).unwrap();
        assert_eq!(load_records(&db).unwrap()[0].phash, Some(42));

        let mut changed = rec("/a.jpg", "h2");
        changed.phash = None;
        write_records(std::slice::from_ref(&changed), &db).unwrap();
        assert_eq!(load_records(&db).unwrap()[0].phash, None);
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
        write_records(std::slice::from_ref(&first), &db).unwrap();
        let conn = rusqlite::Connection::open(&db).unwrap();
        // Cluster 17 is a deliberate foreign-key violation (location
        // processing writes these ids after creating the clusters), so the
        // corrupting UPDATE runs with enforcement lifted.
        conn.execute_batch("PRAGMA foreign_keys = OFF").unwrap();
        conn.execute(
            "UPDATE file_hashes SET location_name = 'Üsküdar', location_cluster_id = 17 WHERE path = '/a.jpg'",
            [],
        )
        .unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON").unwrap();
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
