mod common;
use common::videre_bin;
use rusqlite::Connection;
use std::io::Write;
use std::process::{Command, Stdio};
use tempfile::tempdir;

/// One file with a known exif_date, no created_at/modified_at set.
/// Returns (db_path, file_path).
fn fixture_db(dir: &std::path::Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let file = dir.join("a.jpg");
    std::fs::write(&file, b"img_a").unwrap();

    let db = dir.join("test.db");
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch(
        "CREATE TABLE file_hashes (
            path TEXT PRIMARY KEY, hash TEXT NOT NULL, size_bytes INTEGER,
            created_at TEXT, modified_at TEXT, ext TEXT, phash INTEGER,
            exif_date TEXT, gps_lat REAL, gps_lon REAL, width INTEGER, height INTEGER
        );",
    )
    .unwrap();
    videre_core::db::ensure_file_hashes_columns(&conn);
    conn.execute(
        "INSERT INTO file_hashes (path, hash, exif_date) VALUES (?1, 'haaa', '2019-06-15T10:00:00')",
        rusqlite::params![file.to_str().unwrap()],
    )
    .unwrap();
    (db, file)
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
    db: &std::path::Path,
    extra_args: &[&str],
    stdin_input: Option<&str>,
) -> std::process::Output {
    let mut cmd = Command::new(videre_bin());
    cmd.arg("fix-dates").arg("--db").arg(db).args(extra_args);
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
    let dir = tempdir().unwrap();
    let (db, file) = fixture_db(dir.path());
    let before = mtime_year(&file);

    let out = run_fix_dates(&db, &[], Some("n\n"));
    assert!(out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("Aborted"), "{stderr}");
    assert_eq!(mtime_year(&file), before, "declining must not touch mtime");
}

#[test]
fn accepting_the_prompt_updates_the_file() {
    let dir = tempdir().unwrap();
    let (db, file) = fixture_db(dir.path());

    let out = run_fix_dates(&db, &[], Some("y\n"));
    assert!(out.status.success());
    assert_eq!(
        mtime_year(&file),
        2019,
        "accepting should set mtime from exif_date"
    );
}

#[test]
fn yes_flag_skips_the_prompt_entirely() {
    let dir = tempdir().unwrap();
    let (db, file) = fixture_db(dir.path());

    // No stdin provided at all, if the prompt were shown, this would hang
    // (read_line would block); --yes must bypass it.
    let out = run_fix_dates(&db, &["--yes"], None);
    assert!(out.status.success());
    assert_eq!(
        mtime_year(&file),
        2019,
        "--yes should proceed without a prompt"
    );
}

#[test]
fn dry_run_never_prompts() {
    let dir = tempdir().unwrap();
    let (db, file) = fixture_db(dir.path());
    let before = mtime_year(&file);

    // No stdin provided, dry-run must not block on a prompt either.
    let out = run_fix_dates(&db, &["--dry-run"], None);
    assert!(out.status.success());
    assert_eq!(
        mtime_year(&file),
        before,
        "dry-run must not modify the file"
    );
}

#[test]
fn eof_on_stdin_is_treated_as_no() {
    let dir = tempdir().unwrap();
    let (db, file) = fixture_db(dir.path());
    let before = mtime_year(&file);

    // Empty stdin (immediate EOF, as when stdin is /dev/null) must be treated
    // as declining, not accepted and not a hang.
    let out = run_fix_dates(&db, &[], Some(""));
    assert!(out.status.success());
    assert_eq!(
        mtime_year(&file),
        before,
        "EOF on stdin must be treated as 'no'"
    );
}
