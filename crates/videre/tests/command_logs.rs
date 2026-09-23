mod common;

use common::TestLibrary;

fn logs(lib: &TestLibrary) -> std::path::PathBuf {
    lib.context().paths.state.join("logs")
}

fn last_json_line(path: &std::path::Path) -> serde_json::Value {
    let text =
        std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_str(text.lines().last().expect("a log line")).unwrap()
}

#[test]
fn a_failing_command_leaves_its_error_in_its_own_log_and_says_it_once() {
    let lib = TestLibrary::new();
    lib.scan();
    let out = lib
        .cmd()
        .args(["search", "--model", "yok/böyle-bir-model", "deniz kenarı"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(stderr.matches("error: ").count(), 1, "{stderr}");
    let line = last_json_line(&logs(&lib).join("search.log"));
    assert_eq!(line["level"], "ERROR");
    assert_eq!(line["spans"][0]["command"], "search");
    assert!(stderr.contains(line["fields"]["message"].as_str().unwrap()));
}

#[test]
fn a_json_error_on_stdout_is_logged_but_not_repeated_on_stderr() {
    let lib = TestLibrary::new();
    lib.scan();
    let out = lib
        .cmd()
        .args([
            "search",
            "--json",
            "--model",
            "yok/böyle-bir-model",
            "deniz kenarı",
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let _: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("error: "), "{stderr}");
    let line = last_json_line(&logs(&lib).join("search.log"));
    assert_eq!(line["level"], "ERROR");
}

fn parsed_lines(path: &std::path::Path) -> Vec<videre_core::error_log::LogLine> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(videre_core::error_log::parse_line)
        .collect()
}

#[test]
fn a_pipeline_stage_failure_is_logged_in_pipeline_log_with_its_stage() {
    let lib = TestLibrary::new();
    lib.scan();
    // Holding scan's command lock makes the pipeline's scan stage fail the
    // same way every time, without models.
    let held = videre_core::library_locks::try_command(&lib.context(), "scan").unwrap();
    let out = lib
        .cmd()
        .args([
            "pipeline",
            "--yes",
            "--json",
            "--skip",
            "faces,embed,classify,locations",
        ])
        .output()
        .unwrap();
    drop(held);
    let _ = out;
    let lines = parsed_lines(&logs(&lib).join("pipeline.log"));
    let failure = lines
        .iter()
        .find(|l| l.level == videre_core::error_log::LineLevel::Error)
        .unwrap_or_else(|| panic!("no error in pipeline.log: {lines:?}"));
    assert_eq!(failure.command.as_deref(), Some("pipeline"));
    assert_eq!(failure.stage.as_deref(), Some("scan"));
    assert_eq!(failure.kind.as_deref(), Some("library_busy"));
    let scan_log = std::fs::read_to_string(logs(&lib).join("scan.log")).unwrap_or_default();
    assert!(
        !scan_log.contains("busy") && !scan_log.contains("in use"),
        "the failure belongs to pipeline.log only: {scan_log}"
    );
}

#[test]
fn a_later_clean_run_supersedes_an_earlier_failure() {
    let lib = TestLibrary::new();
    lib.scan();
    let failed = lib
        .cmd()
        .args(["config", "set", "log-format", "xml"])
        .output()
        .unwrap();
    assert!(!failed.status.success());
    let runs = videre_core::error_log::latest_runs(&lib.context()).unwrap();
    let config = runs.iter().find(|r| r.command == "config").unwrap();
    assert_eq!(config.errors, 1);

    let clean = lib.cmd().arg("config").output().unwrap();
    assert!(clean.status.success());
    let runs = videre_core::error_log::latest_runs(&lib.context()).unwrap();
    let config = runs.iter().find(|r| r.command == "config").unwrap();
    assert_eq!(
        (config.errors, config.warnings),
        (0, 0),
        "the clean run is the latest one: {config:?}"
    );
}

#[test]
fn status_shows_the_latest_run_and_a_clean_run_clears_it() {
    let lib = TestLibrary::new();
    lib.scan();
    let dir = logs(&lib);
    std::fs::create_dir_all(&dir).unwrap();
    let line = |run: &str, level: &str, msg: &str| {
        format!(
            r#"{{"timestamp":"2026-09-23T11:05:01Z","level":"{level}","fields":{{"message":"{msg}","kind":"source_unavailable","stage":"scan"}},"spans":[{{"command":"watch","run":"{run}","name":"run"}}]}}"#
        )
    };
    std::fs::write(
        dir.join("watch.log"),
        [
            line(
                "R1",
                "ERROR",
                "videre watch: scan stage: /Volumes/Arşiv/Çağla.jpg timed out",
            ),
            line("R1", "WARN", "skipping /Volumes/Arşiv/b.jpg"),
        ]
        .join("\n")
            + "\n",
    )
    .unwrap();

    let out = lib.cmd().args(["status", "--check"]).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("Recent problems"), "{text}");
    assert!(text.contains("watch"), "{text}");
    assert!(text.contains("1 error(s) (scan 1), 1 warning(s)"), "{text}");
    assert!(
        text.contains("run 2026-09-23"),
        "the run's time tells an old failure from a current one: {text}"
    );
    assert!(
        text.contains("last: source_unavailable: videre watch: scan stage: /Volumes/Arşiv/Çağla.jpg timed out"),
        "{text}"
    );
    assert!(
        !out.status.success(),
        "a latest run with errors is a problem"
    );

    let json = lib.cmd().args(["status", "--json"]).output().unwrap();
    let doc: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    let watch = doc["report"]["logs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["command"] == "watch")
        .unwrap_or_else(|| panic!("no watch entry: {doc}"));
    assert_eq!(watch["errors"], 1);
    assert_eq!(watch["warnings"], 1);

    // A genuinely clean later run of the same command clears it: one real
    // watch cycle (the test-only bounded mode) with nothing to report.
    let clean = lib
        .cmd()
        .args(["watch", "--silent", "--scan"])
        .env("VIDERE_WATCH_ONCE", "1")
        .output()
        .unwrap();
    assert!(
        clean.status.success(),
        "{}",
        String::from_utf8_lossy(&clean.stderr)
    );
    let out = lib.cmd().args(["status", "--check"]).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{text}");
    assert!(!text.contains("Recent problems"), "{text}");
}

#[test]
fn debug_level_records_decisions_in_the_trace_file_only() {
    let lib = TestLibrary::new();
    lib.scan();
    let set = lib
        .cmd()
        .args(["config", "set", "log-level", "debug"])
        .status()
        .unwrap();
    assert!(set.success());
    let out = lib.cmd().args(["stats", "--json"]).output().unwrap();
    assert!(out.status.success());
    let _: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(
        !String::from_utf8_lossy(&out.stderr).contains("lock"),
        "debug never reaches the terminal"
    );

    let trace = parsed_lines(&logs(&lib).join("stats.trace.log"));
    assert!(
        trace
            .iter()
            .any(|l| l.level == videre_core::error_log::LineLevel::Debug
                && l.message.contains("log settings")),
        "{trace:?}"
    );
    assert!(
        trace
            .iter()
            .any(|l| l.level == videre_core::error_log::LineLevel::Debug
                && l.message.contains("lock")),
        "{trace:?}"
    );
    let primary = parsed_lines(&logs(&lib).join("stats.log"));
    assert!(primary
        .iter()
        .all(|l| l.level != videre_core::error_log::LineLevel::Debug));
}

#[test]
fn reading_an_uninitialized_library_creates_nothing() {
    let lib = TestLibrary::new();
    let _ = lib.cmd().arg("stats").output().unwrap();
    assert!(!lib.context().paths.state.exists());
}

#[test]
fn a_symlinked_logs_directory_changes_nothing_about_the_command() {
    let lib = TestLibrary::new();
    lib.scan();
    let elsewhere = tempfile::tempdir().unwrap();
    std::fs::remove_dir_all(logs(&lib)).ok();
    std::os::unix::fs::symlink(elsewhere.path(), logs(&lib)).unwrap();
    let out = lib.cmd().args(["stats", "--json"]).output().unwrap();
    assert!(out.status.success());
    let _: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("file logging disabled"), "{stderr}");
    assert_eq!(std::fs::read_dir(elsewhere.path()).unwrap().count(), 0);
}

#[test]
fn log_settings_are_set_and_shown_through_config() {
    let lib = TestLibrary::new();
    let set = lib
        .cmd()
        .args(["config", "set", "log-level", "debug"])
        .output()
        .unwrap();
    assert!(
        set.status.success(),
        "{}",
        String::from_utf8_lossy(&set.stderr)
    );
    let bad = lib
        .cmd()
        .args(["config", "set", "log-format", "xml"])
        .output()
        .unwrap();
    assert!(!bad.status.success());
    let bad_size = lib
        .cmd()
        .args(["config", "set", "log-max-size-mb", "0"])
        .output()
        .unwrap();
    assert!(!bad_size.status.success());

    let shown = lib.cmd().arg("config").output().unwrap();
    let out = String::from_utf8_lossy(&shown.stdout);
    assert!(out.contains("log-level:     debug"), "{out}");
    assert!(out.contains("log-format:    json"), "{out}");
    assert!(out.contains("log-max-size-mb: 10 MB"), "{out}");
    assert!(out.contains("log-keep:      5"), "{out}");
    assert!(out.contains("log-max-age-days: 30 days"), "{out}");
}

#[test]
fn a_partially_failing_prune_logs_its_failure_where_status_finds_it() {
    use std::os::unix::fs::PermissionsExt;
    let lib = TestLibrary::new();
    lib.scan();
    // An orphaned thumbnail in a cache directory that refuses deletion: the
    // sweep counts it as a failure while the rest of the prune succeeds.
    let thumbs = lib.context().cache.thumbnails;
    std::fs::create_dir_all(&thumbs).unwrap();
    let orphan = thumbs.join(format!("{}_240.jpg", "ab".repeat(32)));
    std::fs::write(&orphan, b"x").unwrap();
    std::fs::set_permissions(&thumbs, std::fs::Permissions::from_mode(0o555)).unwrap();
    // As root the directory mode is not enforced, and there is nothing to test.
    if std::fs::write(thumbs.join("probe"), b"x").is_ok() {
        std::fs::set_permissions(&thumbs, std::fs::Permissions::from_mode(0o755)).unwrap();
        return;
    }
    let out = lib.cmd().arg("prune").output().unwrap();
    std::fs::set_permissions(&thumbs, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(!out.status.success(), "a prune with errors exits non-zero");

    let errors: Vec<_> = parsed_lines(&logs(&lib).join("prune.log"))
        .into_iter()
        .filter(|l| l.level == videre_core::error_log::LineLevel::Error)
        .collect();
    assert!(
        errors
            .iter()
            .any(|l| l.path.as_deref() == Some(orphan.to_str().unwrap())),
        "{errors:?}"
    );
    let status = lib.cmd().args(["status", "--check"]).output().unwrap();
    assert!(!status.status.success(), "status --check finds the failure");
}

#[test]
fn silent_hides_progress_but_never_a_warning() {
    let lib = TestLibrary::new();
    // Logs start once the library exists; its very first scan creates it.
    lib.scan();
    let out = lib
        .cmd()
        .args(["scan", "--silent", "--xmp", "newest"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("warning: --xmp newest is not yet implemented"),
        "{stderr}"
    );
    assert!(parsed_lines(&logs(&lib).join("scan.log"))
        .iter()
        .any(|l| l.level == videre_core::error_log::LineLevel::Warn
            && l.message.contains("--xmp newest")));
}

#[test]
fn prune_keeps_a_warning_for_rows_it_skipped_as_unreachable() {
    let lib = TestLibrary::new();
    lib.copy_fixture("ai-generated-couple.jpg", "İstanbul/Çağla_2019.jpg");
    lib.scan();
    // The whole directory is gone, the shape of an unmounted drive: prune
    // keeps the rows and says so, and that must survive in the log.
    std::fs::remove_dir_all(lib.root.join("İstanbul")).unwrap();
    let out = lib.cmd().args(["prune", "--silent"]).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        parsed_lines(&logs(&lib).join("prune.log"))
            .iter()
            .any(|l| l.level == videre_core::error_log::LineLevel::Warn
                && l.message.contains("skipped as unreachable")),
        "{}",
        std::fs::read_to_string(logs(&lib).join("prune.log")).unwrap_or_default()
    );
}
