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

    Ok(LibraryStats { total_files, total_size_bytes, total_photos, total_videos })
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
}
