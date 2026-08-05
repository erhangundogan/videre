use std::fs;
use std::process::Command;
use tempfile::tempdir;

/// Points `VIDERE_HOME` at a throwaway directory for this whole test binary.
/// Spawned `videre` child processes inherit the environment, so their lock
/// files land there instead of the developer's real `~/.videre/locks`, locks
/// live under the videre home now rather than beside the database, so without
/// this every run would leave permanent litter in the real home (test database
/// names are random, so the files would accumulate rather than be reused).
///
/// Called from `videre_bin()` so it covers every spawn site automatically. The
/// `set_var` runs inside `get_or_init` so it happens exactly once: tests share
/// a process and run in parallel, and calling `set_var` from several threads
/// would otherwise race every concurrent `getenv`. Tests that set their own
/// `VIDERE_HOME` per-command still win, since `.env()` overrides what's
/// inherited.
fn isolated_home() {
    static HOME: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    HOME.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("videre-it-home-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create isolated test home");
        std::env::set_var("VIDERE_HOME", &dir);
        dir
    });
}

fn videre_bin() -> std::path::PathBuf {
    isolated_home();
    let mut path = std::env::current_exe().unwrap();
    path.pop(); // deps/
    path.pop(); // debug/
    path.push("videre");
    path
}

#[test]
fn stats_reports_library_totals_and_never_run_pipelines() {
    let scan_dir = tempdir().unwrap();
    let out_dir = tempdir().unwrap();
    let db_path = out_dir.path().join("hashes.db");

    fs::write(scan_dir.path().join("a.jpg"), b"content").unwrap();

    let status = Command::new(videre_bin())
        .arg("scan")
        .arg("--silent")
        .arg("--output-sqlite")
        .arg(&db_path)
        .arg(scan_dir.path())
        .status()
        .expect("failed to run videre scan");
    assert!(status.success());

    let out = Command::new(videre_bin())
        .arg("stats")
        .arg("--db")
        .arg(&db_path)
        .output()
        .expect("failed to run videre stats");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Library: 1 file(s)"), "{stdout}");
    assert!(stdout.contains("scan"), "{stdout}");
    // faces never ran against this db
    assert!(stdout.contains("faces"), "{stdout}");
}

#[test]
fn stats_json_includes_library_and_pipelines() {
    let scan_dir = tempdir().unwrap();
    let out_dir = tempdir().unwrap();
    let db_path = out_dir.path().join("hashes.db");

    fs::write(scan_dir.path().join("a.jpg"), b"content").unwrap();

    Command::new(videre_bin())
        .arg("scan")
        .arg("--silent")
        .arg("--output-sqlite")
        .arg(&db_path)
        .arg(scan_dir.path())
        .status()
        .expect("failed to run videre scan");

    let out = Command::new(videre_bin())
        .arg("stats")
        .arg("--db")
        .arg(&db_path)
        .arg("--json")
        .output()
        .expect("failed to run videre stats");
    assert!(out.status.success());
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout)
        .expect("stdout must be one valid JSON object");
    assert_eq!(doc["schema_version"], 1);
    assert_eq!(doc["library"]["total_files"], 1);
    let pipelines = doc["pipelines"].as_array().unwrap();
    assert_eq!(pipelines.len(), videre_core::pipeline_runs::TRACKED_COMMANDS.len());
    let scan_entry = pipelines.iter().find(|p| p["command"] == "scan").unwrap();
    assert_eq!(scan_entry["status"], "success");
    let faces_entry = pipelines.iter().find(|p| p["command"] == "faces").unwrap();
    assert_eq!(faces_entry["status"], serde_json::Value::Null);
}

#[test]
fn stats_tracks_prune_runs() {
    let scan_dir = tempdir().unwrap();
    let out_dir = tempdir().unwrap();
    let db_path = out_dir.path().join("hashes.db");

    fs::write(scan_dir.path().join("a.jpg"), b"content").unwrap();

    Command::new(videre_bin())
        .arg("scan").arg("--silent").arg("--output-sqlite").arg(&db_path).arg(scan_dir.path())
        .status().expect("failed to run videre scan");

    let status = Command::new(videre_bin())
        .arg("prune").arg("--db").arg(&db_path).arg("--silent")
        .status().expect("failed to run videre prune");
    assert!(status.success());

    let out = Command::new(videre_bin())
        .arg("stats").arg("--db").arg(&db_path).arg("--json")
        .output().expect("failed to run videre stats");
    assert!(out.status.success());
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let pipelines = doc["pipelines"].as_array().unwrap();
    let prune_entry = pipelines.iter().find(|p| p["command"] == "prune").unwrap();
    assert_eq!(prune_entry["status"], "success");
}

#[test]
fn stats_tracks_locations_runs() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute_batch(
        "CREATE TABLE file_hashes (
            path        TEXT PRIMARY KEY,
            hash        TEXT NOT NULL,
            size_bytes  INTEGER,
            created_at  TEXT,
            modified_at TEXT,
            ext         TEXT,
            phash       INTEGER,
            exif_date   TEXT,
            gps_lat     REAL,
            gps_lon     REAL,
            width       INTEGER,
            height      INTEGER,
            location_name TEXT,
            location_cluster_id INTEGER
        );",
    )
    .unwrap();
    drop(conn);

    let status = Command::new(videre_bin())
        .arg("locations")
        .arg("--db")
        .arg(&db_path)
        .arg("--silent")
        .status()
        .expect("failed to run videre locations");
    assert!(status.success());

    let out = Command::new(videre_bin())
        .arg("stats")
        .arg("--db")
        .arg(&db_path)
        .arg("--json")
        .output()
        .expect("failed to run videre stats");
    assert!(out.status.success());
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let pipelines = doc["pipelines"].as_array().unwrap();
    let entry = pipelines.iter().find(|p| p["command"] == "locations").unwrap();
    assert_eq!(entry["status"], "success");
}

#[test]
fn stats_check_exits_zero_when_nothing_failed_or_crashed() {
    let scan_dir = tempdir().unwrap();
    let out_dir = tempdir().unwrap();
    let db_path = out_dir.path().join("hashes.db");

    fs::write(scan_dir.path().join("a.jpg"), b"content").unwrap();

    Command::new(videre_bin())
        .arg("scan").arg("--silent").arg("--output-sqlite").arg(&db_path).arg(scan_dir.path())
        .status().expect("failed to run videre scan");

    // Both text and --json modes should compose with --check.
    let text = Command::new(videre_bin())
        .arg("stats").arg("--db").arg(&db_path).arg("--check")
        .status().expect("failed to run videre stats --check");
    assert!(text.success(), "no tracked command has failed/crashed, so --check must exit 0");

    let json = Command::new(videre_bin())
        .arg("stats").arg("--db").arg(&db_path).arg("--json").arg("--check")
        .status().expect("failed to run videre stats --json --check");
    assert!(json.success());
}

#[test]
fn stats_check_exits_nonzero_when_a_command_failed() {
    let scan_dir = tempdir().unwrap();
    let out_dir = tempdir().unwrap();
    let db_path = out_dir.path().join("hashes.db");

    fs::write(scan_dir.path().join("a.jpg"), b"content").unwrap();

    Command::new(videre_bin())
        .arg("scan").arg("--silent").arg("--output-sqlite").arg(&db_path).arg(scan_dir.path())
        .status().expect("failed to run videre scan");

    // Simulate a prior failed run by writing directly into pipeline_runs.
    // Exercising the CLI's own failure path for every tracked command would
    // be its own large test; this isolates --check's exit-code contract.
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute(
        "INSERT OR REPLACE INTO pipeline_runs (command, started_at, finished_at, status, duration_ms, summary)
         VALUES ('faces', '2026-01-01 00:00:00', '2026-01-01 00:00:01', 'failed', 1000, 'boom')",
        [],
    ).unwrap();
    drop(conn);

    let out = Command::new(videre_bin())
        .arg("stats").arg("--db").arg(&db_path).arg("--check")
        .output().expect("failed to run videre stats --check");
    assert!(!out.status.success(), "a failed command must make --check exit non-zero");
    // Output is unchanged by --check, normal stats text is still printed.
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("faces"), "{stdout}");

    let json_out = Command::new(videre_bin())
        .arg("stats").arg("--db").arg(&db_path).arg("--json").arg("--check")
        .output().expect("failed to run videre stats --json --check");
    assert!(!json_out.status.success());
    let doc: serde_json::Value = serde_json::from_slice(&json_out.stdout).unwrap();
    assert_eq!(doc["schema_version"], 1, "--json output must still be valid, unaffected by --check");
}

#[test]
fn stats_errors_cleanly_on_missing_db_without_creating_one() {
    let home = tempdir().unwrap();
    let db_path = home.path().join("does-not-exist.db");

    let out = Command::new(videre_bin())
        .arg("stats")
        .arg("--db")
        .arg(&db_path)
        .output()
        .expect("failed to run videre stats");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("no database found"), "{stderr}");
    assert!(!db_path.exists(), "stats must not create a database file");
}
