//! Where per-command logs live, the one function that turns a failure into a
//! logged event, and the reader. Writer and reader share the names here so
//! they cannot disagree about the layout.
//!
//! Every failure is logged once, at the highest boundary that still knows
//! what failed, through [`report`]. Lower levels never log an error they also
//! return; they add detail with `.context(...)` and, where the cause is
//! known, an [`ErrorKind`].

use crate::error_kind::ErrorKind;
use crate::io_timeout::STAT_TIMEOUT;
use crate::library::{bounded_op, LibraryContext};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// The directory, under the library's `.videre`, holding the log files.
pub const LOGS_DIR: &str = "logs";

/// Events for the log file only (a failure the command already showed on
/// stdout, as a JSON error document); the terminal layer filters it out.
pub const FILE_ONLY_TARGET: &str = "videre::file_only";

/// The target of the one line every run writes to its primary log when it
/// starts. Without it a clean run leaves no line, and an older failed run
/// would still read as the latest one. The terminal filters it out.
pub const RUN_MARKER_TARGET: &str = "videre::run";

/// `<command>.log`: errors and warnings, always.
pub fn primary_log_name(command: &str) -> String {
    format!("{command}.log")
}

/// `<command>.trace.log`: everything at the configured level, when that
/// level is `info` or `debug`.
pub fn trace_log_name(command: &str) -> String {
    format!("{command}.trace.log")
}

/// The logs directory, created on demand, or `None` when the library has no
/// `.videre` yet: looking at an uninitialized library must create nothing.
/// A symlinked state or logs directory is refused, like all library state.
pub fn logs_dir_for_writing(ctx: &LibraryContext) -> Result<Option<PathBuf>> {
    let state = &ctx.paths.state;
    let owned = state.clone();
    let state_exists =
        bounded_op(
            state,
            "stat",
            STAT_TIMEOUT,
            move || match std::fs::symlink_metadata(&owned) {
                Ok(_) => Ok(true),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
                Err(e) => Err(e),
            },
        )?;
    if !state_exists {
        return Ok(None);
    }
    crate::library_locks::reject_dir_redirect(state, "the library state directory")?;
    let dir = state.join(LOGS_DIR);
    crate::library_locks::reject_dir_redirect(&dir, "the logs directory")?;
    let owned = dir.clone();
    bounded_op(&dir, "create", STAT_TIMEOUT, move || {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&owned)
    })
    .with_context(|| format!("create {}", dir.display()))?;
    Ok(Some(dir))
}

/// Refuse a redirected log file and prove it can be opened for append, owner
/// only (it holds paths to personal media): a new file is created `0600` and
/// an existing one is tightened to it before anything is appended. A failure
/// here disables file logging for the run; it never fails the command.
pub fn check_log_file(path: &Path) -> Result<()> {
    crate::library_locks::reject_redirect(path, "the log file")?;
    let owned = path.to_path_buf();
    bounded_op(path, "open", STAT_TIMEOUT, move || {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&owned)?;
        if file.metadata()?.permissions().mode() & 0o777 != 0o600 {
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    })
    .with_context(|| format!("open {}", path.display()))
}

/// Delete rotated files of `command` older than `log_max_age_days`. Only
/// names `<command>.log.N` and `<command>.trace.log.N` directly in `dir` are
/// touched, never through a link, and never the active files. Best effort:
/// call it only after [`logs_dir_for_writing`] proved the volume answers.
pub fn sweep_expired(dir: &Path, command: &str, settings: &crate::library_config::LibraryConfig) {
    let Some(cutoff) = std::time::SystemTime::now().checked_sub(std::time::Duration::from_secs(
        settings.log_max_age_days.saturating_mul(86_400),
    )) else {
        return;
    };
    let prefixes = [
        format!("{}.", primary_log_name(command)),
        format!("{}.", trace_log_name(command)),
    ];
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let rotated = prefixes.iter().any(|p| {
            name.strip_prefix(p.as_str())
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        });
        if !rotated {
            continue;
        }
        let Ok(meta) = entry.path().symlink_metadata() else {
            continue;
        };
        if meta.file_type().is_file() && meta.modified().is_ok_and(|m| m < cutoff) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

static STAGE: std::sync::Mutex<Option<&'static str>> = std::sync::Mutex::new(None);

/// The stage `pipeline` or `watch` is running; the previous stage comes back
/// when it is dropped.
pub struct StageGuard {
    previous: Option<&'static str>,
    _span: tracing::span::EnteredSpan,
}

/// Enter one stage of `pipeline` or `watch`. Stages run one at a time in a
/// process, so the current stage is process-wide state: unlike a span it is
/// visible from rayon worker threads, which is where per-file skips are
/// reported from.
pub fn enter_stage(name: &'static str) -> StageGuard {
    let previous = STAGE.lock().map(|mut s| s.replace(name)).unwrap_or(None);
    StageGuard {
        previous,
        _span: tracing::info_span!("stage", stage = name).entered(),
    }
}

impl Drop for StageGuard {
    fn drop(&mut self) {
        if let Ok(mut s) = STAGE.lock() {
            *s = self.previous;
        }
    }
}

fn current_stage() -> &'static str {
    STAGE.lock().ok().and_then(|s| *s).unwrap_or("")
}

/// Log one failure at a boundary: the whole `{e:#}` chain as the message,
/// plus its kind, remediation, affected path and current stage. The one
/// place a failure becomes an event, so every boundary records the same
/// fields. An empty field means unknown.
pub fn report(level: tracing::Level, err: &anyhow::Error, path: Option<&str>) {
    let kind = ErrorKind::in_chain(err);
    let code = kind.map(ErrorKind::code).unwrap_or("");
    let remediation = kind.and_then(ErrorKind::remediation).unwrap_or("");
    let path = path.unwrap_or("");
    let stage = current_stage();
    let message = format!("{err:#}");
    if level == tracing::Level::ERROR {
        tracing::error!(kind = code, remediation, path, stage, "{message}");
    } else {
        tracing::warn!(kind = code, remediation, path, stage, "{message}");
    }
}

/// Log a failure the command already presented on stdout: file only. The
/// target literal must stay equal to [`FILE_ONLY_TARGET`].
pub fn report_file_only(message: &str) {
    tracing::error!(target: "videre::file_only", stage = current_stage(), "{message}");
}

/// Severity of one recorded line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LineLevel {
    Error,
    Warn,
    Info,
    Debug,
}

impl LineLevel {
    fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "error" => Some(Self::Error),
            "warn" => Some(Self::Warn),
            "info" => Some(Self::Info),
            "debug" | "trace" => Some(Self::Debug),
            _ => None,
        }
    }
}

/// One parsed log line, whichever format wrote it. Absent and empty fields
/// both read as `None`.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct LogLine {
    pub ts: String,
    pub level: LineLevel,
    pub command: Option<String>,
    pub run: Option<String>,
    pub stage: Option<String>,
    pub kind: Option<String>,
    pub path: Option<String>,
    pub message: String,
}

/// The latest run of one command, as its primary log records it.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct CommandLogSummary {
    pub command: String,
    pub run: String,
    /// Timestamp of the run's first recorded line.
    pub started: String,
    pub errors: u64,
    pub warnings: u64,
    /// Per stage: (errors, warnings). Direct invocations have no stage.
    pub by_stage: std::collections::BTreeMap<String, (u64, u64)>,
    pub last_error: Option<LogLine>,
    /// Lines in the run's files that could not be parsed.
    pub unreadable_lines: u64,
}

fn non_empty(s: Option<&str>) -> Option<String> {
    s.filter(|s| !s.is_empty()).map(str::to_owned)
}

/// Parse one line written by either file format; `None` for anything else.
/// The format is detected per line, so a file written before and after a
/// `log_format` change still reads.
pub fn parse_line(line: &str) -> Option<LogLine> {
    let line = line.trim();
    if line.starts_with('{') {
        parse_json(line)
    } else if line.contains('=') {
        parse_logfmt(line)
    } else {
        None
    }
}

/// The shape `tracing-subscriber`'s JSON formatter writes with a span list.
fn parse_json(line: &str) -> Option<LogLine> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    let fields = v.get("fields")?;
    let field = |name: &str| non_empty(fields.get(name).and_then(|f| f.as_str()));
    let spans = v.get("spans").and_then(|s| s.as_array());
    let span_field = |span: &str, name: &str| {
        spans.and_then(|spans| {
            spans
                .iter()
                .find(|s| s.get("name").and_then(|n| n.as_str()) == Some(span))
                .and_then(|s| non_empty(s.get(name).and_then(|f| f.as_str())))
        })
    };
    Some(LogLine {
        ts: v.get("timestamp")?.as_str()?.to_owned(),
        level: LineLevel::parse(v.get("level")?.as_str()?)?,
        command: span_field("run", "command"),
        run: span_field("run", "run"),
        stage: field("stage").or_else(|| span_field("stage", "stage")),
        kind: field("kind"),
        path: field("path"),
        message: fields.get("message")?.as_str()?.to_owned(),
    })
}

/// `key=value` pairs, where a value is bare (up to the next space) or
/// double-quoted with `\"` and `\\` escapes, as `tracing-logfmt` writes them.
fn logfmt_pairs(line: &str) -> Option<std::collections::HashMap<String, String>> {
    let mut pairs = std::collections::HashMap::new();
    let mut chars = line.chars().peekable();
    loop {
        while chars.peek() == Some(&' ') {
            chars.next();
        }
        if chars.peek().is_none() {
            return Some(pairs);
        }
        let mut key = String::new();
        loop {
            match chars.next() {
                Some('=') => break,
                Some(' ') | None => return None,
                Some(c) => key.push(c),
            }
        }
        let mut value = String::new();
        if chars.peek() == Some(&'"') {
            chars.next();
            loop {
                match chars.next()? {
                    '\\' => value.push(match chars.next()? {
                        'n' => '\n',
                        't' => '\t',
                        'r' => '\r',
                        c => c,
                    }),
                    '"' => break,
                    c => value.push(c),
                }
            }
        } else {
            while let Some(&c) = chars.peek() {
                if c == ' ' {
                    break;
                }
                value.push(c);
                chars.next();
            }
        }
        pairs.insert(key, value);
    }
}

fn parse_logfmt(line: &str) -> Option<LogLine> {
    let pairs = logfmt_pairs(line)?;
    let get = |k: &str| non_empty(pairs.get(k).map(String::as_str));
    Some(LogLine {
        ts: get("ts")?,
        level: LineLevel::parse(pairs.get("level")?)?,
        command: get("command"),
        run: get("run"),
        stage: get("stage"),
        kind: get("kind"),
        path: get("path"),
        message: pairs
            .get("message")
            .or_else(|| pairs.get("msg"))?
            .to_owned(),
    })
}

/// Read one log file, bounded like every read on the library volume.
fn read_log(path: &Path) -> Result<Option<String>> {
    let owned = path.to_path_buf();
    let len = bounded_op(
        path,
        "stat",
        STAT_TIMEOUT,
        move || match std::fs::symlink_metadata(&owned) {
            Ok(m) if m.is_file() => Ok(Some(m.len())),
            Ok(_) => Ok(None),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        },
    )?;
    let Some(len) = len else { return Ok(None) };
    let owned = path.to_path_buf();
    let budget = crate::io_timeout::timeout_for_size(len, crate::io_timeout::min_read_rate_mb_s());
    let bytes = bounded_op(path, "read", budget, move || std::fs::read(&owned))?;
    Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
}

/// Lines of one file with each line's run resolved: a line written outside
/// the run span (a worker thread) belongs to the run of the line before it.
/// `None` marks an unreadable line, including one naming another command.
fn resolved_lines(text: &str, command: &str) -> Vec<Option<(String, LogLine)>> {
    let mut previous_run: Option<String> = None;
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|raw| {
            let line = parse_line(raw)?;
            if line.command.as_deref().is_some_and(|c| c != command) {
                return None;
            }
            let run = line.run.clone().or_else(|| previous_run.clone())?;
            previous_run = Some(run.clone());
            Some((run, line))
        })
        .collect()
}

/// The latest run of every command that has a primary log. Trace files are
/// never read: errors and warnings are always in the primary file. A missing
/// logs directory means nothing was recorded, and reading creates nothing.
pub fn latest_runs(ctx: &LibraryContext) -> Result<Vec<CommandLogSummary>> {
    let dir = ctx.paths.state.join(LOGS_DIR);
    let owned = dir.clone();
    let names = bounded_op(
        &dir,
        "read",
        STAT_TIMEOUT,
        move || match std::fs::read_dir(&owned) {
            Ok(entries) => Ok(entries
                .flatten()
                .filter_map(|e| e.file_name().into_string().ok())
                .collect::<Vec<_>>()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(e),
        },
    )?;
    let mut commands: Vec<&str> = names
        .iter()
        .filter_map(|n| n.strip_suffix(".log"))
        .filter(|c| !c.ends_with(".trace") && !c.is_empty())
        .collect();
    commands.sort_unstable();
    let mut out = Vec::new();
    for command in commands {
        if let Some(summary) = latest_run_of(&dir, command, &names)? {
            out.push(summary);
        }
    }
    Ok(out)
}

fn latest_run_of(dir: &Path, command: &str, names: &[String]) -> Result<Option<CommandLogSummary>> {
    // Newest first: the active file, then .1, .2, ... while they exist.
    let mut files = vec![primary_log_name(command)];
    for n in 1.. {
        let name = format!("{}.{n}", primary_log_name(command));
        if !names.contains(&name) {
            break;
        }
        files.push(name);
    }
    let mut latest: Option<String> = None;
    // Collected newest file first; lines within a file stay in order.
    let mut per_file: Vec<Vec<LogLine>> = Vec::new();
    let mut unreadable = 0u64;
    for name in &files {
        let Some(text) = read_log(&dir.join(name))? else {
            continue;
        };
        let lines = resolved_lines(&text, command);
        if latest.is_none() {
            latest = lines.iter().rev().flatten().next().map(|(r, _)| r.clone());
            if latest.is_none() {
                continue;
            }
        }
        let run = latest.as_deref().unwrap_or_default();
        let of_run: Vec<LogLine> = lines
            .iter()
            .flatten()
            .filter(|(r, _)| r == run)
            .map(|(_, l)| l.clone())
            .collect();
        if of_run.is_empty() {
            break;
        }
        unreadable += lines.iter().filter(|l| l.is_none()).count() as u64;
        per_file.push(of_run);
    }
    let Some(run) = latest else { return Ok(None) };
    let lines: Vec<LogLine> = per_file.into_iter().rev().flatten().collect();
    let mut summary = CommandLogSummary {
        command: command.to_owned(),
        run,
        started: lines.first().map(|l| l.ts.clone()).unwrap_or_default(),
        errors: 0,
        warnings: 0,
        by_stage: Default::default(),
        last_error: None,
        unreadable_lines: unreadable,
    };
    for line in lines {
        let stage = line.stage.clone();
        let bump = |s: &mut CommandLogSummary, error: bool| {
            if let Some(stage) = &stage {
                let entry = s.by_stage.entry(stage.clone()).or_default();
                if error {
                    entry.0 += 1;
                } else {
                    entry.1 += 1;
                }
            }
        };
        match line.level {
            LineLevel::Error => {
                summary.errors += 1;
                bump(&mut summary, true);
                summary.last_error = Some(line);
            }
            LineLevel::Warn => {
                summary.warnings += 1;
                bump(&mut summary, false);
            }
            LineLevel::Info | LineLevel::Debug => {}
        }
    }
    Ok(Some(summary))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error_kind::ErrorKind;
    use anyhow::Context;
    use std::sync::{Arc, Mutex};

    pub(super) fn library(with_state: bool) -> (tempfile::TempDir, crate::library::LibraryContext) {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("Fotoğraflar");
        std::fs::create_dir(&root).unwrap();
        let ctx = crate::library::LibraryContext::new(&root, &t.path().join("cache")).unwrap();
        if with_state {
            std::fs::create_dir(&ctx.paths.state).unwrap();
        }
        (t, ctx)
    }

    #[test]
    fn parses_the_json_shape_tracing_writes() {
        let line = r#"{"timestamp":"2026-09-23T11:05:01.1Z","level":"WARN","fields":{"message":"skipping /Fotoğraflar/Çağla.jpg: unreadable","kind":"decode_failed","path":"/Fotoğraflar/Çağla.jpg","remediation":"","stage":"scan"},"target":"videre::commands::scan","spans":[{"name":"run","command":"watch","run":"R1"}]}"#;
        let parsed = parse_line(line).unwrap();
        assert_eq!(parsed.level, LineLevel::Warn);
        assert_eq!(parsed.ts, "2026-09-23T11:05:01.1Z");
        assert_eq!(parsed.command.as_deref(), Some("watch"));
        assert_eq!(parsed.run.as_deref(), Some("R1"));
        assert_eq!(parsed.stage.as_deref(), Some("scan"));
        assert_eq!(parsed.kind.as_deref(), Some("decode_failed"));
        assert_eq!(parsed.path.as_deref(), Some("/Fotoğraflar/Çağla.jpg"));
        assert_eq!(
            parsed.message,
            "skipping /Fotoğraflar/Çağla.jpg: unreadable"
        );
    }

    #[test]
    fn parses_logfmt_with_quoted_values() {
        let line = r#"ts=2026-09-23T11:05:01.1Z level=error target=videre::commands::scan span=stage command=watch run=R1 stage=scan kind=source_unavailable path="/Volumes/Arşiv/a b.jpg" message="hash failed: \"read\" timed out""#;
        let parsed = parse_line(line).unwrap();
        assert_eq!(parsed.level, LineLevel::Error);
        assert_eq!(parsed.command.as_deref(), Some("watch"));
        assert_eq!(parsed.stage.as_deref(), Some("scan"));
        assert_eq!(parsed.path.as_deref(), Some("/Volumes/Arşiv/a b.jpg"));
        assert_eq!(parsed.message, "hash failed: \"read\" timed out");
    }

    #[test]
    fn logfmt_escaped_newlines_come_back_as_newlines() {
        let line =
            r#"ts=t level=error message="no embeddings\n  run: videre embed" command=search run=R"#;
        let parsed = parse_line(line).unwrap();
        assert_eq!(parsed.message, "no embeddings\n  run: videre embed");
    }

    #[test]
    fn empty_fields_read_as_absent_and_garbage_is_skipped() {
        let line = r#"{"timestamp":"t","level":"ERROR","fields":{"message":"m","kind":"","path":"","stage":""},"spans":[{"name":"run","command":"scan","run":"R"}]}"#;
        let parsed = parse_line(line).unwrap();
        assert_eq!((parsed.kind, parsed.path, parsed.stage), (None, None, None));
        assert!(parse_line("not a log line").is_none());
        assert!(parse_line("").is_none());
    }

    fn json_line(run: &str, level: &str, stage: &str, msg: &str) -> String {
        format!(
            r#"{{"timestamp":"t-{msg}","level":"{level}","fields":{{"message":"{msg}","stage":"{stage}"}},"spans":[{{"name":"run","command":"watch","run":"{run}"}}]}}"#
        )
    }

    #[test]
    fn latest_run_spans_rotation_and_ignores_older_runs() {
        let (_t, ctx) = library(true);
        let dir = logs_dir_for_writing(&ctx).unwrap().unwrap();
        std::fs::write(
            dir.join("watch.log.2"),
            json_line("OLD", "ERROR", "scan", "old") + "\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("watch.log.1"),
            [
                json_line("OLD", "ERROR", "scan", "old2"),
                json_line("NEW", "WARN", "faces", "w1"),
            ]
            .join("\n")
                + "\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("watch.log"),
            [
                json_line("NEW", "ERROR", "scan", "e1"),
                json_line("NEW", "ERROR", "scan", "e2"),
                "garbage".to_owned(),
            ]
            .join("\n")
                + "\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("watch.trace.log"),
            json_line("NEW", "ERROR", "", "x"),
        )
        .unwrap();

        let runs = latest_runs(&ctx).unwrap();
        assert_eq!(runs.len(), 1, "trace files are not read: {runs:?}");
        let w = &runs[0];
        assert_eq!((w.command.as_str(), w.run.as_str()), ("watch", "NEW"));
        assert_eq!((w.errors, w.warnings), (2, 1));
        assert_eq!(w.by_stage["scan"], (2, 0));
        assert_eq!(w.by_stage["faces"], (0, 1));
        assert_eq!(w.last_error.as_ref().unwrap().message, "e2");
        assert_eq!(w.unreadable_lines, 1);
        assert_eq!(w.started, "t-w1");
    }

    #[test]
    fn a_line_without_a_run_joins_the_run_before_it() {
        let (_t, ctx) = library(true);
        let dir = logs_dir_for_writing(&ctx).unwrap().unwrap();
        let with_run = r#"{"timestamp":"t1","level":"WARN","fields":{"message":"a"},"spans":[{"name":"run","command":"scan","run":"R9"}]}"#;
        let worker = r#"{"timestamp":"t2","level":"WARN","fields":{"message":"skipping /Fotoğraflar/b.jpg","stage":""}}"#;
        std::fs::write(dir.join("scan.log"), format!("{with_run}\n{worker}\n")).unwrap();
        let runs = latest_runs(&ctx).unwrap();
        assert_eq!(
            (
                runs[0].command.as_str(),
                runs[0].run.as_str(),
                runs[0].warnings
            ),
            ("scan", "R9", 2)
        );
    }

    #[test]
    fn no_logs_directory_means_no_runs() {
        let (_t, ctx) = library(true);
        assert!(latest_runs(&ctx).unwrap().is_empty());
        assert!(
            !ctx.paths.state.join(LOGS_DIR).exists(),
            "reading must not create it"
        );
    }

    #[test]
    fn sweep_deletes_only_expired_rotations_of_this_command() {
        let dir = tempfile::tempdir().unwrap();
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(40 * 86_400);
        let old = filetime::FileTime::from_system_time(old);
        for name in [
            "scan.log.1",
            "scan.trace.log.3",
            "scan.log",
            "notes.txt",
            "faces.log.1",
            "scan.log.bak",
        ] {
            let p = dir.path().join(name);
            std::fs::write(&p, "x").unwrap();
            filetime::set_file_mtime(&p, old).unwrap();
        }
        std::fs::write(dir.path().join("scan.log.2"), "fresh").unwrap();

        let settings = crate::library_config::LibraryConfig::default();
        sweep_expired(dir.path(), "scan", &settings);

        let mut left: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        assert_eq!(
            left,
            [
                "faces.log.1",
                "notes.txt",
                "scan.log",
                "scan.log.2",
                "scan.log.bak"
            ]
        );
    }

    #[test]
    fn an_uninitialized_library_gets_no_logs_directory() {
        let (_t, ctx) = library(false);
        assert!(logs_dir_for_writing(&ctx).unwrap().is_none());
        assert!(!ctx.paths.state.exists());
    }

    #[test]
    fn a_symlinked_logs_directory_is_refused() {
        let (t, ctx) = library(true);
        let elsewhere = t.path().join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, ctx.paths.state.join(LOGS_DIR)).unwrap();
        assert!(logs_dir_for_writing(&ctx).is_err());
    }

    #[test]
    fn an_existing_log_file_is_tightened_to_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let (_t, ctx) = library(true);
        let dir = logs_dir_for_writing(&ctx).unwrap().unwrap();
        let file = dir.join(primary_log_name("scan"));
        std::fs::write(&file, "").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        check_log_file(&file).unwrap();
        assert_eq!(
            std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn a_later_clean_run_is_the_latest_run() {
        let (_t, ctx) = library(true);
        let dir = logs_dir_for_writing(&ctx).unwrap().unwrap();
        let failed = r#"{"timestamp":"t1","level":"ERROR","fields":{"message":"boom"},"spans":[{"name":"run","command":"scan","run":"R1"}]}"#;
        let marker = r#"{"timestamp":"t2","level":"INFO","fields":{"message":"run started"},"target":"videre::run","spans":[{"name":"run","command":"scan","run":"R2"}]}"#;
        std::fs::write(dir.join("scan.log"), format!("{failed}\n{marker}\n")).unwrap();
        let runs = latest_runs(&ctx).unwrap();
        assert_eq!(runs[0].run, "R2");
        assert_eq!((runs[0].errors, runs[0].warnings), (0, 0));
        assert!(runs[0].last_error.is_none());
    }

    #[test]
    fn a_new_log_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let (_t, ctx) = library(true);
        let dir = logs_dir_for_writing(&ctx).unwrap().unwrap();
        let file = dir.join(primary_log_name("scan"));
        check_log_file(&file).unwrap();
        assert_eq!(
            std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[derive(Clone, Default)]
    struct Buf(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for Buf {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn json_dispatch() -> (Buf, tracing::Dispatch) {
        let buf = Buf::default();
        let w = buf.clone();
        let sub = tracing_subscriber::fmt()
            .json()
            .with_writer(move || w.clone())
            .finish();
        (buf, tracing::Dispatch::new(sub))
    }

    #[test]
    fn report_records_kind_remediation_and_path() {
        let (buf, dispatch) = json_dispatch();
        let err = Err::<(), _>(std::io::Error::other("read timed out"))
            .context(ErrorKind::SourceUnavailable)
            .unwrap_err();
        tracing::dispatcher::with_default(&dispatch, || {
            report(tracing::Level::WARN, &err, Some("/Fotoğraflar/Çağla.jpg"))
        });
        let line: serde_json::Value = serde_json::from_slice(&buf.0.lock().unwrap()).unwrap();
        assert_eq!(line["level"], "WARN");
        assert_eq!(line["fields"]["kind"], "source_unavailable");
        assert_eq!(line["fields"]["path"], "/Fotoğraflar/Çağla.jpg");
        assert!(line["fields"]["remediation"]
            .as_str()
            .unwrap()
            .starts_with("Reconnect"));
        assert!(line["fields"]["message"]
            .as_str()
            .unwrap()
            .contains("read timed out"));
    }

    #[test]
    fn report_carries_the_current_stage_even_from_another_thread() {
        let (buf, dispatch) = json_dispatch();
        let stage = tracing::dispatcher::with_default(&dispatch, || enter_stage("scan"));
        let d = dispatch.clone();
        std::thread::spawn(move || {
            tracing::dispatcher::with_default(&d, || {
                report(
                    tracing::Level::WARN,
                    &anyhow::anyhow!("unreadable"),
                    Some("/a.jpg"),
                )
            })
        })
        .join()
        .unwrap();
        drop(stage);
        let line: serde_json::Value = serde_json::from_slice(&buf.0.lock().unwrap()).unwrap();
        assert_eq!(line["fields"]["stage"], "scan");
    }
}
