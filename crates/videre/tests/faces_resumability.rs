//! Mechanically verifies `videre faces`'s kill-mid-run resumability claim,
//! rather than only reasoning about it in the abstract: actually kills the
//! process (SIGKILL, not a graceful Ctrl-C) partway through a real run and
//! asserts a resumed run picks up correctly with no images permanently lost.

mod common;
use common::{face_models_cached, shared_cache_guard, skip_without_models, TestLibrary};

use rusqlite::Connection;
use std::path::Path;
use std::time::{Duration, Instant};

/// Populates the library with `n` `file_hashes` rows, each a distinct fake hash
/// pointing at its own copy of the real `sample_with_exif.jpg` fixture under the
/// library root (real JPEG bytes, so detection genuinely runs SCRFD on each).
fn fixture_library(n: usize) -> TestLibrary {
    let lib = TestLibrary::new();
    let root = lib.context().paths.root;
    let conn = lib.init_db();
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_with_exif.jpg");
    for i in 0..n {
        let path = root.join(format!("img_{i:04}.jpg"));
        std::fs::copy(&source, &path).unwrap();
        let hash = format!("h{i:04}");
        conn.execute(
            "INSERT INTO file_hashes (path, hash, ext) VALUES (?1, ?2, 'jpg')",
            rusqlite::params![path.to_str().unwrap(), hash],
        )
        .unwrap();
    }
    lib
}

fn scanned_count(db: &Path) -> i64 {
    let Ok(conn) = Connection::open(db) else {
        return 0;
    };
    conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='faces_scanned'",
        [],
        |r| r.get::<_, i64>(0),
    )
    .ok()
    .filter(|&n| n > 0)
    .and_then(|_| {
        conn.query_row("SELECT COUNT(*) FROM faces_scanned", [], |r| r.get(0))
            .ok()
    })
    .unwrap_or(0)
}

#[test]
fn kill_mid_run_then_resume_processes_every_image_exactly_once() {
    // Held for the whole test, not just the first spawn: this one SIGKILLs its
    // child, and a concurrent first-time weights download in another test
    // binary would otherwise be the thing that dies half-written.
    if skip_without_models("faces", face_models_cached()) {
        return;
    }
    let _serial = shared_cache_guard();
    const N: usize = 10;
    let lib = fixture_library(N);
    let db = lib.db();

    // --workers 1: deterministic, strictly-incremental progress to poll against.
    let mut child = lib
        .cmd()
        .args(["faces", "--workers", "1", "--silent"])
        .spawn()
        .expect("failed to spawn videre faces");

    let deadline = Instant::now() + Duration::from_secs(60);
    let killed_at = loop {
        assert!(
            Instant::now() < deadline,
            "process never showed partial progress within 60s"
        );
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

    // Resume: a plain rerun should pick up exactly where it left off.
    let status = lib
        .cmd()
        .args(["faces", "--workers", "1", "--silent"])
        .status()
        .expect("failed to run resumed videre faces");
    assert!(status.success(), "resumed run should exit 0");

    let final_count = scanned_count(&db);
    assert_eq!(
        final_count as usize, N,
        "every image must end up scanned exactly once after resuming"
    );

    let conn = Connection::open(&db).unwrap();
    let distinct: i64 = conn
        .query_row("SELECT COUNT(DISTINCT hash) FROM faces_scanned", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(
        distinct as usize, N,
        "no hash should be recorded more than once"
    );
}
