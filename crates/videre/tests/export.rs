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
