//! The explicit identity of one media library.
//!
//! Everything videre derives from a library (its database, config, locks,
//! embeddings, thumbnails) lives under the library root itself, in the
//! reserved `<root>/.videre` state directory, or in a cache keyed by that
//! root, instead of one process-global home resolved from the environment.
//! [`LibraryContext`] is that arrangement as a value: constructed from
//! explicit inputs, reading no environment variable, and creating no
//! directory anywhere. Choosing the root is a separate, earlier decision
//! that happens at the CLI; this type only makes the chosen root precise.
//!
//! The root is pinned for the process lifetime. Construction opens a handle
//! on the canonical root and records the directory's device/inode identity;
//! [`LibraryContext::ensure_root_identity`] rechecks, bounded, and errors if
//! the path now names a different directory, because a context that silently
//! followed a replaced or unmounted root would write one library's state
//! into another's. A symlink used to reach the library root is supported:
//! canonicalization makes aliases of one root the same library, sharing one
//! cache namespace.

use crate::io_timeout;
use crate::library_config::LibraryConfig;
use anyhow::{bail, Context, Result};
use std::fs::File;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The reserved state directory videre keeps inside every library root.
/// Crate visible: the pinned-I/O layer refuses the same name as a sidecar
/// target, so the literal is written once, here.
pub(crate) const STATE_DIR: &str = ".videre";

/// Where everything derived from one library root lives. All paths are
/// absolute; the root is the canonical root, so every field is the same in
/// every context opened on the same library no matter which alias reached it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryPaths {
    /// The canonical library root this context is pinned to.
    pub root: PathBuf,
    /// `<root>/.videre`, reserved for videre's own state inside the library.
    pub state: PathBuf,
    /// The library's SQLite database.
    pub db: PathBuf,
    /// The library's `config.toml`.
    pub config: PathBuf,
    /// Default JSONL export path (`scan --output` with no value).
    pub jsonl: PathBuf,
    /// Directory holding this library's per-model embedding databases.
    pub embeddings: PathBuf,
    /// Directory holding flock sidecar lock files for this library's database.
    pub locks: PathBuf,
}

/// Cache locations for one library, namespaced under an explicit base.
///
/// The base is an input, not a discovery: it is used as given and need not
/// exist until a writer creates it, so a context can be built for a library
/// before any cache directory has been brought into being.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachePaths {
    /// The cache root exactly as supplied; writers create it, readers of this
    /// struct never do.
    pub base: PathBuf,
    /// This library's thumbnail cache, under a per-library key.
    pub thumbnails: PathBuf,
    /// The shared reverse-geocoding cache; not per-library.
    pub geo: PathBuf,
}

/// Process-lifetime state shared by every clone of a context: the open root
/// handle, the identity pinned from it, and the index-validation memo.
#[derive(Debug)]
struct Shared {
    /// An open handle on the canonical root directory, held so later stages
    /// can operate on the pinned directory rather than through a name that a
    /// rename or unmount can silently repoint.
    root_handle: File,
    root_dev: u64,
    root_ino: u64,
    /// Memo slot recording a successful index validation, so the expensive
    /// check happens once per context rather than per command. Success only,
    /// by construction: there is deliberately no way to record a failure,
    /// because a failed validation must be retried, not remembered.
    index_validated: Mutex<bool>,
}

/// One media library as an explicit, immutable value.
///
/// Clones share the pinned identity and the memo (the interior state is
/// behind an `Arc`); the path and settings structs are plain data computed
/// once from the canonical root and identical in every clone. There is no
/// connection here, no global, and no lookup against the environment: a
/// context is fully described by what it was constructed with.
#[derive(Clone, Debug)]
pub struct LibraryContext {
    /// Derived paths under the canonical root.
    pub paths: LibraryPaths,
    /// Cache locations under the explicit cache base.
    pub cache: CachePaths,
    /// Settings in force for this context, loaded once from the library's
    /// `config.toml` at construction (built-in defaults when it is absent).
    /// A snapshot: editing the file later does not mutate an existing
    /// context, only a context built after the edit.
    pub settings: LibraryConfig,
    identity: Arc<Shared>,
}

fn paths_for(root: PathBuf) -> LibraryPaths {
    let state = root.join(STATE_DIR);
    LibraryPaths {
        root,
        db: state.join("hashes.db"),
        config: state.join("config.toml"),
        jsonl: state.join("hashes.jsonl"),
        embeddings: state.join("embeddings"),
        locks: state.join("locks"),
        state,
    }
}

/// The cache key for one library: the BLAKE3 digest of the canonical root.
///
/// Keying on the canonical root is what makes symlink aliases of one root
/// share a cache. It must never be a face id or the database basename: every
/// library's database is named `hashes.db`, so that key would point all
/// libraries at one shared cache.
fn cache_for(base: &Path, paths: &LibraryPaths) -> CachePaths {
    let key = blake3::hash(paths.root.as_os_str().as_bytes()).to_hex();
    CachePaths {
        base: base.to_path_buf(),
        thumbnails: base
            .join("videre/libraries")
            .join(key.as_str())
            .join("thumbnails"),
        geo: base.join("videre/geo"),
    }
}

/// Run one path operation inside a budget, mapping both failure shapes to
/// errors that name the path.
///
/// The budget is a parameter rather than a constant only so tests can prove
/// the bound with a sleeping body, the way `io_timeout`'s own tests do;
/// production callers pass [`io_timeout::STAT_TIMEOUT`], the right ceiling
/// for metadata-sized work. On timeout the message is built from strings
/// already in hand: re-reading the very path that failed to answer is the
/// mistake `TimedOutAfter::describe` exists to prevent. Crate visible: the
/// config layer runs its file operations through the same bound.
pub(crate) fn bounded_op<T, F>(path: &Path, op: &str, budget: Duration, f: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> std::io::Result<T> + Send + 'static,
{
    match io_timeout::run_with_timeout(budget, f) {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(e)) => Err(anyhow::Error::new(e).context(format!("{} {}", op, path.display()))),
        Err(io_timeout::TimedOut) => bail!(
            "could not {} {} after {}s (the drive did not respond - is it connected?)",
            op,
            path.display(),
            budget.as_secs()
        ),
    }
}

/// The device/inode pair that names a directory itself, independent of which
/// of its possibly many path spellings was used to reach it.
fn dir_identity(meta: &std::fs::Metadata) -> (u64, u64) {
    (meta.dev(), meta.ino())
}

/// Whether an error chain bottoms out in `NotFound`, so a caller can phrase a
/// missing thing as missing rather than as a generic failure. Crate visible
/// for the same reader as `bounded_op`: the config layer.
pub(crate) fn root_cause_is_not_found(e: &anyhow::Error) -> bool {
    e.root_cause()
        .downcast_ref::<std::io::Error>()
        .is_some_and(|io| io.kind() == std::io::ErrorKind::NotFound)
}

/// Refuse a root that is itself the reserved state directory.
///
/// Selecting `<library>/.videre` would nest a state directory inside a state
/// directory (`.videre/.videre`) and almost always means videre was pointed
/// one level too deep. The error names the path and says what to select
/// instead, so it is actionable without guessing at intent.
fn reject_reserved(path: &Path) -> Result<()> {
    if path.file_name() == Some(std::ffi::OsStr::new(STATE_DIR)) {
        bail!(
            "{} is videre's reserved state directory inside a library, not a library; select the directory that contains it",
            path.display()
        );
    }
    Ok(())
}

/// Resolve `root` to its canonical form, reporting the distinct ways a root
/// can fail to exist rather than one generic error.
///
/// Returns the canonical path and whether the given path was itself a
/// symlink; that fact is only used to phrase the dangling case, because
/// `canonicalize` reports a dangling link and an absent path identically as
/// `NotFound`.
fn canonical_root(root: &Path) -> Result<(PathBuf, bool)> {
    let owned = root.to_path_buf();
    // lstat first, bounded: on a stale mount this is the call that would
    // block forever, so it runs inside a budget before anything else, the
    // same stat-first ordering run_with_timeout_for_path_detailed uses.
    let meta = match bounded_op(root, "read", io_timeout::STAT_TIMEOUT, move || {
        std::fs::symlink_metadata(&owned)
    }) {
        Ok(meta) => meta,
        Err(e) if root_cause_is_not_found(&e) => {
            bail!("library root {} does not exist", root.display())
        }
        Err(e) => return Err(e),
    };
    let is_symlink = meta.file_type().is_symlink();
    let owned = root.to_path_buf();
    match bounded_op(root, "resolve", io_timeout::STAT_TIMEOUT, move || {
        std::fs::canonicalize(&owned)
    }) {
        Ok(canonical) => Ok((canonical, is_symlink)),
        Err(e) if is_symlink && root_cause_is_not_found(&e) => bail!(
            "library root {} is a dangling symlink: its target does not exist",
            root.display()
        ),
        Err(e) => Err(e),
    }
}

/// Open the canonical root and pin its identity from the open handle itself.
///
/// The identity comes from an fstat of the handle, not from a second name
/// lookup, so it is the identity of the directory actually opened. One
/// further bounded lstat of the name then confirms the handle corresponds to
/// the canonical path; a mismatch means the directory was replaced in the
/// window between resolution and open, and no context is built on that race.
fn open_pinned(canonical: &Path) -> Result<(File, std::fs::Metadata)> {
    let owned = canonical.to_path_buf();
    let handle = bounded_op(canonical, "open", io_timeout::STAT_TIMEOUT, move || {
        File::open(&owned)
    })?;
    // fstat through a duplicate of the fd: the bounded runner needs an owned
    // 'static closure, and dup on an already-open fd is not a path operation,
    // so it needs no budget of its own.
    let dup = handle
        .try_clone()
        .with_context(|| format!("dupe handle on {}", canonical.display()))?;
    let meta = bounded_op(canonical, "stat", io_timeout::STAT_TIMEOUT, move || {
        dup.metadata()
    })?;
    if !meta.is_dir() {
        bail!("library root {} is not a directory", canonical.display());
    }
    let pinned = dir_identity(&meta);
    let owned = canonical.to_path_buf();
    // lstat, deliberately: the output of canonicalize never has a symlink as
    // its final component, so this stat not following symlinks is strictly
    // stronger here, and it is the only stat that proves the name was not
    // swapped for a link. A following stat would compare the link target's
    // identity, which both the open above and the verification would then
    // agree on, pinning the context to the wrong library without any
    // disagreement to observe. ensure_root_identity, in contrast,
    // intentionally follows: a renamed root reached through a symlink left
    // behind still names the pinned directory and must keep validating.
    let named = bounded_op(canonical, "read", io_timeout::STAT_TIMEOUT, move || {
        std::fs::symlink_metadata(&owned)
    })?;
    if !named.is_dir() {
        bail!(
            "library root {} changed while it was being opened: it is no longer a directory",
            canonical.display()
        );
    }
    if dir_identity(&named) != pinned {
        bail!(
            "library root {} changed while it was being opened",
            canonical.display()
        );
    }
    Ok((handle, meta))
}

impl LibraryContext {
    /// Build a context for `root`, keeping derived caches under `cache_base`.
    ///
    /// Creates nothing, anywhere: neither the state directory inside the root
    /// nor any part of the cache base. Both are writers' job, so looking at a
    /// library never litters it. The library's `config.toml` is read here
    /// when present: settings are snapshotted into the context, an edit to
    /// the file after construction does not mutate it, and an invalid or
    /// corrupt file fails construction rather than silently defaulting.
    pub fn new(root: &Path, cache_base: &Path) -> Result<Self> {
        // Every path handed to lower layers is absolute. A relative input
        // would have to be resolved against the process's cwd, which is
        // exactly the ambient state this type exists to exclude, so it is
        // refused rather than quietly absorbed.
        if !root.is_absolute() {
            bail!(
                "library root must be an absolute path, got {}",
                root.display()
            );
        }
        if !cache_base.is_absolute() {
            bail!(
                "cache base must be an absolute path, got {}",
                cache_base.display()
            );
        }
        // The reserved name is refused before anything touches the
        // filesystem: the mistake is in the argument, not in the world.
        reject_reserved(root)?;
        // The input is absolute, so canonicalization cannot consult the cwd;
        // it exists to make aliases of one root converge on one library.
        let (canonical, _) = canonical_root(root)?;
        // The canonical spelling is rechecked, not just the given one:
        // reaching a reserved directory through a symlink is still selecting
        // it.
        reject_reserved(&canonical)?;
        let (handle, meta) = open_pinned(&canonical)?;
        let paths = paths_for(canonical);
        let cache = cache_for(cache_base, &paths);
        // The config belongs to the library, so it is loaded through the
        // pinned root's derived paths, after root validation and before the
        // context exists: an invalid config must fail here, at the entrance,
        // not surface as surprising behaviour mid-command.
        let settings = crate::library_config::load(&paths)?;
        Ok(Self {
            paths,
            cache,
            settings,
            identity: Arc::new(Shared {
                root_handle: handle,
                root_dev: meta.dev(),
                root_ino: meta.ino(),
                index_validated: Mutex::new(false),
            }),
        })
    }

    /// Recheck that the canonical root still names the directory pinned at
    /// construction, and error if it does not.
    ///
    /// Bounded: one bounded stat; a check that could hang on a wedged mount
    /// is not a check. The error is built from strings in hand; the failing
    /// path is never touched again to phrase the message, the same rule
    /// `TimedOutAfter::describe` exists to enforce.
    pub fn ensure_root_identity(&self) -> Result<()> {
        let canonical = self.paths.root.clone();
        match bounded_op(
            &self.paths.root,
            "read",
            io_timeout::STAT_TIMEOUT,
            move || std::fs::metadata(&canonical),
        ) {
            Ok(meta) => {
                if dir_identity(&meta) != (self.identity.root_dev, self.identity.root_ino) {
                    bail!(
                        "library root {} no longer names the directory this context was opened for; the folder was replaced, renamed, or its volume unmounted",
                        self.paths.root.display()
                    );
                }
                Ok(())
            }
            Err(e) if root_cause_is_not_found(&e) => bail!(
                "library root {} no longer exists",
                self.paths.root.display()
            ),
            Err(e) => Err(e),
        }
    }

    /// The open handle on the canonical library root, held for the process
    /// lifetime. Used by the pinned-I/O layers built on this context; crate
    /// visible deliberately, since it is a building block, not an answer.
    pub(crate) fn root_handle(&self) -> &File {
        &self.identity.root_handle
    }

    /// Whether a successful index validation has already been memoized for
    /// this context. Shared by every clone; failures are never memoized.
    pub(crate) fn index_validated(&self) -> bool {
        *self.identity.index_validated.lock().unwrap()
    }

    /// Record a successful index validation. Only success is recordable: a
    /// failed validation must be retried rather than remembered.
    pub(crate) fn mark_index_validated(&self) {
        *self.identity.index_validated.lock().unwrap() = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// Writes to the process's real stderr, bypassing libtest's output
    /// capture. A skip is a passing test, and libtest captures the print
    /// macros for passing tests, so an `eprintln!` skip message is invisible
    /// in a normal `cargo test` run and only appears under `--nocapture`;
    /// writing to fd 2 directly sidesteps the capture. Same pattern as the
    /// integration suite's `common::write_past_test_capture`, local here
    /// because videre-core unit tests have no shared helper. `ManuallyDrop`
    /// because dropping a `File` built from a borrowed fd would close fd 2
    /// for the rest of the process.
    fn write_past_test_capture(msg: &str) {
        use std::io::Write;
        use std::os::fd::FromRawFd;

        let mut stderr = std::mem::ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(2) });
        let _ = stderr.write_all(msg.as_bytes());
        let _ = stderr.flush();
    }

    #[test]
    fn paths_are_local_and_root_aliases_share_identity() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("photos");
        let alias = temp.path().join("alias");
        std::fs::create_dir(&root).unwrap();
        std::os::unix::fs::symlink(&root, &alias).unwrap();
        let cache = temp.path().join("cache");
        let a = LibraryContext::new(&root, &cache).unwrap();
        let b = LibraryContext::new(&alias, &cache).unwrap();
        // macOS tempdirs live under /var or /tmp, both symlinks into
        // /private, so the canonical root is not the spelling the test
        // created; comparing against canonicalize(root) is the
        // platform-independent statement of "state lives in the root itself".
        let canonical = std::fs::canonicalize(&root).unwrap();
        assert_eq!(a.paths.db, canonical.join(".videre/hashes.db"));
        assert_eq!(a.cache.thumbnails, b.cache.thumbnails);
        assert!(!a.paths.state.exists());
        // The cache base is an explicit input that may not exist yet, and
        // constructing a context must not bring it into existence.
        assert!(!cache.exists());
        assert!(a.ensure_root_identity().is_ok());
        std::fs::rename(&root, temp.path().join("old")).unwrap();
        std::fs::create_dir(&root).unwrap();
        assert!(a.ensure_root_identity().is_err());
        // The alias context pinned the same canonical directory, so it sees
        // the same replacement.
        assert!(b.ensure_root_identity().is_err());
    }

    #[test]
    fn sibling_roots_get_distinct_cache_namespaces() {
        let temp = tempfile::tempdir().unwrap();
        let one = temp.path().join("2024");
        let two = temp.path().join("2025");
        std::fs::create_dir(&one).unwrap();
        std::fs::create_dir(&two).unwrap();
        let cache = temp.path().join("cache");
        let a = LibraryContext::new(&one, &cache).unwrap();
        let b = LibraryContext::new(&two, &cache).unwrap();
        assert_ne!(a.cache.thumbnails, b.cache.thumbnails);
        // The key material is pinned exactly: the digest of the canonical
        // root, never the db basename (which every library shares) or a face
        // id. A change here is a silent cache reset for every library.
        let key = blake3::hash(a.paths.root.as_os_str().as_bytes()).to_hex();
        assert_eq!(
            a.cache.thumbnails,
            cache
                .join("videre/libraries")
                .join(key.as_str())
                .join("thumbnails")
        );
        // Geo is shared across libraries, not namespaced per root.
        assert_eq!(a.cache.geo, cache.join("videre/geo"));
        assert_eq!(b.cache.geo, a.cache.geo);
    }

    #[test]
    fn a_missing_root_is_rejected_with_its_path() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("nowhere");
        let err = LibraryContext::new(&missing, temp.path()).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("does not exist"), "{msg}");
        assert!(msg.contains("nowhere"), "{msg}");
        // And it is not misreported as a dead drive, which it is not.
        assert!(!msg.contains("did not respond"), "{msg}");
    }

    #[test]
    fn a_file_where_the_root_should_be_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("library.txt");
        std::fs::write(&file, b"not a library").unwrap();
        let err = LibraryContext::new(&file, temp.path()).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("not a directory"), "{msg}");
        assert!(msg.contains("library.txt"), "{msg}");
    }

    #[test]
    fn a_directory_named_videre_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("photos/.videre");
        std::fs::create_dir_all(&state).unwrap();
        let err = LibraryContext::new(&state, temp.path()).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains(".videre"), "{msg}");
        assert!(msg.contains("reserved"), "{msg}");
        assert!(msg.contains("photos"), "{msg}");

        // Reaching that directory through a symlink is the same selection:
        // the canonical name is checked, not just the spelling first given.
        let alias = temp.path().join("shortcut");
        std::os::unix::fs::symlink(&state, &alias).unwrap();
        let err = LibraryContext::new(&alias, temp.path()).unwrap_err();
        assert!(format!("{err:#}").contains("reserved"));
    }

    #[test]
    fn a_dangling_root_symlink_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let link = temp.path().join("library");
        std::os::unix::fs::symlink(temp.path().join("gone"), &link).unwrap();
        let err = LibraryContext::new(&link, temp.path()).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("dangling"), "{msg}");
        assert!(msg.contains("library"), "{msg}");
    }

    #[test]
    fn a_root_swapped_for_a_symlink_during_open_is_rejected() {
        // The race this guards lives between two adjacent syscalls inside
        // open_pinned (canonicalize has already run, the name is swapped for
        // a symlink before the verification stat), so it cannot be produced
        // end to end deterministically. The post-race state can be: hand
        // open_pinned a name that is now a symlink to another directory.
        // A stat that follows the link compares the decoy directory's
        // identity against the handle the open just got on that same decoy,
        // and they agree, pinning the context to the wrong library; the
        // lstat sees the link itself and rejects it.
        let temp = tempfile::tempdir().unwrap();
        let real = temp.path().join("photos");
        let decoy = temp.path().join("other-library");
        std::fs::create_dir(&real).unwrap();
        std::fs::create_dir(&decoy).unwrap();
        // Any name handed to open_pinned plays the canonical name; a symlink
        // there is exactly the swapped state, since canonicalize never
        // returns one.
        let swapped = temp.path().join("swapped");
        std::os::unix::fs::symlink(&decoy, &swapped).unwrap();
        let err = open_pinned(&swapped).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("changed while it was being opened"), "{msg}");
        assert!(msg.contains("swapped"), "{msg}");
    }

    #[test]
    fn an_inaccessible_root_is_rejected_with_its_path() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("photos");
        std::fs::create_dir(&root).unwrap();
        // Root bypasses permission bits entirely (a stock Docker image runs
        // as root), so the behaviour is probed rather than the uid checked,
        // same probe the integration suite's permissions_are_enforced uses.
        let probe = temp.path().join("probe");
        std::fs::write(&probe, b"x").unwrap();
        std::fs::set_permissions(&probe, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::read(&probe).is_ok() {
            write_past_test_capture(
                "SKIP: running as root, so chmod 000 does not block opening a directory\n",
            );
            return;
        }
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o000)).unwrap();
        let err = LibraryContext::new(&root, temp.path()).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("photos"), "{msg}");
        assert!(msg.contains("denied"), "{msg}");
        let _ = std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755));
    }

    #[test]
    fn a_path_operation_past_its_budget_is_cut_off_and_names_the_path() {
        // A real wedged mount cannot be produced portably, so the bound is
        // proven the way io_timeout's own tests prove theirs: a body that
        // sleeps past a tiny budget. The leaked sleeping thread is the
        // documented tradeoff of run_with_timeout.
        let start = std::time::Instant::now();
        let err = bounded_op(
            Path::new("/Volumes/wedged/library"),
            "read",
            Duration::from_millis(50),
            || {
                std::thread::sleep(Duration::from_secs(5));
                Ok::<(), std::io::Error>(())
            },
        )
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("/Volumes/wedged/library"), "{msg}");
        assert!(msg.contains("did not respond"), "{msg}");
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn selecting_home_is_allowed_and_a_child_touches_nothing_outside_itself() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let photos = home.join("Photos");
        std::fs::create_dir_all(&photos).unwrap();
        let cache = temp.path().join("cache");
        // A HOME is just another directory as far as the context is
        // concerned: selecting it puts the state in HOME/.videre.
        let ctx = LibraryContext::new(&home, &cache).unwrap();
        let home_canonical = std::fs::canonicalize(&home).unwrap();
        assert_eq!(ctx.paths.db, home_canonical.join(".videre/hashes.db"));
        // Selecting its Photos child must not reach back out and create the
        // old global HOME/.videre, which home-based resolution would have.
        let _child = LibraryContext::new(&photos, &cache).unwrap();
        assert!(!home.join(".videre").exists());
    }

    #[test]
    fn relative_inputs_are_rejected_rather_than_resolved_against_cwd() {
        // Resolving a relative root would consult the process's cwd, which is
        // exactly the ambient state this type exists to exclude.
        let err = LibraryContext::new(Path::new("photos"), Path::new("/cache")).unwrap_err();
        assert!(format!("{err:#}").contains("absolute"), "{err:#}");
        let err = LibraryContext::new(Path::new("/tmp/photos"), Path::new("cache")).unwrap_err();
        assert!(format!("{err:#}").contains("absolute"), "{err:#}");
    }

    #[test]
    fn a_new_context_carries_the_built_in_settings() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("photos");
        std::fs::create_dir(&root).unwrap();
        let ctx = LibraryContext::new(&root, &temp.path().join("cache")).unwrap();
        assert_eq!(
            ctx.settings.default_model,
            crate::embeddings::DEFAULT_MODEL_ID
        );
        assert_eq!(
            ctx.settings.xmp_precedence,
            crate::marks::XmpPrecedence::default()
        );
        assert!(!ctx.settings.export_xmp_on_watch);
        assert_eq!(ctx.settings.min_read_rate_mb_s, None);
    }

    #[test]
    fn clones_share_the_pinned_identity_and_the_validation_memo() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("photos");
        std::fs::create_dir(&root).unwrap();
        let ctx = LibraryContext::new(&root, &temp.path().join("cache")).unwrap();
        let clone = ctx.clone();
        // The accessor hands out a handle on the pinned directory, so later
        // stages can read through it rather than through a re-lookup.
        assert!(ctx.root_handle().metadata().unwrap().is_dir());
        assert!(!clone.index_validated());
        ctx.mark_index_validated();
        assert!(
            clone.index_validated(),
            "clones must share the memo, not copy it"
        );
        assert!(ctx.ensure_root_identity().is_ok());
        assert!(clone.ensure_root_identity().is_ok());
    }
}
