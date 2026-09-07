mod common;

/// Scan one real fixture into a fresh directory-local library.
fn library_with_one_file() -> common::TestLibrary {
    let lib = common::TestLibrary::new();
    lib.copy_fixture("tiny.jpg", "a.jpg");
    lib.scan();
    lib
}

#[test]
fn stats_reports_library_totals_and_never_run_pipelines() {
    let lib = library_with_one_file();
    let out = lib
        .cmd()
        .arg("stats")
        .output()
        .expect("failed to run videre stats");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Library: 1 file(s)"), "{stdout}");
    assert!(stdout.contains("scan"), "{stdout}");
    // faces never ran against this library
    assert!(stdout.contains("faces"), "{stdout}");
}

#[test]
fn stats_json_includes_library_and_pipelines() {
    let lib = library_with_one_file();
    let out = lib
        .cmd()
        .args(["stats", "--json"])
        .output()
        .expect("failed to run videre stats");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let doc: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("stdout must be one valid JSON object");
    assert_eq!(doc["schema_version"], 1);
    assert_eq!(doc["library"]["total_files"], 1);
    let pipelines = doc["pipelines"].as_array().unwrap();
    assert_eq!(
        pipelines.len(),
        videre_core::pipeline_runs::TRACKED_COMMANDS.len()
    );
    let scan_entry = pipelines.iter().find(|p| p["command"] == "scan").unwrap();
    assert_eq!(scan_entry["status"], "success");
    let faces_entry = pipelines.iter().find(|p| p["command"] == "faces").unwrap();
    assert_eq!(faces_entry["status"], serde_json::Value::Null);
}

#[test]
fn stats_tracks_prune_runs() {
    let lib = library_with_one_file();
    // `prune` gains its directory-local entry point in a later task, so its run
    // is seeded directly here: this test is about `stats` reporting a tracked
    // command's status, not about running prune.
    lib.conn()
        .execute(
            "INSERT OR REPLACE INTO pipeline_runs
                 (command, started_at, finished_at, status, duration_ms, summary)
             VALUES ('prune', '2026-01-01 00:00:00', '2026-01-01 00:00:01', 'success', 5, NULL)",
            [],
        )
        .unwrap();

    let out = lib
        .cmd()
        .args(["stats", "--json"])
        .output()
        .expect("failed to run videre stats");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let pipelines = doc["pipelines"].as_array().unwrap();
    let prune_entry = pipelines.iter().find(|p| p["command"] == "prune").unwrap();
    assert_eq!(prune_entry["status"], "success");
}

#[test]
fn stats_tracks_locations_runs() {
    let lib = common::TestLibrary::new();
    // A GPS row so locations has something to cluster, seeded directly rather
    // than scanned so the coordinate is the fixture. The path must sit under the
    // canonical root or the row-containment guard rejects the whole database.
    let path = lib.context().paths.root.join("a.jpg");
    lib.init_db()
        .execute(
            "INSERT INTO file_hashes (path, hash, gps_lat, gps_lon)
             VALUES (?1, 'h1', 41.0082, 28.9784)",
            [path.to_string_lossy().as_ref()],
        )
        .unwrap();

    let status = lib
        .cmd()
        .args(["locations", "--silent"])
        .status()
        .expect("failed to run videre locations");
    assert!(status.success());

    let out = lib
        .cmd()
        .args(["stats", "--json"])
        .output()
        .expect("failed to run videre stats");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let pipelines = doc["pipelines"].as_array().unwrap();
    let entry = pipelines
        .iter()
        .find(|p| p["command"] == "locations")
        .unwrap();
    assert_eq!(entry["status"], "success");
}

#[test]
fn stats_check_exits_zero_when_nothing_failed_or_crashed() {
    let lib = library_with_one_file();

    // Both text and --json modes should compose with --check.
    let text = lib
        .cmd()
        .args(["stats", "--check"])
        .status()
        .expect("failed to run videre stats --check");
    assert!(
        text.success(),
        "no tracked command has failed/crashed, so --check must exit 0"
    );

    let json = lib
        .cmd()
        .args(["stats", "--json", "--check"])
        .status()
        .expect("failed to run videre stats --json --check");
    assert!(json.success());
}

#[test]
fn stats_check_exits_nonzero_when_a_command_failed() {
    let lib = library_with_one_file();

    // Simulate a prior failed run by writing directly into pipeline_runs.
    // Exercising the CLI's own failure path for every tracked command would
    // be its own large test; this isolates --check's exit-code contract.
    lib.conn()
        .execute(
            "INSERT OR REPLACE INTO pipeline_runs (command, started_at, finished_at, status, duration_ms, summary)
             VALUES ('faces', '2026-01-01 00:00:00', '2026-01-01 00:00:01', 'failed', 1000, 'boom')",
            [],
        ).unwrap();

    let out = lib
        .cmd()
        .args(["stats", "--check"])
        .output()
        .expect("failed to run videre stats --check");
    assert!(
        !out.status.success(),
        "a failed command must make --check exit non-zero"
    );
    // Output is unchanged by --check, normal stats text is still printed.
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("faces"), "{stdout}");

    let json_out = lib
        .cmd()
        .args(["stats", "--json", "--check"])
        .output()
        .expect("failed to run videre stats --json --check");
    assert!(!json_out.status.success());
    let doc: serde_json::Value = serde_json::from_slice(&json_out.stdout).unwrap();
    assert_eq!(
        doc["schema_version"], 1,
        "--json output must still be valid, unaffected by --check"
    );
}

#[test]
fn stats_errors_cleanly_on_missing_db_without_creating_one() {
    // A library that was never initialized: no .videre database.
    let lib = common::TestLibrary::new();

    let out = lib
        .cmd()
        .arg("stats")
        .output()
        .expect("failed to run videre stats");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("not initialized"), "{stderr}");
    assert!(!lib.db().exists(), "stats must not create a database file");
}
