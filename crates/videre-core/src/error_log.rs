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

/// Refuse a redirected log file and prove it can be opened for append,
/// creating it owner-only (it holds paths to personal media). A failure here
/// disables file logging for the run; it never fails the command.
pub fn check_log_file(path: &Path) -> Result<()> {
    crate::library_locks::reject_redirect(path, "the log file")?;
    let owned = path.to_path_buf();
    bounded_op(path, "open", STAT_TIMEOUT, move || {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&owned)
            .map(|_| ())
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
