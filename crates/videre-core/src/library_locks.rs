//! Library-scoped advisory locks: how videre processes coordinate around one
//! library, keyed by the library's own state directory instead of a
//! process-global home.
//!
//! Three lock files under `<root>/.videre/locks`, one per concern:
//! `activity.lock` for "the library is in use" (shared while its database is
//! only being read, exclusive while its state changes shape),
//! `init.lock` for "state is being brought into being or reconfigured", and
//! `<command>.lock` so one command runs against the library at a time. Every
//! lock is a non-blocking `flock`: a contender fails immediately with an
//! error naming the library, never waits, the same refusal the global
//! `pipeline_runs::acquire_lock` established for commands.
//!
//! The acquisition order is fixed: activity first, then init when needed,
//! then command. Every writer takes them in that order, so two writers can
//! never deadlock by holding what the other wants. A config edit takes init
//! only and never the activity lock, so configuring a library neither waits
//! for nor delays running work.
//!
//! Lock files are created inside an already existing locks directory and are
//! never unlinked: the lock lives on the inode, so removing a held lock file
//! would not release anything, it would only let the next process create a
//! fresh file and a second, independent lock. A reader whose library has no
//! state directory fails before creating anything, not even the locks
//! directory, so merely looking at a library never litters it.
//!
//! Redirected state is refused rather than followed. A lock file that is a
//! symlink, or that is hard-linked into another location, is an error, and
//! so is a `.videre` that is itself a symlink: any of those would silently
//! couple two libraries' coordination, exactly the way a hard-linked
//! database would couple their data.
//!
//! One known, accepted gap, documented rather than engineered around:
//! releasing the init lock waits for no one, so an `io_timeout` worker
//! thread abandoned by a timed-out config edit may still complete its
//! rename after the lock has been released (see `library_config::edit`).
//! Closing the gap would mean blocking lock release on a thread that may
//! never finish, trading a rare stale read for a hang.

use crate::io_timeout::STAT_TIMEOUT;
use crate::library::{bounded_op, root_cause_is_not_found, LibraryContext};
use anyhow::{bail, Context, Result};
use fs2::FileExt;
use std::fs::{File, OpenOptions};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

/// How the library's activity is held: many concurrent readers, or one
/// process changing the library's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityMode {
    /// The holder only reads; any number of shared holders may coexist.
    Shared,
    /// The holder changes the library's state; no other holder is allowed.
    Exclusive,
}

/// A held library activity lock. Dropping releases it (closing the file
/// releases the `flock`), and process death releases it too, so nothing
/// correct depends on `Drop` running: the same OS backstop
/// `pipeline_runs::LockGuard` relies on.
#[derive(Debug)]
pub struct ActivityGuard(#[allow(dead_code)] File);

/// A held library init lock, serializing initialization and config edits.
/// Same release semantics as [`ActivityGuard`].
#[derive(Debug)]
pub struct InitGuard(#[allow(dead_code)] File);

/// A held per-command lock. Carries the library and command it was taken
/// for, so a caller handing it to bookkeeping such as
/// `pipeline_runs::track_in` can be verified against what it claims to be
/// rather than trusted.
#[derive(Debug)]
pub struct CommandGuard {
    #[allow(dead_code)]
    file: File,
    root: PathBuf,
    command: String,
}

impl CommandGuard {
    /// Verify this guard was taken for exactly `ctx` and `command`.
    ///
    /// A guard handed to the wrong library or command is a wiring bug, and
    /// silently accepting it would record one library's run against another
    /// (or one command's row against another command's), so it is refused
    /// before anything is written. This is a wiring check, not a
    /// root-identity check: it compares canonical path strings, and root
    /// identity across time is enforced by the context's
    /// [`LibraryContext::ensure_root_identity`] at open boundaries.
    pub(crate) fn ensure_matches(&self, ctx: &LibraryContext, command: &str) -> Result<()> {
        anyhow::ensure!(
            self.command == command,
            "the command lock is held for '{}', not '{}'",
            self.command,
            command
        );
        anyhow::ensure!(
            self.root == ctx.paths.root,
            "the command lock was taken for library {}, not {}",
            self.root.display(),
            ctx.paths.root.display()
        );
        Ok(())
    }
}

/// The lock file for one concern inside the library's locks directory.
fn lock_path(ctx: &LibraryContext, kind: &str) -> PathBuf {
    ctx.paths.locks.join(format!("{kind}.lock"))
}

/// Refuse a command name that would not name one file inside the locks
/// directory. Command names come from videre's own call sites, so this is
/// hygiene rather than a security boundary, but a `/` or a leading dot would
/// escape the locks directory or hide the file, and neither should pass
/// unnoticed.
fn validate_command_name(command: &str) -> Result<()> {
    anyhow::ensure!(!command.is_empty(), "a command lock needs a command name");
    anyhow::ensure!(
        !command.contains('/') && !command.contains('\\') && !command.starts_with('.'),
        "{command:?} is not a valid command lock name"
    );
    Ok(())
}

/// `lstat`, mapped to `Ok(None)` for a missing path. Not following symlinks
/// is the point: this is how a redirect is seen as a redirect rather than as
/// whatever it points at.
fn lstat_maybe(path: &Path) -> Result<Option<std::fs::Metadata>> {
    let owned = path.to_path_buf();
    match bounded_op(path, "read", STAT_TIMEOUT, move || {
        std::fs::symlink_metadata(&owned)
    }) {
        Ok(meta) => Ok(Some(meta)),
        Err(e) if root_cause_is_not_found(&e) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Refuse redirected state: `path`, when it exists, must be a regular file
/// with exactly one hard link.
///
/// A symlink could point anywhere, outside the library and outside the
/// volume, and a multiply-linked file makes two names share one underlying
/// state, coupling two libraries without any visible sign. Neither is
/// something videre itself ever creates, so both are treated as the
/// misconfiguration they are. Absent is fine: creation is the caller's
/// decision.
pub(crate) fn reject_redirect(path: &Path, what: &str) -> Result<()> {
    let Some(meta) = lstat_maybe(path)? else {
        return Ok(());
    };
    if meta.file_type().is_symlink() {
        bail!(
            "{what} {} must not be a symlink; redirected library state is not supported",
            path.display()
        );
    }
    if meta.nlink() > 1 {
        bail!(
            "{what} {} is hard-linked into another location; two libraries cannot share one {}",
            path.display(),
            what
        );
    }
    Ok(())
}

/// Refuse a redirected state directory: `path`, when it exists, must not be
/// a symlink. The directory counterpart of [`reject_redirect`], narrowed to
/// symlinks because a directory's `nlink` counts its subdirectories rather
/// than names sharing an inode, so the multi-link rule for files is
/// meaningless here (and hard links to directories are not creatable to
/// begin with). Absent is fine: creation is the caller's decision.
pub(crate) fn reject_dir_redirect(path: &Path, what: &str) -> Result<()> {
    let Some(meta) = lstat_maybe(path)? else {
        return Ok(());
    };
    if meta.file_type().is_symlink() {
        bail!(
            "{what} {} must not be a symlink; redirected library state is not supported",
            path.display()
        );
    }
    Ok(())
}

/// `path`, when it exists, must be a real directory, not reached through a
/// symlink. `create_dir_all` would happily follow a symlinked directory and
/// build videre's state somewhere else entirely, so the check comes first.
/// `Ok(None)` means absent, which callers may go on to create.
fn existing_dir(path: &Path) -> Result<Option<std::fs::Metadata>> {
    let Some(meta) = lstat_maybe(path)? else {
        return Ok(None);
    };
    if meta.file_type().is_symlink() {
        bail!(
            "{} must not be a symlink; redirected library state is not supported",
            path.display()
        );
    }
    anyhow::ensure!(meta.is_dir(), "{} is not a directory", path.display());
    Ok(Some(meta))
}

/// A reader's state check: the pinned root must still be the pinned root,
/// and its `.videre` must already exist. Fails before creating anything, so
/// looking at an uninitialized library leaves it exactly as it was.
pub(crate) fn verify_state(ctx: &LibraryContext) -> Result<()> {
    ctx.ensure_root_identity()?;
    if existing_dir(&ctx.paths.state)?.is_none() {
        bail!(
            "library {} is not initialized: {} does not exist",
            ctx.paths.root.display(),
            ctx.paths.state.display()
        );
    }
    Ok(())
}

/// The state and locks directories, which only writers create. A state
/// directory that already exists is validated rather than trusted, so a
/// symlinked or non-directory `.videre` is refused before anything is built
/// on top of it.
pub(crate) fn ensure_state_and_locks(ctx: &LibraryContext) -> Result<()> {
    ctx.ensure_root_identity()?;
    existing_dir(&ctx.paths.state)?;
    existing_dir(&ctx.paths.locks)?;
    let owned = ctx.paths.locks.clone();
    bounded_op(&ctx.paths.locks, "create", STAT_TIMEOUT, move || {
        std::fs::create_dir_all(&owned)
    })
    .with_context(|| format!("create {}", ctx.paths.locks.display()))?;
    Ok(())
}

/// The checks every lock acquisition runs first: the state directory exists
/// (created by initialization, never by locking), and so does the locks
/// directory. Failing here is what keeps a reader from bringing either into
/// existence.
fn require_locks(ctx: &LibraryContext) -> Result<()> {
    verify_state(ctx)?;
    if existing_dir(&ctx.paths.locks)?.is_none() {
        bail!(
            "library {} is not initialized: {} does not exist",
            ctx.paths.root.display(),
            ctx.paths.locks.display()
        );
    }
    Ok(())
}

/// Open (creating if absent) and non-blockingly lock one lock file. The file
/// is never removed afterwards; the lock is released by closing it.
fn acquire_lock_file(path: &Path, exclusive: bool, busy: String, what: &str) -> Result<File> {
    reject_redirect(path, what)?;
    let owned = path.to_path_buf();
    let file = bounded_op(path, "open", STAT_TIMEOUT, move || {
        OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(&owned)
    })
    .with_context(|| format!("open lock file {}", path.display()))?;
    // fs2's trait methods are called by their full path on purpose: std grew
    // inherent file-locking methods of the same names (1.89), and an inherent
    // method silently wins over a trait method, which would mix two locking
    // implementations without any error to notice.
    let taken = if exclusive {
        FileExt::try_lock_exclusive(&file)
    } else {
        FileExt::try_lock_shared(&file)
    };
    taken.map_err(|_| anyhow::anyhow!("{busy}"))?;
    Ok(file)
}

/// Take the library's activity lock: shared by readers, exclusive for
/// whoever changes the library's state (initialization, a schema upgrade).
/// Non-blocking; a held lock is an immediate error naming the library.
pub fn try_activity(ctx: &LibraryContext, mode: ActivityMode) -> Result<ActivityGuard> {
    require_locks(ctx)?;
    let busy = format!(
        "library {} is in use by another videre process (its activity lock is held)",
        ctx.paths.root.display()
    );
    let file = acquire_lock_file(
        &lock_path(ctx, "activity"),
        mode == ActivityMode::Exclusive,
        busy,
        "the library activity lock file",
    )?;
    Ok(ActivityGuard(file))
}

/// Take the library's init lock, serializing state creation and config
/// edits. Never taken after the activity lock by the same process (see the
/// module's acquisition order); config edits take it without activity.
pub fn try_init(ctx: &LibraryContext) -> Result<InitGuard> {
    require_locks(ctx)?;
    let busy = format!(
        "another videre process is initializing library {}",
        ctx.paths.root.display()
    );
    let file = acquire_lock_file(
        &lock_path(ctx, "init"),
        true,
        busy,
        "the library init lock file",
    )?;
    Ok(InitGuard(file))
}

/// Take the library's per-command lock, so one command runs against the
/// library at a time. Different commands do not contend with each other,
/// only with themselves.
pub fn try_command(ctx: &LibraryContext, command: &str) -> Result<CommandGuard> {
    validate_command_name(command)?;
    require_locks(ctx)?;
    let busy = format!(
        "{command} is already running against library {}",
        ctx.paths.root.display()
    );
    let file = acquire_lock_file(
        &lock_path(ctx, command),
        true,
        busy,
        "the library command lock file",
    )?;
    Ok(CommandGuard {
        file,
        root: ctx.paths.root.clone(),
        command: command.to_string(),
    })
}

/// Whether another live process currently holds `command`'s lock for this
/// library. A pure probe: creates nothing and never blocks, so it is safe on
/// any read path; a missing lock file (or a library with no state at all)
/// is simply not running.
pub fn command_locked(ctx: &LibraryContext, command: &str) -> Result<bool> {
    validate_command_name(command)?;
    let path = lock_path(ctx, command);
    // The redirected-state rules apply to a probe too: a symlinked or
    // multiply-linked lock file is a misconfiguration to report, not a lock
    // to answer for.
    reject_redirect(&path, "the library command lock file")?;
    let Some(meta) = lstat_maybe(&path)? else {
        return Ok(false);
    };
    anyhow::ensure!(
        !meta.is_dir(),
        "lock file {} is a directory",
        path.display()
    );
    let owned = path.clone();
    let file = bounded_op(&path, "open", STAT_TIMEOUT, move || {
        OpenOptions::new().read(true).write(true).open(&owned)
    })
    .with_context(|| format!("open lock file {}", path.display()))?;
    match file.try_lock_exclusive() {
        // Free for the taking means nobody holds it; release immediately so
        // the probe itself never shows up as contention.
        Ok(()) => {
            FileExt::unlock(&file).ok();
            Ok(false)
        }
        Err(_) => Ok(true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::LibraryContext;

    /// One library root plus a context on it, with the state directory and
    /// its locks directory already in place: the minimum locking needs, so
    /// lock behaviour can be tested without pulling the database layer in.
    fn locked_library() -> (tempfile::TempDir, LibraryContext) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("photos");
        std::fs::create_dir(&root).unwrap();
        let ctx = LibraryContext::new(&root, &temp.path().join("cache")).unwrap();
        std::fs::create_dir_all(&ctx.paths.locks).unwrap();
        (temp, ctx)
    }

    #[test]
    fn shared_activity_coexists_but_exclusive_does_not() {
        let (_t, ctx) = locked_library();
        let one = try_activity(&ctx, ActivityMode::Shared).unwrap();
        let two = try_activity(&ctx, ActivityMode::Shared).unwrap();
        // Any shared holder excludes an exclusive one...
        assert!(
            try_activity(&ctx, ActivityMode::Exclusive).is_err(),
            "a shared activity lock must refuse an exclusive taker"
        );
        drop(one);
        assert!(
            try_activity(&ctx, ActivityMode::Exclusive).is_err(),
            "one remaining shared holder is still enough to refuse exclusive"
        );
        drop(two);
        // ...and with none left, exclusive is available again.
        let _ex = try_activity(&ctx, ActivityMode::Exclusive).unwrap();
    }

    #[test]
    fn an_exclusive_activity_lock_refuses_both_modes() {
        let (_t, ctx) = locked_library();
        let _ex = try_activity(&ctx, ActivityMode::Exclusive).unwrap();
        assert!(try_activity(&ctx, ActivityMode::Exclusive).is_err());
        assert!(try_activity(&ctx, ActivityMode::Shared).is_err());
    }

    #[test]
    fn an_held_init_lock_refuses_a_second_taker_and_an_edit() {
        let (_t, ctx) = locked_library();
        let held = try_init(&ctx).unwrap();
        let err = try_init(&ctx).unwrap_err();
        assert!(format!("{err:#}").contains("initializing"), "{err:#}");
        // A config edit is serialized by the same lock, and fails cleanly
        // rather than waiting for the holder.
        let err = crate::library_config::edit(
            &ctx,
            crate::library_config::ConfigKey::ReadRate,
            Some(toml::Value::Integer(9)),
        )
        .unwrap_err();
        assert!(
            format!("{err:#}").contains("initializing"),
            "an edit must fail while the init lock is held: {err:#}"
        );
        drop(held);
        crate::library_config::edit(
            &ctx,
            crate::library_config::ConfigKey::ReadRate,
            Some(toml::Value::Integer(9)),
        )
        .unwrap();
    }

    #[test]
    fn command_locks_contend_only_with_themselves() {
        let (_t, ctx) = locked_library();
        let scan = try_command(&ctx, "scan").unwrap();
        assert!(try_command(&ctx, "scan").is_err());
        let _faces = try_command(&ctx, "faces").unwrap();
        // The probe answers for a held command and for a free one; faces and
        // scan are held, watch was never taken.
        assert!(command_locked(&ctx, "scan").unwrap());
        assert!(command_locked(&ctx, "faces").unwrap());
        assert!(!command_locked(&ctx, "watch").unwrap());
        drop(scan);
        assert!(!command_locked(&ctx, "scan").unwrap());
        try_command(&ctx, "scan").unwrap();
    }

    #[test]
    fn a_missing_state_directory_fails_without_creating_anything() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("photos");
        std::fs::create_dir(&root).unwrap();
        let ctx = LibraryContext::new(&root, &temp.path().join("cache")).unwrap();
        assert!(try_activity(&ctx, ActivityMode::Shared).is_err());
        assert!(try_init(&ctx).is_err());
        assert!(try_command(&ctx, "scan").is_err());
        // Not even the state directory may appear, let alone a locks
        // directory or a lock file.
        assert!(!ctx.paths.state.exists());
        // The pure probe answers false rather than erroring, and still
        // creates nothing.
        assert!(!command_locked(&ctx, "scan").unwrap());
        assert!(!ctx.paths.state.exists());
    }

    #[test]
    fn lock_files_are_never_unlinked() {
        let (_t, ctx) = locked_library();
        {
            let _a = try_activity(&ctx, ActivityMode::Shared).unwrap();
            let _i = try_init(&ctx).unwrap();
            let _c = try_command(&ctx, "scan").unwrap();
        }
        // Released, but still present: unlinking a lock file lets the next
        // process create a fresh one and a second, independent lock.
        assert!(lock_path(&ctx, "activity").exists());
        assert!(lock_path(&ctx, "init").exists());
        assert!(lock_path(&ctx, "scan").exists());
    }

    #[test]
    fn redirected_lock_files_are_refused() {
        let (_t, ctx) = locked_library();
        // A symlinked activity lock would coordinate whichever file it
        // points at, not this library's.
        let outside = ctx.paths.root.join("outside.lock");
        std::os::unix::fs::symlink(&outside, lock_path(&ctx, "activity")).unwrap();
        let err = try_activity(&ctx, ActivityMode::Shared).unwrap_err();
        assert!(format!("{err:#}").contains("symlink"), "{err:#}");
        std::fs::remove_file(lock_path(&ctx, "activity")).unwrap();

        // A hard-linked lock file couples two libraries' coordination.
        let real = ctx.paths.locks.join("real.lock");
        std::fs::write(&real, b"").unwrap();
        std::fs::hard_link(&real, lock_path(&ctx, "init")).unwrap();
        let err = try_init(&ctx).unwrap_err();
        assert!(format!("{err:#}").contains("hard-linked"), "{err:#}");
    }

    #[test]
    fn a_symlinked_state_directory_is_refused_for_locking() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("photos");
        let elsewhere = temp.path().join("elsewhere");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&elsewhere).unwrap();
        std::fs::create_dir(elsewhere.join("locks")).unwrap();
        // .videre pointing outside the library root: an externally linked
        // state directory must not be used, not even read from.
        std::os::unix::fs::symlink(&elsewhere, root.join(".videre")).unwrap();
        let ctx = LibraryContext::new(&root, &temp.path().join("cache")).unwrap();
        let err = try_activity(&ctx, ActivityMode::Shared).unwrap_err();
        assert!(format!("{err:#}").contains("symlink"), "{err:#}");
    }

    #[test]
    fn root_aliases_share_one_librarys_locks() {
        let (temp, ctx) = locked_library();
        let alias = temp.path().join("alias");
        std::os::unix::fs::symlink(&ctx.paths.root, &alias).unwrap();
        // The alias context canonicalizes to the same root, so it locks the
        // same files: a library is one library however it was reached.
        let via_alias = LibraryContext::new(&alias, &temp.path().join("cache")).unwrap();
        assert_eq!(via_alias.paths.locks, ctx.paths.locks);
        let _held = try_init(&via_alias).unwrap();
        assert!(
            try_init(&ctx).is_err(),
            "a lock taken through the alias must be visible through the root"
        );
        assert!(try_activity(&ctx, ActivityMode::Exclusive).is_ok());
    }

    #[test]
    fn command_names_that_cannot_be_one_file_are_refused() {
        let (_t, ctx) = locked_library();
        for bad in ["", "a/b", "..", ".hidden", "a\\b"] {
            assert!(try_command(&ctx, bad).is_err(), "{bad:?}");
            assert!(command_locked(&ctx, bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_command_guard_reports_the_library_and_command_it_was_taken_for() {
        let (temp, ctx) = locked_library();
        let guard = try_command(&ctx, "scan").unwrap();
        assert!(guard.ensure_matches(&ctx, "scan").is_ok());
        assert!(guard.ensure_matches(&ctx, "faces").is_err());
        // A different library's context, with its own locks, is not this
        // guard's library even though the command name matches.
        let other_root = temp.path().join("other");
        std::fs::create_dir(&other_root).unwrap();
        let other = LibraryContext::new(&other_root, &temp.path().join("cache")).unwrap();
        assert!(guard.ensure_matches(&other, "scan").is_err());
    }

    #[test]
    fn a_lock_file_open_past_its_budget_fails_closed_without_restatting_it() {
        // Lock acquisition itself cannot time out: the flock is non-blocking
        // by design and fails immediately when held, which the contention
        // tests above prove. The bounded surface is the stat-and-open path
        // every acquisition runs first (`lstat_maybe`, then
        // `acquire_lock_file`'s open), so that is what carries the injected
        // budget, the same 1ns-under-margin pattern as `library.rs` and
        // `library_guard.rs`.
        let (_t, ctx) = locked_library();
        let lock = lock_path(&ctx, "activity");
        std::fs::write(&lock, b"").unwrap();
        let start = std::time::Instant::now();
        let owned = lock.clone();
        let err = crate::library::bounded_op(
            &lock,
            "open",
            std::time::Duration::from_nanos(1),
            move || {
                OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create(true)
                    .open(&owned)
                    .map(|_| ())
            },
        )
        .unwrap_err();
        // The file is removed before the message is formatted: an error that
        // still names the exact path and phrasing cannot have consulted the
        // filesystem to build itself, which is the unbounded re-stat mistake
        // `TimedOutAfter::describe` exists to prevent. The abandoned worker
        // thread may open the (existing) file before or after the removal;
        // either way its result is discarded and nothing is asserted on it.
        std::fs::remove_file(&lock).unwrap();
        let msg = format!("{err:#}");
        assert!(msg.contains("did not respond"), "{msg}");
        assert!(msg.contains("activity.lock"), "{msg}");
        assert!(start.elapsed() < std::time::Duration::from_secs(2));
    }
}
