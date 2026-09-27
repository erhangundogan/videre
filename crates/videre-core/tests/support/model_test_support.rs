// Shared test-only controls for real-model tests in separate crates.

/// The first rollout disables model tests only when CI explicitly sets `0`.
/// Local behavior stays unchanged until the opt-in mode is implemented.
pub fn ci_model_tests_disabled(raw: Option<&str>) -> bool {
    raw == Some("0")
}

/// Return early from a model test in CI, with a skip visible under libtest.
pub fn skip_ci_model_test(name: &str) -> bool {
    if !ci_model_tests_disabled(std::env::var("VIDERE_TEST_MODELS").ok().as_deref()) {
        return false;
    }
    write_past_test_capture(&format!(
        "SKIP: {name} needs real model weights; VIDERE_TEST_MODELS=0.\n"
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
