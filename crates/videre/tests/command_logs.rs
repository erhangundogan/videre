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
