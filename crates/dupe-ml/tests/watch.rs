use rusqlite::Connection;
use std::process::Command;
use tempfile::tempdir;

fn watch_bin() -> std::path::PathBuf {
    let mut path = std::env::current_exe().unwrap();
    path.pop();
    path.pop();
    path.push("dupe-watch");
    path
}

#[test]
fn scan_stage_populates_file_hashes() {
    let dir = tempdir().unwrap();
    let pics = dir.path().join("pics");
    std::fs::create_dir(&pics).unwrap();
    std::fs::write(pics.join("a.jpg"), b"dummy-bytes").unwrap();
    let db = dir.path().join("test.db");

    // Run one cycle directly via a very short interval, then kill after
    // giving it time for exactly one cycle.
    let mut child = Command::new(watch_bin())
        .arg(&pics)
        .arg("--output-sqlite").arg(&db)
        .arg("--scan")
        .arg("--interval").arg("3600") // long enough we only observe one cycle
        .arg("--silent")
        .spawn()
        .expect("failed to spawn dupe-watch");
    std::thread::sleep(std::time::Duration::from_millis(800));
    child.kill().ok();
    child.wait().ok();

    let conn = Connection::open(&db).unwrap();
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM file_hashes", [], |r| r.get(0)).unwrap();
    assert_eq!(count, 1, "expected the scan stage to have inserted the one file");
}

#[test]
fn faces_stage_skips_hashes_already_processed() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("test.db");
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch(
        "CREATE TABLE file_hashes (path TEXT PRIMARY KEY, hash TEXT NOT NULL, ext TEXT);
         CREATE TABLE faces (id INTEGER PRIMARY KEY, hash TEXT NOT NULL, bbox TEXT NOT NULL,
             landmark TEXT, embedding BLOB NOT NULL, cluster_id INTEGER, person_label TEXT,
             confirmed INTEGER DEFAULT 0, is_primary INTEGER DEFAULT 0);
         INSERT INTO file_hashes (path, hash, ext) VALUES ('/tmp/a.jpg', 'h1', 'jpg');
         INSERT INTO faces (hash, bbox, embedding) VALUES ('h1', '0,0,10,10', X'0000');",
    ).unwrap();
    drop(conn);

    let mut child = Command::new(watch_bin())
        .arg(dir.path())
        .arg("--output-sqlite").arg(&db)
        .arg("--faces")
        .arg("--interval").arg("3600")
        .arg("--silent")
        .spawn()
        .expect("failed to spawn dupe-watch");
    std::thread::sleep(std::time::Duration::from_millis(800));
    let still_running = child.try_wait().unwrap().is_none();
    child.kill().ok();
    child.wait().ok();
    assert!(still_running, "dupe-watch --faces should not have crashed on an already-processed hash");
}

#[test]
fn heic_stage_writes_no_cache_file_for_non_heic_hashes() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("test.db");
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch(
        "CREATE TABLE file_hashes (path TEXT PRIMARY KEY, hash TEXT NOT NULL, ext TEXT);
         INSERT INTO file_hashes (path, hash, ext) VALUES ('/tmp/a.jpg', 'hjpg', 'jpg');",
    ).unwrap();
    drop(conn);

    let mut child = Command::new(watch_bin())
        .arg(dir.path())
        .arg("--output-sqlite").arg(&db)
        .arg("--heic")
        .arg("--interval").arg("3600")
        .arg("--silent")
        .spawn()
        .expect("failed to spawn dupe-watch");
    std::thread::sleep(std::time::Duration::from_millis(800));
    child.kill().ok();
    child.wait().ok();

    assert!(!dupe_core::thumb_cache::thumb_exists("hjpg", 240), "non-HEIC hash must not get a cached thumbnail");
}
