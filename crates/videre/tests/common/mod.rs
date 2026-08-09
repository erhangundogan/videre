//! Shared helpers for the integration tests.
//!
//! Every test file here spawns the `videre` binary as a child process, and two
//! things about that are easy to get wrong in a way nothing fails loudly on.
//! Both used to be solved by copying a helper into each file, which is exactly
//! how `faces_pipeline.rs` and `person_search.rs` ended up without one.
//!
//! In a `tests/common/` subdirectory rather than a `tests/common.rs`, so cargo
//! treats it as a module to include rather than a test binary of its own.

#![allow(dead_code)] // Each test binary uses a different subset of this.

use std::path::{Path, PathBuf};

/// Points `VIDERE_HOME` at a throwaway directory for this whole test binary.
///
/// Locks live under the videre home rather than beside the database, so without
/// this every run leaves permanent litter in the developer's real
/// `~/.videre/locks` (test database names are random, so the files accumulate
/// rather than being reused). Spawned children inherit the environment, so
/// setting it here covers them too.
///
/// The `set_var` runs inside `get_or_init` so it happens exactly once: tests
/// share a process and run in parallel, and a per-test `set_var` would race
/// every concurrent `getenv`. Keyed by process id, and each test binary is its
/// own process, so binaries running in parallel still get separate homes.
///
/// Tests that set their own `VIDERE_HOME` per-command still win, since `.env()`
/// overrides what is inherited.
pub fn isolated_home() -> &'static Path {
    static HOME: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    HOME.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("videre-test-home-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create isolated test home");
        std::env::set_var("VIDERE_HOME", &dir);
        dir
    })
}

/// Path to the `videre` binary under test, with `VIDERE_HOME` already isolated.
///
/// Isolating the home here rather than at each call site is what makes it
/// impossible for a new test file to forget it.
pub fn videre_bin() -> PathBuf {
    isolated_home();
    let mut path = std::env::current_exe().unwrap();
    path.pop(); // deps/
    path.pop(); // debug/
    path.push("videre");
    path
}

/// Serialises children that may download shared model weights.
///
/// `videre embed` (SigLIP) and `videre faces` (InsightFace ONNX) both resolve
/// weights through the same Hugging Face cache, which is not safe for two
/// simultaneous first-time readers: the loser sees a half-written file and
/// fails with something like `load tokenizer: No such file or directory`.
///
/// This has to be a **file** lock, not a `Mutex`. Cargo runs each test file as
/// its own process and runs those processes in parallel, so the racing readers
/// are usually in different processes, which no in-process lock can see. The
/// previous `Mutex` in `embed.rs` correctly serialised that file's own two
/// tests and could never have serialised it against `faces_pipeline.rs`.
///
/// The lock file sits at a fixed path in the system temp directory, shared by
/// every test binary. Deliberately **not** under `VIDERE_HOME`, which
/// `isolated_home` makes per-binary, and which would therefore hand each
/// binary its own uncontended lock.
///
/// Only contended on a cold cache; once the weights are present every holder
/// releases almost immediately, so the cost on a warm machine is negligible.
pub fn shared_cache_guard() -> impl Drop {
    use fs2::FileExt;

    let path = std::env::temp_dir().join("videre-test-model-cache.lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .expect("open shared model-cache lock");
    file.lock_exclusive()
        .expect("flock shared model-cache lock");

    /// Releases the `flock` on drop. The OS also releases it if the test
    /// process dies, so a panicking test cannot wedge the rest of the suite.
    struct Guard(std::fs::File);
    impl Drop for Guard {
        fn drop(&mut self) {
            let _ = fs2::FileExt::unlock(&self.0);
        }
    }
    Guard(file)
}

/// Root of the local Hugging Face cache, honouring `HF_HOME`.
///
/// Both SigLIP and InsightFace land here. Note this is **not** `~/.cache/ort/`,
/// which `CLAUDE.md` claimed for months and which has never existed; a check
/// written against that path would report "cold" forever.
pub fn hf_cache_dir() -> PathBuf {
    if let Ok(home) = std::env::var("HF_HOME") {
        return PathBuf::from(home).join("hub");
    }
    dirs_home().join(".cache").join("huggingface").join("hub")
}

fn dirs_home() -> PathBuf {
    std::env::var("HOME").map(PathBuf::from).unwrap_or_default()
}

/// Whether every one of `files` is present in some snapshot of `repo`.
///
/// Checks the files themselves, not the repo directory: an interrupted
/// download leaves the directory in place with the weights missing, and that
/// state must read as cold or the caller proceeds into a failure.
///
/// The snapshot sha is globbed rather than pinned, since it changes whenever
/// the upstream repo is updated.
fn repo_files_cached(repo: &str, files: &[&str]) -> bool {
    let snapshots = hf_cache_dir()
        .join(format!("models--{}", repo.replace('/', "--")))
        .join("snapshots");
    let Ok(entries) = std::fs::read_dir(&snapshots) else {
        return false;
    };
    entries
        .filter_map(Result::ok)
        .any(|snap| files.iter().all(|f| snap.path().join(f).exists()))
}

/// InsightFace SCRFD detector and ArcFace recogniser, used by `videre faces`.
pub fn face_models_cached() -> bool {
    repo_files_cached("WePrompt/buffalo_l", &["det_10g.onnx", "w600k_r50.onnx"])
}

/// SigLIP weights for the resolved default model, used by `videre embed`.
///
/// Derives the repo from `DEFAULT_MODEL_ID` rather than hardcoding it, so
/// changing the default model cannot silently make this always-false and skip
/// the embed tests forever.
pub fn siglip_cached() -> bool {
    repo_files_cached(
        videre_core::embeddings::DEFAULT_MODEL_ID,
        &["tokenizer.json", "config.json"],
    )
}

/// Whether the caller should return early, printing a loud reason if so.
///
/// Tests never download weights: that is the application's job, triggered by a
/// real `videre embed` or `videre faces` run. A cold cache therefore skips.
///
/// Rust has no native skip, so a skipped test passes, which is a real risk of
/// silently covering nothing. `VIDERE_TEST_REQUIRE_MODELS=1` turns the skip
/// into a panic, and CI sets it after restoring its cache: a skip there means
/// the cache silently stopped working, and the test would otherwise never run
/// anywhere at all.
pub fn skip_without_models(what: &str, cached: bool) -> bool {
    if cached {
        return false;
    }
    let cache = hf_cache_dir();
    if std::env::var("VIDERE_TEST_REQUIRE_MODELS").as_deref() == Ok("1") {
        panic!(
            "VIDERE_TEST_REQUIRE_MODELS=1 but {what} weights are missing from {}. \
             In CI this means the model cache was not restored.",
            cache.display()
        );
    }
    // Deliberately not `eprintln!`. libtest captures the print macros for
    // tests that pass, and a skip passes, so an `eprintln!` here is invisible
    // in a normal `cargo test` run and only appears under `--nocapture`. That
    // is the opposite of loud, and it is the whole reason skipping is
    // acceptable at all. Writing to fd 2 directly sidesteps the capture.
    write_past_test_capture(&format!(
        "SKIP: {what} weights are not cached in {}. \
         Run `videre {what}` once to populate it; tests never download.\n",
        cache.display()
    ));
    true
}

/// Writes to the process's real stderr, bypassing libtest's output capture.
///
/// `ManuallyDrop` because dropping a `File` built from a borrowed fd would
/// close fd 2 for the rest of the process.
fn write_past_test_capture(msg: &str) {
    use std::io::Write;
    use std::os::fd::FromRawFd;

    let mut stderr = std::mem::ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(2) });
    let _ = stderr.write_all(msg.as_bytes());
    let _ = stderr.flush();
}
