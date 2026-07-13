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
