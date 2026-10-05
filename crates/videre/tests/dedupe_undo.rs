//! `videre dedupe --undo`: every `--trash` run records what it moved, and
//! `--undo` puts the newest run back, one run per call.

mod common;
use common::TestLibrary;
use std::path::PathBuf;

/// A library holding `n` Takeout edit pairs (`<folder>_i.jpg` and
/// `<folder>_i-edited.jpg`, distinct content per pair) in `folder`. Names
/// differ per test: tests run in parallel against the one real trash.
fn lib_with_edits(lib: &TestLibrary, folder: &str, n: usize) -> Vec<PathBuf> {
    let mut edits = Vec::new();
    for i in 0..n {
        let original = lib.copy_fixture("tiny.jpg", &format!("{folder}/{folder}_{i}.jpg"));
        // Distinct bytes per pair, so no two pairs are exact duplicates.
        let mut bytes = std::fs::read(&original).unwrap();
        bytes.extend_from_slice(format!("{folder}{i}").as_bytes());
        std::fs::write(&original, &bytes).unwrap();
        let edit = original.with_file_name(format!("{folder}_{i}-edited.jpg"));
        bytes.extend_from_slice(b"edited");
        std::fs::write(&edit, &bytes).unwrap();
        edits.push(edit);
    }
    edits
}

fn trash_dir(lib: &TestLibrary) -> PathBuf {
    lib.context().paths.state.join("trash")
}

fn manifests(lib: &TestLibrary) -> Vec<PathBuf> {
    match std::fs::read_dir(trash_dir(lib)) {
        Ok(r) => r.map(|e| e.unwrap().path()).collect(),
        Err(_) => Vec::new(),
    }
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
fn a_trash_run_writes_one_manifest_and_delete_writes_none() {
    let lib = TestLibrary::new();
    let edits = lib_with_edits(&lib, "Fotoğraflar", 2);
    lib.scan();
    dedupe(&lib, &["--edited", "--delete", "--yes"]);
    assert!(edits.iter().all(|e| !e.exists()));
    assert!(manifests(&lib).is_empty(), "--delete cannot be undone");

    let edits = lib_with_edits(&lib, "Çiçekler_yaz", 2);
    lib.scan();
    let out = lib
        .cmd()
        .args(["dedupe", "--edited", "--trash", "--yes"])
        .output()
        .unwrap();
    assert!(
        out.status.success() && edits.iter().all(|e| !e.exists()),
        "--trash failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let runs = manifests(&lib);
    assert_eq!(runs.len(), 1, "{runs:?}");
    let text = std::fs::read_to_string(&runs[0]).unwrap();
    assert_eq!(text.lines().count(), 2, "{text}");
    for e in &edits {
        assert!(
            text.contains(&*e.file_name().unwrap().to_string_lossy()),
            "{text}"
        );
    }
    // Leave nothing in the trash.
    dedupe(&lib, &["--undo", "--yes", "--silent"]);
}

/// Trash the edits of a fresh set of pairs under `folder`. No skip when the
/// trash does not work: every test library has its own home, so the
/// freedesktop.org home trash is always there on Linux, and macOS always has
/// a Trash. A skip here would let CI pass without testing anything.
fn trash_edits(lib: &TestLibrary, folder: &str, n: usize) -> Vec<PathBuf> {
    let edits = lib_with_edits(lib, folder, n);
    lib.scan();
    let out = lib
        .cmd()
        .args(["dedupe", "--edited", "--trash", "--yes", "--silent"])
        .output()
        .unwrap();
    assert!(
        out.status.success() && edits.iter().all(|e| !e.exists()),
        "--trash failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    edits
}

fn rows(lib: &TestLibrary) -> i64 {
    lib.conn()
        .query_row("SELECT COUNT(*) FROM file_hashes", [], |r| r.get(0))
        .unwrap()
}

#[test]
fn undo_cannot_be_combined_with_trash_delete_or_a_selection() {
    let lib = TestLibrary::new();
    for flag in ["--trash", "--delete", "--edited"] {
        let out = lib.cmd().args(["dedupe", "--undo", flag]).output().unwrap();
        assert_eq!(out.status.code(), Some(2), "{flag}");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("cannot be used with"),
            "{flag}"
        );
    }
}

#[test]
fn undo_with_nothing_recorded_says_so_and_succeeds() {
    let lib = TestLibrary::new();
    lib_with_edits(&lib, "Boş", 1);
    lib.scan();
    let out = dedupe(&lib, &["--undo", "--yes"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("Nothing to undo"), "{stderr}");
}

#[test]
fn undo_dry_run_lists_and_changes_nothing() {
    let lib = TestLibrary::new();
    let edits = trash_edits(&lib, "Deneme", 2);
    let out = dedupe(&lib, &["--undo", "--dry-run"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    for e in &edits {
        assert!(!e.exists(), "a dry run restores nothing");
        assert!(stdout.contains(&*e.to_string_lossy()), "{stdout}");
    }
    assert_eq!(manifests(&lib).len(), 1, "still undoable");
    dedupe(&lib, &["--undo", "--yes", "--silent"]);
}

#[test]
fn undo_restores_the_newest_run_first_and_back_into_the_library() {
    let lib = TestLibrary::new();
    let first = trash_edits(&lib, "Tatil_ilk", 2);
    let second = trash_edits(&lib, "Tatil_son", 1);
    assert_eq!(rows(&lib), 3, "three originals left");

    let out = dedupe(&lib, &["--undo", "--yes"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(second.iter().all(|e| e.exists()), "{stderr}");
    assert!(first.iter().all(|e| !e.exists()), "{stderr}");
    assert!(stderr.contains("Restored 1 file(s)"), "{stderr}");
    assert!(stderr.contains("1 earlier run(s)"), "{stderr}");
    assert_eq!(rows(&lib), 4, "the restored edit is indexed again");

    let out = dedupe(&lib, &["--undo", "--yes"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(first.iter().all(|e| e.exists()), "{stderr}");
    assert_eq!(rows(&lib), 6);
    assert!(manifests(&lib).is_empty());

    let status: serde_json::Value = serde_json::from_slice(
        &lib.cmd()
            .args(["status", "--json"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    let embed = status["report"]["coverage"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["stage"] == "embed")
        .unwrap()
        .clone();
    assert_eq!(embed["outstanding"], 6, "restored files wait for embed");
}

#[test]
fn undo_json_reports_each_outcome_and_never_overwrites() {
    let lib = TestLibrary::new();
    let edits = trash_edits(&lib, "Doğum_günü", 2);
    // One original path is taken again: that file is skipped, never replaced.
    std::fs::write(&edits[0], b"yeni").unwrap();
    let out = dedupe(&lib, &["--undo", "--yes", "--json"]);
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(doc["restored"].as_array().unwrap().len(), 1, "{doc}");
    // Compared canonically: the library records `/private/var`, the test
    // built `/var`.
    let occupied = PathBuf::from(doc["occupied"][0].as_str().unwrap());
    assert_eq!(
        occupied.canonicalize().unwrap(),
        edits[0].canonicalize().unwrap(),
        "{doc}"
    );
    assert_eq!(doc["runs_left"], 1, "the skipped file keeps the run");
    assert_eq!(std::fs::read(&edits[0]).unwrap(), b"yeni");

    // Clear the way and finish, so nothing is left in the trash.
    std::fs::remove_file(&edits[0]).unwrap();
    dedupe(&lib, &["--undo", "--yes", "--silent"]);
    assert!(edits.iter().all(|e| e.exists()));
}

/// Two copies with one name, from different folders, in one run: the trash
/// renames the second (macOS `name 10-05-15-123.jpg`), and both come back
/// to their own folders with their own bytes.
#[test]
fn same_named_copies_from_different_folders_both_come_back() {
    let lib = TestLibrary::new();
    let mut edits = Vec::new();
    for (i, folder) in ["Yaz 2019", "Kış 2020"].iter().enumerate() {
        let original = lib.copy_fixture("tiny.jpg", &format!("{folder}/IMG_aynı_ad.jpg"));
        let mut bytes = std::fs::read(&original).unwrap();
        bytes.extend_from_slice(format!("çift {i}").as_bytes());
        std::fs::write(&original, &bytes).unwrap();
        let edit = original.with_file_name("IMG_aynı_ad-edited.jpg");
        bytes.extend_from_slice(b"edited");
        std::fs::write(&edit, &bytes).unwrap();
        edits.push((edit, bytes));
    }
    lib.scan();
    let out = lib
        .cmd()
        .args(["dedupe", "--edited", "--trash", "--yes", "--silent"])
        .output()
        .unwrap();
    assert!(
        out.status.success() && edits.iter().all(|(e, _)| !e.exists()),
        "--trash failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    dedupe(&lib, &["--undo", "--yes", "--silent"]);
    for (edit, bytes) in &edits {
        assert_eq!(&std::fs::read(edit).unwrap(), bytes, "{edit:?}");
    }
}

/// An edit's XMP sidecar goes to the trash with it and comes back with it,
/// through the platform's real trash.
#[test]
fn a_sidecar_goes_and_comes_back_with_its_edit() {
    let lib = TestLibrary::new();
    let edits = lib_with_edits(&lib, "Yan_dosya", 1);
    let sidecar = PathBuf::from(format!("{}.xmp", edits[0].display()));
    std::fs::write(&sidecar, "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"/>").unwrap();
    lib.scan();
    let out = lib
        .cmd()
        .args(["dedupe", "--edited", "--trash", "--yes", "--silent"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!edits[0].exists() && !sidecar.exists());

    dedupe(&lib, &["--undo", "--yes", "--silent"]);
    assert!(edits[0].exists(), "the edit is back");
    assert_eq!(
        std::fs::read_to_string(&sidecar).unwrap(),
        "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"/>",
        "and its sidecar"
    );
}
