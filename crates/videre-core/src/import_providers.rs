//! Which import sources exist, and how to recognise them.
//!
//! Deliberately data rather than mechanism: adding a provider should mean
//! adding a row here, never editing the ladder in `import_location`.

use crate::import_location::{LayoutProbe, Rung};
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct ProviderDescriptor {
    pub id: &'static str,
    pub display: &'static str,
    /// Which rung this provider starts on. Only Lightroom starts on the
    /// database, because its files live in arbitrary user folders and there is
    /// no layout to fall back to.
    pub default_rung: Rung,
    pub layouts: &'static [LayoutProbe],
    /// Globs, relative to a search root, that find this provider's library.
    pub package_globs: &'static [&'static str],
    /// Structural test: is this path a library of this kind?
    pub detect: fn(&Path) -> bool,
}

/// Apple fans `originals/` out into directories named `0`-`F`, one per leading
/// hex character of the asset UUID, to keep any single directory small.
///
/// This is the signature that survives when only the originals folder is kept:
/// a backup copy has no `database/Photos.sqlite` to detect by, and a folder
/// merely *named* "originals" is common in ordinary photo workflows. Requiring
/// most of the fan-out present, and outnumbering anything else, separates the
/// two without a catalog. Found against a real 399GB backup of 70,854 files
/// that detection missed entirely.
fn has_hex_fanout(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    let (mut hex, mut other) = (0usize, 0usize);
    for e in entries.filter_map(|e| e.ok()) {
        if !e.path().is_dir() {
            continue;
        }
        let name = e.file_name();
        let name = name.to_string_lossy();
        // Tolerates a partial copy: 8 of 16 is still unmistakably the layout.
        if name.len() == 1 && name.chars().all(|c| c.is_ascii_hexdigit()) {
            hex += 1;
        } else {
            other += 1;
        }
    }
    hex >= 8 && hex > other
}

fn is_apple_photos(p: &Path) -> bool {
    // Structure, not name: the folder may be called anything, and the
    // originals directory has been spelled three ways across generations.
    let has_db = p.join("database/Photos.sqlite").exists();
    let has_originals = ["originals", "Masters", "Originals"]
        .iter()
        .any(|d| p.join(d).is_dir());
    (has_db && has_originals)
        || p.extension().is_some_and(|e| e == "photoslibrary")
        || (p.join("Masters").is_dir() && p.join("Database").is_dir())
        // A kept-originals backup: no catalog beside it, so the fan-out is the
        // only evidence. Both the folder holding it and the folder itself.
        || ["originals", "Masters", "Originals"]
            .iter()
            .any(|d| has_hex_fanout(&p.join(d)))
        || has_hex_fanout(p)
}

fn is_lightroom(p: &Path) -> bool {
    p.extension().is_some_and(|e| e == "lrcat")
        || std::fs::read_dir(p).is_ok_and(|mut d| {
            d.any(|e| {
                e.ok()
                    .is_some_and(|e| e.path().extension().is_some_and(|x| x == "lrcat"))
            })
        })
}

/// True when `dir` directly contains at least one Takeout sidecar.
fn has_sidecar(dir: &Path) -> bool {
    std::fs::read_dir(dir).is_ok_and(|d| {
        d.filter_map(|e| e.ok()).any(|e| {
            e.file_name()
                .to_string_lossy()
                .contains(".supplemental-metadat")
        })
    })
}

fn is_takeout(p: &Path) -> bool {
    if p.join("Google Photos").is_dir() || p.join("Takeout/Google Photos").is_dir() {
        return true;
    }
    if has_sidecar(p) {
        return true;
    }
    // Also look one level down. A real export's `Google Photos/` folder holds
    // only album directories, with the sidecars inside them, so pointing at it
    // directly (a natural thing to do) found nothing when this checked the
    // immediate directory alone. Found against a real 36GB Takeout export.
    std::fs::read_dir(p).is_ok_and(|d| {
        d.filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .take(64) // bounded: enough to recognise an export, never a deep walk
            .any(|e| has_sidecar(&e.path()))
    })
}

pub static PROVIDERS: &[ProviderDescriptor] = &[
    ProviderDescriptor {
        id: "apple-photos",
        display: "Apple Photos / iPhoto",
        default_rung: Rung::FolderLayout,
        layouts: &[
            // Newest spelling first. Modern Photos requires the database
            // sibling so a stray `originals/` folder cannot match.
            LayoutProbe {
                dir_names: &["originals"],
                requires_sibling: Some("database/Photos.sqlite"),
            },
            LayoutProbe {
                dir_names: &["Masters", "Originals"],
                requires_sibling: None,
            },
            LayoutProbe {
                dir_names: &["originals"],
                requires_sibling: None,
            },
        ],
        package_globs: &[
            "*.photoslibrary",
            "*.photolibrary",
            "*.migratedphotolibrary",
        ],
        detect: is_apple_photos,
    },
    ProviderDescriptor {
        id: "lightroom",
        display: "Adobe Lightroom Classic",
        default_rung: Rung::Database,
        layouts: &[],
        package_globs: &["*.lrcat", "Lightroom/*.lrcat"],
        detect: is_lightroom,
    },
    ProviderDescriptor {
        id: "google-takeout",
        display: "Google Takeout",
        default_rung: Rung::FolderLayout,
        layouts: &[LayoutProbe {
            dir_names: &["Google Photos"],
            requires_sibling: None,
        }],
        package_globs: &["Takeout", "Takeout*"],
        detect: is_takeout,
    },
];

/// The first provider whose structural test matches, if any.
pub fn detect(path: &Path) -> Option<&'static ProviderDescriptor> {
    PROVIDERS.iter().find(|p| (p.detect)(path))
}

#[derive(Debug)]
pub struct Candidate {
    pub path: PathBuf,
    pub provider: &'static ProviderDescriptor,
}

/// Default places to look, per platform.
///
/// Chosen over any search index because it is the only approach that works
/// everywhere: Spotlight is macOS-only and invisible to users who have narrowed
/// it, and Linux has no dependable equivalent. Measured at 81 ms for the full
/// macOS set, since each entry is one `read_dir` of one directory.
pub fn default_search_roots() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut roots = Vec::new();
    if let Some(h) = home {
        roots.push(h.join("Pictures"));
        roots.push(h.join("Pictures/Lightroom"));
        roots.push(h.join("Documents"));
        roots.push(h.join("Desktop"));
        roots.push(h.join("Downloads"));
    }
    if cfg!(target_os = "macos") {
        if let Ok(vols) = std::fs::read_dir("/Volumes") {
            for v in vols.filter_map(|e| e.ok()) {
                roots.push(v.path());
                roots.push(v.path().join("Pictures"));
            }
        }
    }
    roots
}

/// One level of `read_dir` per root, testing each entry structurally.
pub fn discover_in(roots: &[PathBuf]) -> Vec<Candidate> {
    let mut out = Vec::new();
    for root in roots {
        let Ok(entries) = std::fs::read_dir(root) else {
            continue; // a missing root is normal, not an error
        };
        for entry in entries.filter_map(|e| e.ok()) {
            if let Some(provider) = detect(&entry.path()) {
                out.push(Candidate {
                    path: entry.path(),
                    provider,
                });
            }
        }
    }
    out
}

pub fn discover() -> Vec<Candidate> {
    discover_in(&default_search_roots())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn every_provider_has_a_unique_id_and_a_display_name() {
        let mut ids: Vec<&str> = PROVIDERS.iter().map(|p| p.id).collect();
        let before = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), before, "provider ids must be unique");
        assert!(PROVIDERS.iter().all(|p| !p.display.is_empty()));
    }

    #[test]
    fn only_lightroom_starts_on_the_database_rung() {
        for p in PROVIDERS {
            let expected = if p.id == "lightroom" {
                Rung::Database
            } else {
                Rung::FolderLayout
            };
            assert_eq!(
                p.default_rung, expected,
                "{} has the wrong default rung",
                p.id
            );
        }
    }

    #[test]
    fn detects_a_modern_apple_library_by_structure_not_name() {
        let d = tempdir().unwrap();
        let lib = d.path().join("Some Odd Name");
        fs::create_dir_all(lib.join("originals")).unwrap();
        fs::create_dir_all(lib.join("database")).unwrap();
        fs::write(lib.join("database/Photos.sqlite"), b"").unwrap();
        assert_eq!(detect(&lib).map(|p| p.id), Some("apple-photos"));
    }

    #[test]
    fn a_bare_originals_folder_is_not_an_apple_library() {
        let d = tempdir().unwrap();
        fs::create_dir_all(d.path().join("originals")).unwrap();
        assert_eq!(
            detect(d.path()).map(|p| p.id),
            None,
            "needs the database sibling too"
        );
    }

    #[test]
    fn detects_lightroom_by_catalog_file() {
        let d = tempdir().unwrap();
        fs::write(d.path().join("Catalog.lrcat"), b"").unwrap();
        assert_eq!(detect(d.path()).map(|p| p.id), Some("lightroom"));
    }

    #[test]
    fn detects_takeout_by_sidecar_presence() {
        let d = tempdir().unwrap();
        fs::write(d.path().join("a.jpg"), b"").unwrap();
        fs::write(d.path().join("a.jpg.supplemental-metadata.json"), b"{}").unwrap();
        assert_eq!(detect(d.path()).map(|p| p.id), Some("google-takeout"));
    }

    #[test]
    fn finds_a_library_in_a_search_root() {
        let d = tempdir().unwrap();
        let pics = d.path().join("Pictures");
        let lib = pics.join("Photos Library.photoslibrary");
        fs::create_dir_all(lib.join("originals")).unwrap();
        fs::create_dir_all(lib.join("database")).unwrap();
        fs::write(lib.join("database/Photos.sqlite"), b"").unwrap();

        let found = discover_in(&[pics]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].provider.id, "apple-photos");
        assert_eq!(found[0].path, lib);
    }

    #[test]
    fn finds_several_libraries_and_reports_all() {
        let d = tempdir().unwrap();
        let pics = d.path().join("Pictures");
        let lib = pics.join("A.photoslibrary");
        fs::create_dir_all(lib.join("originals")).unwrap();
        fs::create_dir_all(lib.join("database")).unwrap();
        fs::write(lib.join("database/Photos.sqlite"), b"").unwrap();
        fs::create_dir_all(pics.join("Lightroom")).unwrap();
        fs::write(pics.join("Lightroom/Catalog.lrcat"), b"").unwrap();

        let found = discover_in(&[pics]);
        assert_eq!(
            found.len(),
            2,
            "both libraries must be reported, not the first"
        );
    }

    #[test]
    fn a_missing_search_root_is_skipped_silently() {
        let found = discover_in(&[std::path::PathBuf::from("/definitely/not/here")]);
        assert!(found.is_empty());
    }

    #[test]
    fn detects_takeout_when_sidecars_are_one_level_down() {
        // A real export's "Google Photos" folder contains only album
        // directories; the sidecars live inside them. Pointing straight at it
        // is a natural thing to do and must be recognised.
        let d = tempdir().unwrap();
        let album = d.path().join("Photos from 2019");
        fs::create_dir_all(&album).unwrap();
        fs::write(album.join("a.jpg"), b"").unwrap();
        fs::write(album.join("a.jpg.supplemental-metadata.json"), b"{}").unwrap();
        assert_eq!(detect(d.path()).map(|p| p.id), Some("google-takeout"));
    }

    #[test]
    #[test]
    fn detects_a_kept_originals_backup_with_no_catalog_beside_it() {
        // A real 399GB backup: someone copied only `originals/` off a Photos
        // library, so there is no database to detect by. The hex fan-out is
        // the whole signature.
        let d = tempdir().unwrap();
        let orig = d.path().join("originals");
        for name in "0123456789ABCDEF".chars() {
            fs::create_dir_all(orig.join(name.to_string())).unwrap();
        }
        assert_eq!(detect(d.path()).map(|p| p.id), Some("apple-photos"));
        // ...and when pointed straight at the originals folder itself.
        assert_eq!(detect(&orig).map(|p| p.id), Some("apple-photos"));
    }

    #[test]
    fn a_folder_merely_named_originals_is_not_an_apple_library() {
        // The guard that keeps the above from firing on ordinary workflows:
        // photographers keep an `originals/` folder all the time.
        let d = tempdir().unwrap();
        let orig = d.path().join("originals");
        fs::create_dir_all(orig.join("2024 Holiday")).unwrap();
        fs::create_dir_all(orig.join("Wedding")).unwrap();
        fs::write(orig.join("a.jpg"), b"").unwrap();
        assert!(detect(d.path()).is_none());
    }

    #[test]
    fn an_ordinary_folder_of_photos_detects_as_nothing() {
        let d = tempdir().unwrap();
        fs::write(d.path().join("a.jpg"), b"").unwrap();
        fs::write(d.path().join("b.jpg"), b"").unwrap();
        assert!(
            detect(d.path()).is_none(),
            "a plain folder needs videre scan, not import"
        );
    }
}
