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
