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
