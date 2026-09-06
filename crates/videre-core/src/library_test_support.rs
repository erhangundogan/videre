//! Test-only support shared by the library foundation layers' unit tests.
//!
//! Compiled only for this crate's own test builds (declared `cfg(test)` in
//! `lib.rs`), so it is invisible to every dependent crate and to the published
//! library. One concern lives here, extracted after it had been copied into
//! four separate `#[cfg(test)]` modules: writing past libtest's output
//! capture. The integration suite's copy in
//! `crates/videre/tests/common/mod.rs` is a different crate and stays there.

/// Writes to the process's real stderr, bypassing libtest's output capture.
///
/// A skip is a passing test, and libtest captures the print macros for
/// passing tests, so an `eprintln!` skip message is invisible in a normal
/// `cargo test` run and only appears under `--nocapture`; writing to fd 2
/// directly sidesteps the capture. `ManuallyDrop` because dropping a `File`
/// built from a borrowed fd would close fd 2 for the rest of the process.
pub(crate) fn write_past_test_capture(msg: &str) {
    use std::io::Write;
    use std::os::fd::FromRawFd;

    let mut stderr = std::mem::ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(2) });
    let _ = stderr.write_all(msg.as_bytes());
    let _ = stderr.flush();
}
