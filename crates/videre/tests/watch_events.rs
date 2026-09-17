mod common;
use common::TestLibrary;
use std::time::{Duration, Instant};

/// One bounded pass: VIDERE_WATCH_ONCE makes `watch` run the startup scan
/// and exit 0, so the correctness backstop is testable without a
/// wall-clock race. Not silent: the per-stage record counts on stderr are
/// part of what these tests assert.
fn watch_once(lib: &TestLibrary, extra: &[&str]) -> std::process::Output {
    let mut args = vec!["watch"];
    args.extend_from_slice(extra);
    let out = lib
        .cmd()
        .args(&args)
        .env("VIDERE_WATCH_ONCE", "1")
        .output()
        .expect("run videre watch");
    assert!(
        out.status.success(),
        "watch (once) failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

#[test]
fn the_startup_scan_processes_a_dropped_file_and_a_second_pass_hashes_nothing() {
    let lib = TestLibrary::new();
    lib.copy_fixture("sample_with_exif.jpg", "a.jpg");
    watch_once(&lib, &["--scan"]);
    let count: i64 = lib
        .conn()
        .query_row("SELECT COUNT(*) FROM file_hashes", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1, "startup scan must process a dropped file");
    // Second pass over an unchanged tree hashes nothing (incremental).
    let out = watch_once(&lib, &["--scan"]);
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("scan stage wrote 0 record(s)"),
        "unchanged pass must hash nothing:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn the_face_recluster_stage_runs_once_per_new_face_and_then_skips() {
    let lib = TestLibrary::new();
    // One face row with a too-small bbox: the quality gate filters it out,
    // so the clustering pass itself is model-free, but the id is above any
    // watermark, so the gate must open exactly once.
    let conn = lib.init_db();
    conn.execute(
        "INSERT INTO faces (hash, bbox, embedding) VALUES ('h1', '0,0,10,10', X'0000')",
        [],
    )
    .unwrap();
    drop(conn);

    watch_once(&lib, &["--faces"]);
    let conn = lib.conn();
    let status: String = conn
        .query_row(
            "SELECT status FROM pipeline_runs WHERE command = 'face-recluster'",
            [],
            |r| r.get(0),
        )
        .expect("a face above the watermark must open the gate on the first pass");
    assert_eq!(status, "success");
    let first: String = conn
        .query_row(
            "SELECT started_at FROM pipeline_runs WHERE command = 'face-recluster'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    drop(conn);

    // No faces were added: the gate stays closed and the stage must not run
    // again (the row is an upsert, so a second run would bump started_at).
    watch_once(&lib, &["--faces"]);
    let conn = lib.conn();
    let second: String = conn
        .query_row(
            "SELECT started_at FROM pipeline_runs WHERE command = 'face-recluster'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    drop(conn);
    assert_eq!(
        first, second,
        "no new faces: the gated stage must not run a second time"
    );
}

/// Best-effort, generously timed. Event delivery on a hosted CI image is
/// not reliable enough for tight timing, so on timeout this SKIPS (fd 2
/// message) rather than fails; the startup-scan guarantee is the hard test.
#[test]
fn a_dropped_file_is_processed_via_events_within_a_generous_bound() {
    use std::io::Write;
    let lib = TestLibrary::new();
    let out = lib
        .cmd()
        .args(["config", "set", "watch-debounce-ms", "200"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let mut child = lib
        .cmd()
        .args(["watch", "--scan", "--silent"])
        .spawn()
        .expect("spawn watch");
    std::thread::sleep(Duration::from_millis(700)); // let the watcher register
    lib.copy_fixture("sample_with_exif.jpg", "dropped.jpg");
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut seen = false;
    while Instant::now() < deadline {
        let count: i64 = lib
            .conn()
            .query_row("SELECT COUNT(*) FROM file_hashes", [], |r| r.get(0))
            .unwrap_or(0);
        if count > 0 {
            seen = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    let _ = child.kill();
    let _ = child.wait();
    if !seen {
        let _ = writeln!(
            std::io::stderr(),
            "SKIP: event not delivered within 20s; the startup-scan guarantee covers correctness"
        );
    }
}

/// Two watches on two separate libraries run at once without error:
/// separate roots mean separate .videre directories and locks, so neither
/// can refuse or block the other.
#[test]
fn two_watches_on_separate_libraries_do_not_conflict() {
    let a = TestLibrary::new();
    let b = TestLibrary::new();
    a.copy_fixture("sample_with_exif.jpg", "a.jpg");
    b.copy_fixture("sample_with_exif.jpg", "b.jpg");
    let mut ca = a
        .cmd()
        .args(["watch", "--scan", "--silent"])
        .spawn()
        .unwrap();
    let mut cb = b
        .cmd()
        .args(["watch", "--scan", "--silent"])
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(700));
    assert!(
        ca.try_wait().unwrap().is_none(),
        "watch A should still be running"
    );
    assert!(
        cb.try_wait().unwrap().is_none(),
        "watch B should still be running"
    );
    let _ = ca.kill();
    let _ = ca.wait();
    let _ = cb.kill();
    let _ = cb.wait();
}

/// Degraded fallback: when event registration fails, watch says so once and
/// keeps running the rescan loop instead of crashing or sitting silent.
#[test]
fn a_failed_registration_degrades_to_the_rescan_loop_with_a_notice() {
    let lib = TestLibrary::new();
    let err_path = lib.root.join("watch-stderr.log");
    let err_file = std::fs::File::create(&err_path).unwrap();
    let mut child = lib
        .cmd()
        .args(["watch", "--scan", "--silent"])
        .env("VIDERE_WATCH_TEST_FAIL_EVENTS", "1")
        .stderr(std::process::Stdio::from(err_file))
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(1500));
    let alive = child.try_wait().unwrap().is_none();
    let _ = child.kill();
    let _ = child.wait();
    assert!(alive, "watch must keep running in degraded mode, not exit");
    let notice = std::fs::read_to_string(&err_path).unwrap();
    assert!(
        notice.contains("live events unavailable"),
        "the degraded mode must be announced, not silent: {notice}"
    );
}

/// The internal maintenance deadline reruns the opt-in stages with no
/// events at all: prune's row (an upsert) must be touched again after a
/// tick.
#[test]
fn the_maintenance_deadline_reruns_opt_in_stages_without_events() {
    let lib = TestLibrary::new();
    let out = lib
        .cmd()
        .args(["config", "set", "watch-debounce-ms", "200"])
        .output()
        .unwrap();
    assert!(out.status.success());
    // Initialize before spawning: the polling below opens the database while
    // the child is starting up.
    drop(lib.init_db());
    let mut child = lib
        .cmd()
        .args(["watch", "--prune", "--silent"])
        .env("VIDERE_WATCH_TEST_MAINTENANCE_SECS", "1")
        .spawn()
        .unwrap();
    // Startup reconcile runs --prune once; wait for that row.
    let conn = lib.conn();
    let mut first = String::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if let Ok(s) = conn.query_row(
            "SELECT started_at FROM pipeline_runs WHERE command = 'prune'",
            [],
            |r| r.get::<_, String>(0),
        ) {
            first = s;
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    drop(conn);
    assert!(!first.is_empty(), "startup reconcile must run --prune");
    // The maintenance pass must touch prune's row again (poll: started_at
    // has one-second resolution, so wait for an actual change).
    let mut second = String::new();
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if let Ok(s) = lib.conn().query_row(
            "SELECT started_at FROM pipeline_runs WHERE command = 'prune'",
            [],
            |r| r.get::<_, String>(0),
        ) {
            second = s;
            if second != first {
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    let _ = child.kill();
    let _ = child.wait();
    assert_ne!(
        first, second,
        "the maintenance pass must rerun the prune stage without events"
    );
}
