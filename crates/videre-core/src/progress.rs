use indicatif::{ProgressBar, ProgressStyle};
use std::io::IsTerminal;
use std::sync::atomic::{AtomicU64, Ordering};

/// Reports progress for a batch of N items as an in-place bar (brew/docker/
/// npm style) when stderr is a terminal, or periodic plain-text lines when
/// it isn't (piped to a file, CI log), so a long run never looks hung in a
/// log file, without per-item spam either way. `silent` suppresses the bar
/// and periodic lines entirely, but NOT error output (see `println`) or the
/// caller's own decision about whether to print a final summary.
///
/// Does not track elapsed time itself: callers that need it (e.g.
/// `faces.rs`, whose summary spans both detection and clustering, not just
/// the `Progress`-tracked detection phase) should use their own `Instant`
/// spanning whatever the summary needs to cover.
///
/// Safe to share across threads: every method takes `&self`, so a single
/// `Progress` value can be ticked concurrently from multiple `rayon`
/// worker threads (e.g. from inside a `.par_iter()` closure) with no
/// external `Arc`/`Mutex` wrapping needed at the call site.
pub struct Progress {
    total: u64,
    done: AtomicU64,
    mode: Mode,
    /// What is being counted, for the non-TTY line. "images" for most
    /// callers, but `videre locations` counts coordinates and clusters, and
    /// a log claiming "26744/26744 images processed" for a 70,601-file
    /// library is a number a reader cannot reconcile with anything.
    noun: &'static str,
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
        Progress {
            total,
            done: AtomicU64::new(0),
            mode,
            noun: "images",
        }
    }

    /// `new`, counting something other than images. Affects only the
    /// non-TTY text line; the bar renders a percentage either way.
    pub fn new_counting(total: u64, silent: bool, noun: &'static str) -> Self {
        Progress {
            noun,
            ..Progress::new(total, silent)
        }
    }

    /// Advance by one item. Safe to call concurrently from multiple threads
    /// (e.g. from inside a `rayon` `.par_iter()` closure) via a shared
    /// `&Progress`, no external synchronization needed.
    pub fn tick(&self) {
        self.tick_by(1);
    }

    /// Advance by `n` items at once (for callers that complete work in
    /// batches rather than one item at a time, e.g. `videre embed`'s
    /// chunked pipeline). `n` must not exceed the number of items remaining
    /// toward `total` (mirrors the same implicit contract `tick()` already
    /// has: callers are responsible for not calling it more times, or with
    /// a larger cumulative `n`, than `total` allows). Safe to call
    /// concurrently from multiple threads, same as `tick()`.
    pub fn tick_by(&self, n: u64) {
        let before = self.done.fetch_add(n, Ordering::Relaxed);
        let after = before + n;
        match &self.mode {
            Mode::Bar(bar) => bar.set_position(after),
            Mode::Plain => {
                if after / LOG_INTERVAL != before / LOG_INTERVAL || after == self.total {
                    eprintln!("{}/{} {} processed", after, self.total, self.noun);
                }
            }
            Mode::Silent => {}
        }
    }

    /// Print a line that survives an active progress bar without corrupting
    /// its rendering. Always prints, regardless of `silent`, matches the
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
    /// rather than being overwritten. Does not print anything itself, the
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
        let p = Progress::new(10, true);
        for _ in 0..10 {
            p.tick();
        }
        p.finish();
    }

    #[test]
    fn silent_mode_println_still_prints() {
        // println() must not panic in silent mode; it always writes to
        // stderr regardless of `silent` (verified by not panicking here;
        // capturing stderr output itself is not practical in a unit test).
        let p = Progress::new(5, true);
        p.println("an error message");
    }

    #[test]
    fn zero_total_does_not_panic() {
        let p = Progress::new(0, true);
        p.tick();
        p.finish();
    }

    #[test]
    fn silent_mode_tick_by_does_not_panic() {
        let p = Progress::new(100, true);
        p.tick_by(40);
        p.tick_by(60);
        p.finish();
    }

    #[test]
    fn a_caller_can_count_something_other_than_images() {
        // Regression guard for a real wrong-noun bug: `videre locations`
        // counts coordinates, and printed "26744/26744 images processed"
        // for a library with 70,601 files and 37,767 photos with GPS.
        let p = Progress::new_counting(10, true, "coordinates");
        assert_eq!(p.noun, "coordinates");
        assert_eq!(Progress::new(10, true).noun, "images");
    }

    #[test]
    fn concurrent_tick_from_multiple_threads_reaches_correct_total() {
        use std::sync::Arc;
        let progress = Arc::new(Progress::new(1000, true));
        let handles: Vec<_> = (0..10)
            .map(|_| {
                let p = Arc::clone(&progress);
                std::thread::spawn(move || {
                    for _ in 0..100 {
                        p.tick();
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(progress.done.load(Ordering::Relaxed), 1000);
    }
}

/// Formats a duration the way a person reads one.
///
/// `stats` printed raw milliseconds, so a run that took an hour and a half read
/// as `5412000ms`, and every finished-in line elsewhere printed whole seconds,
/// so a two-hour `faces` run said `done in 7284s`. Both are technically the
/// number and neither is the answer to "how long did that take".
///
/// Lives here rather than in `stats` because four commands print elapsed time -
/// `embed`, `classify`, `faces` and `locations` - and each had rolled its own.
///
/// Sub-second keeps milliseconds, since that is the resolution that matters
/// when something is fast. Above a minute the seconds are dropped from the
/// hours form: nobody reads `2h 14m 7s`.
pub fn human_duration(d: std::time::Duration) -> String {
    let ms = d.as_millis();
    if ms < 1000 {
        return format!("{ms}ms");
    }
    let secs = d.as_secs();
    match secs {
        0..=9 => {
            // One decimal only while it still carries information: at 3.2s the
            // tenths are a tenth of the runtime, at 41s they are noise.
            let s = d.as_millis() as f64 / 1000.0;
            format!("{s:.1}s")
        }
        10..=59 => format!("{secs}s"),
        60..=3599 => {
            let (m, s) = (secs / 60, secs % 60);
            if s == 0 {
                format!("{m}m")
            } else {
                format!("{m}m {s}s")
            }
        }
        _ => {
            let (h, m) = (secs / 3600, (secs % 3600) / 60);
            if m == 0 {
                format!("{h}h")
            } else {
                format!("{h}h {m}m")
            }
        }
    }
}

/// `human_duration` for a millisecond count, which is how `pipeline_runs`
/// stores what it recorded.
pub fn human_duration_ms(ms: u64) -> String {
    human_duration(std::time::Duration::from_millis(ms))
}

#[cfg(test)]
mod duration_tests {
    use super::{human_duration, human_duration_ms};
    use std::time::Duration;

    #[test]
    fn reads_the_way_a_person_would_say_it() {
        let cases = [
            (Duration::from_millis(0), "0ms"),
            (Duration::from_millis(840), "840ms"),
            (Duration::from_millis(1000), "1.0s"),
            (Duration::from_millis(3240), "3.2s"),
            (Duration::from_secs(41), "41s"),
            (Duration::from_secs(59), "59s"),
            (Duration::from_secs(60), "1m"),
            (Duration::from_secs(95), "1m 35s"),
            (Duration::from_secs(3599), "59m 59s"),
            (Duration::from_secs(3600), "1h"),
            (Duration::from_secs(8040), "2h 14m"),
        ];
        for (d, want) in cases {
            assert_eq!(human_duration(d), want, "for {d:?}");
        }
    }

    #[test]
    fn the_millisecond_form_matches() {
        // What `stats` has: pipeline_runs stores duration_ms.
        assert_eq!(human_duration_ms(0), "0ms");
        assert_eq!(human_duration_ms(5_412_000), "1h 30m");
    }

    #[test]
    fn no_unit_is_ever_shown_as_zero() {
        // "2h 0m" and "1m 0s" are noise; the shorter form says the same thing.
        for secs in [3600, 7200, 60, 120, 600] {
            let s = human_duration(Duration::from_secs(secs));
            assert!(!s.contains(" 0m") && !s.contains(" 0s"), "{secs}s gave {s}");
        }
    }
}
