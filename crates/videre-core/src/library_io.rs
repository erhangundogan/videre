//! Originals and sidecars reach the disk only through the pinned root.
//!
//! The guard layer validates path strings before work starts; this module is
//! the other half of that bargain, the one that moves bytes. Resolving a path
//! and then opening it by name is not a safe pair: a descendant symlink
//! swapped in between the two makes the kernel follow the new target, so the
//! validation approved one file while the open reads or writes another. No
//! amount of canonicalizing closes that window, because the canonical answer
//! is itself just another string by the time it is used; the only closure is
//! to stop asking the kernel to follow links after resolution. That is what
//! the walk here does.
//!
//! Both entry points revalidate on every invocation (a validated path is
//! never reused) and resolve through the shared guard, then walk from the
//! root handle the context pinned at construction, opening each component
//! with openat and O_NOFOLLOW. A component that is a symlink at walk time is
//! refused, so a link swapped after resolution cannot redirect the bytes; the
//! kernel never follows one during the walk. Links remain usable the way the
//! guard allows: resolution expands a link whose target is inside the
//! library, and the walk then visits the expanded target's real components,
//! never the link itself. One residual race is accepted and pinned here: a
//! mid-walk component replaced by a different directory between one openat
//! and the next is followed, which can confuse names within the library but
//! cannot escape it, because every step opens a child of a previously held
//! descriptor, the first of which is the pinned root, and no step ever
//! crosses a link.
//!
//! [`open_media`] returns the held file, for readers and for the handle-based
//! timestamp setters; the final component must be a regular file, opened with
//! O_NOFOLLOW so a media name swapped for a link is refused rather than
//! followed. [`replace_sidecar`] anchors the target's parent directory the
//! same way, writes a unique temporary descriptor-relative inside it, and
//! publishes with renameat under that held parent, rechecking the root's
//! identity before publication. A target that is anything other than an
//! absent name or a regular file is refused outright: rename would replace
//! it, and refusing by default keeps a swapped-in link or a stray FIFO
//! visible to the operator instead of quietly absorbing it.
//!
//! Read-only operands that are not library originals, a search's example
//! image living anywhere on disk, stay intentionally outside this boundary:
//! nothing here mutates them. Decoders that can only consume a path get a
//! private staged copy of the already-confined file instead, so outside
//! bytes can never enter through the decode step.
//!
//! Every filesystem operation here is bounded, including the ones an error
//! path performs: an error path touches the filesystem only for a bounded,
//! fail-closed classification stat that rephrases an open failure, and to
//! discard a temporary whose name this call still holds.

use crate::io_timeout::{min_read_rate_mb_s, timeout_for_size, STAT_TIMEOUT};
use crate::library::{bounded_op, root_cause_is_not_found, LibraryContext, STATE_DIR};
use anyhow::{bail, Context, Result};
use rustix::fs::{openat, renameat, statat, unlinkat, AtFlags, FileType, Mode, OFlags};
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Sequence counter making each temporary's name unique within a process; the
/// pid makes it unique across processes. Same shape as the database layer's
/// build temporaries.
static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Attempts to find an unused temporary name before giving up. A collision
/// needs a previous process with the same pid and the same counter position
/// to have left its temporary behind, so even one retry is generous.
const TEMP_ATTEMPTS: usize = 3;

/// Open one original inside the library and return the held file.
///
/// The path is resolved through the shared guard on every call (a validated
/// path is never reused), the resolved components are then walked from the
/// pinned root handle, and the final component is opened with `O_NOFOLLOW`
/// and must be a regular file. Readers, the HTTP surface, and the
/// handle-based timestamp setters all consume the returned file rather than a
/// path that could be re-looked-up and swapped.
pub fn open_media(ctx: &LibraryContext, path: &Path) -> Result<File> {
    ctx.ensure_root_identity()?;
    let candidate = crate::library_guard::rooted(ctx, path);
    let resolved = crate::library_guard::resolve_in_root(ctx, &candidate)?;
    if resolved == ctx.paths.root {
        bail!(
            "the library root {} itself is not a media file",
            resolved.display()
        );
    }
    let name = resolved.file_name().ok_or_else(|| {
        anyhow::anyhow!(
            "{} does not name a file inside library {}",
            path.display(),
            ctx.paths.root.display()
        )
    })?;
    let parent = anchored_directory(ctx, resolved.parent().unwrap_or(Path::new("/")))?;
    open_media_file(&parent, name, &resolved)
}

/// Write a sidecar beside its target, replacing any existing regular file
/// atomically.
///
/// The parent directory is anchored by the same pinned walk, the bytes go to
/// a unique temporary created descriptor-relative inside that held directory,
/// and publication is a `renameat` under the same handle after the root's
/// identity is rechecked. The final name is kept as given rather than
/// resolved: a target that is itself a symlink is refused, because writing
/// through a link is exactly the redirect this module exists to prevent, and
/// the state directory is refused as a sidecar location because its files
/// belong to the database, config, and lock layers.
pub fn replace_sidecar(ctx: &LibraryContext, target: &Path, bytes: &[u8]) -> Result<()> {
    ctx.ensure_root_identity()?;
    let candidate = crate::library_guard::rooted(ctx, target);
    let Some(name) = candidate.file_name().map(OsStr::to_owned) else {
        bail!(
            "sidecar target {} does not name a file inside library {}",
            target.display(),
            ctx.paths.root.display()
        );
    };
    // The final component is deliberately exempt from resolution, so a link
    // in the target's own name is judged below against the held parent
    // instead of being quietly expanded.
    let parent_given = candidate.parent().unwrap_or(Path::new(""));
    let resolved_parent = crate::library_guard::resolve_in_root(ctx, parent_given)?;
    refuse_state_dir(&resolved_parent, &name, &ctx.paths.root)?;
    let display = resolved_parent.join(&name);
    let parent = anchored_directory(ctx, &resolved_parent)?;
    match name_kind(&parent, &name, &display)? {
        Some(FileType::Symlink) => bail!(
            "{} is a symbolic link; delete the link or choose another target, videre never writes through one",
            display.display()
        ),
        Some(FileType::Directory) => bail!(
            "{} is a directory, so it cannot be a sidecar",
            display.display()
        ),
        // Any other present kind, a FIFO, socket, or device sitting in the
        // target's name, is refused with the same default rather than
        // silently replaced by the publication rename; a regular file and an
        // absent name, the create case, are the only kinds that proceed.
        Some(kind) if !kind.is_file() => bail!(
            "{} is not a regular file, so it cannot be a sidecar",
            display.display()
        ),
        _ => {}
    }
    let temp = write_temporary(&parent, bytes, &display)?;
    // The identity recheck sits at the publication boundary: the bytes may
    // have taken seconds to reach the disk, and a root replaced meanwhile
    // must not receive the rename.
    if let Err(e) = ctx.ensure_root_identity() {
        discard_temporary(&parent, &temp, &display);
        return Err(e);
    }
    if let Err(e) = publish(&parent, &temp, &name, &display) {
        discard_temporary(&parent, &temp, &display);
        return Err(e);
    }
    Ok(())
}

/// Walk `dir`, a resolved in-root directory path, from the pinned root
/// handle and return the held directory.
///
/// Every component is opened with `O_NOFOLLOW` and `O_DIRECTORY`, so the
/// kernel is never asked to follow a link during the walk: a component that
/// is a symlink at walk time, a link swapped in after resolution, is refused
/// rather than followed. This is the window that a path-based open cannot
/// close, and the reason the walk exists. The pinned root handle itself is
/// never consumed; each step opens an owned child of the previous one.
fn anchored_directory(ctx: &LibraryContext, dir: &Path) -> Result<File> {
    let rel = dir.strip_prefix(&ctx.paths.root).with_context(|| {
        format!(
            "{} is not inside library {}",
            dir.display(),
            ctx.paths.root.display()
        )
    })?;
    let mut handle = dupe(ctx.root_handle(), &ctx.paths.root)?;
    let mut walked = ctx.paths.root.clone();
    for component in rel.components() {
        match component {
            Component::Normal(name) => {
                walked.push(name);
                handle = open_child_directory(&handle, name, &walked)?;
            }
            Component::CurDir => {}
            _ => bail!(
                "{} must be a resolved path inside the library",
                dir.display()
            ),
        }
    }
    Ok(handle)
}

/// Open one child directory of a held parent, refusing a child that is a
/// symlink instead of following it.
fn open_child_directory(parent: &File, name: &OsStr, display: &Path) -> Result<File> {
    let dup = dupe(parent, display)?;
    let name = name.to_os_string();
    let for_classifying = name.clone();
    let oflags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    match bounded_op(display, "open", STAT_TIMEOUT, move || {
        openat(&dup, &name, oflags, Mode::empty())
            .map(File::from)
            .map_err(std::io::Error::from)
    }) {
        Ok(file) => Ok(file),
        Err(e) => Err(refuse_if_link(e, parent, &for_classifying, display)),
    }
}

/// Open the final component of a media path under its held parent, accepting
/// only a regular file.
fn open_media_file(parent: &File, name: &OsStr, display: &Path) -> Result<File> {
    let dup = dupe(parent, display)?;
    let name = name.to_os_string();
    let for_classifying = name.clone();
    // NONBLOCK so a FIFO swapped into a media name cannot hang the open
    // waiting for a writer that never comes; on regular files the flag has no
    // effect, and the returned handle reads normally.
    let oflags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK;
    let file = match bounded_op(display, "open", STAT_TIMEOUT, move || {
        openat(&dup, &name, oflags, Mode::empty())
            .map(File::from)
            .map_err(std::io::Error::from)
    }) {
        Ok(file) => file,
        Err(e) => {
            return Err(refuse_if_link(
                e,
                parent,
                &for_classifying.as_os_str(),
                display,
            ))
        }
    };
    let meta = {
        let dup = dupe(&file, display)?;
        bounded_op(display, "stat", STAT_TIMEOUT, move || dup.metadata())?
    };
    anyhow::ensure!(
        meta.is_file(),
        "{} is not a regular file",
        display.display()
    );
    Ok(file)
}

/// The file type of `name` inside the held directory, without following it,
/// or `None` when no such name exists. `fstatat` keeps the check on the same
/// anchored directory the publication will rename within, so the answer and
/// the rename cannot disagree about which directory they mean.
fn name_kind(parent: &File, name: &OsStr, display: &Path) -> Result<Option<FileType>> {
    let dup = dupe(parent, display)?;
    let name = name.to_os_string();
    match bounded_op(display, "read", STAT_TIMEOUT, move || {
        statat(&dup, &name, AtFlags::SYMLINK_NOFOLLOW)
            .map(|stat| FileType::from_raw_mode(stat.st_mode))
            .map_err(std::io::Error::from)
    }) {
        Ok(kind) => Ok(Some(kind)),
        Err(e) if root_cause_is_not_found(&e) => Ok(None),
        Err(e) => Err(e),
    }
}

/// A sidecar never lives in the reserved state directory: the database,
/// config, and lock layers own those names and their redirect refusals, so a
/// writer that would land there is refused before any handle is opened. The
/// one degenerate spelling, a sidecar named exactly `.videre` in the root,
/// would shadow the state directory a later initialization expects to create,
/// so it is refused for the same reason.
fn refuse_state_dir(resolved_parent: &Path, name: &OsStr, root: &Path) -> Result<()> {
    let under_state = resolved_parent
        .strip_prefix(root)
        .ok()
        .and_then(|rel| rel.components().next())
        .is_some_and(|first| first.as_os_str() == OsStr::new(STATE_DIR));
    let shadows_state = resolved_parent == root && name == OsStr::new(STATE_DIR);
    if under_state || shadows_state {
        bail!(
            "{} is reserved for videre's state directory, which is not a sidecar location",
            resolved_parent.join(name).display()
        );
    }
    Ok(())
}

/// Write `bytes` into a uniquely named temporary inside the held directory,
/// fsynced so the data survives the rename, and return its name. The name is
/// dot-prefixed, fixed-length, and clearly videre's, so debris from a crash
/// is recognizable and can never exceed a filesystem's name limit. An
/// attempt that fails after creating its temporary discards it before
/// returning: nothing sweeps the media tree the way initialization sweeps
/// the state directory, so a failed write must clean up after itself.
fn write_temporary(parent: &File, bytes: &[u8], display: &Path) -> Result<OsString> {
    for _ in 0..TEMP_ATTEMPTS {
        let name = OsString::from(format!(
            ".videre-sidecar-{}-{}.tmp",
            std::process::id(),
            TMP_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let dup = dupe(parent, display)?;
        let open_name = name.clone();
        let bytes = bytes.to_vec();
        let result = bounded_op(display, "write", STAT_TIMEOUT, move || {
            let oflags =
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
            let mode = Mode::from_bits(0o644).expect("a plain permission triplet is always valid");
            let mut file = File::from(openat(&dup, &open_name, oflags, mode)?);
            std::io::Write::write_all(&mut file, &bytes)?;
            file.sync_all()?;
            Ok(())
        });
        match result {
            Ok(()) => return Ok(name),
            Err(e) if already_exists(&e) => continue,
            Err(e) => {
                // The failure may predate the openat (a read-only parent,
                // when there is nothing to remove) or come after it (ENOSPC,
                // EIO, a timeout), and only this arm still holds the failed
                // attempt's name: discarding it here is what keeps a failed
                // write from littering the media tree with temporaries
                // nothing will ever sweep. On a timeout the abandoned worker
                // keeps writing into the unlinked inode, which the kernel
                // reclaims when its descriptor closes.
                discard_temporary(parent, &name, display);
                return Err(e);
            }
        }
    }
    bail!(
        "could not find an unused temporary name next to {}",
        display.display()
    )
}

/// Rename the temporary over the target name inside the same held directory,
/// then fsync the directory so the rename is durable.
///
/// Plain `renameat` is the right publication for a replace: it swaps the name
/// atomically, and it never follows a symlink, so a final name swapped for a
/// link between the check and the rename loses the link rather than writing
/// through it. `RenameFlags::NOREPLACE` exists on both platforms but would
/// refuse the legitimate replacement of an existing sidecar, which is this
/// function's purpose, so the refusal-by-default posture lives in the
/// checked name kinds instead.
fn publish(parent: &File, temp: &OsStr, name: &OsStr, display: &Path) -> Result<()> {
    let rename = dupe(parent, display)?;
    let temp = temp.to_os_string();
    let name = name.to_os_string();
    bounded_op(display, "publish", STAT_TIMEOUT, move || {
        renameat(&rename, &temp, &rename, &name)?;
        Ok(())
    })?;
    // The fd-based counterpart of library_db::sync_dir: the directory is
    // already held, so it is synced through the handle rather than reopened
    // by name.
    let sync = dupe(parent, display)?;
    bounded_op(display, "sync", STAT_TIMEOUT, move || sync.sync_all())?;
    Ok(())
}

/// Best-effort bounded removal of a temporary this call created.
///
/// Unlike the database layer's build temporaries, which sit inside videre's
/// own state directory and are swept by the next initialization, a sidecar
/// temporary lands in the user's media tree, so a failed publication cleans
/// up after itself while the volume still answers. A removal that itself
/// fails is swallowed, never masking the error that caused it.
fn discard_temporary(parent: &File, temp: &OsStr, display: &Path) {
    let _ = (|| -> Result<()> {
        let dup = dupe(parent, display)?;
        let temp = temp.to_os_string();
        bounded_op(display, "remove", STAT_TIMEOUT, move || {
            unlinkat(&dup, &temp, AtFlags::empty())?;
            Ok(())
        })?;
        Ok(())
    })();
}

/// A private, self-removing copy of one confined original, for decoders that
/// can only consume a path. Dropping it removes the copy; the held file keeps
/// the inode alive for the copy's whole observed lifetime.
pub struct StagedCopy {
    path: PathBuf,
    parent: File,
    name: OsString,
    #[allow(dead_code)]
    held: File,
}

impl StagedCopy {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for StagedCopy {
    fn drop(&mut self) {
        discard_temporary(&self.parent, &self.name, &self.path);
    }
}

/// Stage a private copy of an already-confined original in library state,
/// preserving `extension` so extension-routed decoders behave.
///
/// The source is the held file from [`open_media`], never a path revalidated
/// by name, so the bytes copied are exactly the bytes validation anchored.
pub fn staged_copy(ctx: &LibraryContext, source: &File, extension: &OsStr) -> Result<StagedCopy> {
    ctx.ensure_root_identity()?;
    let scratch = &ctx.paths.state;
    let dir = anchored_directory(ctx, scratch)?;
    let mut name = OsString::from(format!(
        ".videre-stage-{}-{}",
        std::process::id(),
        TMP_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    if !extension.is_empty() {
        name.push(".");
        name.push(extension);
    }
    let path = scratch.join(&name);
    // The copy reads the whole original, so its budget scales with size the
    // way every whole-file read in io_timeout does, never the constant that
    // fits metadata-sized work: a healthy multi-GB original on a slow but
    // functional drive legitimately outlasts STAT_TIMEOUT. The size comes
    // from an fstat of the already-open source, so no name lookup a wedged
    // mount could hang is added, and a stat that cannot answer falls back to
    // the stat ceiling rather than guessing larger.
    let budget = {
        let probe = dupe(source, scratch)?;
        bounded_op(scratch, "stat", STAT_TIMEOUT, move || probe.metadata())
            .map(|meta| timeout_for_size(meta.len(), min_read_rate_mb_s()))
            .unwrap_or(STAT_TIMEOUT)
    };
    let open_name = name.clone();
    let dup_dir = dupe(&dir, scratch)?;
    let mut read = dupe(source, scratch)?;
    let held = bounded_op(&path, "stage", budget, move || {
        let oflags =
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let mode = Mode::from_bits(0o600).expect("a plain permission triplet is always valid");
        let mut held = File::from(openat(&dup_dir, &open_name, oflags, mode)?);
        // Rewind so the copy is the whole original, whatever position a
        // previous reader left the shared handle at.
        std::io::Seek::seek(&mut read, std::io::SeekFrom::Start(0))?;
        std::io::copy(&mut read, &mut held)?;
        held.sync_all()?;
        Ok(held)
    })?;
    Ok(StagedCopy {
        path,
        parent: dir,
        name,
        held,
    })
}

/// Duplicate a handle for a bounded operation's owned closure, naming the
/// path if the dup itself fails.
fn dupe(file: &File, display: &Path) -> Result<File> {
    file.try_clone()
        .with_context(|| format!("dupe a handle on {}", display.display()))
}

/// Rephrase a failed `O_NOFOLLOW` open as a symlink refusal when the name is
/// one.
///
/// The kernel's errno for this is not stable across platforms: Linux says
/// ELOOP, while macOS answers `O_NOFOLLOW | O_DIRECTORY` with ENOTDIR, which
/// is indistinguishable from a plain file sitting where a directory was
/// wanted. So the refusal is classified by statting the name without
/// following it: under `O_NOFOLLOW` an open that failed on a symlink failed
/// because of the symlink, whatever errno carried the news. A classification
/// stat that itself fails, or a name that vanished, keeps the original error
/// rather than inventing a cause.
fn refuse_if_link(
    original: anyhow::Error,
    parent: &File,
    name: &OsStr,
    display: &Path,
) -> anyhow::Error {
    if matches!(
        name_kind(parent, name, display),
        Ok(Some(FileType::Symlink))
    ) {
        anyhow::anyhow!(
            "{} is a symbolic link, so the library changed after it was resolved; refusing to follow it",
            display.display()
        )
    } else {
        original
    }
}

/// Whether an error chain bottoms out in EEXIST, the kernel's report that a
/// `CREATE | EXCL` name was already taken.
fn already_exists(e: &anyhow::Error) -> bool {
    e.root_cause()
        .downcast_ref::<std::io::Error>()
        .is_some_and(|io| rustix::io::Errno::from_io_error(io) == Some(rustix::io::Errno::EXIST))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::LibraryContext;
    use crate::library_test_support::write_past_test_capture;
    use std::ffi::OsStr;
    use std::fs::File;
    use std::io::Read;
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};
    use std::path::Path;

    /// One library whose canonical root is ready for fixtures. Fixtures are
    /// built under `ctx.paths.root` (already canonical) rather than the
    /// spelling first given, because macOS tempdirs canonicalize into
    /// /private and compared paths must agree on one form.
    fn library() -> (tempfile::TempDir, LibraryContext) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("photos");
        std::fs::create_dir(&root).unwrap();
        let ctx = LibraryContext::new(&root, &temp.path().join("cache")).unwrap();
        (temp, ctx)
    }

    /// The bytes a held file yields, so opens can be checked against what was
    /// written.
    fn read_all(mut file: File) -> Vec<u8> {
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).unwrap();
        bytes
    }

    /// The regression this module exists for: a descendant link that was
    /// allowed at validation time and swapped before the write must not be
    /// able to move the bytes out of the library.
    #[test]
    fn changed_descendant_link_cannot_redirect_a_sidecar() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("photos");
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(root.join("inside")).unwrap();
        std::fs::create_dir(&outside).unwrap();
        let link = root.join("link");
        std::os::unix::fs::symlink(root.join("inside"), &link).unwrap();
        let ctx = crate::library::LibraryContext::new(&root, &temp.path().join("cache")).unwrap();
        crate::library_guard::validate_paths(&ctx, &[link.clone()]).unwrap();
        std::fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        assert!(replace_sidecar(&ctx, &link.join("image.xmp"), b"private").is_err());
        assert!(!outside.join("image.xmp").exists());
        // Nothing else leaked through the swapped link either.
        assert!(std::fs::read_dir(&outside).unwrap().next().is_none());
    }

    /// The walk's own refusal, exercised in the post-race state: a component
    /// that is a symlink at walk time is never followed, whichever directory
    /// it points at, because the kernel is never asked to.
    #[test]
    fn the_walk_refuses_a_component_that_is_now_a_symlink() {
        let (temp, ctx) = library();
        let outside = temp.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::create_dir(ctx.paths.root.join("real")).unwrap();
        for target in [ctx.paths.root.join("real"), outside.clone()] {
            let link = ctx.paths.root.join("link");
            std::os::unix::fs::symlink(&target, &link).unwrap();
            let err = anchored_directory(&ctx, &link).unwrap_err();
            let msg = format!("{err:#}");
            assert!(msg.contains("symbolic link"), "{msg}");
            let deeper = anchored_directory(&ctx, &link.join("deeper"));
            assert!(deeper.is_err());
            // The link's target is untouched by the refusal.
            assert!(std::fs::read_dir(&outside).unwrap().next().is_none());
            std::fs::remove_file(&link).unwrap();
        }
        // A real directory still walks, from the pinned handle.
        let held = anchored_directory(&ctx, &ctx.paths.root.join("real")).unwrap();
        assert!(held.metadata().unwrap().is_dir());
    }

    /// A sidecar target that is itself a link to an outside file must be
    /// refused, and the outside file must keep exactly its bytes: videre
    /// never writes through a link, not even by replacing it silently.
    #[test]
    fn a_final_sidecar_symlink_cannot_overwrite_its_target() {
        let (temp, ctx) = library();
        let outside = temp.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("image.xmp"), b"keep").unwrap();
        std::os::unix::fs::symlink(outside.join("image.xmp"), ctx.paths.root.join("image.xmp"))
            .unwrap();
        let err = replace_sidecar(&ctx, &ctx.paths.root.join("image.xmp"), b"private").unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("symbolic link"), "{msg}");
        assert_eq!(std::fs::read(outside.join("image.xmp")).unwrap(), b"keep");
        // The link itself is left in place, still a link.
        let meta = std::fs::symlink_metadata(ctx.paths.root.join("image.xmp")).unwrap();
        assert!(meta.file_type().is_symlink());
    }

    /// A replaced root fails the operation at the identity recheck, and the
    /// replacement directory is never populated through the stale pin.
    #[test]
    fn a_replaced_root_fails_the_operation() {
        let (temp, ctx) = library();
        std::fs::create_dir(ctx.paths.root.join("Trips")).unwrap();
        std::fs::write(ctx.paths.root.join("Trips/f.jpg"), b"jpeg").unwrap();
        // Sanity: the same context opens the file while the root still names
        // the pinned directory.
        let media = ctx.paths.root.join("Trips/f.jpg");
        assert_eq!(read_all(open_media(&ctx, &media).unwrap()), b"jpeg");
        std::fs::rename(&ctx.paths.root, temp.path().join("old")).unwrap();
        std::fs::create_dir(&ctx.paths.root).unwrap();
        let err = open_media(&ctx, &media).unwrap_err();
        assert!(format!("{err:#}").contains("no longer names"), "{err:#}");
        let err = replace_sidecar(&ctx, &ctx.paths.root.join("f.xmp"), b"x").unwrap_err();
        assert!(format!("{err:#}").contains("no longer names"), "{err:#}");
        assert!(std::fs::read_dir(&ctx.paths.root).unwrap().next().is_none());
    }

    /// A symlink alias of the root is the same library: it reaches the same
    /// pinned handle and the same files.
    #[test]
    fn an_alias_of_the_root_reaches_the_same_files() {
        let (temp, ctx) = library();
        std::fs::create_dir(ctx.paths.root.join("Trips")).unwrap();
        std::fs::write(ctx.paths.root.join("Trips/f.jpg"), b"jpeg").unwrap();
        let alias = temp.path().join("alias");
        std::os::unix::fs::symlink(&ctx.paths.root, &alias).unwrap();
        crate::library_guard::validate_paths(&ctx, &[alias.clone()]).unwrap();
        assert_eq!(
            read_all(open_media(&ctx, &alias.join("Trips/f.jpg")).unwrap()),
            b"jpeg"
        );
        replace_sidecar(&ctx, &alias.join("Trips/f.xmp"), b"marks").unwrap();
        assert_eq!(
            std::fs::read(ctx.paths.root.join("Trips/f.xmp")).unwrap(),
            b"marks"
        );
    }

    /// An unchanged in-root symlink still reaches its target: resolution
    /// expands it, and the walk visits the real directory's components.
    #[test]
    fn an_unchanged_in_root_symlink_still_reaches_its_target() {
        let (_temp, ctx) = library();
        std::fs::create_dir(ctx.paths.root.join("Trips")).unwrap();
        std::fs::write(ctx.paths.root.join("Trips/IMG_0001.jpg"), b"jpeg").unwrap();
        std::os::unix::fs::symlink(
            ctx.paths.root.join("Trips"),
            ctx.paths.root.join("trips-alias"),
        )
        .unwrap();
        crate::library_guard::validate_paths(&ctx, &[ctx.paths.root.join("trips-alias")]).unwrap();
        let media = ctx.paths.root.join("trips-alias/IMG_0001.jpg");
        assert_eq!(read_all(open_media(&ctx, &media).unwrap()), b"jpeg");
        let sidecar = ctx.paths.root.join("trips-alias/IMG_0001.xmp");
        replace_sidecar(&ctx, &sidecar, b"marks").unwrap();
        // The sidecar landed in the real directory, not "in" the link.
        assert_eq!(
            std::fs::read(ctx.paths.root.join("Trips/IMG_0001.xmp")).unwrap(),
            b"marks"
        );
        let meta = std::fs::symlink_metadata(ctx.paths.root.join("trips-alias")).unwrap();
        assert!(meta.file_type().is_symlink());
    }

    /// Names travel as bytes, so Turkish characters and spaces round-trip
    /// through the walk exactly as they are stored.
    #[test]
    fn unicode_and_spaces_round_trip() {
        let (_temp, ctx) = library();
        let dir = ctx.paths.root.join("Fotoğraflar 2024/İstanbul Günü");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("IMG 0001.jpeg"), b"jpeg").unwrap();
        let media = dir.join("IMG 0001.jpeg");
        assert_eq!(read_all(open_media(&ctx, &media).unwrap()), b"jpeg");
        replace_sidecar(&ctx, &dir.join("IMG 0001.xmp"), b"marks").unwrap();
        assert_eq!(std::fs::read(dir.join("IMG 0001.xmp")).unwrap(), b"marks");
    }

    /// A sidecar target that is anything other than an absent name or a
    /// regular file is refused rather than silently replaced by the rename:
    /// a directory is the kind a test makes directly, and a FIFO is the one
    /// further kind it can make portably.
    #[test]
    fn a_non_regular_sidecar_target_is_refused_not_replaced() {
        let (_temp, ctx) = library();
        std::fs::create_dir(ctx.paths.root.join("Trips")).unwrap();
        let err = replace_sidecar(&ctx, &ctx.paths.root.join("Trips"), b"x").unwrap_err();
        assert!(format!("{err:#}").contains("directory"), "{err:#}");
        // rustix gates mknodat away from apple, so the FIFO comes from the
        // platform's own mkfifo; when the tool is absent the refusal is
        // simply unproven here rather than falsely green.
        let fifo = ctx.paths.root.join("pipe.xmp");
        let made = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .map(|status| status.success());
        match made {
            Ok(true) => {
                let err = replace_sidecar(&ctx, &fifo, b"x").unwrap_err();
                assert!(format!("{err:#}").contains("not a regular file"), "{err:#}");
                // The FIFO was refused, not replaced by the publication.
                let meta = std::fs::symlink_metadata(&fifo).unwrap();
                assert!(meta.file_type().is_fifo());
            }
            _ => write_past_test_capture(
                "SKIP: mkfifo unavailable, so the FIFO sidecar refusal is unproven here\n",
            ),
        }
    }

    /// An unwritable target directory refuses the sidecar, creating nothing.
    #[test]
    fn an_unwritable_directory_refuses_the_sidecar() {
        let (temp, ctx) = library();
        let locked = ctx.paths.root.join("locked");
        std::fs::create_dir(&locked).unwrap();
        // Root bypasses permission bits entirely (a stock Docker image runs
        // as root), so the behaviour is probed rather than the uid checked,
        // same probe as `library.rs`'s inaccessible-root test.
        let probe = temp.path().join("probe");
        std::fs::write(&probe, b"x").unwrap();
        std::fs::set_permissions(&probe, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::read(&probe).is_ok() {
            write_past_test_capture(
                "SKIP: running as root, so chmod 555 does not block creating a sidecar\n",
            );
            return;
        }
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555)).unwrap();
        let err = replace_sidecar(&ctx, &locked.join("image.xmp"), b"private").unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("locked"), "{msg}");
        assert!(msg.contains("denied"), "{msg}");
        let _ = std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755));
        // Nothing was created, not even a temporary.
        assert!(std::fs::read_dir(&locked).unwrap().next().is_none());
    }

    /// A write that fails after the temporary exists (ENOSPC, EIO, a
    /// timeout) must discard that temporary out of the media tree: unlike
    /// the database layer's build temporaries, nothing ever sweeps here.
    ///
    /// The failure is produced deterministically with RLIMIT_FSIZE, the one
    /// portable way to make a write fail on a healthy machine: a write that
    /// crosses the whole-file limit comes back EFBIG, the same error class a
    /// full volume produces. SIGXFSZ is ignored first because its default
    /// disposition would kill the whole test process, and both the limit and
    /// the disposition are restored before any assertion can bail, since
    /// they are process-wide and this binary's tests run as parallel
    /// threads. The limit sits far above every other write this test binary
    /// makes, whose largest is measured in kilobytes, so no sibling test can
    /// cross it inside the window.
    #[test]
    fn a_failed_write_discards_its_temporary() {
        let (_temp, ctx) = library();
        let dir = ctx.paths.root.join("full");
        std::fs::create_dir(&dir).unwrap();
        let limit: libc::rlim_t = 16 << 20;
        let bytes = vec![b'x'; (limit as usize) * 2];
        unsafe {
            let previous = libc::signal(libc::SIGXFSZ, libc::SIG_IGN);
            assert_ne!(previous, libc::SIG_ERR);
            let mut saved = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            assert_eq!(libc::getrlimit(libc::RLIMIT_FSIZE, &mut saved), 0);
            let limited = libc::rlimit {
                rlim_cur: limit,
                rlim_max: saved.rlim_max,
            };
            assert_eq!(libc::setrlimit(libc::RLIMIT_FSIZE, &limited), 0);
            let err = replace_sidecar(&ctx, &dir.join("image.xmp"), &bytes).unwrap_err();
            assert_eq!(libc::setrlimit(libc::RLIMIT_FSIZE, &saved), 0);
            libc::signal(libc::SIGXFSZ, previous);
            // The failure was the write itself, not an earlier stage.
            let msg = format!("{err:#}");
            assert!(msg.contains("write"), "{msg}");
            assert!(msg.contains("image.xmp"), "{msg}");
        }
        // Nothing was published and no temporary was left behind.
        assert!(
            std::fs::read_dir(&dir).unwrap().next().is_none(),
            "a failed sidecar write must discard its temporary out of the media tree"
        );
    }

    /// open_media hands out regular in-library files only: directories are
    /// refused, an in-root final link resolves to the real file it names, an
    /// out-of-root final link is refused outright, and a FIFO in a media name
    /// is an error rather than a hang.
    #[test]
    fn open_media_returns_only_a_regular_in_library_file() {
        let (temp, ctx) = library();
        std::fs::create_dir(ctx.paths.root.join("Trips")).unwrap();
        std::fs::write(ctx.paths.root.join("Trips/f.jpg"), b"jpeg").unwrap();
        let media = ctx.paths.root.join("Trips/f.jpg");
        assert_eq!(read_all(open_media(&ctx, &media).unwrap()), b"jpeg");
        let err = open_media(&ctx, &ctx.paths.root.join("Trips")).unwrap_err();
        assert!(format!("{err:#}").contains("not a regular file"), "{err:#}");
        std::os::unix::fs::symlink(&media, ctx.paths.root.join("alias.jpg")).unwrap();
        assert_eq!(
            read_all(open_media(&ctx, &ctx.paths.root.join("alias.jpg")).unwrap()),
            b"jpeg"
        );
        std::fs::write(temp.path().join("outside.jpg"), b"secret").unwrap();
        std::os::unix::fs::symlink(
            temp.path().join("outside.jpg"),
            ctx.paths.root.join("evil.jpg"),
        )
        .unwrap();
        let err = open_media(&ctx, &ctx.paths.root.join("evil.jpg")).unwrap_err();
        assert!(format!("{err:#}").contains("outside library"), "{err:#}");
        assert!(open_media(&ctx, &ctx.paths.root).is_err());
        // rustix gates mknodat away from apple, so the FIFO comes from the
        // platform's own mkfifo; when the tool is absent the refusal is
        // simply unproven here rather than falsely green.
        let fifo = ctx.paths.root.join("hang.jpg");
        let made = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .map(|status| status.success());
        match made {
            Ok(true) => {
                let err = open_media(&ctx, &fifo).unwrap_err();
                assert!(format!("{err:#}").contains("not a regular file"), "{err:#}");
            }
            _ => write_past_test_capture(
                "SKIP: mkfifo unavailable, so the FIFO refusal is unproven here\n",
            ),
        }
    }

    /// Publication replaces an existing sidecar atomically and leaves only
    /// the sidecar behind: no temporaries, in success or in refusal.
    #[test]
    fn replace_sidecar_replaces_an_existing_sidecar_and_leaves_no_temporaries() {
        let (_temp, ctx) = library();
        let dir = ctx.paths.root.join("out");
        std::fs::create_dir(&dir).unwrap();
        let target = dir.join("image.xmp");
        replace_sidecar(&ctx, &target, b"one").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"one");
        replace_sidecar(&ctx, &target, b"two").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"two");
        let names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            vec!["image.xmp".to_string()],
            "the publication must leave only the sidecar behind"
        );
    }

    /// The reserved state directory is not a sidecar location: its files are
    /// owned by the database, config, and lock layers, with their own
    /// redirect refusals.
    #[test]
    fn state_directory_targets_are_refused_for_sidecars() {
        let (_temp, ctx) = library();
        let err = replace_sidecar(&ctx, &ctx.paths.state.join("image.xmp"), b"x").unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("state directory"), "{msg}");
        // A file named exactly .videre in the root would shadow the state
        // directory a later initialization expects to create.
        let err = replace_sidecar(&ctx, &ctx.paths.root.join(".videre"), b"x").unwrap_err();
        assert!(format!("{err:#}").contains("state directory"), "{err:#}");
    }

    /// A missing parent directory is refused, and nothing is created on the
    /// way to the refusal.
    #[test]
    fn a_missing_parent_directory_is_refused_without_creating_anything() {
        let (_temp, ctx) = library();
        let err = replace_sidecar(&ctx, &ctx.paths.root.join("gone/image.xmp"), b"x").unwrap_err();
        assert!(format!("{err:#}").contains("gone"), "{err:#}");
        assert!(!ctx.paths.root.join("gone").exists());
    }

    /// Relative paths resolve against the library root, never a process cwd.
    #[test]
    fn relative_paths_resolve_against_the_library_root() {
        let (_temp, ctx) = library();
        std::fs::write(ctx.paths.root.join("f.jpg"), b"jpeg").unwrap();
        assert_eq!(
            read_all(open_media(&ctx, Path::new("f.jpg")).unwrap()),
            b"jpeg"
        );
        replace_sidecar(&ctx, Path::new("f.xmp"), b"marks").unwrap();
        assert_eq!(
            std::fs::read(ctx.paths.root.join("f.xmp")).unwrap(),
            b"marks"
        );
    }

    /// The staged copy for path-only decoders copies the confined file's
    /// bytes under the original's extension, and removes itself when dropped.
    #[test]
    fn a_staged_copy_preserves_bytes_and_extension_and_cleans_up() {
        let (_temp, ctx) = library();
        std::fs::write(ctx.paths.root.join("scan.dng"), b"raw-ish").unwrap();
        let confined = open_media(&ctx, &ctx.paths.root.join("scan.dng")).unwrap();
        std::fs::create_dir(&ctx.paths.state).unwrap();
        let staged = staged_copy(&ctx, &confined, OsStr::new("dng")).unwrap();
        assert_eq!(staged.path.extension(), Some(OsStr::new("dng")));
        assert_eq!(std::fs::read(&staged.path).unwrap(), b"raw-ish");
        let path = staged.path.clone();
        drop(staged);
        assert!(!path.exists(), "dropping the staged copy must remove it");
        // An extensionless original stages without inventing a suffix.
        let staged = staged_copy(&ctx, &confined, OsStr::new("")).unwrap();
        assert_eq!(staged.path.extension(), None);
        let path = staged.path.clone();
        drop(staged);
        assert!(!path.exists());
    }
}
