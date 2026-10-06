//! What each trash run moved, so `dedupe undo` can put it back.
//!
//! Every `dedupe trash` run (and every gallery Delete or Trash copies) writes
//! one JSONL manifest under `<root>/.videre/trash/`, a line per file as it
//! goes, so a run stopped halfway still records exactly what it moved. The
//! manifests form a stack:
//! `dedupe undo` restores the newest and removes it, so the next one reaches
//! the run before.

use std::io::Write as _;
use std::path::{Path, PathBuf};

/// A file moved to the trash: where it was, and where it landed when the
/// platform says (macOS). Recorded rather than searched for, because listing
/// the macOS Trash needs Full Disk Access while a known path in it does not,
/// and a name clash there gets an unguessable name (`a.jpg 11-52-15-741.jpg`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Moved {
    pub path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trashed: Option<PathBuf>,
}

/// One trashed library file, with what proves a trash copy is the same file.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Entry {
    #[serde(flatten)]
    pub file: Moved,
    pub hash: String,
    pub size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sidecar: Option<Moved>,
}

/// Appends the entries of one run. The file appears with the first entry, so
/// a run that moved nothing leaves no manifest to undo.
pub struct Writer {
    path: PathBuf,
    file: Option<std::fs::File>,
}

fn dir(state: &Path) -> PathBuf {
    state.join("trash")
}

impl Writer {
    pub fn new(state: &Path, source: &str) -> Writer {
        let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%S%.3fZ");
        Writer {
            path: dir(state).join(format!("{stamp}-{source}.jsonl")),
            file: None,
        }
    }

    pub fn record(&mut self, entry: &Entry) -> anyhow::Result<()> {
        if self.file.is_none() {
            std::fs::create_dir_all(self.path.parent().expect("manifest has a parent"))?;
            self.file = Some(
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&self.path)?,
            );
        }
        let file = self.file.as_mut().expect("opened above");
        let mut line = serde_json::to_string(entry)?;
        line.push('\n');
        file.write_all(line.as_bytes())?;
        file.flush()?;
        Ok(())
    }
}

/// One recorded run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub path: PathBuf,
    /// When it ran, for messages (`2026-10-05 10:00 UTC`).
    pub when: String,
    /// `dedupe` or `gallery`.
    pub source: String,
    pub entries: Vec<Entry>,
}

/// The recorded runs, newest first. The file names start with a UTC stamp,
/// so name order is time order.
pub fn runs(state: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let dir = dir(state);
    let read = match std::fs::read_dir(&dir) {
        Ok(r) => r,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut paths: Vec<PathBuf> = read
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
        .collect();
    paths.sort();
    paths.reverse();
    Ok(paths)
}

pub fn read(path: &Path) -> anyhow::Result<Run> {
    let text = std::fs::read_to_string(path)?;
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let mut entries = Vec::with_capacity(lines.len());
    for (i, line) in lines.iter().enumerate() {
        match serde_json::from_str::<Entry>(line) {
            Ok(e) => entries.push(e),
            // A process killed mid-write leaves a torn last line; every line
            // before it is whole, since each is flushed before the next file
            // moves.
            Err(_) if i + 1 == lines.len() => {}
            Err(e) => anyhow::bail!("{}: line {}: {e}", path.display(), i + 1),
        }
    }
    let name = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let (stamp, source) = name.split_once('-').unwrap_or((name.as_str(), ""));
    let when = chrono::NaiveDateTime::parse_from_str(stamp, "%Y%m%dT%H%M%S%.3fZ")
        .map(|t| t.format("%Y-%m-%d %H:%M UTC").to_string())
        .unwrap_or_else(|_| stamp.to_string());
    Ok(Run {
        path: path.to_path_buf(),
        when,
        source: source.to_string(),
        entries,
    })
}

/// Rewrite `run`'s manifest to hold only `remaining`, or remove it when
/// nothing remains.
pub fn keep_only(run: &Run, remaining: &[Entry]) -> anyhow::Result<()> {
    if remaining.is_empty() {
        match std::fs::remove_file(&run.path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
            _ => return Ok(()),
        }
    }
    let mut text = String::new();
    for e in remaining {
        text.push_str(&serde_json::to_string(e)?);
        text.push('\n');
    }
    let tmp = run.path.with_extension("jsonl.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, &run.path)?;
    Ok(())
}

/// A place a trashed file may be now. `info` is the Linux `.trashinfo`
/// describing it, removed once the file is back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub file: PathBuf,
    pub info: Option<PathBuf>,
}

/// Where each entry's file, and its sidecar, may be in the trash now.
pub trait Locate {
    fn candidates(&self, entries: &[Entry]) -> Vec<(Vec<Candidate>, Vec<Candidate>)>;
}

fn recorded(m: &Moved) -> Vec<Candidate> {
    m.trashed
        .iter()
        .filter(|p| p.symlink_metadata().is_ok())
        .map(|p| Candidate {
            file: p.clone(),
            info: None,
        })
        .collect()
}

/// Where the trash run recorded each file landed (macOS; tests everywhere).
#[cfg(any(target_os = "macos", test))]
pub struct Recorded;

#[cfg(any(target_os = "macos", test))]
impl Locate for Recorded {
    fn candidates(&self, entries: &[Entry]) -> Vec<(Vec<Candidate>, Vec<Candidate>)> {
        entries
            .iter()
            .map(|e| {
                let side = e.sidecar.as_ref().map(recorded).unwrap_or_default();
                (recorded(&e.file), side)
            })
            .collect()
    }
}

/// The freedesktop.org trash's own listing, read once, newest first per
/// original path, after any recorded landing.
#[cfg(not(target_os = "macos"))]
pub struct Listed;

#[cfg(not(target_os = "macos"))]
impl Locate for Listed {
    fn candidates(&self, entries: &[Entry]) -> Vec<(Vec<Candidate>, Vec<Candidate>)> {
        let mut items = trash::os_limited::list().unwrap_or_default();
        items.sort_by_key(|i| std::cmp::Reverse(i.time_deleted));
        let listed = |m: &Moved| -> Vec<Candidate> {
            let mut out = recorded(m);
            for item in items.iter().filter(|i| i.original_path() == m.path) {
                let info = PathBuf::from(&item.id);
                let (Some(dir), Some(stem)) =
                    (info.parent().and_then(Path::parent), info.file_stem())
                else {
                    continue;
                };
                out.push(Candidate {
                    file: dir.join("files").join(stem),
                    info: Some(info.clone()),
                });
            }
            out
        };
        entries
            .iter()
            .map(|e| {
                let side = e.sidecar.as_ref().map(&listed).unwrap_or_default();
                (listed(&e.file), side)
            })
            .collect()
    }
}

/// The platform's way of finding trashed files.
pub fn system() -> Box<dyn Locate> {
    #[cfg(target_os = "macos")]
    return Box::new(Recorded);
    #[cfg(not(target_os = "macos"))]
    return Box::new(Listed);
}

/// What undoing one entry will do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Put back from this place in the trash.
    Restore(Candidate),
    /// Something is already at the original path; never overwritten.
    Occupied,
    /// Nothing in the trash has this file's content (emptied, or replaced).
    NotInTrash,
}

/// The newest run and what undoing it would do, entry by entry; `None` when
/// no run is recorded.
pub fn plan_latest(state: &Path, locate: &dyn Locate) -> anyhow::Result<Option<(Run, Vec<Step>)>> {
    let Some(newest) = runs(state)?.into_iter().next() else {
        return Ok(None);
    };
    let run = read(&newest)?;
    let candidates = locate.candidates(&run.entries);
    let steps = run
        .entries
        .iter()
        .zip(candidates)
        .map(|(entry, (files, _))| {
            if entry.file.path.symlink_metadata().is_ok() {
                return Step::Occupied;
            }
            files
                .into_iter()
                .find(|c| is_the_same_file(entry, &c.file))
                .map_or(Step::NotInTrash, Step::Restore)
        })
        .collect();
    Ok(Some((run, steps)))
}

/// Same size and content hash: a trash copy that is not provably the file
/// that was trashed is never taken.
fn is_the_same_file(entry: &Entry, candidate: &Path) -> bool {
    candidate.metadata().is_ok_and(|m| m.len() == entry.size)
        && videre::hasher::hash_file(candidate).is_ok_and(|r| r.hash == entry.hash)
}

/// Move `from` back to `to`, creating its folder, never replacing a file.
fn put_back(from: &Path, to: &Path) -> std::io::Result<()> {
    if to.symlink_metadata().is_ok() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "a file is already there",
        ));
    }
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match std::fs::rename(from, to) {
        // A trash on another volume than the original (Linux home trash).
        Err(e) if e.raw_os_error() == Some(libc::EXDEV) => {
            std::fs::copy(from, to)?;
            std::fs::remove_file(from)
        }
        other => other,
    }
}

/// What an undo did.
#[derive(Debug, Default, serde::Serialize)]
pub struct UndoReport {
    pub when: String,
    pub restored: Vec<PathBuf>,
    pub not_in_trash: Vec<PathBuf>,
    pub occupied: Vec<PathBuf>,
    pub failed: Vec<(PathBuf, String)>,
    /// Runs still recorded after this one, for the next `dedupe undo`.
    pub runs_left: usize,
}

/// Put back the newest run's files, bring them back into the library, and
/// pop the run off the stack. Entries skipped because their path is taken,
/// or that failed to move, stay recorded so the run can be undone again.
pub fn undo_latest(
    conn: &rusqlite::Connection,
    library: &videre_core::library::LibraryContext,
    locate: &dyn Locate,
    progress: &videre_core::progress::Progress,
) -> anyhow::Result<Option<UndoReport>> {
    let state = &library.paths.state;
    let Some((run, steps)) = plan_latest(state, locate)? else {
        return Ok(None);
    };
    let sidecars: Vec<Vec<Candidate>> = locate
        .candidates(&run.entries)
        .into_iter()
        .map(|(_, side)| side)
        .collect();
    let mut report = UndoReport {
        when: run.when.clone(),
        ..UndoReport::default()
    };
    let mut keep = Vec::new();
    for ((entry, step), side) in run.entries.iter().zip(steps).zip(sidecars) {
        let path = &entry.file.path;
        match step {
            Step::Occupied => {
                report.occupied.push(path.clone());
                keep.push(entry.clone());
                progress.skip(
                    &path.display().to_string(),
                    anyhow::anyhow!("a file is already there"),
                );
            }
            Step::NotInTrash => {
                report.not_in_trash.push(path.clone());
                progress.skip(
                    &path.display().to_string(),
                    anyhow::anyhow!("not in the trash"),
                );
            }
            Step::Restore(c) => match put_back(&c.file, path) {
                Ok(()) => {
                    if let Some(info) = &c.info {
                        let _ = std::fs::remove_file(info);
                    }
                    // The sidecar follows its photo; one that cannot come
                    // back is warned about and never fails the photo.
                    if let (Some(s), Some(sc)) = (&entry.sidecar, side.first()) {
                        match put_back(&sc.file, &s.path) {
                            Ok(()) => {
                                if let Some(info) = &sc.info {
                                    let _ = std::fs::remove_file(info);
                                }
                            }
                            Err(e) => {
                                tracing::warn!("could not put back the sidecar {:?}: {e}", s.path)
                            }
                        }
                    }
                    report.restored.push(path.clone());
                    progress.tick();
                }
                Err(e) => {
                    report.failed.push((path.clone(), e.to_string()));
                    keep.push(entry.clone());
                    progress.skip(&path.display().to_string(), e.into());
                }
            },
        }
    }
    // Back in the library before returning: rows return now, and embeddings
    // and faces show as outstanding for the next embed, faces or watch.
    crate::indexing::index_paths(
        conn,
        library,
        report.restored.clone(),
        library.settings.xmp_precedence,
        // Quiet: the restore already showed its own progress.
        true,
    )?;
    keep_only(&run, &keep)?;
    report.runs_left = runs(state)?.len();
    Ok(Some(report))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str) -> Entry {
        Entry {
            file: Moved {
                path: PathBuf::from(path),
                trashed: Some(PathBuf::from(format!("Trash/{path}"))),
            },
            hash: format!("h-{path}"),
            size: 3,
            sidecar: None,
        }
    }

    fn write_run(state: &Path, name: &str, entries: &[Entry]) -> PathBuf {
        let mut w = Writer {
            path: dir(state).join(name),
            file: None,
        };
        for e in entries {
            w.record(e).unwrap();
        }
        w.path
    }

    #[test]
    fn a_writer_that_records_nothing_leaves_no_file() {
        let state = tempfile::tempdir().unwrap();
        let _w = Writer::new(state.path(), "dedupe");
        assert!(runs(state.path()).unwrap().is_empty());
    }

    #[test]
    fn entries_round_trip_and_runs_are_newest_first() {
        let state = tempfile::tempdir().unwrap();
        let mut a = entry("Fotoğraflar/IMG_1-edited.jpg");
        a.sidecar = Some(Moved {
            path: PathBuf::from("Fotoğraflar/IMG_1-edited.jpg.xmp"),
            trashed: None,
        });
        let b = entry("Fotoğraflar/çiçek-edited.jpg");
        write_run(
            state.path(),
            "20261005T100000.000Z-dedupe.jsonl",
            &[a.clone()],
        );
        write_run(
            state.path(),
            "20261005T110000.000Z-gallery.jsonl",
            std::slice::from_ref(&b),
        );

        let stack = runs(state.path()).unwrap();
        assert_eq!(stack.len(), 2);
        let newest = read(&stack[0]).unwrap();
        assert_eq!(newest.source, "gallery");
        assert_eq!(newest.when, "2026-10-05 11:00 UTC");
        assert_eq!(newest.entries, vec![b]);
        assert_eq!(read(&stack[1]).unwrap().entries, vec![a]);
    }

    #[test]
    fn keep_only_rewrites_and_deletes_when_empty() {
        let state = tempfile::tempdir().unwrap();
        let (a, b) = (entry("a.jpg"), entry("b.jpg"));
        let path = write_run(
            state.path(),
            "20261005T100000.000Z-dedupe.jsonl",
            &[a.clone(), b.clone()],
        );
        let run = read(&path).unwrap();
        keep_only(&run, std::slice::from_ref(&b)).unwrap();
        assert_eq!(read(&path).unwrap().entries, vec![b]);
        keep_only(&run, &[]).unwrap();
        assert!(!path.exists());
        assert!(runs(state.path()).unwrap().is_empty());
    }

    #[test]
    fn a_torn_last_line_is_ignored() {
        let state = tempfile::tempdir().unwrap();
        let path = write_run(
            state.path(),
            "20261005T100000.000Z-dedupe.jsonl",
            &[entry("a.jpg")],
        );
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        f.write_all(b"{\"path\":\"b.j").unwrap();
        assert_eq!(read(&path).unwrap().entries, vec![entry("a.jpg")]);
    }

    /// A library, its database, and a directory standing in for the trash.
    struct Fixture {
        _dirs: [tempfile::TempDir; 3],
        root: PathBuf,
        trash: PathBuf,
        library: videre_core::library::LibraryContext,
        conn: rusqlite::Connection,
    }

    fn fixture() -> Fixture {
        let dirs = [
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
        ];
        let root = dirs[0].path().canonicalize().unwrap();
        let trash = dirs[2].path().canonicalize().unwrap();
        let library = videre_core::library::LibraryContext::new(&root, dirs[1].path()).unwrap();
        let conn = videre_core::library_db::initialize(&library).unwrap();
        Fixture {
            _dirs: dirs,
            root,
            trash,
            library,
            conn,
        }
    }

    impl Fixture {
        /// Create `rel` with `bytes`, index it, then "trash" it into the
        /// stand-in trash as `landed`, forgetting its row and recording it.
        fn trash(&self, w: &mut Writer, rel: &str, bytes: &[u8], landed: &str) -> PathBuf {
            let path = self.root.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, bytes).unwrap();
            crate::indexing::index_paths(
                &self.conn,
                &self.library,
                vec![path.clone()],
                videre_core::marks::XmpPrecedence::Db,
                true,
            )
            .unwrap();
            let hash: String = self
                .conn
                .query_row(
                    "SELECT hash FROM file_hashes WHERE path = ?1",
                    [path.to_string_lossy()],
                    |r| r.get(0),
                )
                .unwrap();
            let to = self.trash.join(landed);
            std::fs::rename(&path, &to).unwrap();
            self.conn
                .execute(
                    "DELETE FROM file_hashes WHERE path = ?1",
                    [path.to_string_lossy()],
                )
                .unwrap();
            w.record(&Entry {
                file: Moved {
                    path: path.clone(),
                    trashed: Some(to),
                },
                hash,
                size: bytes.len() as u64,
                sidecar: None,
            })
            .unwrap();
            path
        }

        fn writer(&self, stamp: &str) -> Writer {
            Writer {
                path: dir(&self.library.paths.state).join(format!("{stamp}-dedupe.jsonl")),
                file: None,
            }
        }

        fn undo(&self) -> Option<UndoReport> {
            let progress = videre_core::progress::Progress::new_counting(0, true, "files");
            undo_latest(&self.conn, &self.library, &Recorded, &progress).unwrap()
        }

        fn indexed(&self, path: &Path) -> bool {
            self.conn
                .query_row(
                    "SELECT COUNT(*) FROM file_hashes WHERE path = ?1",
                    [path.to_string_lossy()],
                    |r| r.get::<_, i64>(0),
                )
                .unwrap()
                == 1
        }
    }

    #[test]
    fn two_runs_undo_newest_first() {
        let f = fixture();
        let mut first = f.writer("20261005T100000.000Z");
        let a = f.trash(
            &mut first,
            "Fotoğraflar/a-edited.jpg",
            b"birinci",
            "a-edited.jpg",
        );
        let mut second = f.writer("20261005T110000.000Z");
        let b = f.trash(
            &mut second,
            "Fotoğraflar/b-edited.jpg",
            b"ikinci",
            "b-edited.jpg",
        );

        let r = f.undo().unwrap();
        assert_eq!(r.restored, vec![b.clone()]);
        assert_eq!(r.runs_left, 1);
        assert!(b.exists() && !a.exists());

        let r = f.undo().unwrap();
        assert_eq!(r.restored, vec![a.clone()]);
        assert_eq!(r.runs_left, 0);
        assert!(a.exists());
        assert!(f.undo().is_none(), "nothing left to undo");
    }

    #[test]
    fn restored_files_are_back_in_the_library() {
        let f = fixture();
        let mut w = f.writer("20261005T100000.000Z");
        let a = f.trash(
            &mut w,
            "çiçek-edited.jpg",
            "çiçek".as_bytes(),
            "çiçek-edited.jpg",
        );
        assert!(!f.indexed(&a));
        f.undo().unwrap();
        assert!(f.indexed(&a));
    }

    #[test]
    fn a_trash_copy_with_other_bytes_is_not_taken() {
        let f = fixture();
        let mut w = f.writer("20261005T100000.000Z");
        let a = f.trash(&mut w, "a.jpg", b"asil", "a.jpg 11-52-15-741.jpg");
        std::fs::write(f.trash.join("a.jpg 11-52-15-741.jpg"), b"baska").unwrap();
        let r = f.undo().unwrap();
        assert_eq!(r.not_in_trash, vec![a.clone()]);
        assert!(r.restored.is_empty() && !a.exists());
        assert!(f.trash.join("a.jpg 11-52-15-741.jpg").exists());
        assert_eq!(r.runs_left, 0, "a run that cannot be restored is dropped");
    }

    #[test]
    fn an_occupied_original_is_skipped_and_the_run_stays_undoable() {
        let f = fixture();
        let mut w = f.writer("20261005T100000.000Z");
        let a = f.trash(&mut w, "a.jpg", b"asil", "a.jpg");
        std::fs::write(&a, b"yeni dosya").unwrap();
        let r = f.undo().unwrap();
        assert_eq!(r.occupied, vec![a.clone()]);
        assert_eq!(
            std::fs::read(&a).unwrap(),
            b"yeni dosya",
            "never overwritten"
        );
        assert_eq!(r.runs_left, 1, "kept for another try");

        std::fs::remove_file(&a).unwrap();
        let r = f.undo().unwrap();
        assert_eq!(r.restored, vec![a.clone()]);
        assert_eq!(std::fs::read(&a).unwrap(), b"asil");
    }

    #[test]
    fn a_file_emptied_from_the_trash_is_reported_and_the_run_is_dropped() {
        let f = fixture();
        let mut w = f.writer("20261005T100000.000Z");
        let a = f.trash(&mut w, "a.jpg", b"asil", "a.jpg");
        std::fs::remove_file(f.trash.join("a.jpg")).unwrap();
        let r = f.undo().unwrap();
        assert_eq!(r.not_in_trash, vec![a]);
        assert!(runs(&f.library.paths.state).unwrap().is_empty());
    }

    #[test]
    fn the_sidecar_comes_back_with_its_photo() {
        let f = fixture();
        let photo = f.root.join("Tatil/deniz.jpg");
        let sidecar = f.root.join("Tatil/deniz.jpg.xmp");
        let mut w = f.writer("20261005T100000.000Z");
        f.trash(&mut w, "Tatil/deniz.jpg", b"deniz", "deniz.jpg");
        // Rewrite the manifest with the sidecar recorded too.
        let run_path = runs(&f.library.paths.state).unwrap().remove(0);
        let mut run = read(&run_path).unwrap();
        std::fs::write(f.trash.join("deniz.jpg.xmp"), b"<x:xmpmeta/>").unwrap();
        run.entries[0].sidecar = Some(Moved {
            path: sidecar.clone(),
            trashed: Some(f.trash.join("deniz.jpg.xmp")),
        });
        keep_only(&run, &run.entries).unwrap();

        f.undo().unwrap();
        assert!(photo.exists() && sidecar.exists());
    }

    #[test]
    fn a_dry_run_plan_changes_nothing() {
        let f = fixture();
        let mut w = f.writer("20261005T100000.000Z");
        let a = f.trash(&mut w, "a.jpg", b"asil", "a.jpg");
        let (run, steps) = plan_latest(&f.library.paths.state, &Recorded)
            .unwrap()
            .unwrap();
        assert_eq!(run.entries[0].file.path, a);
        assert!(matches!(steps[0], Step::Restore(_)));
        assert!(!a.exists());
        assert_eq!(runs(&f.library.paths.state).unwrap().len(), 1);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn system_trash_round_trip() {
        let f = fixture();
        let path = f
            .root
            .join(format!("videre-geri-al-{}.jpg", std::process::id()));
        std::fs::write(&path, b"gercek cop kutusu").unwrap();
        crate::indexing::index_paths(
            &f.conn,
            &f.library,
            vec![path.clone()],
            videre_core::marks::XmpPrecedence::Db,
            true,
        )
        .unwrap();
        let mut w = Writer::new(&f.library.paths.state, "dedupe");
        let progress = videre_core::progress::Progress::new_counting(1, true, "files");
        let res = crate::removal::remove_and_forget(
            Some(&f.conn),
            std::slice::from_ref(&path),
            crate::removal::Method::Trash,
            &progress,
            Some(&mut w),
        );
        assert_eq!(res[0].1, crate::removal::Outcome::Removed);
        assert!(!path.exists());

        let r = undo_latest(&f.conn, &f.library, system().as_ref(), &progress)
            .unwrap()
            .unwrap();
        assert_eq!(r.restored, vec![path.clone()]);
        assert_eq!(std::fs::read(&path).unwrap(), b"gercek cop kutusu");
    }
}
