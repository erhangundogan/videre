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

use std::path::{Path, PathBuf};

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

/// One candidate folder layout for a provider.
///
/// `dir_names` are tried in order, so a provider that renamed its folder across
/// versions lists them newest first.
#[derive(Debug, Clone, Copy)]
pub struct LayoutProbe {
    pub dir_names: &'static [&'static str],
    /// A path relative to the library root that must also exist for this layout
    /// to count. Distinguishes "a folder happens to be called originals" from
    /// "this is an Apple Photos library".
    pub requires_sibling: Option<&'static str>,
}

/// Finds the first matching layout directory beneath `root`.
///
/// Matching is case-insensitive: Apple used `Originals/`, then `Masters/`, then
/// lowercase `originals/`, and on a case-insensitive filesystem the on-disk
/// spelling cannot be relied on.
pub fn probe_layouts(root: &Path, probes: &[LayoutProbe]) -> Option<(PathBuf, Provenance)> {
    let entries: Vec<(String, PathBuf)> = std::fs::read_dir(root)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .map(|e| (e.file_name().to_string_lossy().to_lowercase(), e.path()))
        .collect();

    for probe in probes {
        if let Some(sibling) = probe.requires_sibling {
            if !root.join(sibling).exists() {
                continue;
            }
        }
        for want in probe.dir_names {
            let want_lower = want.to_lowercase();
            if let Some((_, path)) = entries.iter().find(|(name, _)| *name == want_lower) {
                return Some((path.clone(), Provenance::Layout(want)));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn probe(dir_names: &'static [&'static str], sibling: Option<&'static str>) -> LayoutProbe {
        LayoutProbe {
            dir_names,
            requires_sibling: sibling,
        }
    }

    #[test]
    fn finds_a_layout_directory_by_name() {
        let d = tempdir().unwrap();
        fs::create_dir(d.path().join("originals")).unwrap();
        let got = probe_layouts(d.path(), &[probe(&["originals"], None)]).unwrap();
        assert_eq!(got.0, d.path().join("originals"));
        assert_eq!(got.1, Provenance::Layout("originals"));
    }

    #[test]
    fn tries_layout_names_in_order() {
        let d = tempdir().unwrap();
        fs::create_dir(d.path().join("Masters")).unwrap();
        let got = probe_layouts(
            d.path(),
            &[probe(&["originals", "Masters", "Originals"], None)],
        )
        .unwrap();
        assert_eq!(
            got.1,
            Provenance::Layout("Masters"),
            "falls through to the second name"
        );
    }

    #[test]
    fn a_required_sibling_must_exist() {
        let d = tempdir().unwrap();
        fs::create_dir(d.path().join("originals")).unwrap();
        // No database/Photos.sqlite, so the probe must not match.
        assert!(probe_layouts(
            d.path(),
            &[probe(&["originals"], Some("database/Photos.sqlite"))]
        )
        .is_none());

        fs::create_dir_all(d.path().join("database")).unwrap();
        fs::write(d.path().join("database/Photos.sqlite"), b"").unwrap();
        assert!(probe_layouts(
            d.path(),
            &[probe(&["originals"], Some("database/Photos.sqlite"))]
        )
        .is_some());
    }

    #[test]
    fn matches_case_insensitively_because_apple_renamed_the_folder() {
        // Early iPhoto used Originals/, iPhoto 9 used Masters/, modern Photos
        // uses lowercase originals/. On a case-insensitive filesystem the same
        // name can arrive either way.
        let d = tempdir().unwrap();
        fs::create_dir(d.path().join("ORIGINALS")).unwrap();
        let got = probe_layouts(d.path(), &[probe(&["originals"], None)]);
        assert!(got.is_some(), "layout matching must not depend on case");
    }

    #[test]
    fn no_layout_present_is_none_not_an_error() {
        let d = tempdir().unwrap();
        fs::create_dir(d.path().join("something-else")).unwrap();
        assert!(probe_layouts(d.path(), &[probe(&["originals"], None)]).is_none());
    }

    #[test]
    fn a_file_named_like_the_layout_does_not_match() {
        let d = tempdir().unwrap();
        fs::write(d.path().join("originals"), b"not a directory").unwrap();
        assert!(probe_layouts(d.path(), &[probe(&["originals"], None)]).is_none());
    }

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
