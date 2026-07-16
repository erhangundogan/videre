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
fn config_show_works_with_empty_home() {
    let home = tempdir().unwrap();
    let out = Command::new(videre_bin())
        .arg("config")
        .env("VIDERE_HOME", home.path())
        .output()
        .expect("failed to run videre config");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("db:            (not set)"), "{stdout}");
    assert!(stdout.contains("videre config set db"), "the not-set hint must name the settable key: {stdout}");
    assert!(stdout.contains("hashes.db"), "{stdout}");
}

#[test]
fn config_set_and_unset_db_roundtrip() {
    let home = tempdir().unwrap();
    let set = Command::new(videre_bin())
        .arg("config").arg("set").arg("db").arg("/tmp/custom.db")
        .env("VIDERE_HOME", home.path())
        .status()
        .expect("failed to run videre config set");
    assert!(set.success());
    assert!(home.path().join("config.toml").exists());

    let show = Command::new(videre_bin())
        .arg("config")
        .env("VIDERE_HOME", home.path())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&show.stdout);
    assert!(stdout.contains("db:            /tmp/custom.db"), "{stdout}");

    let unset = Command::new(videre_bin())
        .arg("config").arg("unset").arg("db")
        .env("VIDERE_HOME", home.path())
        .status()
        .unwrap();
    assert!(unset.success());
    let show2 = Command::new(videre_bin())
        .arg("config")
        .env("VIDERE_HOME", home.path())
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&show2.stdout).contains("db:            (not set)"));
}

#[test]
fn config_set_rejects_unknown_key() {
    let home = tempdir().unwrap();
    let out = Command::new(videre_bin())
        .arg("config").arg("set").arg("nope").arg("/x")
        .env("VIDERE_HOME", home.path())
        .output()
        .unwrap();
    assert!(!out.status.success(), "unknown config key must be rejected");
}

#[test]
fn config_set_and_unset_path_roundtrip() {
    let home = tempdir().unwrap();
    // absent: row shows the not-set hint naming the settable key
    let show0 = Command::new(videre_bin())
        .arg("config")
        .env("VIDERE_HOME", home.path())
        .output()
        .unwrap();
    let stdout0 = String::from_utf8_lossy(&show0.stdout);
    assert!(stdout0.contains("resolved path: (not set)"), "{stdout0}");
    assert!(stdout0.contains("videre config set path"), "{stdout0}");

    let set = Command::new(videre_bin())
        .arg("config").arg("set").arg("path").arg("/tmp/photos")
        .env("VIDERE_HOME", home.path())
        .status()
        .unwrap();
    assert!(set.success());
    let show = Command::new(videre_bin())
        .arg("config")
        .env("VIDERE_HOME", home.path())
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&show.stdout).contains("resolved path: /tmp/photos"),
        "{}",
        String::from_utf8_lossy(&show.stdout)
    );

    let unset = Command::new(videre_bin())
        .arg("config").arg("unset").arg("path")
        .env("VIDERE_HOME", home.path())
        .status()
        .unwrap();
    assert!(unset.success());
    let show2 = Command::new(videre_bin())
        .arg("config")
        .env("VIDERE_HOME", home.path())
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&show2.stdout).contains("resolved path: (not set)"));
}
