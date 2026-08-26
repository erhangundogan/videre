//! End-to-end test for `videre export --xmp`: a rated file gets a sidecar
//! carrying the rating in portable element form, and `--dry-run` writes nothing.

mod common;
use common::videre_bin as bin;
use std::path::Path;
use std::process::Command;
use tempfile::tempdir;

fn run(args: &[&str]) -> String {
    let out = Command::new(bin()).args(args).output().expect("run videre");
    assert!(
        out.status.success(),
        "videre {args:?} failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn export_xmp_writes_sidecar_with_rating_and_dry_run_writes_nothing() {
    let dir = tempdir().unwrap();
    let photos = dir.path().join("photos");
    std::fs::create_dir(&photos).unwrap();
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tiny.jpg");
    let img = photos.join("IMG.jpg");
    std::fs::copy(&src, &img).expect("copy fixture");
    let db = dir.path().join("hashes.db");
    let (photos_s, db_s) = (photos.to_str().unwrap(), db.to_str().unwrap());

    run(&["scan", photos_s, "--db", db_s, "--silent"]);
    run(&[
        "mark", "--path", photos_s, "--rating", "4", "--db", db_s, "--silent",
    ]);

    let sidecar = photos.join("IMG.jpg.xmp");

    // --dry-run must not write.
    run(&[
        "export",
        "--xmp",
        "--dry-run",
        "--path",
        photos_s,
        "--db",
        db_s,
        "--silent",
    ]);
    assert!(!sidecar.exists(), "--dry-run must not write a sidecar");

    // Real export writes the sidecar with the rating in element form.
    run(&[
        "export", "--xmp", "--path", photos_s, "--db", db_s, "--silent",
    ]);
    assert!(sidecar.exists(), "expected {}", sidecar.display());
    let doc = std::fs::read_to_string(&sidecar).unwrap();
    assert!(doc.contains("<xmp:Rating>4</xmp:Rating>"), "got: {doc}");
}
