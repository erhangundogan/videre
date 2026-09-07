mod common;
use common::TestLibrary;
use std::io::Write;
use std::path::{Path, PathBuf};

/// 1 January 2019, 12:00 UTC, as Takeout writes it: Unix seconds in a string.
const TAKEN: i64 = 1_546_344_000;

/// A minimal Takeout export under `root`: one photo whose mtime is the export
/// date (wrong) and whose real capture time is only in the sidecar.
fn takeout_tree(root: &Path) -> PathBuf {
    let album = root.join("Google Photos/Album");
    std::fs::create_dir_all(&album).unwrap();
    let photo = album.join("a.jpg");
    std::fs::write(&photo, b"x").unwrap();
    std::fs::write(
        album.join("a.jpg.supplemental-metadata.json"),
        format!(
            r#"{{"title":"a.jpg",
                 "photoTakenTime":{{"timestamp":"{TAKEN}"}},
                 "creationTime":{{"timestamp":"1700000000"}}}}"#
        ),
    )
    .unwrap();
    filetime::set_file_mtime(&photo, filetime::FileTime::from_unix_time(1_700_000_000, 0)).unwrap();
    photo
}

/// A modern Apple package under `root`: `originals/` plus the
/// `database/Photos.sqlite` sibling that makes detection structural.
fn apple_library(root: &Path, name: &str, file_bytes: usize, count: usize) -> PathBuf {
    let lib = root.join(name);
    std::fs::create_dir_all(lib.join("originals/0")).unwrap();
    std::fs::create_dir_all(lib.join("database")).unwrap();
    std::fs::write(lib.join("database/Photos.sqlite"), b"").unwrap();
    for i in 0..count {
        std::fs::write(
            lib.join(format!("originals/0/{i}.jpg")),
            vec![b'x'; file_bytes],
        )
        .unwrap();
    }
    lib
}

fn mtime_seconds(p: &Path) -> i64 {
    filetime::FileTime::from_last_modification_time(&std::fs::metadata(p).unwrap()).unix_seconds()
}

/// Runs `videre import <args>` inside this library, returning stderr + stdout.
fn run(lib: &TestLibrary, args: &[&str]) -> String {
    let out = lib.cmd().arg("import").args(args).output().unwrap();
    String::from_utf8_lossy(&out.stderr).to_string() + &String::from_utf8_lossy(&out.stdout)
}

/// Runs `videre import <args>` with `answer` on stdin.
fn import_answering(lib: &TestLibrary, args: &[&str], answer: &str) -> String {
    let mut child = lib
        .cmd()
        .arg("import")
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(answer.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    String::from_utf8_lossy(&out.stderr).to_string() + &String::from_utf8_lossy(&out.stdout)
}

#[test]
fn external_originals_are_rejected_before_import_writes() {
    // An Apple package inside the library, but --originals pointing outside it:
    // the located write targets are external, so the whole run is rejected
    // before any preflight, confirmation or mtime change.
    let lib = TestLibrary::new();
    apple_library(&lib.root, "Photos Library.photoslibrary", 400 * 1024, 2);
    let external = TestLibrary::new();
    let photo = external.copy_fixture("tiny.jpg", "outside.jpg");
    let before = std::fs::metadata(&photo).unwrap().modified().unwrap();

    let out = lib
        .cmd()
        .args(["import", "Photos Library.photoslibrary", "--originals"])
        .arg(&external.root)
        .arg("--yes")
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("outside library"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        std::fs::metadata(&photo).unwrap().modified().unwrap(),
        before
    );
    assert!(!lib.db().exists(), "import must not create a database");
}

#[test]
fn import_with_no_recognisable_library_explains_and_suggests_scan() {
    let lib = TestLibrary::new();
    std::fs::create_dir_all(lib.root.join("plain")).unwrap();
    std::fs::write(lib.root.join("plain/a.jpg"), b"x").unwrap();
    std::fs::write(lib.root.join("plain/b.jpg"), b"x").unwrap();

    let text = run(&lib, &["plain"]);
    assert!(text.contains("videre scan"), "must point at scan: {text}");
}

#[test]
fn import_detects_an_apple_library_and_names_it() {
    let lib = TestLibrary::new();
    apple_library(&lib.root, "Photos Library.photoslibrary", 16, 1);

    let text = run(&lib, &["Photos Library.photoslibrary", "--dry-run"]);
    assert!(
        text.contains("Apple Photos"),
        "must name the provider: {text}"
    );
    assert!(
        text.contains("originals"),
        "must report the rung used: {text}"
    );
}

#[test]
fn dry_run_changes_nothing_on_disk() {
    let lib = TestLibrary::new();
    apple_library(&lib.root, "L.photoslibrary", 16, 1);
    let f = lib.root.join("L.photoslibrary/originals/0/0.jpg");
    let before = std::fs::metadata(&f).unwrap().modified().unwrap();

    run(&lib, &["L.photoslibrary", "--dry-run", "--yes"]);
    assert_eq!(before, std::fs::metadata(&f).unwrap().modified().unwrap());
}

#[test]
fn a_library_of_tiny_files_warns_that_it_looks_optimised() {
    let lib = TestLibrary::new();
    apple_library(&lib.root, "Optimised.photoslibrary", 1024, 20);

    let text = run(&lib, &["Optimised.photoslibrary", "--dry-run"]);
    assert!(
        text.contains("median file size"),
        "must warn about the population: {text}"
    );
    assert!(
        text.contains("Optimise Mac Storage"),
        "must name the setting: {text}"
    );
}

#[test]
fn a_library_of_normal_sized_files_does_not_warn() {
    let lib = TestLibrary::new();
    apple_library(&lib.root, "Normal.photoslibrary", 400 * 1024, 8);

    let text = run(&lib, &["Normal.photoslibrary", "--dry-run"]);
    assert!(
        !text.contains("median file size"),
        "no false positive: {text}"
    );
}

#[test]
fn a_nearly_empty_originals_folder_warns_about_a_referenced_library() {
    let lib = TestLibrary::new();
    let pkg = apple_library(&lib.root, "Referenced.photoslibrary", 16, 2);
    std::fs::create_dir_all(pkg.join("resources/derivatives")).unwrap();
    std::fs::write(
        pkg.join("resources/derivatives/previews.blob"),
        vec![b'x'; 400_000],
    )
    .unwrap();

    let text = run(&lib, &["Referenced.photoslibrary", "--dry-run"]);
    assert!(
        text.contains("referenced library"),
        "must explain the empty originals: {text}"
    );
}

#[test]
fn the_preflight_checklist_prints_and_anything_but_y_aborts() {
    let lib = TestLibrary::new();
    apple_library(&lib.root, "Ask.photoslibrary", 400 * 1024, 4);

    let text = import_answering(&lib, &["Ask.photoslibrary"], "n\n");
    assert!(
        text.contains("Download Originals to this Mac"),
        "the checklist must print: {text}"
    );
    assert!(text.contains("Aborted"), "anything but y aborts: {text}");
}

#[test]
fn yes_skips_the_prompt_but_still_prints_the_checklist() {
    let lib = TestLibrary::new();
    apple_library(&lib.root, "Yes.photoslibrary", 400 * 1024, 4);

    let text = run(&lib, &["Yes.photoslibrary", "--yes"]);
    assert!(
        text.contains("Download Originals to this Mac"),
        "the checklist must appear even when nobody is asked: {text}"
    );
    assert!(!text.contains("Aborted"), "--yes must proceed: {text}");
}

#[test]
fn takeout_import_corrects_the_date_from_the_sidecar() {
    let lib = TestLibrary::new();
    let photo = takeout_tree(&lib.root);
    assert_ne!(mtime_seconds(&photo), TAKEN, "fixture must start wrong");

    let text = run(&lib, &[".", "--yes"]);
    assert_eq!(
        mtime_seconds(&photo),
        TAKEN,
        "must apply photoTakenTime, not creationTime: {text}"
    );
}

#[test]
fn takeout_dry_run_leaves_every_timestamp_alone() {
    let lib = TestLibrary::new();
    let photo = takeout_tree(&lib.root);
    let before = mtime_seconds(&photo);

    let text = run(&lib, &[".", "--dry-run", "--yes"]);
    assert_eq!(mtime_seconds(&photo), before, "dry run must write nothing");
    assert!(
        text.contains("1 matched a sidecar"),
        "must still report what it found: {text}"
    );
}
