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
