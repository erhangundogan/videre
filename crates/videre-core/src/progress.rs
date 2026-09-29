use crate::io_timeout::{ProgressHandle, ProgressSnapshot};
use indicatif::{ProgressBar, ProgressStyle};
use std::collections::VecDeque;
use std::io::IsTerminal;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Reports progress for a batch of N items as an in-place bar (brew/docker/
/// npm style) when stderr is a terminal, or periodic plain-text lines when
/// it isn't (piped to a file, CI log), so a long run never looks hung in a
/// log file, without per-item spam either way. `silent` suppresses the bar
/// and periodic lines entirely, but NOT skipped items (see `skip`) or the
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
    id: u64,
    total: u64,
    done: AtomicU64,
    mode: Mode,
    /// What is being counted, for the non-TTY line. "images" for most
    /// callers, but `videre locations` counts coordinates and clusters, and
    /// a log claiming "26744/26744 images processed" for a 70,601-file
    /// library is a number a reader cannot reconcile with anything.
    noun: &'static str,
    /// Files being read right now whose bytes are worth showing (see
    /// `track`), plus the ticker thread that renders them.
    tracked: Arc<Tracked>,
}

/// A file counter says nothing while one large file hashes: a 300 GiB video
/// printed nothing for minutes, which reads as a hang. So files at least this
/// big show their bytes beside the bar. Below it (about 7 s at 150 MB/s) the
/// file counter moves often enough on its own.
const LARGE_FILE_BYTES: u64 = 1 << 30;
/// How often the readout refreshes on a terminal.
const TICK: Duration = Duration::from_secs(1);
/// How often a non-terminal run logs the readout.
const PLAIN_EVERY: Duration = Duration::from_secs(30);
/// The rate is measured over this much recent history, so a stall shows as a
/// falling rate before the no-progress timeout skips the file.
const RATE_WINDOW: Duration = Duration::from_secs(5);

struct Entry {
    id: u64,
    name: String,
    handle: ProgressHandle,
    samples: VecDeque<(Instant, u64)>,
}

#[derive(Default)]
struct Tracked {
    entries: Mutex<Vec<Entry>>,
    next_id: AtomicU64,
    ticker: Mutex<Option<std::thread::JoinHandle<()>>>,
    stop: AtomicBool,
    ticker_exited: AtomicBool,
}

/// Keeps a file in the readout while it is being read; dropping it removes
/// the file.
pub struct TrackGuard {
    tracked: Arc<Tracked>,
    id: u64,
}

impl Drop for TrackGuard {
    fn drop(&mut self) {
        if let Ok(mut entries) = self.tracked.entries.lock() {
            entries.retain(|entry| entry.id != self.id);
        }
    }
}

/// One file as the readout shows it: `done` already clamped to `size`.
#[derive(Debug, Clone, PartialEq)]
struct Shown {
    name: String,
    done: u64,
    size: u64,
    /// `None` until there are two samples to measure between.
    rate: Option<u64>,
}

/// A file worth showing, or `None` below the size threshold or before its
/// size is known. Rereads can count past the size, so `done` is clamped.
fn shown_file(name: &str, seen: ProgressSnapshot, rate: Option<u64>) -> Option<Shown> {
    let size = seen.size.filter(|size| *size >= LARGE_FILE_BYTES)?;
    Some(Shown {
        name: name.to_owned(),
        done: seen.bytes.min(size),
        size,
        rate,
    })
}

/// The text beside the bar: one file by name, several summed. The rate is
/// left off until every shown file has one, since a missing rate is not a
/// stall and must not read as `0 B/s`.
fn readout(files: &[Shown]) -> Option<String> {
    let bytes = crate::disk::human_bytes;
    let rate = |rate: Option<u64>| {
        rate.map(|r| format!(", {}/s", bytes(r)))
            .unwrap_or_default()
    };
    match files {
        [] => None,
        [one] => Some(format!(
            "{} {} of {}{}",
            one.name,
            bytes(one.done),
            bytes(one.size),
            rate(one.rate)
        )),
        many => {
            let done = many.iter().map(|f| f.done).sum();
            let size = many.iter().map(|f| f.size).sum();
            let total = many.iter().map(|f| f.rate).sum::<Option<u64>>();
            Some(format!(
                "{} large files: {} of {}{}",
                many.len(),
                bytes(done),
                bytes(size),
                rate(total)
            ))
        }
    }
}

/// Bytes per second across the samples inside the last `RATE_WINDOW`: the
/// newest against the oldest in the window. A stall is `Some(0)`; fewer
/// than two samples is `None`, no measurement yet.
fn rate(samples: &VecDeque<(Instant, u64)>, now: Instant) -> Option<u64> {
    let recent: Vec<&(Instant, u64)> = samples
        .iter()
        .filter(|(at, _)| now.saturating_duration_since(*at) <= RATE_WINDOW)
        .collect();
    let (Some(first), Some(last)) = (recent.first(), recent.last()) else {
        return None;
    };
    let span = last.0.saturating_duration_since(first.0).as_secs_f64();
    if span <= 0.0 {
        return None;
    }
    Some((last.1.saturating_sub(first.1) as f64 / span) as u64)
}

/// The ticker: sample each tracked handle once a second, then show the
/// readout beside the bar, or log it every `PLAIN_EVERY` without a terminal.
fn run_ticker(tracked: Arc<Tracked>, bar: Option<ProgressBar>) {
    let mut last_line: Option<Instant> = None;
    while !tracked.stop.load(Ordering::SeqCst) {
        std::thread::park_timeout(TICK);
        if tracked.stop.load(Ordering::SeqCst) {
            break;
        }
        let now = Instant::now();
        let shown: Vec<Shown> = match tracked.entries.lock() {
            Ok(mut entries) => entries
                .iter_mut()
                .filter_map(|entry| {
                    let seen = entry.handle.snapshot();
                    entry.samples.push_back((now, seen.bytes));
                    while entry
                        .samples
                        .front()
                        .is_some_and(|(at, _)| now.saturating_duration_since(*at) > RATE_WINDOW)
                    {
                        entry.samples.pop_front();
                    }
                    shown_file(&entry.name, seen, rate(&entry.samples, now))
                })
                .collect(),
            Err(_) => break,
        };
        let text = readout(&shown);
        match &bar {
            Some(bar) => bar.set_message(text.unwrap_or_default()),
            None => {
                if let Some(text) = text {
                    if last_line.is_none_or(|at| at.elapsed() >= PLAIN_EVERY) {
                        tracing::info!("{text}");
                        last_line = Some(now);
                    }
                }
            }
        }
    }
    tracked.ticker_exited.store(true, Ordering::SeqCst);
}

enum Mode {
    Bar(ProgressBar),
    /// Non-TTY fallback: print one line every LOG_INTERVAL ticks.
    Plain,
    /// --silent: no bar, no periodic lines. Skipped items still print (see skip).
    Silent,
}

const LOG_INTERVAL: u64 = 25;

/// The bar currently on screen, with the id of the `Progress` that owns it,
/// so terminal output from elsewhere can hide it for the moment it writes.
static ACTIVE_BAR: std::sync::Mutex<Option<(u64, ProgressBar)>> = std::sync::Mutex::new(None);
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Run `f` with any active progress bar hidden, so a line written to the
/// terminal (by the logging layer) never tears the bar. A no-op when no bar
/// is showing.
pub fn with_bars_suspended<R>(f: impl FnOnce() -> R) -> R {
    let bar = ACTIVE_BAR
        .lock()
        .ok()
        .and_then(|b| b.as_ref().map(|(_, bar)| bar.clone()));
    match bar {
        Some(bar) => bar.suspend(f),
        None => f(),
    }
}

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
                ProgressStyle::with_template("{bar:40} {percent}% {msg}")
                    .unwrap()
                    .progress_chars("=> "),
            );
            Mode::Bar(bar)
        } else {
            Mode::Plain
        };
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        if let Mode::Bar(bar) = &mode {
            if let Ok(mut active) = ACTIVE_BAR.lock() {
                *active = Some((id, bar.clone()));
            }
        }
        Progress {
            id,
            total,
            done: AtomicU64::new(0),
            mode,
            noun: "images",
            tracked: Arc::new(Tracked::default()),
        }
    }

    /// A progress in the non-terminal mode, whatever the test's stderr is.
    #[cfg(test)]
    fn plain_for_test(total: u64) -> Self {
        let mut progress = Progress::new(total, true);
        progress.mode = Mode::Plain;
        progress
    }

    /// Show `name`'s bytes beside the bar while it is being read through
    /// `handle`, if it turns out to be a large file (see `LARGE_FILE_BYTES`),
    /// until the returned guard drops. The first tracked file starts the
    /// ticker that renders the readout; `--silent` shows nothing and starts
    /// no thread.
    pub fn track(&self, name: &str, handle: ProgressHandle) -> TrackGuard {
        let id = self.tracked.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let guard = TrackGuard {
            tracked: Arc::clone(&self.tracked),
            id,
        };
        let bar = match &self.mode {
            Mode::Silent => return guard,
            Mode::Bar(bar) => Some(bar.clone()),
            Mode::Plain => None,
        };
        if let Ok(mut entries) = self.tracked.entries.lock() {
            entries.push(Entry {
                id,
                name: name.to_owned(),
                handle,
                samples: VecDeque::new(),
            });
        }
        if let Ok(mut ticker) = self.tracked.ticker.lock() {
            if ticker.is_none() {
                let tracked = Arc::clone(&self.tracked);
                *ticker = std::thread::Builder::new()
                    .name("videre-progress".into())
                    .spawn(move || run_ticker(tracked, bar))
                    .ok();
            }
        }
        guard
    }

    /// Stop and join the readout ticker, if one was started.
    fn stop_ticker(&self) {
        self.tracked.stop.store(true, Ordering::SeqCst);
        let handle = self.tracked.ticker.lock().ok().and_then(|mut t| t.take());
        if let Some(handle) = handle {
            handle.thread().unpark();
            let _ = handle.join();
        }
    }

    /// `new`, counting something other than images. Affects only the
    /// non-TTY text line; the bar renders a percentage either way.
    pub fn new_counting(total: u64, silent: bool, noun: &'static str) -> Self {
        let mut progress = Progress::new(total, silent);
        progress.noun = noun;
        progress
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
                    tracing::info!("{}/{} {} processed", after, self.total, self.noun);
                }
            }
            Mode::Silent => {}
        }
    }

    /// Record one item that was not processed, as a warning naming it. Shown
    /// and logged regardless of `silent`: a skipped file is data that was not
    /// processed, not routine progress. The terminal line is written with the
    /// bar hidden (see `with_bars_suspended`), so it never tears the bar.
    pub fn skip(&self, subject: &str, err: anyhow::Error) {
        crate::error_log::report(
            tracing::Level::WARN,
            &err.context(format!("skipping {subject}")),
            Some(subject),
        );
    }

    /// Clears the bar (if any) so the final summary prints cleanly below it
    /// rather than being overwritten. Does not print anything itself, the
    /// caller assembles and prints its own summary line(s).
    pub fn finish(self) {
        // Before clearing, so the ticker cannot write a message after it.
        self.stop_ticker();
        if let Mode::Bar(bar) = &self.mode {
            bar.finish_and_clear();
        }
    }
}

impl Drop for Progress {
    /// Stop the readout ticker, and unregister this bar unless a newer one
    /// has taken its place.
    fn drop(&mut self) {
        self.stop_ticker();
        if let Ok(mut active) = ACTIVE_BAR.lock() {
            if active.as_ref().is_some_and(|(id, _)| *id == self.id) {
                *active = None;
            }
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
    fn a_skip_is_a_warning_with_the_subject_as_its_path_even_when_silent() {
        #[derive(Clone, Default)]
        struct Buf(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
        impl std::io::Write for Buf {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(b);
                Ok(b.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let buf = Buf::default();
        let w = buf.clone();
        let sub = tracing_subscriber::fmt()
            .json()
            .with_writer(move || w.clone())
            .finish();
        tracing::subscriber::with_default(sub, || {
            let p = Progress::new(5, true);
            p.skip(
                "/Fotoğraflar/Çağla.heic",
                anyhow::anyhow!("decode failed")
                    .context(crate::error_kind::ErrorKind::DecodeFailed),
            );
        });
        let line: serde_json::Value = serde_json::from_slice(&buf.0.lock().unwrap()).unwrap();
        assert_eq!(line["level"], "WARN");
        assert_eq!(line["fields"]["path"], "/Fotoğraflar/Çağla.heic");
        assert_eq!(
            line["fields"]["message"],
            "skipping /Fotoğraflar/Çağla.heic: the file could not be decoded: decode failed"
        );
        assert_eq!(line["fields"]["kind"], "decode_failed");
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

    const GB: u64 = 1 << 30;

    #[test]
    fn one_large_file_reads_with_its_name() {
        let shown = shown_file(
            "Fotoğraf_İzmir.mov",
            crate::io_timeout::ProgressSnapshot {
                bytes: 12 * GB,
                size: Some(300 * GB),
            },
            Some(158_000_000),
        )
        .unwrap();
        assert_eq!(
            readout(&[shown]).unwrap(),
            "Fotoğraf_İzmir.mov 12.0 GB of 300.0 GB, 150.7 MB/s"
        );
    }

    #[test]
    fn a_file_without_a_rate_yet_reads_without_one() {
        let shown = shown_file(
            "yeni.mov",
            crate::io_timeout::ProgressSnapshot {
                bytes: GB,
                size: Some(4 * GB),
            },
            None,
        )
        .unwrap();
        assert_eq!(readout(&[shown]).unwrap(), "yeni.mov 1.0 GB of 4.0 GB");
    }

    #[test]
    fn several_large_files_are_summed() {
        let a = shown_file(
            "a.mov",
            crate::io_timeout::ProgressSnapshot {
                bytes: GB,
                size: Some(2 * GB),
            },
            Some(1 << 20),
        )
        .unwrap();
        let b = shown_file(
            "b.mov",
            crate::io_timeout::ProgressSnapshot {
                bytes: 3 * GB,
                size: Some(4 * GB),
            },
            Some(1 << 20),
        )
        .unwrap();
        assert_eq!(
            readout(&[a, b]).unwrap(),
            "2 large files: 4.0 GB of 6.0 GB, 2.0 MB/s"
        );
    }

    #[test]
    fn files_below_a_gibibyte_or_without_a_size_are_not_shown() {
        let small = crate::io_timeout::ProgressSnapshot {
            bytes: 10,
            size: Some(GB - 1),
        };
        assert!(shown_file("küçük.jpg", small, None).is_none());
        let unknown = crate::io_timeout::ProgressSnapshot {
            bytes: 10,
            size: None,
        };
        assert!(shown_file("belirsiz.mov", unknown, None).is_none());
        assert!(readout(&[]).is_none());
    }

    #[test]
    fn done_never_exceeds_size() {
        let shown = shown_file(
            "tekrar.mov",
            crate::io_timeout::ProgressSnapshot {
                bytes: 3 * GB,
                size: Some(2 * GB),
            },
            None,
        )
        .unwrap();
        assert_eq!(shown.done, 2 * GB);
    }

    #[test]
    fn the_rate_is_the_last_five_seconds() {
        let now = std::time::Instant::now();
        let at =
            |secs_ago: u64, bytes: u64| (now - std::time::Duration::from_secs(secs_ago), bytes);
        // 1000 bytes long ago must not count; 500 bytes over the last 4 s do.
        let samples: std::collections::VecDeque<_> =
            [at(20, 0), at(10, 1000), at(4, 1000), at(0, 1500)].into();
        assert_eq!(rate(&samples, now), Some(125));
        // A stall reads as zero.
        let stalled: std::collections::VecDeque<_> = [at(4, 1500), at(0, 1500)].into();
        assert_eq!(rate(&stalled, now), Some(0));
        let single: std::collections::VecDeque<_> = [at(0, 1500)].into();
        // One sample is no rate at all, not a stall.
        assert_eq!(rate(&single, now), None);
    }

    #[test]
    fn finish_stops_the_ticker() {
        let p = Progress::plain_for_test(1);
        let handle = crate::io_timeout::ProgressHandle::new();
        let guard = p.track("büyük.mov", handle);
        let tracked = std::sync::Arc::clone(&p.tracked);
        assert!(!tracked.ticker_exited.load(Ordering::SeqCst));
        drop(guard);
        p.finish();
        assert!(tracked.ticker_exited.load(Ordering::SeqCst));
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
