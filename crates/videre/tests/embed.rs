use std::fs;
use std::process::Command;
use tempfile::tempdir;

fn videre_bin() -> std::path::PathBuf {
    let mut path = std::env::current_exe().unwrap();
    path.pop(); // deps/
    path.pop(); // debug/
    path.push("videre");
    path
}

#[test]
#[cfg(target_os = "macos")]
fn embed_produces_an_embeddings_row_for_a_real_video() {
    let scan_dir = tempdir().unwrap();
    let out_dir = tempdir().unwrap();
    let db_path = out_dir.path().join("hashes.db");

    fs::copy("tests/fixtures/red_1s.mp4", scan_dir.path().join("clip.mp4")).unwrap();

    let scan_status = Command::new(videre_bin())
        .arg("scan")
        .arg("--silent")
        .arg("--output-sqlite")
        .arg(&db_path)
        .arg(scan_dir.path())
        .status()
        .expect("failed to run videre scan");
    assert!(scan_status.success());

    let embed_status = Command::new(videre_bin())
        .arg("embed")
        .arg("--db")
        .arg(&db_path)
        .arg("--silent")
        .status()
        .expect("failed to run videre embed");
    assert!(embed_status.success());

    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM embeddings", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1, "the video's content hash should have an embeddings row");
}
