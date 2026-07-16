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

/// First-use convenience for `dedupe`: if the caller gave an explicit
/// directory and no default path is configured yet, adopt it as the default
/// so future bare `videre dedupe` / `videre watch` calls need no argument.
/// Only fires once (the unset check makes it a true "first use", not a
/// silent overwrite of a value the user already chose or set explicitly).
/// Best-effort: any error here (unreadable HOME, unwritable config) is
/// swallowed rather than failing the dedupe run over a convenience feature.
/// Prints a one-line stderr note unless `silent`, since even a one-time
/// automatic config write should be visible, not silent.
pub(crate) fn maybe_adopt_default_path(explicit: Option<&std::path::Path>, silent: bool) {
    let Some(dir) = explicit else { return };
    let Ok(home) = videre_core::home::videre_home() else { return };
    let Ok(config) = videre_core::home::load_config(&home) else { return };
    if config.default_path.is_some() {
        return;
    }
    if videre_core::home::set_default_path(&home, dir).is_ok() && !silent {
        eprintln!(
            "videre: saved {:?} as the default path (first use); \
             change it anytime with 'videre config set path <dir>' \
             or remove it with 'videre config unset path'",
            dir
        );
    }
}
