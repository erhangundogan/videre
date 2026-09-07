mod common;
use common::TestLibrary;
use std::io::Write;
use std::path::PathBuf;
use std::process::Stdio;

/// A library with one on-disk file whose row carries a known exif_date and no
/// created_at/modified_at. The file path sits under the canonical root so the
/// row-containment guard accepts the database. Returns the file path.
fn fixture_library() -> (TestLibrary, PathBuf) {
    let lib = TestLibrary::new();
    let file = lib.context().paths.root.join("a.jpg");
    std::fs::write(&file, b"img_a").unwrap();
    lib.init_db()
        .execute(
            "INSERT INTO file_hashes (path, hash, exif_date)
             VALUES (?1, 'haaa', '2019-06-15T10:00:00')",
            [file.to_string_lossy().as_ref()],
        )
        .unwrap();
    (lib, file)
}

fn mtime_year(path: &std::path::Path) -> i32 {
    use chrono::{Datelike, Local, TimeZone};
    let modified = std::fs::metadata(path).unwrap().modified().unwrap();
    let secs = modified
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    Local.timestamp_opt(secs, 0).unwrap().year()
}

fn run_fix_dates(
    lib: &TestLibrary,
    extra_args: &[&str],
    stdin_input: Option<&str>,
) -> std::process::Output {
    let mut cmd = lib.cmd();
    cmd.arg("fix-dates").args(extra_args);
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("failed to run videre fix-dates");
    if let Some(input) = stdin_input {
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
    }
    child
        .wait_with_output()
        .expect("failed to wait on videre fix-dates")
}

#[test]
fn declining_the_prompt_leaves_the_file_unmodified() {
    let (lib, file) = fixture_library();
    let before = mtime_year(&file);

    let out = run_fix_dates(&lib, &[], Some("n\n"));
    assert!(out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("Aborted"), "{stderr}");
    assert_eq!(mtime_year(&file), before, "declining must not touch mtime");
}

#[test]
fn accepting_the_prompt_updates_the_file() {
    let (lib, file) = fixture_library();
    let out = run_fix_dates(&lib, &[], Some("y\n"));
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        mtime_year(&file),
        2019,
        "accepting should set mtime from exif_date"
    );
}

#[test]
fn yes_flag_skips_the_prompt_entirely() {
    let (lib, file) = fixture_library();
    // No stdin provided at all: if the prompt were shown this would hang.
    let out = run_fix_dates(&lib, &["--yes"], None);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        mtime_year(&file),
        2019,
        "--yes should proceed without a prompt"
    );
}

#[test]
fn dry_run_never_prompts() {
    let (lib, file) = fixture_library();
    let before = mtime_year(&file);

    let out = run_fix_dates(&lib, &["--dry-run"], None);
    assert!(out.status.success());
    assert_eq!(
        mtime_year(&file),
        before,
        "dry-run must not modify the file"
    );
}

#[test]
fn eof_on_stdin_is_treated_as_no() {
    let (lib, file) = fixture_library();
    let before = mtime_year(&file);

    // Empty stdin (immediate EOF) must be treated as declining, not a hang.
    let out = run_fix_dates(&lib, &[], Some(""));
    assert!(out.status.success());
    assert_eq!(
        mtime_year(&file),
        before,
        "EOF on stdin must be treated as 'no'"
    );
}
