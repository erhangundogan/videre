//! Tests for `TestLibrary`, the owned per-child temporary library used by the
//! directory-local-libraries refactor.
//!
//! These inspect `Command` construction only: no child is spawned here, so a
//! green run proves the context (cwd, HOME, HF_HOME, VIDERE_HOME, --library)
//! is established without mutating the test process's own environment or
//! touching either library on disk.

mod common;

#[test]
fn child_context_is_explicit_without_mutating_parent_environment() {
    let before = std::env::current_dir().unwrap();
    let a = common::TestLibrary::new();
    let b = common::TestLibrary::new();
    let cmd = a.from(&b.root);
    assert_eq!(cmd.get_current_dir(), Some(b.root.as_path()));
    let args: Vec<_> = cmd.get_args().collect();
    assert_eq!(args[0], std::ffi::OsStr::new("--library"));
    assert_eq!(args[1], a.root.as_os_str());
    let envs: Vec<_> = cmd.get_envs().collect();
    assert!(envs.contains(&(std::ffi::OsStr::new("HOME"), Some(a.home.as_os_str()))));
    assert!(envs.contains(&(
        std::ffi::OsStr::new("HF_HOME"),
        Some(a.home.join(".cache/huggingface").as_os_str())
    )));
    assert!(envs.contains(&(std::ffi::OsStr::new("VIDERE_HOME"), None)));
    assert_eq!(std::env::current_dir().unwrap(), before);
    assert!(!a.db().exists());
    assert!(!b.db().exists());
}
