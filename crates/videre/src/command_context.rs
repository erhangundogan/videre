use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// How this invocation selected its library root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LibrarySource {
    Cwd,
    Argument,
}

impl LibrarySource {
    pub fn label(self) -> &'static str {
        match self {
            Self::Cwd => "invocation directory",
            Self::Argument => "--library",
        }
    }
}

/// Library identity plus invocation-only path context for one command.
#[derive(Clone)]
pub struct CommandContext {
    pub library: Arc<videre_core::library::LibraryContext>,
    pub invocation_dir: PathBuf,
    pub source: LibrarySource,
}

impl CommandContext {
    /// Capture cwd once, resolve the selected root there, and construct one
    /// immutable library context for the whole invocation.
    pub fn capture(selected: Option<PathBuf>) -> Result<Self> {
        let invocation_dir = std::env::current_dir().context("read the invocation directory")?;
        let (root, source) = match selected {
            Some(path) => {
                let root = if path.is_absolute() {
                    path
                } else {
                    invocation_dir.join(path)
                };
                (root, LibrarySource::Argument)
            }
            None => (invocation_dir.clone(), LibrarySource::Cwd),
        };
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| anyhow::anyhow!("cannot locate the library cache: HOME is not set"))?;
        if !home.is_absolute() {
            bail!(
                "cannot locate the library cache: HOME must be absolute, got {}",
                home.display()
            );
        }
        let library = videre_core::library::LibraryContext::new(&root, &home.join(".cache"))?;
        if let Some(rate) = library.settings.min_read_rate_mb_s {
            videre_core::io_timeout::set_min_read_rate_mb_s(rate);
        }
        Ok(Self {
            library: Arc::new(library),
            invocation_dir,
            source,
        })
    }

    /// Resolve an explicit file operand against the invocation directory.
    pub fn operand(&self, path: &Path) -> PathBuf {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.invocation_dir.join(path)
        }
    }
}

/// The standard boundary for one of the eight tracked commands against an
/// already-initialized library: open the database, take the library's activity
/// lease in `mode`, take the per-command lock, and run `work` under pipeline-run
/// bookkeeping.
///
/// The acquisition order is fixed and shared by every tracked command so two
/// commands can never deadlock by taking the same locks in different orders:
/// database open (any exclusive schema preparation happens and releases inside
/// `open_existing`), then the activity lease, then the command lock. `Shared`
/// lets ordinary operations run alongside each other and alongside readers;
/// `Exclusive` is for maintenance that must not overlap anything else in the
/// same library (prune, and destructive face reprocessing/reclustering). Both
/// contend only within one library: an unrelated library is a different set of
/// lock files.
pub fn with_tracked_command<T>(
    ctx: &CommandContext,
    command_name: &str,
    mode: videre_core::library_locks::ActivityMode,
    work: impl FnOnce(&rusqlite::Connection) -> Result<T>,
) -> Result<T> {
    let conn = videre_core::library_db::open_existing(&ctx.library)?;
    let _activity = videre_core::library_locks::try_activity(&ctx.library, mode)?;
    let command = videre_core::library_locks::try_command(&ctx.library, command_name)?;
    videre_core::pipeline_runs::track_in(&conn, &ctx.library, &command, command_name, || {
        work(&conn)
    })
}
