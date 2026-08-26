//! End-to-end tests for photo marks: XMP is read on scan, and the `--xmp`
//! precedence rule decides whether the file or the database wins on re-scan.

mod common;
use common::videre_bin as bin;
use std::path::Path;
use std::process::Command;
use tempfile::tempdir;

/// Copies the tiny fixture jpg into `dir` as `IMG.jpg` and writes an adjacent
/// `IMG.jpg.xmp` sidecar carrying the given rating. Returns the image path.
fn fixture_with_sidecar(dir: &Path, rating: i64) -> std::path::PathBuf {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tiny.jpg");
    let img = dir.join("IMG.jpg");
    std::fs::copy(&src, &img).expect("copy fixture");
    let sidecar = dir.join("IMG.jpg.xmp");
    std::fs::write(
        &sidecar,
        format!(
            r#"<?xpacket begin="?"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF
 xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
 xmlns:xmp="http://ns.adobe.com/xap/1.0/">
 <rdf:Description xmp:Rating="{rating}"/>
</rdf:RDF></x:xmpmeta><?xpacket end="w"?>"#
        ),
    )
    .expect("write sidecar");
    img
}

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
fn scan_imports_xmp_rating_and_db_precedence_holds() {
    let dir = tempdir().unwrap();
    let photos = dir.path().join("photos");
    std::fs::create_dir(&photos).unwrap();
    fixture_with_sidecar(&photos, 3);
    let db = dir.path().join("hashes.db");
    let (photos_s, db_s) = (photos.to_str().unwrap(), db.to_str().unwrap());

    // Scan reads the sidecar: the rating is imported.
    run(&["scan", photos_s, "--db", db_s, "--silent"]);
    assert!(
        run(&["search", "--rating", "3", "--db", db_s]).contains("IMG.jpg"),
        "expected rating 3 imported from the sidecar"
    );

    // Override in videre.
    run(&[
        "mark", "--path", photos_s, "--rating", "5", "--db", db_s, "--silent",
    ]);
    assert!(run(&["search", "--rating", "5", "--db", db_s]).contains("IMG.jpg"));

    // Default precedence is db: a re-scan does not clobber the 5.
    run(&["scan", photos_s, "--db", db_s, "--silent"]);
    assert!(
        run(&["search", "--rating", "5", "--db", db_s]).contains("IMG.jpg"),
        "db should win by default"
    );

    // --xmp file: the file's 3 wins, so it is no longer >= 4.
    run(&["scan", photos_s, "--db", db_s, "--xmp", "file", "--silent"]);
    assert!(
        !run(&["search", "--rating", "4", "--db", db_s]).contains("IMG.jpg"),
        "--xmp file should revert the rating to 3"
    );
    assert!(run(&["search", "--rating", "3", "--db", db_s]).contains("IMG.jpg"));
}

/// Copies the fixture jpg into `dir` as `name`, with no sidecar.
fn plain_fixture(dir: &Path, name: &str) -> std::path::PathBuf {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tiny.jpg");
    let img = dir.join(name);
    std::fs::copy(&src, &img).expect("copy fixture");
    img
}

#[test]
fn export_xmp_writes_sidecars_and_dry_run_writes_nothing() {
    let dir = tempdir().unwrap();
    let photos = dir.path().join("photos");
    std::fs::create_dir(&photos).unwrap();
    let img = plain_fixture(&photos, "A.jpg");
    let db = dir.path().join("hashes.db");
    let (photos_s, db_s) = (photos.to_str().unwrap(), db.to_str().unwrap());
    let sidecar = photos.join("A.jpg.xmp");

    run(&["scan", photos_s, "--db", db_s, "--silent"]);
    run(&[
        "mark", "--path", photos_s, "--rating", "4", "--label", "Green", "--db", db_s, "--silent",
    ]);

    // Dry run writes nothing.
    run(&[
        "mark",
        "--path",
        photos_s,
        "--export-xmp",
        "--dry-run",
        "--db",
        db_s,
        "--silent",
    ]);
    assert!(!sidecar.exists(), "--dry-run must not write a sidecar");

    // Real export writes a sidecar carrying the rating and label.
    run(&[
        "mark",
        "--path",
        photos_s,
        "--export-xmp",
        "--db",
        db_s,
        "--silent",
    ]);
    assert!(sidecar.exists(), "expected {}", sidecar.display());
    let doc = std::fs::read_to_string(&sidecar).unwrap();
    assert!(doc.contains("xmp:Rating=\"4\""), "got: {doc}");
    assert!(doc.contains("Green"), "got: {doc}");

    // And it reads back into a fresh library.
    let db2 = dir.path().join("hashes2.db");
    let db2_s = db2.to_str().unwrap();
    // The scan needs the image beside its sidecar; both are already in photos.
    let _ = &img;
    run(&["scan", photos_s, "--db", db2_s, "--silent"]);
    assert!(run(&["search", "--rating", "4", "--db", db2_s]).contains("A.jpg"));
}
