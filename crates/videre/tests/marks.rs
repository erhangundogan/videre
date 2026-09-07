//! End-to-end tests for photo marks: XMP is read on scan, and the `--xmp`
//! precedence rule decides whether the file or the database wins on re-scan.

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

/// Copies the tiny fixture jpg into the library as `photos/IMG.jpg` and writes
/// an adjacent `IMG.jpg.xmp` sidecar carrying the given rating.
fn fixture_with_sidecar(lib: &TestLibrary, rating: i64) {
    lib.copy_fixture("tiny.jpg", "photos/IMG.jpg");
    let sidecar = lib.root.join("photos/IMG.jpg.xmp");
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
}

#[test]
fn scan_imports_xmp_rating_and_db_precedence_holds() {
    let lib = TestLibrary::new();
    fixture_with_sidecar(&lib, 3);

    // Scan reads the sidecar: the rating is imported.
    lib.scan();
    assert!(
        run(&lib, &["search", "--rating", "3"]).contains("IMG.jpg"),
        "expected rating 3 imported from the sidecar"
    );

    // Override in videre.
    run(
        &lib,
        &["mark", "--path", "photos", "--rating", "5", "--silent"],
    );
    assert!(run(&lib, &["search", "--rating", "5"]).contains("IMG.jpg"));

    // Default precedence is db: a re-scan does not clobber the 5.
    lib.scan();
    assert!(
        run(&lib, &["search", "--rating", "5"]).contains("IMG.jpg"),
        "db should win by default"
    );

    // --xmp file: the file's 3 wins, so it is no longer >= 4.
    run(&lib, &["scan", "--xmp", "file", "--silent"]);
    assert!(
        !run(&lib, &["search", "--rating", "4"]).contains("IMG.jpg"),
        "--xmp file should revert the rating to 3"
    );
    assert!(run(&lib, &["search", "--rating", "3"]).contains("IMG.jpg"));
}

#[test]
fn export_xmp_writes_sidecars_and_dry_run_writes_nothing() {
    let lib = TestLibrary::new();
    lib.copy_fixture("tiny.jpg", "photos/A.jpg");
    lib.scan();
    let sidecar = lib.root.join("photos/A.jpg.xmp");

    run(
        &lib,
        &[
            "mark", "--path", "photos", "--rating", "4", "--label", "Green", "--silent",
        ],
    );

    // Dry run writes nothing.
    run(
        &lib,
        &[
            "mark",
            "--path",
            "photos",
            "--export-xmp",
            "--dry-run",
            "--silent",
        ],
    );
    assert!(!sidecar.exists(), "--dry-run must not write a sidecar");

    // Real export writes a sidecar carrying the rating and label.
    run(
        &lib,
        &["mark", "--path", "photos", "--export-xmp", "--silent"],
    );
    assert!(sidecar.exists(), "expected {}", sidecar.display());
    let doc = std::fs::read_to_string(&sidecar).unwrap();
    assert!(doc.contains("<xmp:Rating>4</xmp:Rating>"), "got: {doc}");
    assert!(doc.contains("Green"), "got: {doc}");

    // And it reads back into a fresh library carrying the image and its sidecar.
    let fresh = TestLibrary::new();
    std::fs::create_dir_all(fresh.root.join("photos")).unwrap();
    std::fs::copy(
        lib.root.join("photos/A.jpg"),
        fresh.root.join("photos/A.jpg"),
    )
    .unwrap();
    std::fs::copy(&sidecar, fresh.root.join("photos/A.jpg.xmp")).unwrap();
    fresh.scan();
    assert!(run(&fresh, &["search", "--rating", "4"]).contains("A.jpg"));
}

#[test]
fn stats_reports_marks() {
    let lib = TestLibrary::new();
    lib.copy_fixture("tiny.jpg", "photos/S.jpg");
    lib.scan();
    run(
        &lib,
        &[
            "mark", "--path", "photos", "--rating", "5", "--like", "--silent",
        ],
    );
    let out = run(&lib, &["stats"]);
    assert!(out.contains("Marks: 1 rated"), "got: {out}");
    assert!(out.contains("1 liked"), "got: {out}");
}

// `prune` gains its directory-local entry point in a later task; this test
// stays red until then.
#[test]
fn prune_drops_orphaned_marks() {
    let lib = TestLibrary::new();
    lib.copy_fixture("tiny.jpg", "photos/one.jpg");
    lib.copy_fixture("sample_with_exif.jpg", "photos/two.jpg");
    lib.scan();
    run(
        &lib,
        &["mark", "--path", "photos", "--rating", "5", "--silent"],
    );

    let before = run(&lib, &["search", "--rating", "5"]);
    assert!(
        before.contains("one.jpg") && before.contains("two.jpg"),
        "both should be rated: {before}"
    );

    // Delete one photo from disk; prune removes its stale row and orphan mark.
    std::fs::remove_file(lib.root.join("photos/one.jpg")).unwrap();
    run(&lib, &["prune", "--silent"]);

    let after = run(&lib, &["search", "--rating", "5"]);
    assert!(
        !after.contains("one.jpg"),
        "orphan mark must be gone: {after}"
    );
    assert!(
        after.contains("two.jpg"),
        "surviving mark must remain: {after}"
    );
}
