mod common;
use common::TestLibrary;

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
