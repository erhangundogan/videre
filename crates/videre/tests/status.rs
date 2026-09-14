//! Integration tests for `videre status`: the read-only truth surface for
//! the pipeline. These spawn the real binary against a real (temporary)
//! library, so the assertions cover the whole path from database to text.

mod common;
use common::TestLibrary;

#[test]
fn status_reports_the_embed_gap_and_names_the_command() {
    let lib = TestLibrary::new();
    lib.copy_fixture("tiny.jpg", "photos/IMG.jpg");
    lib.scan();

    let out = String::from_utf8(lib.cmd().arg("status").output().unwrap().stdout).unwrap();
    assert!(out.to_lowercase().contains("embed"), "got: {out}");
    assert!(
        out.to_lowercase().contains("videre embed"),
        "status must name the command that closes the gap, got: {out}"
    );
}

#[test]
fn status_fresh_library_is_healthy_for_check() {
    // A brand-new library is fully stale (nothing scanned, nothing embedded)
    // and that must not read as a failure: staleness is informational, only
    // a failed or crashed pipeline run is (spec decision D5).
    let lib = TestLibrary::new();
    lib.copy_fixture("tiny.jpg", "photos/IMG.jpg");
    lib.scan();
    let out = lib
        .cmd()
        .args(["status", "--check"])
        .status()
        .unwrap();
    assert!(out.success(), "--check must exit zero without failed runs");
}

#[test]
fn status_json_carries_every_block() {
    let lib = TestLibrary::new();
    lib.copy_fixture("tiny.jpg", "photos/IMG.jpg");
    lib.scan();

    let out = String::from_utf8(
        lib.cmd()
            .args(["status", "--json"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    let doc: serde_json::Value = serde_json::from_str(&out).expect("valid JSON on stdout");
    assert!(doc["schema_version"].is_u64());
    let report = &doc["report"];
    assert!(report["coverage"].as_array().unwrap().len() >= 4);
    assert!(report["pipelines"].is_array());
    assert!(report["watch"].is_object());
    assert!(report["embed_model"].is_string());
}
