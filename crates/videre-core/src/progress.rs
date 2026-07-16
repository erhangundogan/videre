use indicatif::{ProgressBar, ProgressStyle};
use std::io::IsTerminal;

/// Reports progress for a batch of N items as an in-place bar (brew/docker/
/// npm style) when stderr is a terminal, or periodic plain-text lines when
/// it isn't (piped to a file, CI log) - so a long run never looks hung in a
/// log file, without per-item spam either way. `silent` suppresses the bar
/// and periodic lines entirely, but NOT error output (see `println`) or the
/// caller's own decision about whether to print a final summary.
///
/// Does not track elapsed time itself: callers that need it (e.g.
/// `faces.rs`, whose summary spans both detection and clustering, not just
/// the `Progress`-tracked detection phase) should use their own `Instant`
/// spanning whatever the summary needs to cover.
pub struct Progress {
    total: u64,
    done: u64,
    mode: Mode,
}

enum Mode {
    Bar(ProgressBar),
    /// Non-TTY fallback: print one line every LOG_INTERVAL ticks.
    Plain,
    /// --silent: no bar, no periodic lines. Errors still print (see println).
    Silent,
}

const LOG_INTERVAL: u64 = 25;

impl Progress {
    /// Creates a progress reporter for `total` items. When stderr is a TTY,
    /// renders an in-place bar. When it isn't, falls back to one plain-text
    /// line every `LOG_INTERVAL` items. `silent` suppresses both.
    pub fn new(total: u64, silent: bool) -> Self {
        let mode = if silent {
            Mode::Silent
        } else if std::io::stderr().is_terminal() {
            let bar = ProgressBar::new(total);
            bar.set_style(
                ProgressStyle::with_template("{bar:40} {percent}%")
                    .unwrap()
                    .progress_chars("=> "),
            );
            Mode::Bar(bar)
        } else {
            Mode::Plain
        };
        Progress { total, done: 0, mode }
    }

    /// Advance by one item.
    pub fn tick(&mut self) {
        self.done += 1;
        match &self.mode {
            Mode::Bar(bar) => bar.set_position(self.done),
            Mode::Plain => {
                if self.done.is_multiple_of(LOG_INTERVAL) || self.done == self.total {
                    eprintln!("{}/{} images processed", self.done, self.total);
                }
            }
            Mode::Silent => {}
        }
    }

    /// Print a line that survives an active progress bar without corrupting
    /// its rendering. Always prints, regardless of `silent` - matches the
    /// existing unconditional behavior of per-image error messages
    /// (`detect failed ...`, `embed_batch failed ...`, `write failed ...`),
    /// which must stay visible even under --silent since they indicate data
    /// loss, not routine progress.
    pub fn println(&self, msg: &str) {
        match &self.mode {
            Mode::Bar(bar) => bar.println(msg),
            Mode::Plain | Mode::Silent => eprintln!("{msg}"),
        }
    }

    /// Clears the bar (if any) so the final summary prints cleanly below it
    /// rather than being overwritten. Does not print anything itself - the
    /// caller assembles and prints its own summary line(s).
    pub fn finish(self) {
        if let Mode::Bar(bar) = self.mode {
            bar.finish_and_clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silent_mode_tick_does_not_panic() {
        let mut p = Progress::new(10, true);
        for _ in 0..10 {
            p.tick();
        }
        p.finish();
    }

    #[test]
    fn silent_mode_println_still_prints() {
        // println() must not panic in silent mode; it always writes to
        // stderr regardless of `silent` (verified by not panicking here -
        // capturing stderr output itself is not practical in a unit test).
        let p = Progress::new(5, true);
        p.println("an error message");
    }

    #[test]
    fn zero_total_does_not_panic() {
        let mut p = Progress::new(0, true);
        p.tick();
        p.finish();
    }
}
