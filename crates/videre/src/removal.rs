//! Move files to the operating system's trash (recoverable), the deletion
//! primitive behind `dedupe --remove`. Isolated here so the `trash` crate has a
//! single import site and the deletion flow can be exercised without driving the
//! whole command.

use std::path::PathBuf;

/// Move each path to the OS trash, returning a per-path result so the caller can
/// count successes, report skips, and stop on a run of failures. Never hard
/// deletes: a move to the system trash is recoverable.
pub fn trash_paths(paths: &[PathBuf]) -> Vec<(PathBuf, Result<(), String>)> {
    paths
        .iter()
        .map(|p| {
            let result = trash::delete(p).map_err(|e| e.to_string());
            (p.clone(), result)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trash_paths_moves_a_file_off_disk_or_skips() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("victim.jpg");
        std::fs::write(&f, b"x").unwrap();
        let res = trash_paths(std::slice::from_ref(&f));
        // Moved to the trash: it is gone from its old path. If the platform
        // could not trash here (no XDG/Finder trash in this environment),
        // there is nothing to assert; the wrapper reported the error.
        if let Ok(()) = &res[0].1 {
            assert!(!f.exists(), "trashed file must be gone from its path");
        }
    }
}
