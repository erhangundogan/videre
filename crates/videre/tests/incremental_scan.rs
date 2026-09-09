//! `videre scan` is incremental by default: a file whose row is already current
//! (unchanged size + mtime, and complete for the requested work) is skipped
//! rather than re-read and re-hashed. `--force` re-reads everything. These are
//! behavioural checks against the real binary; `total_files` in the `--json`
//! summary is the count *processed* this run, so a no-op rescan reports 0.

mod common;
use common::TestLibrary;
use serde_json::Value;

/// Run `videre scan --json [extra...]` and return how many files it processed.
fn scan_processed(lib: &TestLibrary, extra: &[&str]) -> u64 {
    let mut args = vec!["scan", "--json"];
    args.extend_from_slice(extra);
    let out = lib.cmd().args(&args).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).expect("scan --json output");
    v["total_files"].as_u64().expect("total_files")
}

#[test]
fn second_scan_of_an_unchanged_library_processes_nothing() {
    let lib = TestLibrary::new();
    lib.copy_fixture("tiny.jpg", "a.jpg");
    lib.copy_fixture("sample_with_exif.jpg", "b.jpg");
    assert_eq!(scan_processed(&lib, &[]), 2, "first scan processes both");
    assert_eq!(
        scan_processed(&lib, &[]),
        0,
        "unchanged rescan processes none"
    );
    assert_eq!(
        scan_processed(&lib, &["--force"]),
        2,
        "--force reprocesses all"
    );
}

#[test]
fn a_changed_file_is_reprocessed_a_new_file_is_picked_up() {
    let lib = TestLibrary::new();
    lib.copy_fixture("tiny.jpg", "a.jpg");
    assert_eq!(scan_processed(&lib, &[]), 1);
    // change content: size and mtime both move
    std::fs::write(lib.root.join("a.jpg"), b"different bytes entirely").unwrap();
    assert_eq!(scan_processed(&lib, &[]), 1, "edited file reprocessed");
    lib.copy_fixture("sample_with_exif.jpg", "c.jpg");
    assert_eq!(scan_processed(&lib, &[]), 1, "only the new file");
}

#[test]
fn an_unchanged_row_missing_mime_is_still_processed() {
    let lib = TestLibrary::new();
    lib.copy_fixture("tiny.jpg", "a.jpg");
    scan_processed(&lib, &[]);
    lib.conn()
        .execute("UPDATE file_hashes SET mime = NULL", [])
        .unwrap();
    assert_eq!(scan_processed(&lib, &[]), 1, "mime backfill still happens");
}

#[test]
fn similar_reprocesses_an_unchanged_file_that_has_no_phash() {
    let lib = TestLibrary::new();
    lib.copy_fixture("tiny.jpg", "a.jpg");
    scan_processed(&lib, &[]); // no phash yet
    assert_eq!(
        scan_processed(&lib, &["--similar"]),
        1,
        "phash backfill for an unchanged file"
    );
    assert_eq!(
        scan_processed(&lib, &["--similar"]),
        0,
        "now current for --similar"
    );
}
