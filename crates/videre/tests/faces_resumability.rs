//! Mechanically verifies `videre faces`'s kill-mid-run resumability claim,
//! rather than only reasoning about it in the abstract: actually kills the
//! process (SIGKILL, not a graceful Ctrl-C) partway through a real run and
//! asserts a resumed run picks up correctly with no images permanently lost.

use rusqlite::Connection;
use std::process::Command;
use std::time::{Duration, Instant};
use tempfile::tempdir;

fn bin() -> std::path::PathBuf {
    let mut p = std::env::current_exe().unwrap();
    p.pop(); // deps/
    p.pop(); // debug/
    p.push("videre");
    p
}

/// Populates `db` with `n` `file_hashes` rows, each a distinct fake hash
/// pointing at its own copy of the real `sample_with_exif.jpg` fixture (real
/// JPEG bytes, so detection genuinely decodes and runs SCRFD on each one -
/// not a synthetic/corrupt file). Distinct hashes (not derived from content)
/// so each row is a genuinely separate unit of resumable work, mirroring how
/// other fixtures in this file set already fabricate hashes directly rather
/// than computing them.
fn fixture_db(dir: &std::path::Path, n: usize) -> std::path::PathBuf {
    let db = dir.join("test.db");
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch(
        "CREATE TABLE file_hashes (path TEXT PRIMARY KEY, hash TEXT NOT NULL,
         size_bytes INTEGER, created_at TEXT, modified_at TEXT, ext TEXT,
         phash INTEGER, exif_date TEXT, gps_lat REAL, gps_lon REAL,
         width INTEGER, height INTEGER);",
    )
    .unwrap();
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_with_exif.jpg");
    for i in 0..n {
        let path = dir.join(format!("img_{i:04}.jpg"));
        std::fs::copy(&source, &path).unwrap();
        let hash = format!("h{i:04}");
        conn.execute(
            "INSERT INTO file_hashes (path, hash, ext) VALUES (?1, ?2, 'jpg')",
            rusqlite::params![path.to_str().unwrap(), hash],
        )
        .unwrap();
    }
    db
}

fn scanned_count(db: &std::path::Path) -> i64 {
    let Ok(conn) = Connection::open(db) else { return 0 };
    conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='faces_scanned'",
        [],
        |r| r.get::<_, i64>(0),
    )
    .ok()
    .filter(|&n| n > 0)
    .and_then(|_| conn.query_row("SELECT COUNT(*) FROM faces_scanned", [], |r| r.get(0)).ok())
    .unwrap_or(0)
}

#[test]
fn kill_mid_run_then_resume_processes_every_image_exactly_once() {
    const N: usize = 10;
    let dir = tempdir().unwrap();
    let db = fixture_db(dir.path(), N);

    // --workers 1: deterministic, strictly-incremental progress to poll
    // against (this test is about interrupt/resume correctness, not
    // multi-worker parallelism, which is covered elsewhere).
    let mut child = Command::new(bin())
        .arg("faces")
        .arg("--db").arg(&db)
        .arg("--workers").arg("1")
        .arg("--silent")
        .spawn()
        .expect("failed to spawn videre faces");

    // Poll real on-disk progress instead of a fixed sleep, so this isn't
    // flaky relative to machine speed: wait until some but not all images
    // have been recorded as scanned, proving the kill below genuinely lands
    // mid-run rather than before start or after completion.
    let deadline = Instant::now() + Duration::from_secs(60);
    let killed_at = loop {
        assert!(Instant::now() < deadline, "process never showed partial progress within 60s");
        let n = scanned_count(&db);
        if n > 0 && (n as usize) < N {
            break n;
        }
        if (n as usize) >= N {
            panic!("run completed before we could interrupt it - increase N or slow the poll");
        }
        std::thread::sleep(Duration::from_millis(20));
    };

    child.kill().expect("failed to SIGKILL videre faces");
    child.wait().expect("failed to reap killed process");

    let after_kill = scanned_count(&db);
    assert!(after_kill > 0, "expected some progress to survive the kill");
    assert!(
        (after_kill as usize) < N,
        "expected partial progress ({after_kill}/{N}) after a mid-run kill, not full completion"
    );
    assert!(
        after_kill >= killed_at,
        "faces_scanned count must never go backwards: was {killed_at} when we killed, now {after_kill}"
    );

    // Resume: a plain rerun should pick up exactly where it left off and
    // finish covering every image, with no errors.
    let status = Command::new(bin())
        .arg("faces")
        .arg("--db").arg(&db)
        .arg("--workers").arg("1")
        .arg("--silent")
        .status()
        .expect("failed to run resumed videre faces");
    assert!(status.success(), "resumed run should exit 0");

    let final_count = scanned_count(&db);
    assert_eq!(final_count as usize, N, "every image must end up scanned exactly once after resuming");

    // faces_scanned.hash is a PRIMARY KEY, so duplicate-processing would have
    // already failed the INSERT rather than silently double-counting - this
    // is an explicit belt-and-suspenders check of that invariant.
    let conn = Connection::open(&db).unwrap();
    let distinct: i64 = conn.query_row("SELECT COUNT(DISTINCT hash) FROM faces_scanned", [], |r| r.get(0)).unwrap();
    assert_eq!(distinct as usize, N, "no hash should be recorded more than once");
}
