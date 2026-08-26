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
