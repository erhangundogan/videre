//! End-to-end test for `videre tag`: add a tag to a selection, find it with
//! `search --tag`, then remove it and confirm it no longer matches.

mod common;
use common::TestLibrary;
use std::path::Path;

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
fn invalid_path_rejects_tag_mutation_even_with_valid_selection() {
    let a = TestLibrary::new();
    let b = TestLibrary::new();
    a.copy_fixture("tiny.jpg", "Trips/a.jpg");
    a.scan();
    let before = common::feature_fixture::snapshot_database(&a.db());
    let out = a
        .cmd()
        .args(["tag", "--add", "holiday", "--path", "Trips", "--path"])
        .arg(&b.root)
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "an out-of-root path must reject the mutation"
    );
    assert_eq!(
        common::feature_fixture::snapshot_database(&a.db()),
        before,
        "a rejected tag must leave the database unchanged"
    );
}

#[test]
fn tag_add_is_searchable_and_remove_clears_it() {
    let lib = TestLibrary::new();
    lib.copy_fixture("tiny.jpg", "photos/IMG.jpg");
    lib.scan();

    run(
        &lib,
        &["tag", "--add", "beach", "--path", "photos", "--silent"],
    );
    assert!(
        run(&lib, &["search", "--tag", "beach"]).contains("IMG.jpg"),
        "the tagged file should be searchable by --tag"
    );

    run(
        &lib,
        &["tag", "--remove", "beach", "--path", "photos", "--silent"],
    );
    assert!(
        !run(&lib, &["search", "--tag", "beach"]).contains("IMG.jpg"),
        "removing the tag should stop it matching"
    );
}

#[test]
fn scan_imports_dc_subject_keywords_as_tags_from_a_real_sidecar() {
    // Use the genuine exiftool-produced sidecar (dc:subject = holiday, beach) as
    // the photo's sidecar, so the import path is exercised against real
    // third-party output, not a hand-written approximation.
    let lib = TestLibrary::new();
    lib.copy_fixture("tiny.jpg", "photos/IMG.jpg");
    let sidecar_src =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/xmp/thirdparty-lightroom.xmp");
    std::fs::copy(&sidecar_src, lib.root.join("photos/IMG.jpg.xmp")).unwrap();
    lib.scan();

    assert!(
        run(&lib, &["search", "--tag", "holiday"]).contains("IMG.jpg"),
        "dc:subject keywords in the sidecar must import as tags"
    );
    assert!(run(&lib, &["search", "--tag", "beach"]).contains("IMG.jpg"));
}

#[test]
fn export_writes_tags_as_dc_subject_keywords() {
    let lib = TestLibrary::new();
    lib.copy_fixture("tiny.jpg", "photos/IMG.jpg");
    lib.scan();

    run(
        &lib,
        &["tag", "--add", "sunset", "--path", "photos", "--silent"],
    );
    run(&lib, &["export", "--xmp", "--path", "photos", "--silent"]);

    let sidecar = lib.root.join("photos/IMG.jpg.xmp");
    let doc = std::fs::read_to_string(sidecar).unwrap();
    assert!(
        doc.contains("<rdf:li>sunset</rdf:li>"),
        "the tag should be written as a dc:subject keyword; got: {doc}"
    );
    assert!(doc.contains("dc:subject"));
}
