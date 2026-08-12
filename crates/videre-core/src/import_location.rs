//! Where an import's source files live.
//!
//! Every `videre import` source resolves file locations through the same
//! ladder, declared per provider in `import_providers`:
//!
//! ```text
//! [ provider database ]   only when opted in, or when there is no alternative
//!         |
//!   known folder layouts  the default entry point
//!         |
//!     ask the user        --originals <dir>, or plain videre scan
//! ```
//!
//! The default never opens a provider database. A source that invents its own
//! discovery scheme is a defect in that source: a vendor changing their layout
//! should cost one rung, not the feature.
//!
//! Asking a catalog *where to look* is not the same as asking it *what is
//! there*. Location may come from a database; content always comes from the
//! files.

use std::path::PathBuf;

/// Ordered most precise first, which is also the order they are tried.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Rung {
    Database,
    FolderLayout,
    AskUser,
}

/// How a given run actually found the files. Reported so a bug report can
/// distinguish a schema change from a layout change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Provenance {
    Database,
    Layout(&'static str),
    UserSupplied,
}

impl Provenance {
    pub fn describe(&self) -> String {
        match self {
            Provenance::Database => "the provider catalog".to_string(),
            Provenance::Layout(name) => format!("{name}/"),
            Provenance::UserSupplied => "--originals".to_string(),
        }
    }
}

/// The outcome of locating a source's files.
#[derive(Debug)]
pub enum Located {
    Found {
        roots: Vec<PathBuf>,
        via: Provenance,
    },
    /// Every rung failed. `tried` is human-readable, one line per rung, and is
    /// printed verbatim so the user can see what was attempted.
    NotFound { tried: Vec<String> },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provenance_describes_how_files_were_found() {
        assert_eq!(Provenance::Layout("originals").describe(), "originals/");
        assert_eq!(Provenance::Database.describe(), "the provider catalog");
        assert_eq!(Provenance::UserSupplied.describe(), "--originals");
    }

    #[test]
    fn rungs_order_from_most_to_least_precise() {
        assert!(Rung::Database < Rung::FolderLayout);
        assert!(Rung::FolderLayout < Rung::AskUser);
    }
}
