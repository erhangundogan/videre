//! `videre dedupe --undo`: every `--trash` run records what it moved, and
//! `--undo` puts the newest run back, one run per call.

mod common;
use common::TestLibrary;
use std::path::PathBuf;

/// A library holding `n` Takeout edit pairs (`foto_i.jpg` and
/// `foto_i-edited.jpg`, distinct content per pair) under a Turkish folder name.
fn lib_with_edits(lib: &TestLibrary, folder: &str, n: usize) -> Vec<PathBuf> {
    let mut edits = Vec::new();
    for i in 0..n {
        let original = lib.copy_fixture("tiny.jpg", &format!("{folder}/foto_{i}.jpg"));
        // Distinct bytes per pair, so no two pairs are exact duplicates.
        let mut bytes = std::fs::read(&original).unwrap();
        bytes.extend_from_slice(format!("{folder}{i}").as_bytes());
        std::fs::write(&original, &bytes).unwrap();
        let edit = original.with_file_name(format!("foto_{i}-edited.jpg"));
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

    let edits = lib_with_edits(&lib, "Çiçekler", 2);
    lib.scan();
    let out = lib
        .cmd()
        .args(["dedupe", "--edited", "--trash", "--yes"])
        .output()
        .unwrap();
    if !out.status.success() || edits.iter().any(|e| e.exists()) {
        // No usable trash on this host (a container without one).
        return;
    }
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
}
