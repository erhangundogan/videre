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
