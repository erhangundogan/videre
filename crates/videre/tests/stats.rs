mod common;

/// Scan one real fixture into a fresh directory-local library.
fn library_with_one_file() -> common::TestLibrary {
    let lib = common::TestLibrary::new();
    lib.copy_fixture("tiny.jpg", "a.jpg");
    lib.scan();
    lib
}

#[test]
fn stats_reports_library_totals_without_pipeline_status() {
    // Pipeline run health is operational state, and it lives in `videre
    // status` now; `stats` is pure inventory, so the two surfaces cannot
    // drift into telling different stories about the same runs.
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
    assert!(
        !stdout.contains("Pipeline status"),
        "stats must not report pipeline status, {stdout}"
    );
}

#[test]
fn stats_json_has_no_pipelines_field() {
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
    assert!(
        doc["pipelines"].is_null(),
        "pipeline state moved to videre status --json"
    );
}

#[test]
fn stats_check_flag_is_no_longer_accepted() {
    // --check moved to `videre status` along with the pipeline block; stats
    // is a command that never fails a script, so the flag must be gone
    // rather than silently accepted and ignored.
    let lib = library_with_one_file();
    let out = lib
        .cmd()
        .args(["stats", "--check"])
        .output()
        .expect("failed to run videre stats --check");
    assert!(
        !out.status.success(),
        "--check must not be silently accepted on stats"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("unexpected argument"), "{stderr}");
}

#[test]
fn status_tracks_prune_runs() {
    let lib = library_with_one_file();
    // The run is seeded directly here: this test is about status reporting a
    // tracked command's last outcome, not about running prune.
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
        .args(["status", "--json"])
        .output()
        .expect("failed to run videre status");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let pipelines = doc["report"]["pipelines"].as_array().unwrap();
    let prune_entry = pipelines.iter().find(|p| p["command"] == "prune").unwrap();
    assert_eq!(prune_entry["status"], "success");
}

#[test]
fn status_tracks_locations_runs() {
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
        .args(["status", "--json"])
        .output()
        .expect("failed to run videre status");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let pipelines = doc["report"]["pipelines"].as_array().unwrap();
    let entry = pipelines
        .iter()
        .find(|p| p["command"] == "locations")
        .unwrap();
    assert_eq!(entry["status"], "success");
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
