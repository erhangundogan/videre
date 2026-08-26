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
