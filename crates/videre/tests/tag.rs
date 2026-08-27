//! End-to-end test for `videre tag`: add a tag to a selection, find it with
//! `search --tag`, then remove it and confirm it no longer matches.

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
fn tag_add_is_searchable_and_remove_clears_it() {
    let dir = tempdir().unwrap();
    let photos = dir.path().join("photos");
    std::fs::create_dir(&photos).unwrap();
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tiny.jpg");
    std::fs::copy(&src, photos.join("IMG.jpg")).unwrap();
    let db = dir.path().join("hashes.db");
    let (photos_s, db_s) = (photos.to_str().unwrap(), db.to_str().unwrap());

    run(&["scan", photos_s, "--db", db_s, "--silent"]);
    run(&[
        "tag", "--add", "beach", "--path", photos_s, "--db", db_s, "--silent",
    ]);

    assert!(
        run(&["search", "--tag", "beach", "--db", db_s]).contains("IMG.jpg"),
        "the tagged file should be searchable by --tag"
    );

    run(&[
        "tag", "--remove", "beach", "--path", photos_s, "--db", db_s, "--silent",
    ]);
    assert!(
        !run(&["search", "--tag", "beach", "--db", db_s]).contains("IMG.jpg"),
        "removing the tag should stop it matching"
    );
}

#[test]
fn scan_imports_dc_subject_keywords_as_tags_from_a_real_sidecar() {
    // Use the genuine exiftool-produced sidecar (dc:subject = holiday, beach) as
    // the photo's sidecar, so the import path is exercised against real third-party
    // output, not a hand-written approximation.
    let dir = tempdir().unwrap();
    let photos = dir.path().join("photos");
    std::fs::create_dir(&photos).unwrap();
    let img = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tiny.jpg");
    std::fs::copy(&img, photos.join("IMG.jpg")).unwrap();
    let sidecar_src =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/xmp/thirdparty-lightroom.xmp");
    std::fs::copy(&sidecar_src, photos.join("IMG.jpg.xmp")).unwrap();
    let db = dir.path().join("hashes.db");
    let (photos_s, db_s) = (photos.to_str().unwrap(), db.to_str().unwrap());

    run(&["scan", photos_s, "--db", db_s, "--silent"]);
    assert!(
        run(&["search", "--tag", "holiday", "--db", db_s]).contains("IMG.jpg"),
        "dc:subject keywords in the sidecar must import as tags"
    );
    assert!(run(&["search", "--tag", "beach", "--db", db_s]).contains("IMG.jpg"));
}

#[test]
fn export_writes_tags_as_dc_subject_keywords() {
    let dir = tempdir().unwrap();
    let photos = dir.path().join("photos");
    std::fs::create_dir(&photos).unwrap();
    let img = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tiny.jpg");
    std::fs::copy(&img, photos.join("IMG.jpg")).unwrap();
    let db = dir.path().join("hashes.db");
    let (photos_s, db_s) = (photos.to_str().unwrap(), db.to_str().unwrap());

    run(&["scan", photos_s, "--db", db_s, "--silent"]);
    run(&[
        "tag", "--add", "sunset", "--path", photos_s, "--db", db_s, "--silent",
    ]);
    run(&[
        "export", "--xmp", "--path", photos_s, "--db", db_s, "--silent",
    ]);

    let sidecar = photos.join("IMG.jpg.xmp");
    let doc = std::fs::read_to_string(sidecar).unwrap();
    assert!(
        doc.contains("<rdf:li>sunset</rdf:li>"),
        "the tag should be written as a dc:subject keyword; got: {doc}"
    );
    assert!(doc.contains("dc:subject"));
}
