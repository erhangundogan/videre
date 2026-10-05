//! What each trash run moved, so `dedupe --undo` can put it back.
//!
//! Every `--trash` run (and every gallery Delete) writes one JSONL manifest
//! under `<root>/.videre/trash/`, a line per file as it goes, so a run stopped
//! halfway still records exactly what it moved. The manifests form a stack:
//! `--undo` restores the newest and removes it, so the next `--undo` reaches
//! the run before.

use std::io::Write as _;
use std::path::{Path, PathBuf};

/// One trashed file.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Entry {
    pub path: PathBuf,
    pub hash: String,
    pub size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sidecar: Option<PathBuf>,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str) -> Entry {
        Entry {
            path: PathBuf::from(path),
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
        a.sidecar = Some(PathBuf::from("Fotoğraflar/IMG_1-edited.jpg.xmp"));
        let b = entry("Fotoğraflar/çiçek-edited.jpg");
        write_run(state.path(), "20261005T100000.000Z-dedupe.jsonl", &[a.clone()]);
        write_run(state.path(), "20261005T110000.000Z-gallery.jsonl", &[b.clone()]);

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
        let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(b"{\"path\":\"b.j").unwrap();
        assert_eq!(read(&path).unwrap().entries, vec![entry("a.jpg")]);
    }
}
