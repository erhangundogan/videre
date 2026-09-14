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
        .args(["dedupe", "--remove", "--dry-run"])
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
        .args(["dedupe", "--remove", "--yes"])
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
fn remove_rejects_similar_json_and_html() {
    let (lib, _a, _b) = lib_with_spaced_duplicate();
    let combos: [&[&str]; 3] = [
        &["--remove", "--similar"],
        &["--remove", "--json"],
        &["--remove", "--html"],
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
