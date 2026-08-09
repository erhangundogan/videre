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
/// `videre embed` pulls SigLIP through the Hugging Face cache and `videre
/// faces` pulls the InsightFace ONNX weights through `~/.cache/ort/`. Neither
/// cache is safe for two simultaneous first-time readers: the loser sees a
/// half-written file and fails with something like `load tokenizer: No such
/// file or directory`.
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
