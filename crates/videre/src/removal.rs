//! Move files to the operating system's trash (recoverable), the deletion
//! primitive behind `dedupe --remove`. Isolated here so the `trash` crate has a
//! single import site and the deletion flow can be exercised without driving the
//! whole command.

use std::path::PathBuf;

/// Move each path to the OS trash, returning a per-path result so the caller can
/// count successes, report skips, and stop on a run of failures. Never hard
/// deletes: a move to the system trash is recoverable.
///
/// A photo that moved takes its `<file>.<ext>.xmp` sidecar with it, so no
/// orphan sidecar is left describing a photo that is gone. A sidecar that will
/// not move is warned about and never turns the photo's result into a failure.
pub fn trash_paths(paths: &[PathBuf]) -> Vec<(PathBuf, Result<(), String>)> {
    paths
        .iter()
        .map(|p| {
            let result = trash::delete(p).map_err(|e| e.to_string());
            if result.is_ok() {
                let sidecar = crate::xmp::write::sidecar_path(p);
                if sidecar.exists() {
                    if let Err(e) = trash::delete(&sidecar) {
                        tracing::warn!("could not trash the sidecar {sidecar:?}: {e}");
                    }
                }
            }
            (p.clone(), result)
        })
        .collect()
}

/// The sidecars that would go to the trash with `paths`, for a dry run.
pub fn sidecars_of(paths: &[PathBuf]) -> Vec<PathBuf> {
    paths
        .iter()
        .map(|p| crate::xmp::write::sidecar_path(p))
        .filter(|s| s.exists())
        .collect()
}

/// Trash library files and forget the rows of those that moved, in one
/// transaction, so the library stops listing them at once. Derived data
/// (marks, tags, faces, embeddings) stays for `videre prune`, as after
/// `dedupe --remove`. A file that could not be trashed keeps its row.
pub fn trash_and_forget(
    conn: &rusqlite::Connection,
    paths: &[PathBuf],
) -> rusqlite::Result<Vec<(PathBuf, Result<(), String>)>> {
    let results = trash_paths(paths);
    let tx = conn.unchecked_transaction()?;
    for (path, result) in &results {
        if result.is_ok() {
            tx.execute(
                "DELETE FROM file_hashes WHERE path = ?1",
                [path.to_string_lossy()],
            )?;
        }
    }
    tx.commit()?;
    Ok(results)
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

    #[test]
    fn a_trashed_photo_takes_its_sidecar_with_it() {
        let dir = tempfile::tempdir().unwrap();
        let photo = dir.path().join("kopya çiçek.jpg");
        let sidecar = dir.path().join("kopya çiçek.jpg.xmp");
        let other = dir.path().join("çiçek.jpg.xmp");
        std::fs::write(&photo, b"x").unwrap();
        std::fs::write(&sidecar, b"<x:xmpmeta/>").unwrap();
        std::fs::write(&other, b"<x:xmpmeta/>").unwrap();
        let res = trash_paths(std::slice::from_ref(&photo));
        if let Ok(()) = &res[0].1 {
            assert!(!sidecar.exists(), "the sidecar was left behind");
            assert!(other.exists(), "another photo's sidecar must stay");
        }
    }
}
