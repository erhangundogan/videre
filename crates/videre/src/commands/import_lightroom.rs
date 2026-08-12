//! Adobe Lightroom Classic: the catalog rung of the location contract.
//!
//! Lightroom is the one source that starts on the database rung, and the one
//! for which that rung is not optional. It never owns the files: the catalog
//! is a set of pointers to ordinary folders the user chose, so there is no
//! `originals/` to fall back to and no layout to probe. Reading `.lrcat` *is*
//! the mechanism, not an enhancement.
//!
//! Location only. What gets imported still comes from the files themselves,
//! exactly as for every other source.

use std::path::{Path, PathBuf};

/// Tables and the columns actually read, probed before any query so an
/// unfamiliar catalog produces a message naming what was missing rather than a
/// SQL error or a panic. Adobe's schema has held across many Lightroom Classic
/// versions, but the same defensive posture applies regardless.
const REQUIRED: &[(&str, &[&str])] = &[
    ("AgLibraryRootFolder", &["id_local", "absolutePath"]),
    (
        "AgLibraryFolder",
        &["id_local", "rootFolder", "pathFromRoot"],
    ),
    ("AgLibraryFile", &["id_local", "folder"]),
];

/// One folder the catalog references, and whether its volume is mounted.
#[derive(Debug)]
pub(crate) struct RootFolder {
    pub path: PathBuf,
    pub online: bool,
}

/// The `.lrcat` for a path that is either the catalog or the folder holding it.
pub(crate) fn find_catalog(path: &Path) -> Option<PathBuf> {
    if path.extension().is_some_and(|e| e == "lrcat") {
        return Some(path.to_path_buf());
    }
    let mut catalogs: Vec<PathBuf> = std::fs::read_dir(path)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "lrcat"))
        .collect();
    catalogs.sort();
    catalogs.into_iter().next()
}

/// Every distinct folder the catalog points at, as absolute paths.
///
/// The catalog is copied before reading. Lightroom holds the original open,
/// and copying makes the operation read-only in fact rather than only in
/// intent, so this can never be the thing that corrupts someone's catalog.
pub(crate) fn read_root_folders(catalog: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let copy = CatalogCopy::of(catalog)?;
    let conn = rusqlite::Connection::open(copy.catalog())?;
    check_schema(&conn)?;

    let mut stmt = conn.prepare(
        "SELECT DISTINCT root.absolutePath || folder.pathFromRoot \
         FROM AgLibraryFile   file \
         JOIN AgLibraryFolder folder ON file.folder = folder.id_local \
         JOIN AgLibraryRootFolder root ON folder.rootFolder = root.id_local",
    )?;
    let mut roots: Vec<PathBuf> = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .filter_map(|r| r.ok())
        .map(|s| PathBuf::from(trim_trailing_slash(&s)))
        .collect();
    roots.sort();
    roots.dedup();
    Ok(roots)
}

/// Marks each root online or offline. A catalog routinely references external
/// drives that are not connected: normal, never an error, and never counted as
/// missing files, mirroring how `videre prune` refuses to delete rows for an
/// unplugged drive.
pub(crate) fn classify(roots: Vec<PathBuf>) -> Vec<RootFolder> {
    roots
        .into_iter()
        .map(|path| RootFolder {
            online: path.is_dir(),
            path,
        })
        .collect()
}

pub(crate) fn online_paths(roots: &[RootFolder]) -> Vec<PathBuf> {
    roots
        .iter()
        .filter(|r| r.online)
        .map(|r| r.path.clone())
        .collect()
}

fn check_schema(conn: &rusqlite::Connection) -> anyhow::Result<()> {
    let present: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")?
        .query_map([], |r| r.get::<_, String>(0))?
        .filter_map(|r| r.ok())
        .collect();

    let missing_tables: Vec<&str> = REQUIRED
        .iter()
        .map(|(t, _)| *t)
        .filter(|t| !present.iter().any(|p| p == t))
        .collect();
    anyhow::ensure!(
        missing_tables.is_empty(),
        "unrecognised catalog schema: missing table(s) {}",
        missing_tables.join(", ")
    );

    for (table, columns) in REQUIRED {
        let have: Vec<String> = conn
            .prepare(&format!("PRAGMA table_info({table})"))?
            .query_map([], |r| r.get::<_, String>(1))?
            .filter_map(|r| r.ok())
            .collect();
        let missing: Vec<&str> = columns
            .iter()
            .copied()
            .filter(|c| !have.iter().any(|h| h == c))
            .collect();
        anyhow::ensure!(
            missing.is_empty(),
            "unrecognised catalog schema: {table} has no column(s) {}",
            missing.join(", ")
        );
    }
    Ok(())
}

/// `AgLibraryRootFolder.absolutePath` and `pathFromRoot` both carry trailing
/// slashes, so the concatenation does too. The filesystem root is left alone.
fn trim_trailing_slash(path: &str) -> &str {
    match path.trim_end_matches('/') {
        "" => "/",
        trimmed => trimmed,
    }
}

/// A private copy of the catalog and its WAL companions, deleted on drop.
struct CatalogCopy {
    dir: PathBuf,
    name: std::ffi::OsString,
}

impl CatalogCopy {
    fn of(catalog: &Path) -> anyhow::Result<Self> {
        anyhow::ensure!(catalog.is_file(), "{} is not a file", catalog.display());
        let name = catalog
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("{} has no file name", catalog.display()))?
            .to_os_string();
        let dir = std::env::temp_dir().join(format!(
            "videre-lrcat-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir)?;

        let copy = Self { dir, name };
        std::fs::copy(catalog, copy.catalog())?;
        // The `-wal` holds writes not yet folded into the catalog, so copying
        // the catalog alone can read a stale or inconsistent view.
        for suffix in ["-wal", "-shm"] {
            let mut from = catalog.as_os_str().to_os_string();
            from.push(suffix);
            let from = PathBuf::from(from);
            if from.exists() {
                let mut to = copy.catalog().into_os_string();
                to.push(suffix);
                std::fs::copy(&from, PathBuf::from(to))?;
            }
        }
        Ok(copy)
    }

    fn catalog(&self) -> PathBuf {
        self.dir.join(&self.name)
    }
}

impl Drop for CatalogCopy {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use tempfile::tempdir;

    /// A synthetic catalog holding only the tables the importer reads. No
    /// Adobe fixture is needed or licensable, and the schema below is the
    /// whole contract.
    fn catalog_with(dir: &Path, roots: &[(i64, &str)], folders: &[(i64, i64, &str)]) -> PathBuf {
        let path = dir.join("Catalog.lrcat");
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE AgLibraryRootFolder (id_local INTEGER, absolutePath TEXT, name TEXT);
             CREATE TABLE AgLibraryFolder (id_local INTEGER, rootFolder INTEGER, pathFromRoot TEXT);
             CREATE TABLE AgLibraryFile (id_local INTEGER, folder INTEGER, baseName TEXT, extension TEXT);",
        )
        .unwrap();
        for (id, abs) in roots {
            conn.execute(
                "INSERT INTO AgLibraryRootFolder VALUES (?, ?, 'r')",
                rusqlite::params![id, abs],
            )
            .unwrap();
        }
        for (id, root, from) in folders {
            conn.execute(
                "INSERT INTO AgLibraryFolder VALUES (?, ?, ?)",
                rusqlite::params![id, root, from],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO AgLibraryFile VALUES (?, ?, 'IMG_0001', 'CR2')",
                rusqlite::params![id * 100, id],
            )
            .unwrap();
        }
        path
    }

    #[test]
    fn the_three_table_join_reconstructs_absolute_paths() {
        let d = tempdir().unwrap();
        let base = d.path().to_string_lossy().to_string();
        let catalog = catalog_with(
            d.path(),
            &[(1, &format!("{base}/"))],
            // A nested pathFromRoot is the case a two-table join gets wrong.
            &[(10, 1, "2024/06/"), (11, 1, "")],
        );

        let mut roots = read_root_folders(&catalog).unwrap();
        roots.sort();
        assert_eq!(
            roots,
            vec![
                PathBuf::from(&base),
                PathBuf::from(format!("{base}/2024/06"))
            ]
        );
    }

    #[test]
    fn a_root_folder_that_is_not_mounted_is_offline_not_missing_files() {
        // Mirrors how `videre prune` refuses to delete rows for an unplugged
        // drive: an external volume that is not connected is a normal state of
        // a Lightroom catalog, never an error and never a missing file.
        let d = tempdir().unwrap();
        let here = d.path().to_string_lossy().to_string();
        let catalog = catalog_with(
            d.path(),
            &[
                (1, &format!("{here}/")),
                (2, "/Volumes/Definitely Not Here/"),
            ],
            &[(10, 1, ""), (20, 2, "Archive/")],
        );

        let classified = classify(read_root_folders(&catalog).unwrap());
        assert_eq!(classified.len(), 2, "every root is reported, online or not");

        let offline: Vec<_> = classified.iter().filter(|r| !r.online).collect();
        assert_eq!(offline.len(), 1);
        assert!(offline[0].path.starts_with("/Volumes/Definitely Not Here"));

        let online: Vec<PathBuf> = online_paths(&classified);
        assert_eq!(
            online,
            vec![PathBuf::from(&here)],
            "only mounted roots are handed on to be walked"
        );
    }

    #[test]
    fn an_unfamiliar_schema_fails_clearly_rather_than_panicking() {
        let d = tempdir().unwrap();
        let path = d.path().join("Broken.lrcat");
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch("CREATE TABLE AgLibraryFile (id_local INTEGER, folder INTEGER);")
            .unwrap();
        drop(conn);

        let err = read_root_folders(&path).unwrap_err().to_string();
        assert!(
            err.contains("AgLibraryRootFolder") && err.contains("AgLibraryFolder"),
            "must name the tables that were missing: {err}"
        );
    }

    #[test]
    fn a_missing_column_is_reported_rather_than_queried_blindly() {
        let d = tempdir().unwrap();
        let path = d.path().join("Old.lrcat");
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE AgLibraryRootFolder (id_local INTEGER);
             CREATE TABLE AgLibraryFolder (id_local INTEGER, rootFolder INTEGER, pathFromRoot TEXT);
             CREATE TABLE AgLibraryFile (id_local INTEGER, folder INTEGER);",
        )
        .unwrap();
        drop(conn);

        let err = read_root_folders(&path).unwrap_err().to_string();
        assert!(
            err.contains("absolutePath"),
            "must name the missing column: {err}"
        );
    }

    #[test]
    fn a_catalog_that_cannot_be_read_falls_straight_to_asking_the_user() {
        // Lightroom has no folder layout to fall through to, so a failed
        // catalog read must reach NotFound rather than silently finding
        // nothing.
        let d = tempdir().unwrap();
        let lightroom = videre_core::import_providers::PROVIDERS
            .iter()
            .find(|p| p.id == "lightroom")
            .unwrap();

        let got = videre_core::import_location::locate_with_database(
            lightroom,
            d.path(),
            &videre_core::import_location::LocateOptions::default(),
            None,
        )
        .unwrap();

        match got {
            videre_core::import_location::Located::NotFound { tried } => {
                assert!(
                    tried.iter().any(|t| t.contains("catalog")),
                    "must say the catalog was the rung that failed: {tried:?}"
                );
            }
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[test]
    fn the_catalog_file_is_found_from_the_folder_that_holds_it() {
        let d = tempdir().unwrap();
        let catalog = catalog_with(d.path(), &[(1, "/x/")], &[(10, 1, "")]);
        assert_eq!(find_catalog(d.path()), Some(catalog.clone()));
        assert_eq!(find_catalog(&catalog), Some(catalog));
        assert_eq!(find_catalog(Path::new("/definitely/not/here")), None);
    }
}
