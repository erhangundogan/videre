mod common;
use common::TestLibrary;
use std::io::Write;
use std::process::{Output, Stdio};

/// Run `videre pipeline <args>` against this library, optionally feeding stdin.
/// Mirrors the spawn idiom used by the other integration tests.
fn run_pipeline(lib: &TestLibrary, args: &[&str], stdin_input: Option<&str>) -> Output {
    let mut cmd = lib.cmd();
    cmd.arg("pipeline").args(args);
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("failed to run videre pipeline");
    if let Some(input) = stdin_input {
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
    }
    child
        .wait_with_output()
        .expect("failed to wait on videre pipeline")
}

fn stdout_of(out: &Output) -> String {
    assert!(
        out.status.success(),
        "pipeline should succeed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The HF cache dir for this library's isolated child. Empty means no weights
/// were downloaded.
fn hf_home_is_empty(lib: &TestLibrary) -> bool {
    let dir = lib.home.join(".cache/huggingface");
    match std::fs::read_dir(&dir) {
        Err(_) => true,
        Ok(mut entries) => entries.next().is_none(),
    }
}

#[test]
fn dry_run_prints_the_plan_and_does_no_work() {
    let lib = TestLibrary::new();
    lib.copy_fixture("tiny.jpg", "a.jpg");
    lib.scan();

    let out = run_pipeline(&lib, &["--dry-run"], None);
    let text = stdout_of(&out);
    for stage in ["scan", "faces", "embed", "classify", "locations"] {
        assert!(
            text.contains(stage),
            "plan should list stage {stage}:\n{text}"
        );
    }
    assert!(
        text.contains("dry run") || text.contains("no work"),
        "dry-run should say it did nothing:\n{text}"
    );
    assert!(hf_home_is_empty(&lib), "dry-run must not download weights");
}

#[test]
fn runs_scan_and_locations_skipping_heavy_stages() {
    let lib = TestLibrary::new();
    lib.copy_fixture("tiny.jpg", "a.jpg");

    // --skip embed,classify,faces keeps the run model-free: only scan and
    // locations execute, neither loads weights.
    let out = run_pipeline(&lib, &["--skip", "embed,classify,faces"], None);
    let text = stdout_of(&out);

    assert!(
        text.contains("scan"),
        "scan should appear in the resolved checklist:\n{text}"
    );
    assert!(
        text.contains("locations"),
        "locations should appear:\n{text}"
    );
    assert!(hf_home_is_empty(&lib), "no weights should be downloaded");

    // A pipeline_runs row for scan proves the stage actually ran.
    let status = lib.cmd().args(["status"]).output().unwrap();
    let status_text = String::from_utf8_lossy(&status.stdout);
    assert!(
        status_text.contains("scan"),
        "status should show a scan run:\n{status_text}"
    );
}

#[test]
fn declining_the_gate_skips_embed_and_classify_without_loading_models() {
    let lib = TestLibrary::new();
    lib.copy_fixture("tiny.jpg", "a.jpg");
    lib.scan();

    // Skip faces so embed is the only heavy stage the gate covers; answer "n".
    let out = run_pipeline(&lib, &["--skip", "faces"], Some("n\n"));
    assert!(
        out.status.success(),
        "declining continues and exits 0:\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.to_lowercase().contains("proceed"),
        "the gate prompt should appear on stderr:\n{stderr}"
    );
    assert!(
        hf_home_is_empty(&lib),
        "declining must not download any weights"
    );
}

#[test]
fn yes_runs_embed_when_models_are_available() {
    let _guard = common::shared_cache_guard();
    if common::skip_without_models("embed", common::siglip_cached()) {
        return;
    }
    let lib = TestLibrary::new();
    lib.copy_fixture("tiny.jpg", "a.jpg");
    lib.scan();
    let out = run_pipeline(&lib, &["--skip", "faces,classify,locations", "--yes"], None);
    assert!(
        out.status.success(),
        "yes-run should succeed:\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // One embedding now exists.
    let stats = lib.cmd().args(["stats"]).output().unwrap();
    let stats_text = String::from_utf8_lossy(&stats.stdout);
    assert!(
        stats_text.to_lowercase().contains("embed"),
        "embeddings should be reported:\n{stats_text}"
    );
}

#[test]
fn dry_run_json_lists_the_planned_stages() {
    let lib = TestLibrary::new();
    lib.copy_fixture("tiny.jpg", "a.jpg");
    lib.scan();

    let out = run_pipeline(&lib, &["--dry-run", "--json"], None);
    let text = stdout_of(&out);
    let v: serde_json::Value = serde_json::from_str(text.trim()).expect("valid json");
    assert_eq!(v["dry_run"], serde_json::json!(true));
    let stages = v["stages"].as_array().unwrap();
    assert!(stages.iter().any(|s| s == "embed"));
}
