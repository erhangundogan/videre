//! The workflows the documentation shows must actually work: selecting a
//! library with `--library` from another directory, two libraries staying
//! independent, and a reader refusing an uninitialized library. No models are
//! loaded (the fixture is a plain JPEG that no model-backed command processes
//! here), so these run everywhere the suite runs.

mod common;
use common::TestLibrary;

#[test]
fn documented_scan_stats_and_export_work_from_another_directory() {
    let a = TestLibrary::new();
    let c = TestLibrary::new();
    a.copy_fixture("tiny.jpg", "Trips/a.jpg");

    // Launched from c.root, but pinned to library a via --library.
    assert!(a
        .from(&c.root)
        .args(["scan", "--silent"])
        .status()
        .unwrap()
        .success());

    let output = a.from(&c.root).args(["stats", "--json"]).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let doc: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(doc["library"]["total_files"], 1);

    assert!(a
        .from(&c.root)
        .args(["export", "--jsonl", "--path", "Trips"])
        .status()
        .unwrap()
        .success());
    assert!(a.root.join(".videre/hashes.jsonl").is_file());

    // The launch directory never becomes a library.
    assert!(!c.db().exists());
    assert!(!c.root.join(".videre").exists());
}

#[test]
fn two_libraries_stay_independent() {
    let a = TestLibrary::new();
    let b = TestLibrary::new();
    a.copy_fixture("tiny.jpg", "a-only.jpg");
    b.copy_fixture("sample_with_exif.jpg", "b-only.jpg");
    a.scan();
    b.scan();

    let a_paths = a.cmd().args(["export", "--jsonl"]).status().unwrap();
    assert!(a_paths.success());
    let a_jsonl = std::fs::read_to_string(a.root.join(".videre/hashes.jsonl")).unwrap();
    assert!(a_jsonl.contains("a-only.jpg"));
    assert!(!a_jsonl.contains("b-only.jpg"));
}

#[test]
fn a_reader_refuses_an_uninitialized_library() {
    let lib = TestLibrary::new();
    let out = lib.cmd().args(["stats"]).output().unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("not initialized"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!lib.db().exists(), "a reader must not create the database");
}

#[test]
fn a_relative_path_filter_resolves_against_the_library_root() {
    let lib = TestLibrary::new();
    // Distinct files, so they have distinct hashes: selection resolves to a
    // hash set, and byte-identical copies would share one and both match.
    lib.copy_fixture("tiny.jpg", "Trips/a.jpg");
    lib.copy_fixture("sample_with_exif.jpg", "Other/b.jpg");
    lib.scan();

    // --path is a subtree of the library, given relative to its root.
    let out = lib
        .cmd()
        .args(["export", "--jsonl", "--path", "Trips"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let jsonl = std::fs::read_to_string(lib.root.join(".videre/hashes.jsonl")).unwrap();
    assert!(jsonl.contains("Trips/a.jpg"));
    assert!(
        !jsonl.contains("Other/b.jpg"),
        "the filter must exclude other subtrees"
    );
}
