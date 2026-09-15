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
fn status_reports_a_decode_failed_file_as_skipped_not_outstanding() {
    // A file embed has given up decoding (at the two-strike threshold) must not
    // read as outstanding or keep status suggesting embed; it is reported as
    // skipped instead.
    let lib = TestLibrary::new();
    lib.copy_fixture("tiny.jpg", "photos/IMG.jpg");
    lib.scan();
    {
        let conn = lib.conn();
        let hash: String = conn
            .query_row("SELECT hash FROM file_hashes LIMIT 1", [], |r| r.get(0))
            .unwrap();
        videre_core::decode_failures::ensure_table(&conn).unwrap();
        for _ in 0..videre_core::decode_failures::FAILURE_THRESHOLD {
            videre_core::decode_failures::record(
                &conn,
                &hash,
                videre_core::decode_failures::STAGE_EMBED,
                "x",
            )
            .unwrap();
        }
    }

    let json = String::from_utf8(
        lib.cmd()
            .args(["status", "--json"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    let doc: serde_json::Value = serde_json::from_str(&json).unwrap();
    let embed = doc["report"]["coverage"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["stage"] == "embed")
        .expect("embed coverage present");
    assert_eq!(
        embed["outstanding"], 0,
        "the only file is skipped, not outstanding"
    );
    assert_eq!(embed["skipped"], 1, "and is reported as skipped");

    let text = String::from_utf8(lib.cmd().arg("status").output().unwrap().stdout).unwrap();
    assert!(
        text.contains("skipped as undecodable"),
        "status text must name skipped files, got: {text}"
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
    let out = lib.cmd().args(["status", "--check"]).status().unwrap();
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

#[test]
fn status_check_exits_nonzero_when_a_command_failed() {
    let lib = TestLibrary::new();
    lib.copy_fixture("tiny.jpg", "photos/IMG.jpg");
    lib.scan();

    // Simulate a prior failed run by writing directly into pipeline_runs.
    // Exercising the CLI's own failure path for every tracked command would
    // be its own large test; this isolates --check's exit-code contract.
    lib.conn()
        .execute(
            "INSERT OR REPLACE INTO pipeline_runs (command, started_at, finished_at, status, duration_ms, summary)
             VALUES ('faces', '2026-01-01 00:00:00', '2026-01-01 00:00:01', 'failed', 1000, 'boom')",
            [],
        )
        .unwrap();

    let out = lib
        .cmd()
        .args(["status", "--check"])
        .output()
        .expect("failed to run videre status --check");
    assert!(
        !out.status.success(),
        "a failed command must make --check exit non-zero"
    );
    // Output is unchanged by --check, normal status text is still printed.
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Coverage"), "{stdout}");

    let json_out = lib
        .cmd()
        .args(["status", "--json", "--check"])
        .output()
        .expect("failed to run videre status --json --check");
    assert!(!json_out.status.success());
    let doc: serde_json::Value = serde_json::from_slice(&json_out.stdout).unwrap();
    assert_eq!(
        doc["schema_version"], 1,
        "--json output must still be valid, unaffected by --check"
    );
}
