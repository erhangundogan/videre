//! A held `flock` whose release does not depend on who else has the file.
//!
//! A `flock` belongs to the open file description, not to one descriptor, and
//! closing a descriptor releases it only when that was the description's last
//! reference. A child process spawned by any thread while a lock is held gets
//! such a reference and keeps it until its exec closes the close-on-exec copy.
//! For a large binary that takes long enough to matter: measured with a test
//! thread spawning `videre --version` in a loop, `initialize` followed at once
//! by `open_existing` was refused as "its activity lock is held" on 47 of 300
//! libraries, and 0 of 300 with no spawning thread.
//!
//! So a held lock is released by an explicit unlock on drop, which acts on the
//! description itself, and only then is the file closed. Process death still
//! releases it too, so nothing correct depends on `Drop` running.

use std::fs::File;

/// A file holding a `flock`, released when this is dropped.
#[derive(Debug)]
pub(crate) struct HeldLock(File);

impl HeldLock {
    /// Wrap a file whose lock the caller has just taken.
    pub(crate) fn new(file: File) -> Self {
        HeldLock(file)
    }

    #[cfg(test)]
    pub(crate) fn file(&self) -> &File {
        &self.0
    }
}

impl Drop for HeldLock {
    fn drop(&mut self) {
        // Called by its full path: std's inherent `File::unlock` (1.89) would
        // otherwise win over the trait method, mixing two implementations.
        if let Err(e) = fs2::FileExt::unlock(&self.0) {
            tracing::debug!("unlock failed, the lock now ends when the file closes: {e}");
        }
    }
}
