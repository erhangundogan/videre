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

/// The flag path, which the config-set test below did not cover. `videre embed
/// --model foo` used to abort with "thread 'main' panicked ... model id is
/// owner/name", because validation lived in `commands/config.rs` and guarded
/// only `config set` - one of the two entrances to the same function.
#[test]
fn a_malformed_model_flag_is_an_error_not_a_panic() {
    let home = tempdir().unwrap();
    for cmd in ["embed", "search", "classify"] {
        let mut c = Command::new(videre_bin());
        c.env("VIDERE_HOME", home.path()).arg(cmd);
        if cmd == "search" {
            c.arg("anything");
        }
        let out = c.args(["--model", "foo"]).output().unwrap();
        assert!(!out.status.success(), "{cmd} must reject a bare model name");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("owner/name"),
            "{cmd} must explain the shape, got: {stderr}"
        );
        assert!(
            !stderr.contains("panicked"),
            "{cmd} must not panic, got: {stderr}"
        );
    }
}

#[test]
fn set_model_rejects_an_id_without_an_owner() {
    // Embedder::load does split_once('/').expect(...), so an id with no slash
    // panics at load time. Rejecting it here makes that unreachable.
    //
    // This covered `config set` only, which is why the --model flag went on
    // reaching the panic: the same invariant enforced on one entrance and not
    // the other. See a_malformed_model_flag_is_an_error_not_a_panic above.
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

#[test]
fn config_set_read_rate_roundtrips_and_refuses_zero() {
    let home = tempdir().unwrap();
    let set = |v: &str| {
        Command::new(videre_bin())
            .args(["config", "set", "read-rate", v])
            .env("VIDERE_HOME", home.path())
            .output()
            .expect("failed to run videre config set")
    };

    assert!(set("50").status.success());
    let raw = std::fs::read_to_string(home.path().join("config.toml")).unwrap();
    assert!(
        raw.contains("min_read_rate_mb_s = 50"),
        "must be stored as a TOML integer, not a string: {raw}"
    );

    // Zero would mean an unbounded read timeout, which is precisely the hang
    // the timeout exists to prevent, so it must be refused at the CLI rather
    // than written and tripped over later.
    let zero = set("0");
    assert!(!zero.status.success(), "read-rate 0 must be rejected");
    let raw = std::fs::read_to_string(home.path().join("config.toml")).unwrap();
    assert!(
        raw.contains("min_read_rate_mb_s = 50"),
        "a rejected value must not clobber the previous one: {raw}"
    );

    let bad = set("fast");
    assert!(!bad.status.success(), "a non-number must be rejected");

    assert!(Command::new(videre_bin())
        .args(["config", "unset", "read-rate"])
        .env("VIDERE_HOME", home.path())
        .output()
        .unwrap()
        .status
        .success());
    let raw = std::fs::read_to_string(home.path().join("config.toml")).unwrap();
    assert!(!raw.contains("min_read_rate_mb_s"), "{raw}");
}

/// `videre config` must report the database the commands will actually open.
///
/// It used to call `home::resolve_db_in`, which consults config alone and does
/// not apply the VIDERE_HOME rule. With an explicit home whose config.toml named
/// a different database, `config` printed that database while every command
/// opened `<home>/hashes.db` and failed with "no database found" against a path
/// `config` had never shown. The two must not be resolved by different code.
#[test]
fn config_reports_the_database_commands_actually_open() {
    let home = tempdir().unwrap();

    // A config whose default_db diverges from <home>/hashes.db. VIDERE_HOME is
    // meant to win here, and does for the commands.
    let diverging = home.path().join("elsewhere.db");
    let set = Command::new(videre_bin())
        .args(["config", "set", "db"])
        .arg(&diverging)
        .env("VIDERE_HOME", home.path())
        .status()
        .expect("failed to run videre config set");
    assert!(set.success());

    let show = Command::new(videre_bin())
        .arg("config")
        .env("VIDERE_HOME", home.path())
        .output()
        .expect("failed to run videre config");
    assert!(show.status.success());
    let stdout = String::from_utf8_lossy(&show.stdout);

    let reported = stdout
        .lines()
        .find_map(|l| l.strip_prefix("resolved db:"))
        .expect("config must print a resolved db line")
        .trim()
        .to_string();

    // What a real command opens, read from the error it raises for the missing
    // file. Any command would do; stats needs no arguments.
    let stats = Command::new(videre_bin())
        .arg("stats")
        .env("VIDERE_HOME", home.path())
        .output()
        .expect("failed to run videre stats");
    let stderr = String::from_utf8_lossy(&stats.stderr);

    assert!(
        stderr.contains(&reported),
        "config reported {reported}, but the command opened something else:\n{stderr}"
    );
    // And specifically: VIDERE_HOME wins, so neither may name the config value.
    assert_eq!(
        reported,
        home.path().join("hashes.db").display().to_string(),
        "VIDERE_HOME must outrank that home's own default_db"
    );
    assert!(
        !reported.contains("elsewhere.db"),
        "the overridden config value must not be presented as resolved: {stdout}"
    );
}
