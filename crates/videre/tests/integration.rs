use std::fs;
use std::process::Command;
use tempfile::tempdir;

fn dupe_bin() -> std::path::PathBuf {
    let mut path = std::env::current_exe().unwrap();
    path.pop(); // deps/
    path.pop(); // debug/
    path.push("dupe");
    path
}

#[test]
fn exact_duplicates_appear_in_output_file() {
    let scan_dir = tempdir().unwrap();
    let out_dir = tempdir().unwrap();
    let output = out_dir.path().join("hashes");

    // Two files with identical content
    fs::write(scan_dir.path().join("a.jpg"), b"same content").unwrap();
    fs::write(scan_dir.path().join("b.jpg"), b"same content").unwrap();
    fs::write(scan_dir.path().join("c.jpg"), b"different").unwrap();

    let status = Command::new(dupe_bin())
        .arg("--silent")
        .arg("--output")
        .arg(&output)
        .arg(scan_dir.path())
        .status()
        .expect("failed to run dupe");

    assert!(status.success());

    let content = fs::read_to_string(&output).unwrap();
    let lines: Vec<_> = content.lines().collect();
    assert_eq!(lines.len(), 3); // 3 records written

    // Both identical files have the same hash (order is non-deterministic due to rayon)
    let records: Vec<serde_json::Value> = lines
        .iter()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let mut hash_counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for r in &records {
        *hash_counts.entry(r["hash"].as_str().unwrap().to_string()).or_insert(0) += 1;
    }
    assert!(hash_counts.values().any(|&c| c >= 2), "expected at least one hash to appear twice");
}

#[test]
fn missing_directory_exits_nonzero() {
    let status = Command::new(dupe_bin())
        .arg("--silent")
        .arg("/nonexistent/path/abc123")
        .status()
        .expect("failed to run dupe");
    assert!(!status.success());
}

#[test]
fn exif_fields_populated_for_jpeg_with_exif() {
    let scan_dir = tempdir().unwrap();
    let out_dir = tempdir().unwrap();
    let output = out_dir.path().join("hashes");

    fs::copy(
        "tests/fixtures/sample_with_exif.jpg",
        scan_dir.path().join("photo.jpg"),
    )
    .unwrap();

    let status = Command::new(dupe_bin())
        .arg("--silent")
        .arg("--output")
        .arg(&output)
        .arg(scan_dir.path())
        .status()
        .expect("failed to run dupe");

    assert!(status.success());

    let content = fs::read_to_string(&output).unwrap();
    let record: serde_json::Value = serde_json::from_str(content.trim()).unwrap();

    assert_eq!(record["exif_date"], "2021-08-10T19:34:03");
    assert!(record["gps_lat"].as_f64().is_some());
    assert!(record["gps_lon"].as_f64().is_some());
    assert_eq!(record["width"], 4032);
    assert_eq!(record["height"], 3024);
}

#[test]
fn sqlite_output_writes_records_to_db() {
    let scan_dir = tempdir().unwrap();
    let out_dir = tempdir().unwrap();
    let db_path = out_dir.path().join("hashes.db");

    fs::write(scan_dir.path().join("a.jpg"), b"content alpha").unwrap();
    fs::write(scan_dir.path().join("b.jpg"), b"content beta").unwrap();

    let status = Command::new(dupe_bin())
        .arg("--silent")
        .arg("--output-sqlite")
        .arg(&db_path)
        .arg(scan_dir.path())
        .status()
        .expect("failed to run dupe");

    assert!(status.success());
    assert!(db_path.exists());

    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM file_hashes", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 2);
}

#[test]
fn sqlite_output_upserts_on_repeated_run() {
    let scan_dir = tempdir().unwrap();
    let out_dir = tempdir().unwrap();
    let db_path = out_dir.path().join("hashes.db");

    fs::write(scan_dir.path().join("photo.jpg"), b"original content").unwrap();

    // First run
    Command::new(dupe_bin())
        .arg("--silent")
        .arg("--output-sqlite")
        .arg(&db_path)
        .arg(scan_dir.path())
        .status()
        .expect("failed to run dupe")
        .success()
        .then_some(())
        .expect("first run failed");

    // Second run with same folder — should overwrite, not append
    Command::new(dupe_bin())
        .arg("--silent")
        .arg("--output-sqlite")
        .arg(&db_path)
        .arg(scan_dir.path())
        .status()
        .expect("failed to run dupe")
        .success()
        .then_some(())
        .expect("second run failed");

    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM file_hashes", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1, "upsert should not duplicate records");
}

#[test]
fn sqlite_and_output_flags_conflict() {
    let scan_dir = tempdir().unwrap();
    let out_dir = tempdir().unwrap();

    let status = Command::new(dupe_bin())
        .arg("--output")
        .arg(out_dir.path().join("hashes"))
        .arg("--output-sqlite")
        .arg(out_dir.path().join("hashes.db"))
        .arg(scan_dir.path())
        .status()
        .expect("failed to run dupe");

    assert!(!status.success(), "should fail when both --output and --output-sqlite are given");
}
