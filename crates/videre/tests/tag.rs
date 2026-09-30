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

/// The content hash of the one file at `rel`, as the scan stored it.
fn hash_of(lib: &TestLibrary, rel: &str) -> String {
    let path = lib.root.join(rel).canonicalize().unwrap();
    lib.conn()
        .query_row(
            "SELECT hash FROM file_hashes WHERE path = ?1",
            [path.to_string_lossy().as_ref()],
            |r| r.get(0),
        )
        .unwrap()
}

/// Give a file the category `videre classify` would have given it.
fn classify_as(lib: &TestLibrary, hash: &str, category: &str) {
    lib.conn()
        .execute(
            "INSERT INTO classifications VALUES (?1, ?2, ?3, 0.9, '2026-09-30T10:00:00')",
            [videre_core::embeddings::DEFAULT_MODEL_ID, hash, category],
        )
        .unwrap();
}

#[test]
fn export_writes_tags_but_never_the_category() {
    let lib = TestLibrary::new();
    lib.copy_fixture("tiny.jpg", "Kadıköy/IMG.jpg");
    lib.scan();
    let hash = hash_of(&lib, "Kadıköy/IMG.jpg");
    classify_as(&lib, &hash, "photo");
    run(
        &lib,
        &[
            "tag",
            "--add",
            "doğum günü",
            "--path",
            "Kadıköy",
            "--silent",
        ],
    );
    run(&lib, &["export", "--xmp", "--path", "Kadıköy", "--silent"]);

    let doc = std::fs::read_to_string(lib.root.join("Kadıköy/IMG.jpg.xmp")).unwrap();
    assert!(doc.contains("<rdf:li>doğum günü</rdf:li>"), "{doc}");
    assert!(
        !doc.contains("<rdf:li>photo</rdf:li>"),
        "the category is derived, not the user's keyword: {doc}"
    );
}

#[test]
fn a_file_with_only_a_category_gets_no_sidecar() {
    let lib = TestLibrary::new();
    lib.copy_fixture("tiny.jpg", "Kadıköy/IMG.jpg");
    lib.scan();
    let hash = hash_of(&lib, "Kadıköy/IMG.jpg");
    classify_as(&lib, &hash, "unknown");
    run(&lib, &["export", "--xmp", "--path", "Kadıköy", "--silent"]);
    assert!(!lib.root.join("Kadıköy/IMG.jpg.xmp").exists());
}

fn tags_of(lib: &TestLibrary, hash: &str) -> Vec<String> {
    let mut tags = videre_core::tags::tags_for_hash(&lib.conn(), hash).unwrap();
    tags.sort();
    tags
}

/// The dc:subject keywords of a sidecar, sorted.
fn keywords_in(sidecar: &Path) -> Vec<String> {
    let doc = std::fs::read_to_string(sidecar).unwrap();
    let mut keywords: Vec<String> = doc
        .split("<rdf:li>")
        .skip(1)
        .filter_map(|s| s.split_once("</rdf:li>").map(|(k, _)| k.to_string()))
        .collect();
    keywords.sort();
    keywords
}

#[test]
fn a_rebuilt_library_gets_back_only_the_users_tags() {
    let lib = TestLibrary::new();
    lib.copy_fixture("tiny.jpg", "Kadıköy/IMG.jpg");
    lib.scan();
    let hash = hash_of(&lib, "Kadıköy/IMG.jpg");
    classify_as(&lib, &hash, "photo");
    run(
        &lib,
        &[
            "tag",
            "--add",
            "doğum günü",
            "--path",
            "Kadıköy",
            "--silent",
        ],
    );
    run(&lib, &["export", "--xmp", "--path", "Kadıköy", "--silent"]);

    std::fs::remove_dir_all(lib.root.join(".videre")).unwrap();
    lib.scan();
    assert_eq!(tags_of(&lib, &hash), ["doğum günü"]);
}

#[test]
fn a_full_export_drops_a_category_keyword_an_older_version_wrote() {
    // An older export wrote the category `photo`; another app added `Kadıköy`.
    let lib = TestLibrary::new();
    lib.copy_fixture("tiny.jpg", "Kadıköy/IMG.jpg");
    let sidecar = lib.root.join("Kadıköy/IMG.jpg.xmp");
    std::fs::write(
        &sidecar,
        "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\">\
         <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\
         <rdf:Description rdf:about=\"\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\">\
         <dc:subject><rdf:Bag><rdf:li>photo</rdf:li><rdf:li>Kadıköy</rdf:li></rdf:Bag></dc:subject>\
         </rdf:Description></rdf:RDF></x:xmpmeta>",
    )
    .unwrap();
    lib.scan();
    let hash = hash_of(&lib, "Kadıköy/IMG.jpg");
    assert_eq!(tags_of(&lib, &hash), ["Kadıköy", "photo"]);

    run(
        &lib,
        &["tag", "--remove", "photo", "--path", "Kadıköy", "--silent"],
    );
    classify_as(&lib, &hash, "photo");
    run(&lib, &["export", "--xmp", "--path", "Kadıköy", "--silent"]);
    assert_eq!(keywords_in(&sidecar), ["Kadıköy"]);
}
