//! The process's single `tracing` subscriber: a terminal layer that prints
//! what videre always printed, plus the per-command log files under
//! `.videre/logs/`. Installed by `main` once the command and library are
//! known; library crates only emit events.
//!
//! Events from threads that did not enter the run span (rayon workers) carry
//! no `command`/`run` fields. The reader takes the command from the file name
//! and gives such a line the run of the line before it, and `report` writes
//! the stage explicitly, so per-file skips are still attributed.

use std::io::Write;
use std::path::Path;
use tracing::Level;
use tracing_subscriber::{filter, fmt, layer::SubscriberExt, Layer};
use videre_core::error_log::{
    primary_log_name, trace_log_name, FILE_ONLY_TARGET, RUN_MARKER_TARGET,
};
use videre_core::library_config::{LibraryConfig, LogFormat, LogLevel};

type Boxed = Box<dyn Layer<tracing_subscriber::Registry> + Send + Sync>;
type Writer = tracing_appender::non_blocking::NonBlocking;

/// Keeps the file writers alive; dropping it flushes them (each flush is
/// bounded by `tracing-appender`, about a second) and reports dropped lines.
pub struct LogGuard {
    workers: Vec<tracing_appender::non_blocking::WorkerGuard>,
    counters: Vec<tracing_appender::non_blocking::ErrorCounter>,
    pub run_span: tracing::Span,
}

impl Drop for LogGuard {
    fn drop(&mut self) {
        // Each WorkerGuard flushes its writer as it drops.
        self.workers.clear();
        let dropped: usize = self.counters.iter().map(|c| c.dropped_lines()).sum();
        if dropped > 0 {
            eprintln!(
                "warning: {dropped} log line(s) could not be written in time and were dropped"
            );
        }
    }
}

fn run_id() -> String {
    format!(
        "{}-{}",
        chrono::Utc::now().format("%Y%m%dT%H%M%SZ"),
        std::process::id()
    )
}

/// Only videre's own events reach any layer; dependencies that use
/// `tracing` (the model hub, ONNX Runtime) stay out of the terminal and the
/// files.
fn videre_only(meta: &tracing::Metadata<'_>) -> bool {
    meta.target().starts_with("videre")
}

fn to_level(level: LogLevel) -> Level {
    match level {
        LogLevel::Error => Level::ERROR,
        LogLevel::Warn => Level::WARN,
        LogLevel::Info => Level::INFO,
        LogLevel::Debug => Level::DEBUG,
    }
}

/// A size-rotating, non-blocking writer for one file, or `None` when the
/// file cannot be used, which disables that file and never the command.
fn file_writer(
    path: &Path,
    settings: &LibraryConfig,
) -> Option<(Writer, tracing_appender::non_blocking::WorkerGuard)> {
    if let Err(e) = videre_core::error_log::check_log_file(path) {
        eprintln!("warning: file logging disabled: {e:#}");
        return None;
    }
    let mut options = std::fs::OpenOptions::new();
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.read(true).create(true).append(true).mode(0o600);
    }
    let bytes = usize::try_from(settings.log_max_size_mb)
        .unwrap_or(usize::MAX)
        .saturating_mul(1024 * 1024);
    let keep = usize::try_from(settings.log_keep).unwrap_or(usize::MAX);
    let owned = path.to_path_buf();
    // file-rotate panics if it cannot create the directory. The directory was
    // prepared and checked already, and a logging failure must not end the
    // command, so a panic here only disables this file.
    let rotate = std::panic::catch_unwind(move || {
        file_rotate::FileRotate::new(
            owned,
            file_rotate::suffix::AppendCount::new(keep),
            file_rotate::ContentLimit::BytesSurpassed(bytes),
            file_rotate::compression::Compression::None,
            Some(options),
        )
    });
    let Ok(rotate) = rotate else {
        eprintln!(
            "warning: file logging disabled: could not open {}",
            path.display()
        );
        return None;
    };
    Some(
        tracing_appender::non_blocking::NonBlockingBuilder::default()
            .lossy(true)
            .thread_name("videre-log")
            .finish(rotate),
    )
}

fn file_layer(format: LogFormat, writer: Writer, max: Level) -> Boxed {
    // Spans always pass so their fields (command, run, stage) reach the file,
    // and so does the run's start marker; other events pass at or above `max`.
    let keep = filter::filter_fn(move |meta| {
        videre_only(meta)
            && (meta.is_span() || meta.target() == RUN_MARKER_TARGET || *meta.level() <= max)
    });
    match format {
        LogFormat::Json => fmt::layer()
            .json()
            .with_span_list(true)
            .with_current_span(false)
            .with_writer(writer)
            .with_filter(keep)
            .boxed(),
        LogFormat::Text => tracing_logfmt::builder()
            .with_span_path(false)
            .layer()
            .with_writer(writer)
            .with_filter(keep)
            .boxed(),
    }
}

/// Prints what videre always printed: an `error: ` or `warning: ` prefix,
/// info plain, debug never, and nothing from the file-only target.
struct TerminalFormat;

impl<S, N> fmt::FormatEvent<S, N> for TerminalFormat
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
    N: for<'a> fmt::FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        _ctx: &fmt::FmtContext<'_, S, N>,
        mut writer: fmt::format::Writer<'_>,
        event: &tracing::Event<'_>,
    ) -> std::fmt::Result {
        let prefix = match *event.metadata().level() {
            Level::ERROR => "error: ",
            Level::WARN => "warning: ",
            _ => "",
        };
        let mut message = String::new();
        event.record(&mut MessageOnly(&mut message));
        writeln!(writer, "{prefix}{message}")
    }
}

/// Collects only the event's message; structured fields are for the files.
struct MessageOnly<'a>(&'a mut String);

impl tracing::field::Visit for MessageOnly<'_> {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.0.push_str(value);
        }
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            use std::fmt::Write as _;
            let _ = write!(self.0, "{value:?}");
        }
    }
}

/// stderr, written with any progress bar hidden for the moment.
struct Stderr;

impl Write for Stderr {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        videre_core::progress::with_bars_suspended(|| std::io::stderr().write_all(buf))?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        std::io::stderr().flush()
    }
}

fn terminal_layer() -> Boxed {
    fmt::layer()
        .event_format(TerminalFormat)
        .with_writer(|| Stderr)
        .with_filter(filter::filter_fn(|meta| {
            videre_only(meta)
                && meta.target() != FILE_ONLY_TARGET
                && meta.target() != RUN_MARKER_TARGET
                && (meta.is_span() || *meta.level() <= Level::INFO)
        }))
        .boxed()
}

/// Everything one run needs to write logs, not yet installed. `dir` is the
/// logs directory, or `None` for terminal output only.
pub fn build(
    dir: Option<&Path>,
    command: &str,
    settings: &LibraryConfig,
    run: &str,
) -> (tracing::Dispatch, LogGuard) {
    let mut layers: Vec<Boxed> = vec![terminal_layer()];
    let mut workers = Vec::new();
    let mut counters = Vec::new();
    if let Some(dir) = dir {
        videre_core::error_log::sweep_expired(dir, command, settings);
        // The primary file always keeps errors and warnings, whatever the
        // level: it is the record status reads, and a chatty level must never
        // rotate them out of retention, nor a quiet one leave them out.
        if let Some((w, g)) = file_writer(&dir.join(primary_log_name(command)), settings) {
            counters.push(w.error_counter());
            workers.push(g);
            layers.push(file_layer(settings.log_format, w, Level::WARN));
        }
        if settings.log_level >= LogLevel::Info {
            if let Some((w, g)) = file_writer(&dir.join(trace_log_name(command)), settings) {
                counters.push(w.error_counter());
                workers.push(g);
                layers.push(file_layer(
                    settings.log_format,
                    w,
                    to_level(settings.log_level),
                ));
            }
        }
    }
    let dispatch = tracing::Dispatch::new(tracing_subscriber::registry().with(layers));
    // Created under this dispatcher so its fields reach these layers.
    let run_span = tracing::dispatcher::with_default(
        &dispatch,
        || tracing::info_span!("run", command = %command, run = %run),
    );
    (
        dispatch,
        LogGuard {
            workers,
            counters,
            run_span,
        },
    )
}

static GLOBAL_GUARD: std::sync::Mutex<Option<LogGuard>> = std::sync::Mutex::new(None);

/// Flush and release the installed writers. Registered as the out-of-band
/// exit hook, so an interrupted run keeps its last lines too.
fn flush_installed() {
    if let Ok(mut slot) = GLOBAL_GUARD.lock() {
        slot.take();
    }
}

/// Holds the run span entered; dropping it leaves the span and flushes the
/// files.
pub struct Installed {
    entered: Option<tracing::span::EnteredSpan>,
}

impl Drop for Installed {
    fn drop(&mut self) {
        self.entered.take();
        flush_installed();
    }
}

/// Install logging for this process. File logging is best effort: any
/// failure leaves the terminal layer alone, with one warning.
pub fn install(ctx: &videre_core::library::LibraryContext, command: &str) -> Installed {
    let dir = match videre_core::error_log::logs_dir_for_writing(ctx) {
        Ok(dir) => dir,
        Err(e) => {
            eprintln!("warning: file logging disabled: {e:#}");
            None
        }
    };
    let (dispatch, guard) = build(dir.as_deref(), command, &ctx.settings, &run_id());
    let _ = tracing::dispatcher::set_global_default(dispatch);
    let entered = guard.run_span.clone().entered();
    if let Ok(mut slot) = GLOBAL_GUARD.lock() {
        *slot = Some(guard);
    }
    videre_core::shutdown::set_flush_hook(flush_installed);
    mark_run_start();
    Installed {
        entered: Some(entered),
    }
}

/// The one primary-log line every run writes, inside its run span, so a clean
/// run supersedes an older failed one when the latest run is read back.
fn mark_run_start() {
    tracing::info!(target: "videre::run", "run started");
}

/// Before a library exists there is nowhere to write a file: terminal only.
pub fn install_terminal_only() {
    let dispatch =
        tracing::Dispatch::new(tracing_subscriber::registry().with(vec![terminal_layer()]));
    let _ = tracing::dispatcher::set_global_default(dispatch);
}

#[cfg(test)]
mod tests {
    use super::*;

    const RUN: &str = "20260923T110412Z-4812";

    fn run_with(settings: &LibraryConfig, f: impl FnOnce()) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let (dispatch, guard) = build(Some(dir.path()), "scan", settings, RUN);
        tracing::dispatcher::with_default(&dispatch, || {
            let _run = guard.run_span.clone().entered();
            f()
        });
        drop(guard);
        dir
    }

    fn read(dir: &Path, name: &str) -> String {
        std::fs::read_to_string(dir.join(name)).unwrap_or_default()
    }

    #[test]
    fn default_level_writes_warnings_to_the_primary_file_only() {
        let dir = run_with(&LibraryConfig::default(), || {
            tracing::info!("wrote 3 record(s)");
            tracing::warn!(path = "/Fotoğraflar/Çağla.jpg", "skipping: unreadable");
        });
        let primary = read(dir.path(), "scan.log");
        assert!(primary.contains("skipping: unreadable"), "{primary}");
        assert!(!primary.contains("wrote 3"), "{primary}");
        assert!(!dir.path().join("scan.trace.log").exists());
        let line: serde_json::Value =
            serde_json::from_str(primary.lines().next().unwrap()).unwrap();
        assert_eq!(line["spans"][0]["command"], "scan");
        assert_eq!(line["spans"][0]["run"], RUN);
    }

    #[test]
    fn error_level_still_keeps_warnings_in_the_primary_file() {
        let settings = LibraryConfig {
            log_level: LogLevel::Error,
            ..LibraryConfig::default()
        };
        let dir = run_with(&settings, || tracing::warn!("skipping: unreadable"));
        assert!(read(dir.path(), "scan.log").contains("skipping: unreadable"));
        assert!(!dir.path().join("scan.trace.log").exists());
    }

    #[test]
    fn a_clean_run_leaves_a_start_marker_in_the_primary_file() {
        let dir = run_with(&LibraryConfig::default(), mark_run_start);
        let text = read(dir.path(), "scan.log");
        let line = videre_core::error_log::parse_line(text.lines().next().unwrap()).unwrap();
        assert_eq!(line.level, videre_core::error_log::LineLevel::Info);
        assert_eq!(line.run.as_deref(), Some(RUN));
    }

    #[test]
    fn debug_level_adds_a_trace_file_with_everything() {
        let settings = LibraryConfig {
            log_level: LogLevel::Debug,
            ..LibraryConfig::default()
        };
        let dir = run_with(&settings, || {
            tracing::debug!("eligible: 12 files");
            tracing::warn!("skipping: unreadable");
        });
        let trace = read(dir.path(), "scan.trace.log");
        assert!(trace.contains("eligible: 12 files"), "{trace}");
        assert!(trace.contains("skipping: unreadable"), "{trace}");
        let primary = read(dir.path(), "scan.log");
        assert!(!primary.contains("eligible"), "{primary}");
        assert!(primary.contains("skipping: unreadable"), "{primary}");
    }

    #[test]
    fn text_format_writes_logfmt_with_span_fields() {
        let settings = LibraryConfig {
            log_format: LogFormat::Text,
            ..LibraryConfig::default()
        };
        let dir = run_with(&settings, || tracing::warn!("skipping: unreadable"));
        let primary = read(dir.path(), "scan.log");
        assert!(primary.contains("level=warn"), "{primary}");
        assert!(primary.contains("command=scan"), "{primary}");
    }

    #[test]
    fn third_party_targets_never_reach_the_files() {
        let dir = run_with(&LibraryConfig::default(), || {
            tracing::warn!(target: "hf_hub::api", "noisy dependency warning");
        });
        assert!(!read(dir.path(), "scan.log").contains("noisy"));
    }

    #[test]
    fn the_core_reader_reads_what_both_formats_write() {
        for format in [LogFormat::Json, LogFormat::Text] {
            let settings = LibraryConfig {
                log_format: format,
                ..LibraryConfig::default()
            };
            let dir = run_with(&settings, || {
                let _stage = videre_core::error_log::enter_stage("scan");
                let err = anyhow::anyhow!("read \"timed\" out\n  run: videre scan")
                    .context(videre_core::error_kind::ErrorKind::SourceUnavailable);
                videre_core::error_log::report(
                    Level::WARN,
                    &err,
                    Some("/Volumes/Arşiv/Çağla 2019.jpg"),
                );
            });
            let text = read(dir.path(), "scan.log");
            let line = videre_core::error_log::parse_line(text.lines().next().unwrap())
                .unwrap_or_else(|| panic!("{format:?} line did not parse: {text}"));
            assert_eq!(line.command.as_deref(), Some("scan"), "{format:?}");
            assert_eq!(line.run.as_deref(), Some(RUN), "{format:?}");
            assert_eq!(line.stage.as_deref(), Some("scan"), "{format:?}");
            assert_eq!(
                line.kind.as_deref(),
                Some("source_unavailable"),
                "{format:?}"
            );
            assert_eq!(
                line.path.as_deref(),
                Some("/Volumes/Arşiv/Çağla 2019.jpg"),
                "{format:?}"
            );
            assert!(
                line.message
                    .contains("read \"timed\" out\n  run: videre scan"),
                "{format:?}: {:?}",
                line.message
            );
        }
    }

    #[test]
    fn rotation_keeps_at_most_keep_files() {
        let settings = LibraryConfig {
            log_max_size_mb: 1,
            log_keep: 2,
            ..LibraryConfig::default()
        };
        let dir = run_with(&settings, || {
            let filler = "x".repeat(1024);
            for _ in 0..(4 * 1024) {
                tracing::warn!("{filler}");
            }
        });
        let rotated: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .filter(|n| n.starts_with("scan.log."))
            .collect();
        assert!(!rotated.is_empty(), "the filler must force a rotation");
        assert!(rotated.len() <= 2, "{rotated:?}");
    }
}
