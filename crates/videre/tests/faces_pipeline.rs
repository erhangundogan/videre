use rusqlite::Connection;
use std::process::Command;
use tempfile::tempdir;

fn bin() -> std::path::PathBuf {
    let mut p = std::env::current_exe().unwrap();
    p.pop(); p.pop();
    p.push("videre");
    p
}

fn make_db(dir: &std::path::Path) -> std::path::PathBuf {
    let db = dir.join("test.db");
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch(
        "CREATE TABLE file_hashes (path TEXT PRIMARY KEY, hash TEXT NOT NULL,
         size_bytes INTEGER, created_at TEXT, modified_at TEXT, ext TEXT,
         phash INTEGER, exif_date TEXT, gps_lat REAL, gps_lon REAL,
         width INTEGER, height INTEGER);"
    ).unwrap();
    db
}

#[test]
fn exits_zero_on_empty_db() {
    let dir = tempdir().unwrap();
    let db = make_db(dir.path());
    let status = Command::new(bin())
        .arg("faces")
        .arg(&db).arg("--silent")
        .status().expect("failed to run videre faces");
    assert!(status.success());
}

#[test]
fn creates_faces_table() {
    let dir = tempdir().unwrap();
    let db = make_db(dir.path());
    Command::new(bin()).arg("faces").arg(&db).arg("--silent").status().unwrap();
    let conn = Connection::open(&db).unwrap();
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='faces'", [], |r| r.get(0)
    ).unwrap();
    assert_eq!(n, 1);
}
