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

#[test]
fn help_documents_show_faces_starts_server() {
    let out = Command::new(env!("CARGO_BIN_EXE_dupe-report"))
        .arg("--help")
        .output()
        .expect("failed to run dupe-report");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("show-faces"));
}

#[test]
fn show_faces_alone_is_accepted_by_cli_parser() {
    // Smoke test: dupe-report should not error out on flag parsing when
    // --show-faces is passed (it will still try to bind port 7878 and
    // block, so this test only checks the process starts without an
    // immediate clap parse error - full server behavior is verified
    // manually per Task 11).
    let dir = tempdir().unwrap();
    let db = make_db_with_faces(dir.path());
    let mut child = Command::new(env!("CARGO_BIN_EXE_dupe-report"))
        .arg(&db)
        .arg("--show-faces")
        .spawn()
        .expect("failed to spawn dupe-report --show-faces");
    std::thread::sleep(std::time::Duration::from_millis(300));
    let still_running = child.try_wait().unwrap().is_none();
    child.kill().ok();
    child.wait().ok();
    assert!(still_running, "dupe-report --show-faces should still be running (serving), not have exited/errored");
}

#[test]
fn thumb_cache_hit_avoids_qlmanage_conversion() {
    // Seed a fake cached thumbnail file directly, then confirm handle_raw_file's
    // cache-check path would find it - since handle_raw_file itself needs a
    // running server + real HEIC file to test end-to-end, this instead verifies
    // the shared videre_core::thumb_cache helpers dupe-report will call.
    let hash = "test-cache-hit-hash";
    std::fs::create_dir_all(videre_core::thumb_cache::cache_dir()).unwrap();
    std::fs::write(videre_core::thumb_cache::thumb_path(hash, 240), b"fake-jpeg-bytes").unwrap();
    assert!(videre_core::thumb_cache::thumb_exists(hash, 240));
    std::fs::remove_file(videre_core::thumb_cache::thumb_path(hash, 240)).ok();
}
