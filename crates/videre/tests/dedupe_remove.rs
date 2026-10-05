//! `videre dedupe --remove`: videre moves duplicate copies to the system trash
//! itself, safely, so no shell pipeline word-splits a path with a space.

mod common;
use common::TestLibrary;
use std::path::PathBuf;

/// A library with one exact-duplicate pair under a folder whose name has a
/// space (like a Google Takeout export). Both copies are byte-identical, so they
/// form one duplicate group; dedupe keeps one and lists the other as removable.
fn lib_with_spaced_duplicate() -> (TestLibrary, PathBuf, PathBuf) {
    let lib = TestLibrary::new();
    let a = lib.copy_fixture("tiny.jpg", "Google Photos/keep.jpg");
    let b = lib.copy_fixture("tiny.jpg", "Google Photos/copy 1.jpg");
    lib.scan();
    (lib, a, b)
}

fn remaining(paths: &[&PathBuf]) -> usize {
    paths.iter().filter(|p| p.exists()).count()
}

#[test]
fn print0_lists_losers_nul_delimited_not_newline() {
    let (lib, _a, _b) = lib_with_spaced_duplicate();
    let out = lib.cmd().args(["dedupe", "--print0"]).output().unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // The loser path is NUL-terminated, never newline-terminated: that is the
    // whole point of --print0 (a `| xargs -0` consumer is then safe for the
    // space in "Google Photos/").
    assert!(
        out.stdout.contains(&0u8),
        "stdout must contain a NUL delimiter"
    );
    assert!(
        !out.stdout.contains(&b'\n'),
        "--print0 output must not contain a newline"
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("Google Photos/"),
        "the space-containing loser path must be present intact: {text:?}"
    );
}

#[test]
fn remove_dry_run_lists_and_deletes_nothing() {
    let (lib, a, b) = lib_with_spaced_duplicate();
    let out = lib
        .cmd()
        .args(["dedupe", "--trash", "--dry-run"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Both copies still on disk; a loser path was printed.
    assert_eq!(remaining(&[&a, &b]), 2, "dry-run must delete nothing");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("copy 1.jpg") || stdout.contains("keep.jpg"));
}

#[test]
fn remove_yes_trashes_one_copy_and_keeps_the_other() {
    let (lib, a, b) = lib_with_spaced_duplicate();
    let out = lib
        .cmd()
        .args(["dedupe", "--trash", "--yes"])
        .output()
        .unwrap();
    if out.status.success() {
        // Exactly one of the pair survives; the other went to the trash. The
        // space in the path was handled correctly (a broken pipe would have
        // touched neither or the wrong file).
        assert_eq!(
            remaining(&[&a, &b]),
            1,
            "exactly one copy must remain after --remove"
        );
    } else {
        // The platform could not trash in this environment (no XDG/Finder
        // trash): the command reports it rather than deleting. Mirror the
        // trash_paths wrapper's skip. Nothing must have been removed.
        assert_eq!(remaining(&[&a, &b]), 2);
    }
}

#[test]
fn remove_yes_also_prunes_the_loser_row() {
    // Trashing a duplicate leaves its database row behind: the row describes
    // a file that no longer exists. `dedupe --remove` must run the same
    // cleanup `videre prune` would, so the library never shows a ghost.
    let (lib, a, b) = lib_with_spaced_duplicate();
    let out = lib
        .cmd()
        .args(["dedupe", "--trash", "--yes", "--silent"])
        .output()
        .unwrap();
    if !out.status.success() {
        // No-trash environment (see the test above): nothing was removed, so
        // both rows legitimately remain.
        assert_eq!(remaining(&[&a, &b]), 2);
        return;
    }
    assert_eq!(remaining(&[&a, &b]), 1);
    let rows: i64 = lib
        .conn()
        .query_row("SELECT COUNT(*) FROM file_hashes", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        rows, 1,
        "the removed copy's row must be pruned automatically after --remove"
    );
}

#[test]
fn remove_takes_the_copys_sidecar_and_leaves_the_kept_ones() {
    let (lib, a, b) = lib_with_spaced_duplicate();
    let sidecar = |p: &PathBuf| PathBuf::from(format!("{}.xmp", p.display()));
    for p in [&a, &b] {
        std::fs::write(sidecar(p), "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"/>").unwrap();
    }

    let dry = lib
        .cmd()
        .args(["dedupe", "--trash", "--dry-run"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&dry.stderr);
    assert!(
        stderr.contains("1 XMP sidecar(s) would go with them"),
        "{stderr}"
    );
    assert!(
        !String::from_utf8_lossy(&dry.stdout).contains(".xmp"),
        "stdout stays the media paths alone"
    );

    let out = lib
        .cmd()
        .args(["dedupe", "--trash", "--yes", "--silent"])
        .output()
        .unwrap();
    if !out.status.success() {
        // No-trash environment (see above): nothing moved.
        assert_eq!(remaining(&[&sidecar(&a), &sidecar(&b)]), 2);
        return;
    }
    let (kept, gone) = if a.exists() { (&a, &b) } else { (&b, &a) };
    assert!(!gone.exists());
    assert!(
        !sidecar(gone).exists(),
        "the removed copy's sidecar was left behind"
    );
    assert!(sidecar(kept).exists(), "the kept copy's sidecar must stay");
}

#[test]
fn remove_rejects_similar_json_and_html() {
    let (lib, _a, _b) = lib_with_spaced_duplicate();
    let combos: [&[&str]; 3] = [
        &["--trash", "--similar"],
        &["--trash", "--json"],
        &["--trash", "--html"],
    ];
    for extra in combos {
        let mut args = vec!["dedupe"];
        args.extend_from_slice(extra);
        let out = lib.cmd().args(&args).output().unwrap();
        assert!(
            !out.status.success(),
            "{extra:?} should be rejected: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// A Google Takeout pair: `IMG_1.jpg` and Google Photos' `IMG_1-edited.jpg`
/// beside it. Different bytes, so no exact-duplicate group.
fn lib_with_edited_pair() -> (TestLibrary, PathBuf, PathBuf) {
    let lib = TestLibrary::new();
    let original = lib.copy_fixture("tiny.jpg", "Photos from 2015/IMG_1.jpg");
    let edit = lib.copy_fixture("sample_with_exif.jpg", "Photos from 2015/IMG_1-edited.jpg");
    lib.scan();
    (lib, original, edit)
}

fn dedupe(lib: &TestLibrary, args: &[&str]) -> std::process::Output {
    let out = lib.cmd().arg("dedupe").args(args).output().unwrap();
    assert!(
        out.status.success(),
        "dedupe {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

#[test]
fn edited_pairs_are_listed_only_with_edited() {
    let (lib, _original, _edit) = lib_with_edited_pair();
    let out = dedupe(&lib, &[]);
    assert!(String::from_utf8_lossy(&out.stdout).trim().is_empty());

    let out = dedupe(&lib, &["--edited"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("IMG_1-edited.jpg"), "{stdout}");
    assert!(
        !stdout.contains("IMG_1.jpg\n"),
        "the original is kept: {stdout}"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("1 edited pair(s)"), "{stderr}");
}

#[test]
fn an_edit_in_another_folder_is_not_paired() {
    let lib = TestLibrary::new();
    lib.copy_fixture("tiny.jpg", "Photos from 2015/IMG_1.jpg");
    lib.copy_fixture("sample_with_exif.jpg", "Album/IMG_1-edited.jpg");
    lib.scan();
    let out = dedupe(&lib, &["--edited"]);
    assert!(String::from_utf8_lossy(&out.stdout).trim().is_empty());
}

#[test]
fn edited_remove_trashes_the_edit_and_keeps_the_original() {
    let (lib, original, edit) = lib_with_edited_pair();
    let out = lib
        .cmd()
        .args(["dedupe", "--edited", "--trash", "--yes"])
        .output()
        .unwrap();
    if !out.status.success() {
        // No trash in this environment (see the tests above).
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("trash"), "{stderr}");
        assert_eq!(remaining(&[&original, &edit]), 2);
        return;
    }
    assert!(original.exists(), "the original is kept");
    assert!(!edit.exists(), "the edit goes to the trash");
    let rows: Vec<String> = lib
        .conn()
        .prepare("SELECT path FROM file_hashes")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(rows.len(), 1, "prune dropped the edit's row: {rows:?}");
    assert!(rows[0].ends_with("IMG_1.jpg"), "{rows:?}");
}

#[test]
fn an_edit_that_is_also_an_exact_duplicate_is_removed_once() {
    let lib = TestLibrary::new();
    let original = lib.copy_fixture("tiny.jpg", "Photos from 2015/IMG_1.jpg");
    let edit = lib.copy_fixture("sample_with_exif.jpg", "Photos from 2015/IMG_1-edited.jpg");
    // An album copy of the edit sorts first, so the edit itself is the
    // exact-duplicate loser too.
    let album = lib.copy_fixture("sample_with_exif.jpg", "Album/IMG_1-edited.jpg");
    lib.scan();
    let out = dedupe(&lib, &["--edited", "--trash", "--dry-run"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let listed = stdout.lines().filter(|l| !l.trim().is_empty()).count();
    let unique: std::collections::HashSet<&str> =
        stdout.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(listed, unique.len(), "a path is listed once: {stdout}");
    assert!(
        stdout.contains("Photos from 2015/IMG_1-edited.jpg"),
        "{stdout}"
    );
    assert_eq!(remaining(&[&original, &edit, &album]), 3, "dry run");
}

#[test]
fn edited_json_carries_the_pairs() {
    let (lib, _original, _edit) = lib_with_edited_pair();
    let out = dedupe(&lib, &["--json"]);
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(doc.get("edited_pairs").is_none(), "{doc}");

    let out = dedupe(&lib, &["--json", "--edited"]);
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let pairs = doc["edited_pairs"].as_array().expect("edited_pairs");
    assert_eq!(pairs.len(), 1, "{doc}");
    assert!(pairs[0]["kept"].as_str().unwrap().ends_with("IMG_1.jpg"));
    assert!(pairs[0]["removed"]
        .as_str()
        .unwrap()
        .ends_with("IMG_1-edited.jpg"));
}

/// Every file is half of a pair, so the edits are half the library: far past
/// the bulk-deletion guard's share, and still removed without --force.
#[test]
fn edited_pairs_do_not_trip_the_bulk_guard() {
    let lib = TestLibrary::new();
    let dir = lib.context().paths.root.join("Photos");
    std::fs::create_dir_all(&dir).unwrap();
    // Distinct bytes per file, so no exact-duplicate group forms: only the
    // pairs are removable.
    for i in 0..110 {
        std::fs::write(dir.join(format!("IMG_{i}.jpg")), format!("original {i}")).unwrap();
        std::fs::write(dir.join(format!("IMG_{i}-edited.jpg")), format!("edit {i}")).unwrap();
    }
    lib.scan();
    let out = dedupe(&lib, &["--edited", "--trash", "--dry-run"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("refusing"), "{stderr}");
    assert!(
        stderr.contains("110 Google Photos edit(s) would be moved to the trash"),
        "{stderr}"
    );
}

/// The original went missing after the last scan: its row still pairs with
/// the edit, but the edit is now the only file on disk and must stay.
#[test]
fn an_edit_whose_original_is_gone_from_disk_is_kept() {
    let (lib, original, edit) = lib_with_edited_pair();
    std::fs::remove_file(&original).unwrap();
    let out = dedupe(&lib, &["--edited", "--trash", "--dry-run"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!stdout.contains("IMG_1-edited.jpg"), "{stdout}");

    let out = lib
        .cmd()
        .args(["dedupe", "--edited", "--trash", "--yes"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(edit.exists(), "the only remaining file was removed");
}

/// `A.jpg` has the same content as `B-edited.jpg`, and the edit is the older
/// copy, so exact dedupe alone would keep the edit and remove `A.jpg`. The
/// edit goes as an edit, so `A.jpg` must be the copy that survives.
#[test]
fn an_edit_kept_by_exact_dedupe_does_not_take_its_copy_with_it() {
    let lib = TestLibrary::new();
    lib.copy_fixture("tiny.jpg", "Photos/B.jpg");
    let edit = lib.copy_fixture("sample_with_exif.jpg", "Photos/B-edited.jpg");
    let copy = lib.copy_fixture("sample_with_exif.jpg", "Album/A.jpg");
    filetime::set_file_mtime(&edit, filetime::FileTime::from_unix_time(1_000_000_000, 0)).unwrap();
    filetime::set_file_mtime(&copy, filetime::FileTime::from_unix_time(1_600_000_000, 0)).unwrap();
    lib.scan();
    // Exact dedupe on its own keeps the older edit.
    let out = dedupe(&lib, &[]);
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("Album/A.jpg"),
        "fixture: the edit must be the exact group's keeper"
    );

    let out = dedupe(&lib, &["--edited", "--trash", "--dry-run"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Photos/B-edited.jpg"), "{stdout}");
    assert!(
        !stdout.contains("Album/A.jpg"),
        "a copy must survive: {stdout}"
    );
}

#[test]
fn the_old_remove_flag_is_gone_and_trash_and_delete_exclude_each_other() {
    let (lib, a, b) = lib_with_spaced_duplicate();
    let out = lib
        .cmd()
        .args(["dedupe", "--remove", "--yes"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let out = lib
        .cmd()
        .args(["dedupe", "--trash", "--delete", "--yes"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert_eq!(remaining(&[&a, &b]), 2, "nothing removed by a refused run");
}

#[test]
fn delete_removes_the_copy_and_its_sidecar_permanently() {
    let (lib, a, b) = lib_with_spaced_duplicate();
    for p in [&a, &b] {
        std::fs::write(format!("{}.xmp", p.display()), "<x:xmpmeta/>").unwrap();
    }
    let dry = dedupe(&lib, &["--delete", "--dry-run"]);
    let stderr = String::from_utf8_lossy(&dry.stderr);
    assert!(stderr.contains("would be permanently deleted"), "{stderr}");
    assert_eq!(remaining(&[&a, &b]), 2, "dry run");

    let out = dedupe(&lib, &["--delete", "--yes"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("Deleted 1 file(s)"), "{stderr}");
    assert_eq!(remaining(&[&a, &b]), 1);
    let gone = if a.exists() { &b } else { &a };
    assert!(!PathBuf::from(format!("{}.xmp", gone.display())).exists());
    let rows: i64 = lib
        .conn()
        .query_row("SELECT COUNT(*) FROM file_hashes", [], |r| r.get(0))
        .unwrap();
    assert_eq!(rows, 1);
}

/// An earlier run removed the edit but was stopped before the library was
/// updated: the row is still there, the file is not. Not a failure.
#[test]
fn a_file_an_earlier_run_removed_is_already_gone_not_a_failure() {
    let (lib, original, edit) = lib_with_edited_pair();
    std::fs::remove_file(&edit).unwrap();
    for method in ["--trash", "--delete"] {
        let out = lib
            .cmd()
            .args(["dedupe", "--edited", method, "--yes"])
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{method}: {stderr}");
        assert!(!stderr.contains("could not"), "{method}: {stderr}");
        if method == "--trash" {
            assert!(stderr.contains("1 already gone"), "{stderr}");
        }
    }
    assert!(original.exists());
    let rows: Vec<String> = lib
        .conn()
        .prepare("SELECT path FROM file_hashes")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(rows.len(), 1, "the gone edit's row is forgotten: {rows:?}");
}
