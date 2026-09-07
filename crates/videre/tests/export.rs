//! End-to-end test for `videre export --xmp`: a rated file gets a sidecar
//! carrying the rating in portable element form, and `--dry-run` writes nothing.

mod common;
use common::TestLibrary;

fn run(lib: &TestLibrary, args: &[&str]) -> String {
    let out = lib.cmd().args(args).output().expect("run videre");
    assert!(
        out.status.success(),
        "videre {args:?} failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn export_xmp_writes_sidecar_with_rating_and_dry_run_writes_nothing() {
    let lib = TestLibrary::new();
    lib.copy_fixture("tiny.jpg", "photos/IMG.jpg");
    lib.scan();
    let sidecar = lib.root.join("photos/IMG.jpg.xmp");

    run(
        &lib,
        &["mark", "--path", "photos", "--rating", "4", "--silent"],
    );

    // --dry-run must not write.
    run(
        &lib,
        &[
            "export",
            "--xmp",
            "--dry-run",
            "--path",
            "photos",
            "--silent",
        ],
    );
    assert!(!sidecar.exists(), "--dry-run must not write a sidecar");

    // Real export writes the sidecar with the rating in element form.
    run(&lib, &["export", "--xmp", "--path", "photos", "--silent"]);
    assert!(sidecar.exists(), "expected {}", sidecar.display());
    let doc = std::fs::read_to_string(&sidecar).unwrap();
    assert!(doc.contains("<xmp:Rating>4</xmp:Rating>"), "got: {doc}");
}

#[test]
fn jsonl_is_an_explicit_replacing_snapshot() {
    let lib = TestLibrary::new();
    lib.copy_fixture("tiny.jpg", "Trips/a.jpg");
    lib.copy_fixture("tiny.jpg", "Trips/b.jpg");
    lib.scan();
    let target = lib.root.join(".videre/hashes.jsonl");
    assert!(!target.exists(), "scan must not create the JSONL snapshot");

    run(&lib, &["export", "--jsonl", "--path", "Trips", "--silent"]);
    let lines: Vec<serde_json::Value> = std::fs::read_to_string(&target)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(lines.len(), 2, "one record per path of the duplicate hash");

    // A selection matching nothing replaces the snapshot with an empty file.
    run(
        &lib,
        &["export", "--jsonl", "--path", "missing", "--silent"],
    );
    assert_eq!(std::fs::read(&target).unwrap(), b"");
}

#[test]
fn jsonl_dry_run_preserves_the_previous_snapshot() {
    let lib = TestLibrary::new();
    lib.copy_fixture("tiny.jpg", "a.jpg");
    lib.scan();
    let target = lib.root.join(".videre/hashes.jsonl");

    run(&lib, &["export", "--jsonl", "--silent"]);
    let before = std::fs::read(&target).unwrap();
    assert!(!before.is_empty());

    run(&lib, &["export", "--jsonl", "--dry-run", "--silent"]);
    assert_eq!(
        std::fs::read(&target).unwrap(),
        before,
        "dry-run must not rewrite the snapshot"
    );
}

#[test]
fn xmp_and_jsonl_conflict_and_no_format_errors() {
    let lib = TestLibrary::new();
    lib.copy_fixture("tiny.jpg", "a.jpg");
    lib.scan();

    let both = lib
        .cmd()
        .args(["export", "--xmp", "--jsonl"])
        .output()
        .unwrap();
    assert!(!both.status.success(), "--xmp and --jsonl must conflict");

    let neither = lib.cmd().args(["export"]).output().unwrap();
    assert!(
        !neither.status.success(),
        "export with no format must error"
    );
}
