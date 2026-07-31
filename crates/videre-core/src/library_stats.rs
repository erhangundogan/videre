//! Aggregate library statistics for the desktop app's home dashboard.
//! Plain queries over an open `rusqlite::Connection` - shared source of truth
//! for `videre report`'s stats tile and the Tauri `library_stats` command.
//! See docs/superpowers/specs/2026-07-31-dashboard-stats-backend-design.md
//! (Pass A) for what is and isn't in scope.

use rusqlite::{Connection, Result};
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct LibraryStats {
    pub total_files: i64,
    pub total_size_bytes: i64,
    pub total_photos: i64,
    pub total_videos: i64,
    pub duplicate_group_count: i64,
    pub duplicate_file_count: i64,
    pub wasted_bytes: i64,
    pub faces_detected: i64,
    pub people_named: i64,
}

fn table_exists(conn: &Connection, name: &str) -> Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [name],
        |r| r.get(0),
    )?;
    Ok(count > 0)
}

const PHOTO_EXTS: &str = "'jpg','jpeg','png','gif','webp','bmp','tiff','heic','dng'";
const VIDEO_EXTS: &str = "'mov','mp4'";

pub fn compute(conn: &Connection) -> Result<LibraryStats> {
    let total_files: i64 = conn.query_row("SELECT COUNT(*) FROM file_hashes", [], |r| r.get(0))?;
    let total_size_bytes: i64 =
        conn.query_row("SELECT COALESCE(SUM(size_bytes), 0) FROM file_hashes", [], |r| r.get(0))?;
    let total_photos: i64 = conn.query_row(
        &format!("SELECT COUNT(*) FROM file_hashes WHERE ext IN ({PHOTO_EXTS})"),
        [],
        |r| r.get(0),
    )?;
    let total_videos: i64 = conn.query_row(
        &format!("SELECT COUNT(*) FROM file_hashes WHERE ext IN ({VIDEO_EXTS})"),
        [],
        |r| r.get(0),
    )?;

    let duplicate_group_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM \
         (SELECT hash FROM file_hashes GROUP BY hash HAVING COUNT(*) > 1)",
        [],
        |r| r.get(0),
    )?;
    let duplicate_file_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM file_hashes \
         WHERE hash IN (SELECT hash FROM file_hashes GROUP BY hash HAVING COUNT(*) > 1)",
        [],
        |r| r.get(0),
    )?;
    let wasted_bytes: i64 = conn.query_row(
        "SELECT COALESCE(SUM(size_bytes * (cnt - 1)), 0) FROM \
         (SELECT hash, size_bytes, COUNT(*) as cnt \
          FROM file_hashes GROUP BY hash HAVING cnt > 1)",
        [],
        |r| r.get(0),
    )?;

    let (faces_detected, people_named) = if table_exists(conn, "faces")? {
        let faces_detected: i64 = conn.query_row("SELECT COUNT(*) FROM faces", [], |r| r.get(0))?;
        let people_named: i64 = conn.query_row(
            "SELECT COUNT(DISTINCT person_label) FROM faces \
             WHERE confirmed = 1 AND person_label IS NOT NULL",
            [],
            |r| r.get(0),
        )?;
        (faces_detected, people_named)
    } else {
        (0, 0)
    };

    Ok(LibraryStats {
        total_files,
        total_size_bytes,
        total_photos,
        total_videos,
        duplicate_group_count,
        duplicate_file_count,
        wasted_bytes,
        faces_detected,
        people_named,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE file_hashes (
                path        TEXT PRIMARY KEY,
                hash        TEXT NOT NULL,
                size_bytes  INTEGER,
                ext         TEXT
            );",
        )
        .unwrap();
        conn
    }

    fn insert_file(conn: &Connection, path: &str, hash: &str, size_bytes: i64, ext: &str) {
        conn.execute(
            "INSERT INTO file_hashes (path, hash, size_bytes, ext) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![path, hash, size_bytes, ext],
        )
        .unwrap();
    }

    #[test]
    fn compute_counts_total_files_and_size() {
        let conn = test_db();
        insert_file(&conn, "/a/1.jpg", "h1", 1000, "jpg");
        insert_file(&conn, "/a/2.png", "h2", 2500, "png");

        let stats = compute(&conn).unwrap();
        assert_eq!(stats.total_files, 2);
        assert_eq!(stats.total_size_bytes, 3500);
    }

    #[test]
    fn compute_on_empty_db_returns_zeros() {
        let conn = test_db();
        let stats = compute(&conn).unwrap();
        assert_eq!(stats.total_files, 0);
        assert_eq!(stats.total_size_bytes, 0);
    }

    #[test]
    fn compute_splits_photos_and_videos_by_extension() {
        let conn = test_db();
        insert_file(&conn, "/a/1.jpg", "h1", 100, "jpg");
        insert_file(&conn, "/a/2.heic", "h2", 100, "heic");
        insert_file(&conn, "/a/3.mov", "h3", 100, "mov");
        insert_file(&conn, "/a/4.mp4", "h4", 100, "mp4");
        insert_file(&conn, "/a/5.unknown", "h5", 100, "xyz");

        let stats = compute(&conn).unwrap();
        assert_eq!(stats.total_photos, 2);
        assert_eq!(stats.total_videos, 2);
        assert_eq!(stats.total_files, 5); // unrecognized ext still counts toward total_files
    }

    #[test]
    fn compute_counts_duplicate_groups_and_wasted_bytes() {
        let conn = test_db();
        insert_file(&conn, "/a/1.jpg", "dup-hash", 1000, "jpg");
        insert_file(&conn, "/b/1-copy.jpg", "dup-hash", 1000, "jpg");
        insert_file(&conn, "/a/2.jpg", "dup-hash", 1000, "jpg");
        insert_file(&conn, "/a/3.jpg", "unique-hash", 500, "jpg");

        let stats = compute(&conn).unwrap();
        assert_eq!(stats.duplicate_group_count, 1);
        assert_eq!(stats.duplicate_file_count, 3); // all 3 members of the dup group
        assert_eq!(stats.wasted_bytes, 2000); // (3 - 1) * 1000
    }

    #[test]
    fn compute_with_no_duplicates_reports_zero() {
        let conn = test_db();
        insert_file(&conn, "/a/1.jpg", "h1", 500, "jpg");
        insert_file(&conn, "/a/2.jpg", "h2", 500, "jpg");

        let stats = compute(&conn).unwrap();
        assert_eq!(stats.duplicate_group_count, 0);
        assert_eq!(stats.duplicate_file_count, 0);
        assert_eq!(stats.wasted_bytes, 0);
    }

    #[test]
    fn compute_counts_faces_and_named_people() {
        let conn = test_db();
        conn.execute_batch(
            "CREATE TABLE faces (
                id            INTEGER PRIMARY KEY,
                hash          TEXT NOT NULL,
                bbox          TEXT NOT NULL,
                landmark      TEXT,
                embedding     BLOB NOT NULL,
                cluster_id    INTEGER,
                person_label  TEXT,
                confirmed     INTEGER DEFAULT 0,
                is_primary    INTEGER DEFAULT 0
            );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO faces (id, hash, bbox, embedding, person_label, confirmed) \
             VALUES (1, 'h1', '[]', X'00', 'Alice', 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO faces (id, hash, bbox, embedding, person_label, confirmed) \
             VALUES (2, 'h1', '[]', X'00', 'Alice', 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO faces (id, hash, bbox, embedding, person_label, confirmed) \
             VALUES (3, 'h2', '[]', X'00', NULL, 0)",
            [],
        )
        .unwrap();

        let stats = compute(&conn).unwrap();
        assert_eq!(stats.faces_detected, 3);
        assert_eq!(stats.people_named, 1); // distinct confirmed person_label
    }

    #[test]
    fn compute_without_faces_table_returns_zero_not_error() {
        let conn = test_db(); // no faces table created
        let stats = compute(&conn).unwrap();
        assert_eq!(stats.faces_detected, 0);
        assert_eq!(stats.people_named, 0);
    }
}
