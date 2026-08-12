mod common;
use common::videre_bin;

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
