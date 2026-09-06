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
