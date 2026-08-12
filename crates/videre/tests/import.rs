mod common;
use common::videre_bin;
use std::path::{Path, PathBuf};

/// 1 January 2019, 12:00 UTC, as Takeout writes it: Unix seconds in a string.
const TAKEN: i64 = 1_546_344_000;

/// A minimal Takeout export: one photo whose mtime is the export date (wrong)
/// and whose real capture time is only in the sidecar.
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

fn mtime_seconds(p: &Path) -> i64 {
    filetime::FileTime::from_last_modification_time(&std::fs::metadata(p).unwrap()).unix_seconds()
}

#[test]
fn import_with_no_recognisable_library_explains_and_suggests_scan() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("a.jpg"), b"x").unwrap();
    std::fs::write(d.path().join("b.jpg"), b"x").unwrap();

    let out = std::process::Command::new(videre_bin())
        .args(["import", d.path().to_str().unwrap()])
        .output()
        .unwrap();
    let text =
        String::from_utf8_lossy(&out.stderr).to_string() + &String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("videre scan"), "must point at scan: {text}");
}

#[test]
fn import_detects_an_apple_library_and_names_it() {
    let d = tempfile::tempdir().unwrap();
    let lib = d.path().join("Photos Library.photoslibrary");
    std::fs::create_dir_all(lib.join("originals/0")).unwrap();
    std::fs::create_dir_all(lib.join("database")).unwrap();
    std::fs::write(lib.join("database/Photos.sqlite"), b"").unwrap();
    std::fs::write(lib.join("originals/0/a.jpg"), b"x").unwrap();

    let out = std::process::Command::new(videre_bin())
        .args(["import", lib.to_str().unwrap(), "--dry-run"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stderr).to_string();
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
    let d = tempfile::tempdir().unwrap();
    let lib = d.path().join("L.photoslibrary");
    std::fs::create_dir_all(lib.join("originals/0")).unwrap();
    std::fs::create_dir_all(lib.join("database")).unwrap();
    std::fs::write(lib.join("database/Photos.sqlite"), b"").unwrap();
    let f = lib.join("originals/0/a.jpg");
    std::fs::write(&f, b"x").unwrap();
    let before = std::fs::metadata(&f).unwrap().modified().unwrap();

    std::process::Command::new(videre_bin())
        .args(["import", lib.to_str().unwrap(), "--dry-run", "--yes"])
        .output()
        .unwrap();

    assert_eq!(before, std::fs::metadata(&f).unwrap().modified().unwrap());
}

/// A modern Apple package: `originals/` plus the `database/Photos.sqlite`
/// sibling that makes detection structural rather than name-based.
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

/// Runs `videre import` with `answer` on stdin, returning stderr + stdout.
fn import_answering(args: &[&str], answer: &str) -> String {
    use std::io::Write;
    let mut child = std::process::Command::new(videre_bin())
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
fn a_library_of_tiny_files_warns_that_it_looks_optimised() {
    let d = tempfile::tempdir().unwrap();
    // 20 files of 1 KB: far below what camera originals are, and enough of
    // them that the near-empty referenced check does not fire instead.
    let lib = apple_library(d.path(), "Optimised.photoslibrary", 1024, 20);

    let out = std::process::Command::new(videre_bin())
        .args(["import", lib.to_str().unwrap(), "--dry-run"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stderr).to_string();

    assert!(
        text.contains("median file size"),
        "must warn about the population, not a file: {text}"
    );
    assert!(
        text.contains("Optimise Mac Storage"),
        "must name the setting that causes it: {text}"
    );
}

#[test]
fn a_library_of_normal_sized_files_does_not_warn() {
    let d = tempfile::tempdir().unwrap();
    let lib = apple_library(d.path(), "Normal.photoslibrary", 400 * 1024, 8);

    let out = std::process::Command::new(videre_bin())
        .args(["import", lib.to_str().unwrap(), "--dry-run"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stderr).to_string();

    assert!(
        !text.contains("median file size"),
        "no false positive on an ordinary library: {text}"
    );
}

#[test]
fn a_nearly_empty_originals_folder_warns_about_a_referenced_library() {
    let d = tempfile::tempdir().unwrap();
    let lib = apple_library(d.path(), "Referenced.photoslibrary", 16, 2);
    // The package is otherwise substantial: previews and databases are there,
    // the originals are not, which is exactly what a referenced library is.
    std::fs::create_dir_all(lib.join("resources/derivatives")).unwrap();
    std::fs::write(
        lib.join("resources/derivatives/previews.blob"),
        vec![b'x'; 400_000],
    )
    .unwrap();

    let out = std::process::Command::new(videre_bin())
        .args(["import", lib.to_str().unwrap(), "--dry-run"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stderr).to_string();

    assert!(
        text.contains("referenced library"),
        "must explain the empty originals folder: {text}"
    );
}

#[test]
fn the_preflight_checklist_prints_and_anything_but_y_aborts() {
    let d = tempfile::tempdir().unwrap();
    let lib = apple_library(d.path(), "Ask.photoslibrary", 400 * 1024, 4);

    let text = import_answering(&["import", lib.to_str().unwrap()], "n\n");

    assert!(
        text.contains("Download Originals to this Mac"),
        "the checklist must print: {text}"
    );
    assert!(text.contains("Aborted"), "anything but y aborts: {text}");
}

#[test]
fn yes_skips_the_prompt_but_still_prints_the_checklist() {
    let d = tempfile::tempdir().unwrap();
    let lib = apple_library(d.path(), "Yes.photoslibrary", 400 * 1024, 4);

    let out = std::process::Command::new(videre_bin())
        .args(["import", lib.to_str().unwrap(), "--yes"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stderr).to_string();

    assert!(
        text.contains("Download Originals to this Mac"),
        "the checklist must appear in logs even when nobody is asked: {text}"
    );
    assert!(!text.contains("Aborted"), "--yes must proceed: {text}");
}

#[test]
fn takeout_import_corrects_the_date_from_the_sidecar() {
    let d = tempfile::tempdir().unwrap();
    let photo = takeout_tree(d.path());
    assert_ne!(mtime_seconds(&photo), TAKEN, "fixture must start wrong");

    let out = std::process::Command::new(videre_bin())
        .args(["import", d.path().to_str().unwrap(), "--yes"])
        .output()
        .unwrap();

    assert_eq!(
        mtime_seconds(&photo),
        TAKEN,
        "must apply photoTakenTime, not creationTime: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn takeout_dry_run_leaves_every_timestamp_alone() {
    let d = tempfile::tempdir().unwrap();
    let photo = takeout_tree(d.path());
    let before = mtime_seconds(&photo);

    let out = std::process::Command::new(videre_bin())
        .args(["import", d.path().to_str().unwrap(), "--dry-run", "--yes"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stderr).to_string();

    assert_eq!(mtime_seconds(&photo), before, "dry run must write nothing");
    assert!(
        text.contains("1 matched a sidecar"),
        "must still report what it found: {text}"
    );
}
