use std::path::Path;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

/// Default ceiling for any single blocking file/subprocess operation that
/// touches a path supplied by the caller (e.g. a scanned media file). Chosen
/// to comfortably exceed a slow spinning disk or network share while still
/// surfacing a stale/disconnected mount point (which otherwise blocks the
/// underlying syscall forever on macOS) within one command's run.
pub const DEFAULT_IO_TIMEOUT: Duration = Duration::from_secs(20);

/// The operation did not complete within the given timeout. The spawned
/// worker thread is left to run to completion in the background (there is
/// no safe way to cancel a blocked syscall from the outside); this trades a
/// leaked thread for never hanging the caller.
pub struct TimedOut;

/// Runs `f` on a helper thread and waits up to `timeout` for it to finish.
/// Use this to bound any blocking call (`std::fs::*`, `image::open`, a
/// subprocess `.wait()`) that could otherwise block indefinitely against an
/// unresponsive mount point.
pub fn run_with_timeout<T, F>(timeout: Duration, f: F) -> Result<T, TimedOut>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(timeout).map_err(|_| TimedOut)
}

/// Outcome of waiting on a child process with a deadline.
#[derive(Debug, PartialEq, Eq)]
pub enum WaitOutcome {
    Success,
    Failed,
    TimedOut,
}

/// Polls `child` for completion, killing it if it hasn't exited within
/// `timeout`. Unlike a raw blocking `.wait()`/`.status()`, this guarantees
/// the caller gets control back within roughly `timeout` even if the child
/// itself is stuck on an unresponsive mount point.
pub fn wait_with_timeout(child: &mut std::process::Child, timeout: Duration) -> WaitOutcome {
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return if status.success() {
                    WaitOutcome::Success
                } else {
                    WaitOutcome::Failed
                };
            }
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return WaitOutcome::TimedOut;
                }
                thread::sleep(Duration::from_millis(50));
            }
            Err(_) => return WaitOutcome::Failed,
        }
    }
}

/// Whether a missing file's absence can be trusted as a real deletion.
///
/// `false` when the parent directory is *also* missing: that means the
/// directory, or the whole volume, is gone rather than this one file having
/// been deleted. `videre prune` uses this to avoid deleting every row for an
/// unmounted drive, which additionally destroys the embeddings and cached
/// thumbnails for those hashes (hours of recompute, against minutes to
/// re-scan the rows themselves).
///
/// Deliberately not a mount-table lookup. On macOS `/Volumes` reports the same
/// filesystem as `/` when nothing is mounted there, so an unmounted volume
/// leaves nothing to query; telling "unmounted" from "deleted directory" apart
/// exactly needs either platform-specific enumeration or state recorded at
/// scan time. This rule needs neither and behaves identically on Linux.
///
/// Bounded by `run_with_timeout`, because a stale NFS or SMB mount can hang
/// `metadata` indefinitely and a safety check that hangs is not a safety
/// check. A timeout returns `false`: an unanswerable question must never
/// authorise a deletion.
///
/// A path with no parent (`/`, or a bare relative name) also returns `false`,
/// since there is nothing to corroborate the absence against.
pub fn absence_is_trustworthy(path: &Path) -> bool {
    let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) else {
        return false;
    };
    let parent = parent.to_path_buf();
    run_with_timeout(DEFAULT_IO_TIMEOUT, move || parent.is_dir()).unwrap_or(false)
}

#[cfg(test)]
mod absence_tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("videre-absence-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_missing_file_in_an_existing_directory_is_trustworthy() {
        let dir = tmp("present");
        assert!(absence_is_trustworthy(&dir.join("gone.jpg")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_file_in_a_missing_directory_is_not() {
        // The unmounted-volume shape: neither the file nor its parent exists.
        let dir = tmp("absent");
        let nested = dir.join("subdir");
        assert!(!absence_is_trustworthy(&nested.join("gone.jpg")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_real_present_file_is_trustworthy_too() {
        // The function only judges the parent; callers ask it about paths they
        // already know are missing, but it must not depend on that.
        let dir = tmp("realfile");
        let f = dir.join("here.jpg");
        std::fs::write(&f, b"x").unwrap();
        assert!(absence_is_trustworthy(&f));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_path_without_a_usable_parent_is_not_trustworthy() {
        // Nothing to corroborate the absence against, so refuse rather than
        // authorise a deletion.
        assert!(!absence_is_trustworthy(Path::new("/")));
        assert!(!absence_is_trustworthy(Path::new("bare-name.jpg")));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_ok_when_operation_finishes_before_timeout() {
        let result = run_with_timeout(Duration::from_secs(1), || 42);
        assert!(result.is_ok());
        assert_eq!(result.ok(), Some(42));
    }

    #[test]
    fn returns_timed_out_when_operation_exceeds_timeout() {
        let result = run_with_timeout(Duration::from_millis(50), || {
            thread::sleep(Duration::from_secs(5));
            42
        });
        assert!(result.is_err());
    }

    #[test]
    fn wait_with_timeout_returns_success_for_fast_process() {
        let mut child = std::process::Command::new("true").spawn().unwrap();
        assert_eq!(
            wait_with_timeout(&mut child, Duration::from_secs(5)),
            WaitOutcome::Success
        );
    }

    #[test]
    fn wait_with_timeout_kills_and_returns_timed_out_for_slow_process() {
        let mut child = std::process::Command::new("sleep")
            .arg("5")
            .spawn()
            .unwrap();
        let start = Instant::now();
        assert_eq!(
            wait_with_timeout(&mut child, Duration::from_millis(200)),
            WaitOutcome::TimedOut
        );
        assert!(start.elapsed() < Duration::from_secs(2));
    }
}
