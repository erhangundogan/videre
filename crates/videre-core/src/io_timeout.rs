use std::path::Path;
use std::sync::mpsc;
use std::sync::OnceLock;
use std::thread;
use std::time::{Duration, Instant};

/// Default ceiling for any single blocking file/subprocess operation that
/// touches a path supplied by the caller (e.g. a scanned media file). Chosen
/// to comfortably exceed a slow spinning disk or network share while still
/// surfacing a stale/disconnected mount point (which otherwise blocks the
/// underlying syscall forever on macOS) within one command's run.
pub const DEFAULT_IO_TIMEOUT: Duration = Duration::from_secs(20);

/// Assumed floor throughput for a whole-file read, in MB/s, used to scale the
/// timeout to the size of the file.
///
/// Deliberately far under real hardware: a USB SSD measured 158 MB/s on the
/// library that produced this mechanism. It is a floor, not an estimate, so a
/// degraded link still finishes rather than being declared unreachable.
pub const MIN_READ_RATE_MB_S_DEFAULT: u64 = 20;

/// Ceiling for a `stat`, which is *not* proportional to file size, so unlike a
/// read it is correctly bounded by a constant.
///
/// This is the liveness check: on a stale or disconnected mount `fs::metadata`
/// is itself one of the calls that blocks forever, so a mount that answers it
/// promptly can be trusted to have reported a real size.
pub const STAT_TIMEOUT: Duration = Duration::from_secs(5);

static MIN_READ_RATE_OVERRIDE: OnceLock<u64> = OnceLock::new();

/// Overrides the assumed floor read rate, from config. Call once at startup,
/// before any scan begins; later calls are ignored, as with the qlmanage
/// concurrency override this mirrors.
pub fn set_min_read_rate_mb_s(rate: u64) {
    let _ = MIN_READ_RATE_OVERRIDE.set(rate);
}

/// Pure resolution of the effective rate, split out from the `OnceLock` so the
/// "override if present, else default" logic is unit-testable without touching
/// process-wide state. Same split as `heic::resolve_qlmanage_concurrency`.
fn resolve_min_read_rate(override_val: Option<u64>) -> u64 {
    match override_val {
        // A zero rate would mean an unbounded timeout, reintroducing the hang
        // the whole mechanism exists to prevent. The config layer rejects it;
        // this refuses it again rather than trusting a single gate.
        Some(0) | None => MIN_READ_RATE_MB_S_DEFAULT,
        Some(n) => n,
    }
}

pub fn min_read_rate_mb_s() -> u64 {
    resolve_min_read_rate(MIN_READ_RATE_OVERRIDE.get().copied())
}

/// How long a whole-file read of `size_bytes` may take before it is considered
/// stalled rather than merely large.
///
/// A constant ceiling cannot tell those apart. Measured 2026-08-12: a healthy
/// 3.7 GB video on a drive sustaining 158 MB/s needs ~23s, and was being
/// skipped by a fixed 20s cap with a message blaming the drive. File sizes do
/// not change, so such a file was skipped on every run, forever.
///
/// Never returns less than `DEFAULT_IO_TIMEOUT`, so small files behave exactly
/// as before. Total by construction: saturating arithmetic (a debug build
/// panics on overflow, and computing a timeout must never be the thing that
/// crashes a scan) and a zero rate falls back to the default rather than
/// dividing by zero or returning an unbounded timeout. The config layer
/// rejects a zero rate too; this stays total regardless of who calls it.
pub fn timeout_for_size(size_bytes: u64, rate_mb_s: u64) -> Duration {
    let rate = if rate_mb_s == 0 {
        MIN_READ_RATE_MB_S_DEFAULT
    } else {
        rate_mb_s
    };
    let bytes_per_sec = rate.saturating_mul(1_000_000);
    let secs = size_bytes / bytes_per_sec.max(1);
    Duration::from_secs(secs).max(DEFAULT_IO_TIMEOUT)
}

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

/// Runs `f` with a timeout scaled to the size of `path`.
///
/// For operations that read a file *whole*, where duration really is
/// proportional to size. Not for decoding: a QuickLook poster frame reads a
/// fraction of a video, so scaling by full size would hand a multi-GB file
/// minutes for work that should take a second, and QuickLook hanging on a
/// container with no video track is a known failure mode this project already
/// had to bound.
///
/// The `stat` is bounded separately and *first*, by a constant. That ordering
/// is the safety property: a dead mount fails there, in `STAT_TIMEOUT`, and the
/// read is never attempted, so a large file on a dead mount cannot hang for its
/// scaled timeout. A failed or timed-out `stat` is reported as `TimedOut`
/// rather than guessed around.
pub fn run_with_timeout_for_path<T, F>(path: &Path, f: F) -> Result<T, TimedOut>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let owned = path.to_path_buf();
    let size = run_with_timeout(STAT_TIMEOUT, move || {
        std::fs::metadata(&owned).map(|m| m.len()).ok()
    })
    .map_err(|_| TimedOut)?
    .ok_or(TimedOut)?;
    run_with_timeout(timeout_for_size(size, min_read_rate_mb_s()), f)
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

#[cfg(test)]
mod size_timeout_tests {
    use super::*;

    #[test]
    fn a_small_file_gets_exactly_the_old_constant() {
        // Nothing may get *less* time than before this existed.
        assert_eq!(timeout_for_size(0, 20), DEFAULT_IO_TIMEOUT);
        assert_eq!(timeout_for_size(1_000_000, 20), DEFAULT_IO_TIMEOUT);
        // The crossover: below 400 MB at 20 MB/s the default still wins.
        assert_eq!(timeout_for_size(399_000_000, 20), DEFAULT_IO_TIMEOUT);
    }

    #[test]
    fn the_file_that_produced_this_bug_now_gets_enough_time() {
        // 3.7 GB, skipped on a drive measured at 158 MB/s where a full read
        // needs ~23s, against a fixed 20s cap.
        let t = timeout_for_size(3_700_000_000, 20);
        assert_eq!(t.as_secs(), 185);
        assert!(t.as_secs() > 23, "must exceed the real read time");
    }

    #[test]
    fn the_largest_file_in_the_measured_library_is_bounded_and_finite() {
        assert_eq!(timeout_for_size(5_720_000_000, 20).as_secs(), 286);
    }

    #[test]
    fn a_zero_rate_falls_back_rather_than_dividing_by_zero() {
        // An unbounded timeout would reintroduce the hang this prevents.
        assert_eq!(timeout_for_size(3_700_000_000, 0).as_secs(), 185);
    }

    #[test]
    fn absurd_sizes_neither_panic_nor_overflow() {
        // A debug build panics on overflowing arithmetic, and computing a
        // timeout must never be the thing that crashes a scan.
        let t = timeout_for_size(u64::MAX, 1);
        assert!(t >= DEFAULT_IO_TIMEOUT);
        assert_eq!(timeout_for_size(u64::MAX, u64::MAX), DEFAULT_IO_TIMEOUT);
    }

    #[test]
    fn resolve_uses_the_override_but_refuses_zero() {
        assert_eq!(resolve_min_read_rate(Some(50)), 50);
        assert_eq!(resolve_min_read_rate(None), MIN_READ_RATE_MB_S_DEFAULT);
        assert_eq!(resolve_min_read_rate(Some(0)), MIN_READ_RATE_MB_S_DEFAULT);
    }

    #[test]
    fn a_dead_path_fails_at_the_stat_rather_than_running_the_body() {
        // The safety property: no size means no read, so a large file on a
        // dead mount cannot hang for its scaled timeout.
        let r = run_with_timeout_for_path(
            std::path::Path::new("/nonexistent/videre/definitely-not-here"),
            || 42,
        );
        assert!(r.is_err());
    }

    #[test]
    fn a_real_file_runs_the_body() {
        let d = std::env::temp_dir().join(format!("videre-sz-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("x.bin");
        std::fs::write(&f, b"hello").unwrap();
        assert_eq!(run_with_timeout_for_path(&f, || 42).ok(), Some(42));
        let _ = std::fs::remove_dir_all(&d);
    }
}
