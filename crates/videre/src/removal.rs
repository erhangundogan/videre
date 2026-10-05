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
///
/// With `Method::Trash` and a `manifest`, each file moved to the trash is
/// recorded there as it goes: its content hash and size, so `dedupe --undo`
/// can prove a trash copy is the same file, and where it landed when the
/// platform says.
pub fn remove_and_forget(
    conn: Option<&rusqlite::Connection>,
    paths: &[PathBuf],
    method: Method,
    progress: &videre_core::progress::Progress,
    mut manifest: Option<&mut crate::trash_stack::Writer>,
) -> Vec<(PathBuf, Outcome)> {
    let mut forget = conn.and_then(|c| c.prepare("DELETE FROM file_hashes WHERE path = ?1").ok());
    let mut identity = conn.and_then(|c| {
        c.prepare("SELECT hash, coalesce(size_bytes, 0) FROM file_hashes WHERE path = ?1")
            .ok()
    });
    paths
        .iter()
        .map(|path| {
            // Read before the row is forgotten: the manifest needs the hash.
            let known: Option<(String, i64)> = identity.as_mut().and_then(|stmt| {
                stmt.query_row([path.to_string_lossy()], |r| Ok((r.get(0)?, r.get(1)?)))
                    .ok()
            });
            let (outcome, moved, sidecar) = remove_one(path, method);
            if let (Some(file), Some(w), Some((hash, size))) =
                (moved, manifest.as_deref_mut(), known)
            {
                let entry = crate::trash_stack::Entry {
                    file,
                    hash,
                    size: size.max(0) as u64,
                    sidecar,
                };
                if let Err(e) = w.record(&entry) {
                    tracing::warn!("trashed {path:?} but could not record it for --undo: {e}");
                }
            }
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

/// Move one file to the trash, returning where it landed when the platform
/// says.
///
/// macOS goes through `NSFileManager` directly. The `trash` crate's default
/// there asks Finder through one AppleScript call per file: about 4 files a
/// second, blocking behind any dialog Finder shows, and needing permission to
/// script Finder. `NSFileManager` measured about 400 a second, needs none, and
/// reports the landing path, which the crate does not pass on.
#[cfg(target_os = "macos")]
fn to_trash(path: &Path) -> Result<Option<PathBuf>, String> {
    use objc2::rc::Retained;
    use objc2_foundation::{NSFileManager, NSString, NSURL};
    let Some(utf8) = path.to_str() else {
        // NSURL takes a string; a non-UTF-8 name goes through the crate,
        // which percent-encodes it, and is not recorded.
        use trash::macos::{DeleteMethod, TrashContextExtMacos};
        let mut ctx = trash::TrashContext::default();
        ctx.set_delete_method(DeleteMethod::NsFileManager);
        return ctx.delete(path).map(|()| None).map_err(|e| e.to_string());
    };
    let url = NSURL::fileURLWithPath(&NSString::from_str(utf8));
    let mut landed: Option<Retained<NSURL>> = None;
    NSFileManager::defaultManager()
        .trashItemAtURL_resultingItemURL_error(&url, Some(&mut landed))
        .map_err(|e| e.localizedDescription().to_string())?;
    Ok(landed
        .and_then(|u| u.path())
        .map(|p| PathBuf::from(p.to_string())))
}

/// Elsewhere the freedesktop.org trash, whose landing `--undo` finds through
/// the trash's own listing.
#[cfg(not(target_os = "macos"))]
fn to_trash(path: &Path) -> Result<Option<PathBuf>, String> {
    trash::delete(path).map(|()| None).map_err(|e| e.to_string())
}

type Moved = crate::trash_stack::Moved;

/// Remove one path and its sidecar; for a trashed file, also what moved where.
fn remove_one(path: &Path, method: Method) -> (Outcome, Option<Moved>, Option<Moved>) {
    match path.symlink_metadata() {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return (Outcome::AlreadyGone, None, None)
        }
        Err(e) => return (Outcome::Failed(e.to_string()), None, None),
        Ok(_) => {}
    }
    let remove = |p: &Path| -> Result<Option<Moved>, String> {
        match method {
            Method::Trash => to_trash(p).map(|trashed| {
                Some(Moved {
                    path: p.to_path_buf(),
                    trashed,
                })
            }),
            Method::Delete => std::fs::remove_file(p)
                .map(|()| None)
                .map_err(|e| e.to_string()),
        }
    };
    let moved = match remove(path) {
        Ok(m) => m,
        Err(e) => return (Outcome::Failed(e), None, None),
    };
    // The sidecar goes with its photo, so none is left describing a photo
    // that is gone. One that will not go is warned about, and never turns
    // the photo's removal into a failure.
    let sidecar_path = crate::xmp::write::sidecar_path(path);
    let mut sidecar = None;
    if sidecar_path.exists() {
        match remove(&sidecar_path) {
            Ok(m) => sidecar = m,
            Err(e) => tracing::warn!("could not remove the sidecar {sidecar_path:?}: {e}"),
        }
    }
    (Outcome::Removed, moved, sidecar)
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
        conn.execute_batch(
            "CREATE TABLE file_hashes (path TEXT PRIMARY KEY, hash TEXT, size_bytes INTEGER);",
        )
            .unwrap();
        for p in paths {
            conn.execute(
                "INSERT INTO file_hashes VALUES (?1, 'h', 1)",
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
            None,
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
                remove_and_forget(Some(&conn), std::slice::from_ref(&gone), method, &quiet(1), None);
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
            None,
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
        let state = tempfile::tempdir().unwrap();
        let mut manifest = crate::trash_stack::Writer::new(state.path(), "dedupe");
        let res = remove_and_forget(
            Some(&conn),
            std::slice::from_ref(&f),
            Method::Trash,
            &quiet(1),
            Some(&mut manifest),
        );
        // A platform with no trash here (no XDG trash in a container) fails
        // and keeps the row; otherwise the file and its sidecar are gone,
        // and the run's manifest records both.
        let stack = crate::trash_stack::runs(state.path()).unwrap();
        match &res[0].1 {
            Outcome::Removed => {
                assert!(!f.exists() && !sidecar.exists());
                assert_eq!(row_count(&conn), 0);
                let run = crate::trash_stack::read(&stack[0]).unwrap();
                assert_eq!(run.entries.len(), 1);
                let e = &run.entries[0];
                assert_eq!((e.file.path.as_path(), e.hash.as_str(), e.size), (f.as_path(), "h", 1));
                let side = e.sidecar.as_ref().expect("the sidecar is recorded");
                assert_eq!(side.path, sidecar);
                // Where each landed is recorded on macOS, so --undo needs no
                // listing of the Trash. Put both back so the test leaves
                // nothing in it.
                if cfg!(target_os = "macos") {
                    let landed = e.file.trashed.as_ref().expect("macOS records the landing");
                    assert_eq!(std::fs::read(landed).unwrap(), b"x");
                    std::fs::rename(landed, &f).unwrap();
                    std::fs::rename(side.trashed.as_ref().unwrap(), &sidecar).unwrap();
                }
            }
            Outcome::Failed(_) => {
                assert_eq!(row_count(&conn), 1);
                assert!(stack.is_empty(), "nothing moved, nothing recorded");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn without_a_database_files_still_go() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.jpg");
        std::fs::write(&f, b"x").unwrap();
        let res = remove_and_forget(None, std::slice::from_ref(&f), Method::Delete, &quiet(1), None);
        assert_eq!(res[0].1, Outcome::Removed);
        assert!(!f.exists());
    }

    #[test]
    fn only_files_trashed_are_recorded_and_delete_records_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let gone = dir.path().join("silinmiş-edited.jpg");
        let f = dir.path().join("kopya.jpg");
        std::fs::write(&f, b"x").unwrap();
        let conn = rows_for(&[&gone, &f]);
        let state = tempfile::tempdir().unwrap();

        let mut manifest = crate::trash_stack::Writer::new(state.path(), "dedupe");
        remove_and_forget(
            Some(&conn),
            std::slice::from_ref(&gone),
            Method::Trash,
            &quiet(1),
            Some(&mut manifest),
        );
        assert!(crate::trash_stack::runs(state.path()).unwrap().is_empty());

        let mut manifest = crate::trash_stack::Writer::new(state.path(), "dedupe");
        remove_and_forget(
            Some(&conn),
            std::slice::from_ref(&f),
            Method::Delete,
            &quiet(1),
            Some(&mut manifest),
        );
        assert!(!f.exists());
        assert!(crate::trash_stack::runs(state.path()).unwrap().is_empty());
    }
}
