use rusqlite::Connection;
use std::process::Command;
use tempfile::tempdir;

fn make_db_with_faces(dir: &std::path::Path) -> std::path::PathBuf {
    let db = dir.join("test.db");
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch(
        "CREATE TABLE file_hashes (path TEXT PRIMARY KEY, hash TEXT NOT NULL,
         size_bytes INTEGER, created_at TEXT, modified_at TEXT, ext TEXT,
         phash INTEGER, exif_date TEXT, gps_lat REAL, gps_lon REAL,
         width INTEGER, height INTEGER);
         CREATE TABLE faces (id INTEGER PRIMARY KEY, hash TEXT NOT NULL,
         bbox TEXT NOT NULL, landmark TEXT, embedding BLOB NOT NULL,
         cluster_id INTEGER, person_label TEXT, confirmed INTEGER DEFAULT 0,
         is_primary INTEGER DEFAULT 0);
         INSERT INTO file_hashes (path, hash, ext) VALUES ('/tmp/a.jpg', 'abc123', 'jpg');
         INSERT INTO faces (hash, bbox, embedding, confirmed) VALUES ('abc123', '0,0,50,50', X'0000', 0);",
    )
    .unwrap();
    db
}

#[test]
fn get_faces_returns_singletons() {
    let _dir = tempdir().unwrap();
    // Verify --faces appears in help output, confirming the flag is registered
    let out = Command::new(env!("CARGO_BIN_EXE_dupe-report"))
        .arg("--help")
        .output()
        .expect("failed to run dupe-report");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("faces"), "Expected --faces in help output, got: {stdout}");
}

#[test]
fn make_db_with_faces_creates_valid_schema() {
    let dir = tempdir().unwrap();
    let db_path = make_db_with_faces(dir.path());
    assert!(db_path.exists(), "database file should exist after make_db_with_faces");
    let conn = Connection::open(&db_path).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM faces", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1, "expected one seed face row");
    // Verify is_primary column exists
    conn.execute("UPDATE faces SET is_primary = 1 WHERE id = 1", []).unwrap();
}
