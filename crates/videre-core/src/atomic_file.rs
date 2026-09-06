//! Atomic publication for rebuildable cache and local state files.
//!
//! The writer receives a unique temporary file in the destination directory.
//! Only a successful, flushed and synced write replaces the destination.
//! Original media sidecars use the library I/O layer instead because they
//! require root-anchored path handling.

use anyhow::{Context, Result};
use std::io::Write;
use std::path::Path;

pub fn publish(
    destination: &Path,
    write: impl FnOnce(&mut std::fs::File) -> Result<()>,
) -> Result<()> {
    let parent = destination
        .parent()
        .ok_or_else(|| anyhow::anyhow!("missing output parent for {}", destination.display()))?;
    std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    let mut temp = tempfile::Builder::new()
        .prefix(".tmp-")
        .tempfile_in(parent)
        .with_context(|| format!("create temporary file in {}", parent.display()))?;
    write(temp.as_file_mut())?;
    temp.as_file_mut()
        .flush()
        .with_context(|| format!("flush temporary file for {}", destination.display()))?;
    temp.as_file()
        .sync_all()
        .with_context(|| format!("sync temporary file for {}", destination.display()))?;
    temp.persist(destination)
        .map_err(|error| error.error)
        .with_context(|| format!("publish {}", destination.display()))?;
    std::fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .with_context(|| format!("sync {}", parent.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_write_preserves_prior_bytes_and_removes_the_temporary_file() {
        let temp = tempfile::tempdir().unwrap();
        let destination = temp.path().join("cache.bin");
        std::fs::write(&destination, b"old").unwrap();

        let error = publish(&destination, |file| {
            file.write_all(b"new")?;
            anyhow::bail!("stop before publication")
        })
        .unwrap_err();

        assert!(format!("{error:#}").contains("stop before publication"));
        assert_eq!(std::fs::read(&destination).unwrap(), b"old");
        let names: Vec<_> = std::fs::read_dir(temp.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from("cache.bin")]);
    }

    #[test]
    fn successful_write_atomically_replaces_prior_bytes() {
        let temp = tempfile::tempdir().unwrap();
        let destination = temp.path().join("cache.bin");
        std::fs::write(&destination, b"old").unwrap();

        publish(&destination, |file| {
            file.write_all(b"new")?;
            Ok(())
        })
        .unwrap();

        assert_eq!(std::fs::read(destination).unwrap(), b"new");
    }

    #[test]
    fn simultaneous_publications_use_distinct_temporary_files() {
        let temp = tempfile::tempdir().unwrap();
        let parent = temp.path().to_path_buf();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let mut workers = Vec::new();
        for name in ["a", "b"] {
            let destination = parent.join(name);
            let barrier = barrier.clone();
            workers.push(std::thread::spawn(move || {
                publish(&destination, |file| {
                    file.write_all(name.as_bytes())?;
                    barrier.wait();
                    Ok(())
                })
                .unwrap();
            }));
        }
        for worker in workers {
            worker.join().unwrap();
        }
        assert_eq!(std::fs::read(parent.join("a")).unwrap(), b"a");
        assert_eq!(std::fs::read(parent.join("b")).unwrap(), b"b");
        assert!(std::fs::read_dir(parent).unwrap().all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".tmp-")));
    }
}
