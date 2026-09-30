//! The `videre watch` the gallery starts beside it, so a library being browsed
//! keeps up without the user running a second command.
//!
//! It is a child process, not a thread: watch's model memory and any crash
//! stay out of the server, and its locks, stages and logs are exactly those
//! of a standalone `videre watch`. A watch already running for the library is
//! used as it is and never stopped. The one this starts is stopped when the
//! gallery returns, and `--exit-with` makes it stop by itself if the gallery
//! is killed first, so it is never left behind.
//!
//! Correctness never depends on it: every gallery action leaves the library
//! consistent on its own, and the watch only makes the redo happen sooner.

use crate::command_context::CommandContext;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// How long a stopped watch gets to finish up after SIGINT before it is
/// killed. Its stages are resumable, so a kill loses at most work in flight.
const STOP_GRACE: Duration = Duration::from_secs(10);

/// A watch this gallery started; stopped when dropped.
pub struct WatchChild(Child);

/// Start `videre watch` for the gallery's library, unless the library's
/// `gallery_starts_watch` setting is off or a watch is already running for
/// it. Never fails the gallery: a watch that cannot start is reported and the
/// gallery serves without one.
pub fn start(ctx: &CommandContext) -> Option<WatchChild> {
    let library = &ctx.library;
    if !library.settings.gallery_starts_watch {
        return None;
    }
    match videre_core::library_locks::command_locked(library, "watch") {
        Ok(true) => {
            tracing::info!("videre gallery: using the videre watch already running");
            return None;
        }
        Ok(false) => {}
        Err(e) => {
            tracing::warn!("videre gallery: not starting videre watch: {e:#}");
            return None;
        }
    }
    let spawned = std::env::current_exe().and_then(|exe| {
        Command::new(exe)
            .arg("--library")
            .arg(&library.paths.root)
            .arg("watch")
            .arg("--exit-with")
            .arg(std::process::id().to_string())
            // Watch writes its own `.videre/logs/watch.log`, and the gallery
            // shows its progress; its terminal output would only interleave.
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
    });
    match spawned {
        Ok(child) => {
            tracing::info!(
                "videre gallery: started videre watch; it stops with the gallery \
                 (turn off: videre config set gallery-starts-watch false)"
            );
            Some(WatchChild(child))
        }
        Err(e) => {
            tracing::warn!("videre gallery: could not start videre watch: {e}");
            None
        }
    }
}

impl Drop for WatchChild {
    /// SIGINT first, so watch records the interruption and flushes its logs,
    /// then a kill if it has not exited within [`STOP_GRACE`].
    fn drop(&mut self) {
        let child = &mut self.0;
        if matches!(child.try_wait(), Ok(Some(_))) {
            return;
        }
        if let Ok(pid) = i32::try_from(child.id()) {
            // SAFETY: kill(2) with a pid this process spawned and has not yet
            // reaped, so it cannot name an unrelated process.
            unsafe {
                libc::kill(pid, libc::SIGINT);
            }
        }
        let until = Instant::now() + STOP_GRACE;
        while Instant::now() < until {
            if matches!(child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = child.kill();
        let _ = child.wait();
    }
}
