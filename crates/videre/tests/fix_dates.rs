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

/// The rfc3339 value fix-dates writes for `exif_date`, as the command itself
/// computes it.
fn target(exif_date: &str) -> String {
    videre_core::fix_dates_target::target_modified_at(exif_date).unwrap()
}

/// A library of Turkish-named files where only `değişen.jpg` needs its date
/// fixed: `doğru.jpg` already carries its camera date, on disk and in its row.
fn one_right_one_wrong() -> (TestLibrary, PathBuf, PathBuf) {
    let lib = TestLibrary::new();
    let root = lib.context().paths.root.clone();
    let right = root.join("doğru.jpg");
    let wrong = root.join("değişen.jpg");
    std::fs::write(&right, b"img_right").unwrap();
    std::fs::write(&wrong, b"img_wrong").unwrap();
    let right_date = "2014-09-20T05:39:39";
    let stamp = chrono::DateTime::parse_from_rfc3339(&target(right_date)).unwrap();
    filetime::set_file_mtime(
        &right,
        filetime::FileTime::from_unix_time(stamp.timestamp(), 0),
    )
    .unwrap();
    let conn = lib.init_db();
    conn.execute(
        "INSERT INTO file_hashes (path, hash, exif_date, modified_at)
         VALUES (?1, 'hright', ?2, ?3)",
        rusqlite::params![right.to_string_lossy(), right_date, target(right_date)],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO file_hashes (path, hash, exif_date)
         VALUES (?1, 'hwrong', '2019-06-15T10:00:00')",
        [wrong.to_string_lossy().as_ref()],
    )
    .unwrap();
    (lib, right, wrong)
}

#[test]
fn the_prompt_counts_only_files_whose_date_would_change() {
    let (lib, _right, _wrong) = one_right_one_wrong();
    let out = run_fix_dates(&lib, &[], Some("n\n"));
    assert!(out.status.success());
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        text.contains("set the modified time on 1 file(s)"),
        "the prompt must name the one file that changes, not both: {text}"
    );
}

#[test]
fn a_file_already_at_its_camera_date_is_left_alone() {
    let (lib, right, wrong) = one_right_one_wrong();
    let out = run_fix_dates(&lib, &["--yes"], None);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stdout.contains("değişen.jpg"), "{stdout}");
    assert!(
        !stdout.contains("doğru.jpg"),
        "an already-correct file must not be reported as updated: {stdout}"
    );
    assert!(stderr.contains("1 updated"), "{stderr}");
    assert!(stderr.contains("1 already correct"), "{stderr}");
    assert_eq!(mtime_year(&wrong), 2019);
    assert_eq!(mtime_year(&right), 2014);
}

#[test]
fn nothing_to_fix_asks_nothing_and_says_so() {
    let (lib, _right, wrong) = one_right_one_wrong();
    lib.init_db()
        .execute(
            "DELETE FROM file_hashes WHERE path = ?1",
            [wrong.to_string_lossy().as_ref()],
        )
        .unwrap();
    // No stdin at all: a prompt here would read EOF and abort, so success with
    // no "Aborted" proves none was shown.
    let out = run_fix_dates(&lib, &[], Some(""));
    assert!(out.status.success());
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!text.contains("Continue?"), "{text}");
    assert!(!text.contains("Aborted"), "{text}");
    assert!(text.contains("1 already correct"), "{text}");
}
