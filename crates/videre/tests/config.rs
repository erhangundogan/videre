mod common;
use common::videre_bin;
use std::process::Command;
use tempfile::tempdir;

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
    assert!(
        stdout.contains("videre config set db"),
        "the not-set hint must name the settable key: {stdout}"
    );
    assert!(stdout.contains("hashes.db"), "{stdout}");
}

#[test]
fn config_set_and_unset_db_roundtrip() {
    let home = tempdir().unwrap();
    let set = Command::new(videre_bin())
        .arg("config")
        .arg("set")
        .arg("db")
        .arg("/tmp/custom.db")
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
        .arg("config")
        .arg("unset")
        .arg("db")
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
        .arg("config")
        .arg("set")
        .arg("nope")
        .arg("/x")
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
        .arg("config")
        .arg("set")
        .arg("path")
        .arg("/tmp/photos")
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
        .arg("config")
        .arg("unset")
        .arg("path")
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

#[test]
fn set_model_persists_and_is_shown() {
    let home = tempdir().unwrap();
    let out = Command::new(videre_bin())
        .env("VIDERE_HOME", home.path())
        .args(["config", "set", "model", "google/siglip-base-patch16-224"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let text = std::fs::read_to_string(home.path().join("config.toml")).unwrap();
    assert!(
        text.contains("default_model = \"google/siglip-base-patch16-224\""),
        "stored verbatim, not absolutized: {text}"
    );

    let shown = Command::new(videre_bin())
        .env("VIDERE_HOME", home.path())
        .args(["config"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&shown.stdout);
    assert!(stdout.contains("model:"), "{stdout}");
    assert!(
        stdout.contains("google/siglip-base-patch16-224"),
        "{stdout}"
    );
    assert!(stdout.contains("[from config.toml]"), "{stdout}");
}

#[test]
fn set_model_rejects_an_id_without_an_owner() {
    // Embedder::load does split_once('/').expect(...), so an id with no slash
    // panics at load time. Rejecting it here makes that unreachable.
    let home = tempdir().unwrap();
    let out = Command::new(videre_bin())
        .env("VIDERE_HOME", home.path())
        .args(["config", "set", "model", "siglip-base-patch16-224"])
        .output()
        .unwrap();
    assert!(!out.status.success(), "must reject a bare model name");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("owner/name"), "{stderr}");
    assert!(
        !home.path().join("config.toml").exists(),
        "a rejected value must not be written"
    );
}

#[test]
fn config_shows_the_builtin_default_when_model_is_unset() {
    let home = tempdir().unwrap();
    let out = Command::new(videre_bin())
        .env("VIDERE_HOME", home.path())
        .args(["config"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains(videre_core::embeddings::DEFAULT_MODEL_ID),
        "{stdout}"
    );
    assert!(stdout.contains("videre config set model"), "{stdout}");
}

#[test]
fn unset_model_removes_the_key_and_leaves_others_alone() {
    let home = tempdir().unwrap();
    for args in [
        vec!["config", "set", "db", "/tmp/x.db"],
        vec!["config", "set", "model", "owner/model-224"],
    ] {
        let out = Command::new(videre_bin())
            .env("VIDERE_HOME", home.path())
            .args(&args)
            .output()
            .unwrap();
        assert!(out.status.success());
    }
    let out = Command::new(videre_bin())
        .env("VIDERE_HOME", home.path())
        .args(["config", "unset", "model"])
        .output()
        .unwrap();
    assert!(out.status.success());

    let text = std::fs::read_to_string(home.path().join("config.toml")).unwrap();
    assert!(!text.contains("default_model"), "{text}");
    assert!(
        text.contains("default_db"),
        "unsetting one key must not drop another: {text}"
    );
}
