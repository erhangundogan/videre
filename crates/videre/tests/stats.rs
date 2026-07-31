use std::fs;
use std::process::Command;
use tempfile::tempdir;

fn videre_bin() -> std::path::PathBuf {
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
