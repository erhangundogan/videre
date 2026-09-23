mod common;

use common::TestLibrary;

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
