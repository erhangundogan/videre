pub mod config;
pub mod dedupe;
pub mod embed;
pub mod faces;
pub mod fix_dates;
pub mod mcp;
pub mod prune;
pub mod report;
pub mod search;
pub mod watch;

/// Reader-side db resolution. Explicit paths keep their command's existing
/// semantics untouched; defaulted paths must already exist (SQLite would
/// otherwise create an empty db on open and silently serve an empty library).
pub(crate) fn resolve_reader_db(
    explicit: Option<std::path::PathBuf>,
) -> anyhow::Result<std::path::PathBuf> {
    match explicit {
        Some(p) => Ok(p),
        None => {
            let db = videre_core::home::resolve_db(None)?;
            anyhow::ensure!(
                db.exists(),
                "no database found at {}; run 'videre dedupe <dir>' first",
                db.display()
            );
            Ok(db)
        }
    }
}

/// Directory resolution for the two directory-taking commands (dedupe, watch):
/// an explicit positional wins; otherwise the config `path` key (default_path
/// in config.toml). There is no built-in fallback directory.
pub(crate) fn resolve_directory(
    explicit: Option<std::path::PathBuf>,
) -> anyhow::Result<std::path::PathBuf> {
    match explicit {
        Some(p) => Ok(p),
        None => videre_core::home::default_path()?.ok_or_else(|| {
            anyhow::anyhow!(
                "no directory given and no default path configured; \
                 pass <directory> or run 'videre config set path <dir>'"
            )
        }),
    }
}
