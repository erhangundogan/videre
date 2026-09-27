mod common;

use common::{model_test_support::ModelCacheGuard, TestLibrary};
use std::{
    fs,
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

fn wait_for(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn one_writer_and_a_waiting_follower_share_the_fake_cache() {
    let temp = tempfile::tempdir().unwrap();
    let exe = std::env::current_exe().unwrap();
    let mut leader = Command::new(&exe)
        .args(["--exact", "cache_lock_child"])
        .env("VIDERE_TEST_CACHE_ROLE", "leader")
        .env("VIDERE_TEST_CACHE_ROOT", temp.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    wait_for(&temp.path().join("leader-acquired"));

    let follower = Command::new(&exe)
        .args(["--exact", "cache_lock_child"])
        .env("VIDERE_TEST_CACHE_ROLE", "follower")
        .env("VIDERE_TEST_CACHE_ROOT", temp.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    wait_for(&temp.path().join("follower-ready"));
    assert!(leader.try_wait().unwrap().is_none());
    assert!(!temp.path().join("follower-reused").exists());
    fs::write(temp.path().join("release-leader"), "go").unwrap();

    let first = leader.wait_with_output().unwrap();
    let second = follower.wait_with_output().unwrap();
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert!(temp.path().join("leader-wrote").exists());
    assert!(temp.path().join("follower-reused").exists());
    assert!(!temp.path().join("follower-wrote").exists());
}

#[test]
fn cache_lock_child() {
    let Ok(role) = std::env::var("VIDERE_TEST_CACHE_ROLE") else {
        return;
    };
    let root = std::path::PathBuf::from(std::env::var("VIDERE_TEST_CACHE_ROOT").unwrap());
    if role == "follower" {
        fs::write(root.join("follower-ready"), "ready").unwrap();
    }
    let _guard = ModelCacheGuard::acquire_at(&root.join("cache.lock"));
    let artifact = root.join("fake-model.bin");
    if role == "leader" {
        fs::write(root.join("leader-acquired"), "ready").unwrap();
        wait_for(&root.join("release-leader"));
        fs::write(&artifact, "finished").unwrap();
        fs::write(root.join("leader-wrote"), "one").unwrap();
    } else if artifact.exists() {
        assert_eq!(fs::read_to_string(&artifact).unwrap(), "finished");
        fs::write(root.join("follower-reused"), "yes").unwrap();
    } else {
        fs::write(&artifact, "second writer").unwrap();
        fs::write(root.join("follower-wrote"), "two").unwrap();
    }
}

#[test]
fn model_child_keeps_private_home_but_shares_only_hub_cache() {
    let temp = tempfile::tempdir().unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "model_child_environment_probe"])
        .env("VIDERE_TEST_MODEL_ENV_PROBE", "1")
        .env("VIDERE_TEST_MODELS", "1")
        .env("HF_HUB_CACHE", temp.path().join("developer-hub"))
        .env("HUGGINGFACE_HUB_CACHE", temp.path().join("legacy-hub"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn relative_hub_override_is_absolutized_before_child_changes_directory() {
    let temp = tempfile::tempdir().unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "model_child_environment_probe"])
        .current_dir(temp.path())
        .env("VIDERE_TEST_MODEL_ENV_PROBE", "1")
        .env("VIDERE_TEST_MODELS", "1")
        .env("HF_HUB_CACHE", "relative-hub")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn model_child_environment_probe() {
    if std::env::var("VIDERE_TEST_MODEL_ENV_PROBE").as_deref() != Ok("1") {
        return;
    }
    let library = TestLibrary::new();
    let plain = library.cmd();
    let plain_env = plain.get_envs().collect::<Vec<_>>();
    assert!(!plain_env.iter().any(|(name, value)| {
        (*name == "HF_HUB_CACHE" || *name == "HUGGINGFACE_HUB_CACHE") && value.is_some()
    }));
    // A private lock: this probe inspects a command and loads nothing, so it
    // must not queue behind real model tests during an opt-in run.
    let guard = ModelCacheGuard::acquire_at(&library.home.join("cache.lock"));
    let model = library.model_cmd(&guard);
    let env = model.get_envs().collect::<Vec<_>>();
    let expected_hub = std::env::current_dir()
        .unwrap()
        .join(videre_core::hf_cache::cache_dir());
    assert!(env.iter().any(|(name, value)| {
        *name == "HF_HUB_CACHE" && value == &Some(expected_hub.as_os_str())
    }));
    assert!(env
        .iter()
        .any(|(name, value)| { *name == "HOME" && value == &Some(library.home.as_os_str()) }));
    assert!(env.iter().any(|(name, value)| {
        *name == "HF_HOME" && value == &Some(library.home.join(".cache/huggingface").as_os_str())
    }));
}

#[test]
fn failed_opt_in_loader_fails_without_a_success_marker() {
    let temp = tempfile::tempdir().unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "failed_model_loader_child"])
        .env("VIDERE_TEST_FAKE_LOAD_FAILURE", "1")
        .env("VIDERE_TEST_MODELS", "1")
        .env("VIDERE_TEST_SUCCESS_MARKER", temp.path().join("success"))
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "opt-in loader failure must fail its test binary"
    );
    assert!(!temp.path().join("success").exists());
}

#[test]
fn failed_model_loader_child() {
    if std::env::var("VIDERE_TEST_FAKE_LOAD_FAILURE").as_deref() != Ok("1") {
        return;
    }
    if common::model_test_support::skip_unless_model_tests_enabled("fake loader") {
        return;
    }
    let marker = std::env::var("VIDERE_TEST_SUCCESS_MARKER").unwrap();
    let missing_model = Path::new(&marker).with_file_name("missing-model.bin");
    fs::read(missing_model).expect("opt-in model load");
    fs::write(marker, "loaded").unwrap();
}
