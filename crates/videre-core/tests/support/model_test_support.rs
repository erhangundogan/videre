// Shared test-only controls for real-model tests in separate crates.

/// Real inference is local opt-in, independent of cache warmth.
pub fn parse_model_test_mode(raw: Option<&str>) -> Result<bool, String> {
    match raw {
        None | Some("0") => Ok(false),
        Some("1") => Ok(true),
        Some(value) => Err(format!(
            "invalid VIDERE_TEST_MODELS={value:?}; use 0 (skip) or 1 (run)"
        )),
    }
}

/// Return early unless a developer explicitly requested real-model tests.
pub fn skip_unless_model_tests_enabled(name: &str) -> bool {
    let raw = std::env::var("VIDERE_TEST_MODELS").ok();
    if parse_model_test_mode(raw.as_deref()).unwrap_or_else(|message| panic!("{message}")) {
        return false;
    }
    write_past_test_capture(&format!(
        "SKIP: {name} needs real model weights; run VIDERE_TEST_MODELS=1 make test locally.\n"
    ));
    true
}

/// A passing skip's regular `eprintln!` is swallowed by libtest capture.
pub fn write_past_test_capture(msg: &str) {
    use std::io::Write;
    use std::os::fd::FromRawFd;

    // Borrow fd 2 without closing it when this temporary File goes away.
    let mut stderr = std::mem::ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(2) });
    let _ = stderr.write_all(msg.as_bytes());
    let _ = stderr.flush();
}

/// Cross-process lock for a shared developer model cache.
pub struct ModelCacheGuard(std::fs::File);

impl ModelCacheGuard {
    pub fn acquire() -> Self {
        Self::acquire_at(&std::env::temp_dir().join("videre-test-model-cache.lock"))
    }

    /// The same protocol on another lock file. Tests of the protocol itself use
    /// their own, so a real model test holding the shared lock for minutes
    /// cannot stall them.
    pub fn acquire_at(path: &std::path::Path) -> Self {
        use fs2::FileExt;

        let file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(path)
            .expect("open shared model-cache lock");
        file.lock_exclusive()
            .expect("flock shared model-cache lock");
        Self(file)
    }
}

impl Drop for ModelCacheGuard {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.0);
    }
}
