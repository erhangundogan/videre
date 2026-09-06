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

/// The seeded two-library fixture and logical snapshot helpers, shared by the
/// directory-local command tests.
pub mod feature_fixture;

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
/// Delegates to `videre_core::hf_cache`, which owns this knowledge because
/// `videre-ml`'s own tests need the same check and a second copy would be free
/// to drift.
pub fn hf_cache_dir() -> PathBuf {
    videre_core::hf_cache::cache_dir()
}

/// InsightFace SCRFD detector and ArcFace recogniser, used by `videre faces`.
pub fn face_models_cached() -> bool {
    videre_core::hf_cache::repo_has("WePrompt/buffalo_l", &["det_10g.onnx", "w600k_r50.onnx"])
}

/// SigLIP weights for the resolved default model, used by `videre embed`.
///
/// Derives the repo from `DEFAULT_MODEL_ID` rather than hardcoding it, so
/// changing the default model cannot silently make this always-false and skip
/// the embed tests forever.
pub fn siglip_cached() -> bool {
    videre_core::hf_cache::siglip_ready(videre_core::embeddings::DEFAULT_MODEL_ID)
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

/// A child's stderr with third-party library noise removed.
///
/// ONNX Runtime is linked into every `videre` binary and initialises at
/// startup, even for subcommands that never run inference. On any host whose
/// CPU it cannot identify it prints
/// `onnxruntime cpuid_info warning: Unknown CPU vendor` before `main` gets a
/// say. Measured on ARM64 Linux, which this project documents as a supported
/// platform, so this is not a container artifact to be waved away: a real user
/// there sees it too.
///
/// Assertions that videre printed nothing mean *videre*, not "no library
/// anywhere in the process wrote to fd 2". Without this filter such a test
/// passes on macOS and fails on ARM64 Linux for a reason unrelated to what it
/// is testing.
pub fn stderr_without_library_noise(stderr: &str) -> String {
    stderr
        .lines()
        .filter(|l| !l.contains("onnxruntime"))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

/// Whether the current process can read a file it has no permission bits for.
///
/// True when running as root, which bypasses permission checks entirely, so
/// any test relying on an unreadable file has nothing to test. Probes the
/// behaviour rather than checking the uid: it is the behaviour that matters,
/// and it needs no libc dependency.
pub fn permissions_are_enforced(unreadable_path: &Path) -> bool {
    std::fs::read(unreadable_path).is_err()
}

/// An owned, per-child temporary library.
///
/// One `TestLibrary` is one throwaway world: a `root` directory standing in
/// for the user's library folder, and a private `home` so the child's caches
/// and locks can never reach the developer's real `~/.videre` or Hugging Face
/// cache. The `TempDir` is held in `_temp` precisely so it outlives every
/// child spawned from this instance; dropping the `TestLibrary` drops the
/// directory.
///
/// Deliberately does **not** go through [`videre_bin`], which sets a
/// process-global `VIDERE_HOME` for this test process. That helper remains
/// the right one for the legacy tests, but the whole point here is explicit
/// per-child context: each command gets its own cwd, HOME, and HF_HOME, and
/// `VIDERE_HOME` is removed rather than inherited, so what a child sees is
/// exactly what was configured and nothing ambient. Nothing in this helper
/// mutates the test process's own cwd or environment.
pub struct TestLibrary {
    _temp: tempfile::TempDir,
    /// The library directory the child treats as its working library.
    pub root: PathBuf,
    /// The child's private HOME, holding caches and locks.
    pub home: PathBuf,
}

impl TestLibrary {
    /// Creates the library and home directories under a fresh temp dir.
    pub fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("library");
        let home = temp.path().join("home");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&home).unwrap();
        Self {
            _temp: temp,
            root,
            home,
        }
    }

    /// Where this library's database lives, under `.videre/` in the root.
    pub fn db(&self) -> PathBuf {
        self.root.join(".videre/hashes.db")
    }

    /// A `videre` command rooted in this library, with an explicit context.
    ///
    /// The child runs with `cwd = root`, a private `HOME`, and an `HF_HOME`
    /// inside that home, so no model cache is exposed by default and no real
    /// user state is reachable. `VIDERE_HOME` is removed rather than pointed
    /// somewhere: a child of this helper must not silently inherit the
    /// process-global isolation dir either.
    pub fn cmd(&self) -> std::process::Command {
        let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_videre"));
        cmd.current_dir(&self.root)
            .env("HOME", &self.home)
            .env("HF_HOME", self.home.join(".cache/huggingface"))
            .env_remove("VIDERE_HOME");
        cmd
    }

    /// A `videre` command run from `cwd` but pinned to this library.
    ///
    /// Passing `--library root` is what makes the context explicit: the child
    /// operates on this library no matter which directory it was invoked
    /// from, which is the behaviour the directory-local-libraries tests build
    /// on.
    pub fn from(&self, cwd: &Path) -> std::process::Command {
        let mut cmd = self.cmd();
        cmd.current_dir(cwd).arg("--library").arg(&self.root);
        cmd
    }

    /// Copies a fixture from `tests/fixtures/` into this library.
    ///
    /// `source` is relative to the fixtures directory, `target` relative to
    /// the library root; parent directories of the target are created as
    /// needed. Returns the destination path.
    pub fn copy_fixture(&self, source: &str, target: &str) -> PathBuf {
        let src = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(source);
        let dst = self.root.join(target);
        std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
        std::fs::copy(src, &dst).unwrap();
        dst
    }

    /// A core-level context for this library, for tests that exercise the
    /// foundation layers directly rather than through the binary.
    ///
    /// The cache base sits under the private home, matching what `cmd()`
    /// gives a child, so an in-process context and a spawned child see the
    /// same cache namespace for one library.
    pub fn context(&self) -> videre_core::library::LibraryContext {
        videre_core::library::LibraryContext::new(&self.root, &self.home.join(".cache")).unwrap()
    }

    /// Scan this library through the directory-local command surface.
    pub fn scan(&self) {
        let output = self.cmd().args(["scan", "--silent"]).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// Open the existing local database without creating one.
    pub fn conn(&self) -> rusqlite::Connection {
        videre_core::library_db::open_existing(&self.context()).unwrap()
    }
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
