//! Removing library files: to the operating system's trash (recoverable), or
//! permanently. The primitive behind `dedupe --trash`/`--delete` and the
//! gallery's Delete. Isolated here so the `trash` crate has a single import
//! site and the flow can be exercised without driving a whole command.

use std::path::{Path, PathBuf};

/// How a file leaves the library.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    /// To the operating system's trash, recoverable.
    Trash,
    /// Permanently, at once. Cannot be undone.
    Delete,
}

/// What happened to one path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Removed,
    /// Not on disk any more, an earlier run having removed it, say. Its row
    /// is forgotten like a removed file's, and it is not a failure.
    AlreadyGone,
    Failed(String),
}

impl Outcome {
    /// For callers that only tell success from failure.
    pub fn as_result(&self) -> Result<(), String> {
        match self {
            Outcome::Failed(e) => Err(e.clone()),
            _ => Ok(()),
        }
    }
}

/// Remove each path by `method`, taking its `<file>.<ext>.xmp` sidecar with
/// it, and forget the row of every path that left (or was already gone) as
/// it goes, so a run stopped halfway still leaves the library describing
/// what is on disk. A failure keeps its row and is reported to `progress`
/// at once.
pub fn remove_and_forget(
    conn: Option<&rusqlite::Connection>,
    paths: &[PathBuf],
    method: Method,
    progress: &videre_core::progress::Progress,
) -> Vec<(PathBuf, Outcome)> {
    let trash = trash_context();
    let mut forget = conn.and_then(|c| c.prepare("DELETE FROM file_hashes WHERE path = ?1").ok());
    paths
        .iter()
        .map(|path| {
            let outcome = remove_one(path, method, &trash);
            match &outcome {
                Outcome::Removed | Outcome::AlreadyGone => {
                    // One row at a time, committed at once: an interrupted
                    // run keeps the rows of exactly the files still on disk.
                    if let Some(stmt) = forget.as_mut() {
                        if let Err(e) = stmt.execute([path.to_string_lossy()]) {
                            tracing::warn!("removed {path:?} but could not forget its row: {e}");
                        }
                    }
                    progress.tick();
                }
                Outcome::Failed(e) => {
                    progress.skip(&path.display().to_string(), anyhow::anyhow!("{e}"))
                }
            }
            (path.clone(), outcome)
        })
        .collect()
}

/// The trash, through `NSFileManager` on macOS. The `trash` crate's default
/// there asks Finder through one AppleScript call per file: about 4 files a
/// second, blocking behind any dialog Finder shows, and needing permission to
/// script Finder. `NSFileManager` measured about 400 a second and needs none.
fn trash_context() -> trash::TrashContext {
    #[allow(unused_mut)]
    let mut ctx = trash::TrashContext::default();
    #[cfg(target_os = "macos")]
    {
        use trash::macos::{DeleteMethod, TrashContextExtMacos};
        ctx.set_delete_method(DeleteMethod::NsFileManager);
    }
    ctx
}

fn remove_one(path: &Path, method: Method, trash: &trash::TrashContext) -> Outcome {
    match path.symlink_metadata() {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Outcome::AlreadyGone,
        Err(e) => return Outcome::Failed(e.to_string()),
        Ok(_) => {}
    }
    let remove = |p: &Path| -> Result<(), String> {
        match method {
            Method::Trash => trash.delete(p).map_err(|e| e.to_string()),
            Method::Delete => std::fs::remove_file(p).map_err(|e| e.to_string()),
        }
    };
    if let Err(e) = remove(path) {
        return Outcome::Failed(e);
    }
    // The sidecar goes with its photo, so none is left describing a photo
    // that is gone. One that will not go is warned about, and never turns
    // the photo's removal into a failure.
    let sidecar = crate::xmp::write::sidecar_path(path);
    if sidecar.exists() {
        if let Err(e) = remove(&sidecar) {
            tracing::warn!("could not remove the sidecar {sidecar:?}: {e}");
        }
    }
    Outcome::Removed
}

/// The sidecars that would go with `paths`, for a dry run.
pub fn sidecars_of(paths: &[PathBuf]) -> Vec<PathBuf> {
    paths
        .iter()
        .map(|p| crate::xmp::write::sidecar_path(p))
        .filter(|s| s.exists())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quiet(n: usize) -> videre_core::progress::Progress {
        videre_core::progress::Progress::new_counting(n as u64, true, "files")
    }

    /// A database with a row for each of `paths`.
    fn rows_for(paths: &[&Path]) -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE file_hashes (path TEXT PRIMARY KEY, hash TEXT);")
            .unwrap();
        for p in paths {
            conn.execute(
                "INSERT INTO file_hashes VALUES (?1, 'h')",
                [p.to_string_lossy()],
            )
            .unwrap();
        }
        conn
    }

    fn row_count(conn: &rusqlite::Connection) -> i64 {
        conn.query_row("SELECT COUNT(*) FROM file_hashes", [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn delete_removes_the_file_and_its_sidecar_and_forgets_the_row() {
        let dir = tempfile::tempdir().unwrap();
        let photo = dir.path().join("kopya çiçek.jpg");
        let sidecar = dir.path().join("kopya çiçek.jpg.xmp");
        let kept = dir.path().join("çiçek.jpg");
        for f in [&photo, &sidecar, &kept] {
            std::fs::write(f, b"x").unwrap();
        }
        let conn = rows_for(&[&photo, &kept]);
        let res = remove_and_forget(
            Some(&conn),
            std::slice::from_ref(&photo),
            Method::Delete,
            &quiet(1),
        );
        assert_eq!(res, vec![(photo.clone(), Outcome::Removed)]);
        assert!(!photo.exists() && !sidecar.exists());
        assert!(kept.exists());
        assert_eq!(row_count(&conn), 1, "only the removed file's row goes");
    }

    #[test]
    fn a_file_already_gone_is_not_a_failure_and_its_row_is_forgotten() {
        let dir = tempfile::tempdir().unwrap();
        let gone = dir.path().join("IMG_1-edited.jpg");
        let conn = rows_for(&[&gone]);
        for method in [Method::Trash, Method::Delete] {
            let res =
                remove_and_forget(Some(&conn), std::slice::from_ref(&gone), method, &quiet(1));
            assert_eq!(
                res,
                vec![(gone.clone(), Outcome::AlreadyGone)],
                "{method:?}"
            );
        }
        assert_eq!(row_count(&conn), 0);
    }

    #[cfg(unix)]
    #[test]
    fn a_file_that_will_not_go_keeps_its_row() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let locked = dir.path().join("kilitli");
        std::fs::create_dir(&locked).unwrap();
        let photo = locked.join("foto.jpg");
        std::fs::write(&photo, b"x").unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555)).unwrap();
        // As root, permissions are not enforced and the test means nothing.
        if std::fs::write(locked.join("probe"), b"").is_ok() {
            std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
            return;
        }
        let conn = rows_for(&[&photo]);
        let res = remove_and_forget(
            Some(&conn),
            std::slice::from_ref(&photo),
            Method::Delete,
            &quiet(1),
        );
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(res[0].1, Outcome::Failed(_)), "{res:?}");
        assert!(photo.exists());
        assert_eq!(row_count(&conn), 1);
    }

    #[test]
    fn trash_moves_a_file_off_disk_or_reports_why_not() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("victim.jpg");
        let sidecar = dir.path().join("victim.jpg.xmp");
        std::fs::write(&f, b"x").unwrap();
        std::fs::write(&sidecar, b"<x:xmpmeta/>").unwrap();
        let conn = rows_for(&[&f]);
        let res = remove_and_forget(
            Some(&conn),
            std::slice::from_ref(&f),
            Method::Trash,
            &quiet(1),
        );
        // A platform with no trash here (no XDG trash in a container) fails
        // and keeps the row; otherwise the file and its sidecar are gone.
        match &res[0].1 {
            Outcome::Removed => {
                assert!(!f.exists() && !sidecar.exists());
                assert_eq!(row_count(&conn), 0);
            }
            Outcome::Failed(_) => assert_eq!(row_count(&conn), 1),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn without_a_database_files_still_go() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.jpg");
        std::fs::write(&f, b"x").unwrap();
        let res = remove_and_forget(None, std::slice::from_ref(&f), Method::Delete, &quiet(1));
        assert_eq!(res[0].1, Outcome::Removed);
        assert!(!f.exists());
    }
}
