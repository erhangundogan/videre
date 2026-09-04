//! The guard between user-supplied path filters and one selected library.
//!
//! A path filter says which part of a library a command may touch, so under
//! the directory-local model it is constrained to the library the command was
//! pointed at: relative filters resolve against the library root (never any
//! helper's cwd, which is ambient state by definition), and absolute filters
//! are allowed only when their physically resolved target is the root itself
//! or a descendant of it. [`validate_paths`] is that rule as one shared
//! check, receiving the captured root explicitly through a
//! [`LibraryContext`]; it never changes cwd, opens a child library, or
//! overrides any state path.
//!
//! Membership is decided physically, component by component: each component
//! is stat'd without following it, symlinks are expanded as they are met, and
//! a later `..` pops the component the link resolved *to*, not the link's own
//! name. That is the kernel's semantics, and it is why a lexical normalizer
//! would be wrong: `library/link/..`, where `link` points outside the
//! library, lexically cancels to `library`, while physically it is the
//! outside target's parent. The final comparison is on path components too
//! (`Path::starts_with`), so `/photos-old` is not inside `/photos`.
//!
//! One missing suffix is tolerated, because a filter's purpose is to select
//! indexed rows and those rows can outlive their files: the walk stops at the
//! first component that does not exist, keeps the nearest existing ancestor
//! it already resolved, and normalizes the remaining components onto it
//! before the same containment check, creating nothing on the way.
//! Everything that is not a plain absent component fails closed: permission
//! errors, symlink loops, dangling links, and timeouts are not evidence of a
//! safe missing path, and neither is any inconsistency met mid-resolution.
//!
//! One invalid filter rejects the entire invocation: discarding the bad ones
//! and proceeding would run a command against a different library subset than
//! the user named, with no sign it happened. The returned forms keep the safe
//! given spelling alongside the resolved one, because indexed rows may hold
//! either form, the same reason `selection`'s matching accepts both.

use crate::io_timeout::STAT_TIMEOUT;
use crate::library::{bounded_op, LibraryContext};
use anyhow::{bail, Result};
use std::collections::VecDeque;
use std::ffi::{OsStr, OsString};
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

/// Cap on symlink expansions while resolving one filter, matching the
/// kernel's own loop ceiling (SYMLOOP_MAX, 40): beyond this the path is a
/// loop or an abuse of links, not a directory in a library.
const MAX_LINK_EXPANSIONS: usize = 40;

/// Counts resolutions in tests, proving filesystem work scales with the
/// number of supplied filters rather than with the rows a query matches.
/// Thread-local because libtest runs each test on its own thread: a
/// process-global counter would attribute neighbouring tests' walks to
/// whoever read it.
#[cfg(test)]
thread_local! {
    static RESOLUTIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Test-only read of the resolution counter.
#[cfg(test)]
pub(crate) fn count_resolutions() -> usize {
    RESOLUTIONS.with(std::cell::Cell::get)
}

/// Test-only bump of the resolution counter.
#[cfg(test)]
fn note_resolution() {
    RESOLUTIONS.with(|c| c.set(c.get() + 1));
}

/// Validate every supplied path filter against one library, returning the
/// forms a selection should match.
///
/// Each filter is resolved physically (symlinks and `..` included) inside one
/// five-second budget, and must land on the library root or inside it; the
/// first filter that does not rejects the whole invocation, so a caller can
/// never proceed on a quietly trimmed-down set of filters. The returned forms
/// carry the safe given spelling alongside the resolved one wherever they
/// differ, sorted and deduplicated, ready for the row matcher's either-form
/// comparison. Relative filters are joined onto the canonical root before
/// anything touches the filesystem, so no step of this ever consults a cwd.
pub fn validate_paths(ctx: &LibraryContext, paths: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut forms = Vec::with_capacity(paths.len() * 2);
    for raw in paths {
        let candidate = if raw.is_absolute() {
            raw.clone()
        } else {
            ctx.paths.root.join(raw)
        };
        let resolved = resolve_allow_missing(&candidate)?;
        // Component-wise, not bytes: `starts_with` compares components, so a
        // sibling like `photos-old` is not inside `photos`.
        if !resolved.starts_with(&ctx.paths.root) {
            bail!(
                "path filter {} resolves to {}, which is outside library {}; path filters must name the library itself or a directory inside it",
                raw.display(),
                resolved.display(),
                ctx.paths.root.display()
            );
        }
        forms.push(candidate.clone());
        if resolved != candidate {
            forms.push(resolved);
        }
    }
    forms.sort();
    forms.dedup();
    Ok(forms)
}

/// Resolve one candidate inside a single five-second budget.
///
/// The budget covers the whole component walk for this one filter, not each
/// component: a wedged mount fails the filter once instead of resetting the
/// clock per stat.
fn resolve_allow_missing(candidate: &Path) -> Result<PathBuf> {
    #[cfg(test)]
    {
        note_resolution();
    }
    resolve_budgeted(candidate, STAT_TIMEOUT)
}

/// `resolve_allow_missing` with the budget as a parameter, so tests can prove
/// the bound the same way `library::bounded_op`'s tests prove theirs.
fn resolve_budgeted(candidate: &Path, budget: Duration) -> Result<PathBuf> {
    let owned = candidate.to_path_buf();
    bounded_op(candidate, "resolve", budget, move || walk(&owned))
}

/// `path`'s components as resolution steps. `.` names the directory the walk
/// is already in, so it is dropped here (skipping it is physically
/// identical); `..` and names are kept, and the caller handles the leading
/// `/` of an absolute path. `from_link` marks components that arrived by
/// expanding a symlink rather than from the path as given, which is how a
/// dangling link is told apart from a missing one.
fn steps(path: &Path, from_link: bool) -> Vec<(OsString, bool)> {
    path.components()
        .filter_map(|c| match c {
            Component::Normal(name) => Some((name.to_os_string(), from_link)),
            Component::ParentDir => Some((OsString::from(".."), from_link)),
            Component::CurDir => None,
            Component::RootDir | Component::Prefix(_) => None,
        })
        .collect()
}

/// Physically resolve one absolute path, tolerating a single missing suffix.
///
/// The walk keeps a prefix that is resolved so far (no symlinks left in it)
/// and a queue of components still to process. Each component is stat'd
/// without following it: a symlink is expanded immediately, so a later `..`
/// pops the component the link resolved *to*, exactly as the kernel resolves
/// `link/..`. When a component of the path as given is simply absent, the
/// remainder is normalized onto the already-resolved prefix and returned:
/// the caller checks containment either way, and nothing is ever created.
///
/// Every other outcome is an error, so the caller fails closed: a permission
/// denial, a `..` through a regular file (ENOTDIR), a kernel-reported loop,
/// or a dangling link, which is a component that arrived by expanding a
/// symlink and then did not exist. A missing component of the path as given
/// is the only absence that counts as a missing suffix, because a filter
/// selecting deleted-but-indexed files names those paths directly.
///
/// Names travel as `OsString` end to end; `display()` is used only to phrase
/// error messages, never to compare or build a path.
fn walk(path: &Path) -> std::io::Result<PathBuf> {
    let mut resolved = PathBuf::from("/");
    let mut pending: VecDeque<(OsString, bool)> = steps(path, false).into_iter().collect();
    let mut expansions = 0usize;
    while let Some((name, from_link)) = pending.pop_front() {
        if name.as_os_str() == OsStr::new("..") {
            // At "/" this is a no-op, exactly as the kernel treats /.. as /.
            resolved.pop();
            continue;
        }
        let step = resolved.join(&name);
        let meta = match std::fs::symlink_metadata(&step) {
            Ok(meta) => meta,
            Err(e) if e.kind() == ErrorKind::NotFound => {
                if from_link {
                    return Err(std::io::Error::new(
                        ErrorKind::InvalidInput,
                        format!(
                            "dangling symbolic link on the way to {}: {} does not exist",
                            path.display(),
                            step.display()
                        ),
                    ));
                }
                // The missing suffix: nothing under a missing directory can
                // be a symlink, so lexical is the only semantics available
                // for the remainder. `..` still pops the resolved prefix, so
                // the suffix cannot climb somewhere else unnoticed; where it
                // lands is judged by the same containment check.
                resolved.push(name);
                for (rest, _) in pending.drain(..) {
                    if rest.as_os_str() == OsStr::new("..") {
                        resolved.pop();
                    } else {
                        resolved.push(rest);
                    }
                }
                return Ok(resolved);
            }
            Err(e) => return Err(e),
        };
        if meta.file_type().is_symlink() {
            expansions += 1;
            if expansions > MAX_LINK_EXPANSIONS {
                // FilesystemLoop is the honest kind but still unstable, so
                // the message carries the meaning; the kind is never read.
                return Err(std::io::Error::new(
                    ErrorKind::InvalidInput,
                    format!(
                        "resolving {} crosses more than {MAX_LINK_EXPANSIONS} symbolic links",
                        path.display()
                    ),
                ));
            }
            // A read_link that fails (a link replaced by a directory between
            // the stat and the read fails with EINVAL) falls through as the
            // error it is, so a mid-resolution swap cannot pass silently.
            // Whatever target is read is itself resolved physically, and the
            // final containment check judges where the whole walk landed.
            let target = std::fs::read_link(&step)?;
            if target.is_absolute() {
                resolved = PathBuf::from("/");
            }
            for (expanded, _) in steps(&target, true).into_iter().rev() {
                pending.push_front((expanded, true));
            }
        } else {
            resolved = step;
        }
    }
    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// Writes to the process's real stderr, bypassing libtest's output
    /// capture. A skip is a passing test, and libtest captures the print
    /// macros for passing tests, so an `eprintln!` skip message is invisible
    /// in a normal `cargo test` run; writing to fd 2 directly sidesteps the
    /// capture. Same pattern as `library.rs`, local here because videre-core
    /// unit tests have no shared helper.
    fn write_past_test_capture(msg: &str) {
        use std::io::Write;
        use std::os::fd::FromRawFd;

        let mut stderr = std::mem::ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(2) });
        let _ = stderr.write_all(msg.as_bytes());
        let _ = stderr.flush();
    }

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

    #[test]
    fn mixed_paths_reject_the_entire_selection() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("photos");
        std::fs::create_dir_all(root.join("Trips")).unwrap();
        let ctx = crate::library::LibraryContext::new(&root, &temp.path().join("cache")).unwrap();
        assert!(validate_paths(&ctx, &[std::path::PathBuf::from("Trips")]).is_ok());
        assert!(validate_paths(&ctx, &[std::path::PathBuf::from("missing")]).is_ok());
        assert!(validate_paths(
            &ctx,
            &[
                std::path::PathBuf::from("Trips"),
                std::path::PathBuf::from("../outside")
            ]
        )
        .is_err());
        assert!(!ctx.paths.state.exists());
    }

    #[test]
    fn membership_is_decided_physically_and_component_wise() {
        // Each case gets its own library, so fixtures cannot leak between
        // rows of the table. `setup` and `filter` receive the tempdir (for
        // siblings and outside targets) and the canonical root.
        struct Case {
            name: &'static str,
            setup: fn(&Path, &Path),
            filter: fn(&Path, &Path) -> PathBuf,
            ok: bool,
        }
        let cases = vec![
            Case {
                name: "the library itself, spelled relatively as .",
                setup: |_, _| {},
                filter: |_, _| PathBuf::from("."),
                ok: true,
            },
            Case {
                name: "relative child with spaces and Turkish characters",
                setup: |_, root| {
                    std::fs::create_dir_all(root.join("Trips/Fotoğraflar 2024")).unwrap()
                },
                filter: |_, _| PathBuf::from("Trips/Fotoğraflar 2024"),
                ok: true,
            },
            Case {
                name: "absolute child",
                setup: |_, root| std::fs::create_dir(root.join("Trips")).unwrap(),
                filter: |_, root| root.join("Trips"),
                ok: true,
            },
            Case {
                name: "the root reached through an alias",
                setup: |temp, root| std::os::unix::fs::symlink(root, temp.join("alias")).unwrap(),
                filter: |temp, _| temp.join("alias"),
                ok: true,
            },
            Case {
                name: "an alias to the root then a child",
                setup: |temp, root| std::os::unix::fs::symlink(root, temp.join("alias")).unwrap(),
                filter: |temp, _| temp.join("alias").join("Trips"),
                ok: true,
            },
            Case {
                name: "sibling sharing the root as a string prefix",
                setup: |temp, _| std::fs::create_dir(temp.join("photos-old")).unwrap(),
                filter: |temp, _| temp.join("photos-old"),
                ok: false,
            },
            Case {
                name: "absolute path outside the library",
                setup: |temp, _| std::fs::create_dir(temp.join("elsewhere")).unwrap(),
                filter: |temp, _| temp.join("elsewhere"),
                ok: false,
            },
            Case {
                name: "relative parent traversal",
                setup: |temp, _| std::fs::create_dir(temp.join("outside")).unwrap(),
                filter: |_, _| PathBuf::from("../outside"),
                ok: false,
            },
            Case {
                name: "absolute parent traversal",
                setup: |temp, _| std::fs::create_dir(temp.join("outside")).unwrap(),
                filter: |_, root| root.join("../outside"),
                ok: false,
            },
            Case {
                name: "the library's own parent",
                setup: |_, _| {},
                filter: |_, root| root.join(".."),
                ok: false,
            },
            Case {
                name: "leaving and re-entering the root by name",
                setup: |_, _| {},
                filter: |_, _| PathBuf::from("../photos"),
                ok: true,
            },
            Case {
                name: "in-root symlink pointing outside",
                setup: |temp, root| {
                    let outside = temp.join("elsewhere");
                    std::fs::create_dir(&outside).unwrap();
                    std::os::unix::fs::symlink(&outside, root.join("evil")).unwrap()
                },
                filter: |_, _| PathBuf::from("evil"),
                ok: false,
            },
            Case {
                name: "in-root symlink pointing inside",
                setup: |_, root| {
                    std::fs::create_dir(root.join("Trips")).unwrap();
                    std::os::unix::fs::symlink(root.join("Trips"), root.join("trips-alias"))
                        .unwrap()
                },
                filter: |_, _| PathBuf::from("trips-alias"),
                ok: true,
            },
            Case {
                name: "symlink then dotdot resolves physically, not lexically",
                setup: |temp, root| {
                    let outside = temp.join("elsewhere");
                    std::fs::create_dir_all(outside.join("inner")).unwrap();
                    std::os::unix::fs::symlink(&outside.join("inner"), root.join("jump")).unwrap()
                },
                // Lexically `jump/..` cancels to the root; physically the link
                // resolves outside first, so `..` lands in the outside
                // directory's parent. The guard must follow the filesystem.
                filter: |_, _| PathBuf::from("jump/.."),
                ok: false,
            },
            Case {
                name: "missing suffix under an existing directory",
                setup: |_, root| std::fs::create_dir(root.join("Trips")).unwrap(),
                filter: |_, _| PathBuf::from("Trips/gone"),
                ok: true,
            },
            Case {
                name: "missing suffix several components deep",
                setup: |_, _| {},
                filter: |_, _| PathBuf::from("gone/deeper/still"),
                ok: true,
            },
            Case {
                name: "missing sibling sharing the root as a string prefix",
                setup: |_, _| {},
                filter: |temp, _| temp.join("photos-gone"),
                ok: false,
            },
            Case {
                name: "dangling symlink with a relative target",
                setup: |_, root| {
                    std::os::unix::fs::symlink(root.join("gone-target"), root.join("dangle"))
                        .unwrap()
                },
                filter: |_, _| PathBuf::from("dangle"),
                ok: false,
            },
            Case {
                name: "dangling symlink with an absolute outside target",
                setup: |temp, root| {
                    std::os::unix::fs::symlink(temp.join("nonexistent-target"), root.join("dangle"))
                        .unwrap()
                },
                filter: |_, _| PathBuf::from("dangle"),
                ok: false,
            },
            Case {
                name: "symlink loop",
                setup: |_, root| std::os::unix::fs::symlink("loop", root.join("loop")).unwrap(),
                filter: |_, _| PathBuf::from("loop"),
                ok: false,
            },
        ];
        for case in cases {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().join("photos");
            std::fs::create_dir(&root).unwrap();
            let ctx = LibraryContext::new(&root, &temp.path().join("cache")).unwrap();
            let canonical = ctx.paths.root.clone();
            (case.setup)(&temp.path(), &canonical);
            let filter = (case.filter)(&temp.path(), &canonical);
            let got = validate_paths(&ctx, std::slice::from_ref(&filter));
            assert_eq!(
                got.is_ok(),
                case.ok,
                "case {:?} (filter {:?}): {:?}",
                case.name,
                filter,
                got.map_err(|e| format!("{e:#}"))
            );
        }
    }

    #[test]
    fn one_invalid_filter_rejects_the_valid_ones_in_either_order() {
        let (temp, ctx) = library();
        std::fs::create_dir(ctx.paths.root.join("Trips")).unwrap();
        let outside = temp.path().join("elsewhere");
        std::fs::create_dir(&outside).unwrap();
        for order in [
            vec![PathBuf::from("Trips"), outside.clone()],
            vec![outside.clone(), PathBuf::from("Trips")],
        ] {
            let err = validate_paths(&ctx, &order).unwrap_err();
            let msg = format!("{err:#}");
            assert!(msg.contains("outside library"), "{msg}");
            assert!(msg.contains("elsewhere"), "{msg}");
            assert!(msg.contains(&ctx.paths.root.display().to_string()), "{msg}");
        }
        // Rejecting filters must not bring the library's state into being.
        assert!(!ctx.paths.state.exists());
    }

    #[test]
    fn forms_keep_the_given_spelling_alongside_the_resolved_one() {
        let (_temp, ctx) = library();
        std::fs::create_dir(ctx.paths.root.join("Trips")).unwrap();
        std::os::unix::fs::symlink(
            ctx.paths.root.join("Trips"),
            ctx.paths.root.join("trips-alias"),
        )
        .unwrap();
        // A filter through the alias matches rows stored under either name,
        // so both spellings come back, sorted and deduplicated.
        let forms = validate_paths(&ctx, &[PathBuf::from("trips-alias")]).unwrap();
        assert_eq!(
            forms,
            vec![
                ctx.paths.root.join("Trips"),
                ctx.paths.root.join("trips-alias")
            ]
        );
        // A plain in-root filter resolves to itself: one form.
        let forms = validate_paths(&ctx, &[PathBuf::from("Trips")]).unwrap();
        assert_eq!(forms, vec![ctx.paths.root.join("Trips")]);
        // Repeated filters collapse rather than resolve twice in the output.
        let forms =
            validate_paths(&ctx, &[PathBuf::from("Trips"), PathBuf::from("Trips")]).unwrap();
        assert_eq!(forms, vec![ctx.paths.root.join("Trips")]);
    }

    #[test]
    fn one_resolution_per_supplied_filter_and_none_for_none() {
        let (_temp, ctx) = library();
        std::fs::create_dir(ctx.paths.root.join("Trips")).unwrap();
        std::fs::create_dir(ctx.paths.root.join("2024")).unwrap();
        let before = count_resolutions();
        validate_paths(
            &ctx,
            &[
                PathBuf::from("Trips"),
                ctx.paths.root.join("2024"),
                PathBuf::from("Trips"),
            ],
        )
        .unwrap();
        assert_eq!(
            count_resolutions() - before,
            3,
            "three supplied filters, three resolutions, duplicates included"
        );
        // An empty invocation does no filesystem work at all.
        let before = count_resolutions();
        assert!(validate_paths(&ctx, &[]).unwrap().is_empty());
        assert_eq!(count_resolutions(), before);
    }

    #[test]
    fn an_unsearchable_directory_fails_closed() {
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
                "SKIP: running as root, so chmod 000 does not block searching a directory\n",
            );
            return;
        }
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        // `locked` itself stats fine; the component under it cannot be looked
        // up, and a permission error is not evidence of a safe missing path.
        let err = validate_paths(&ctx, &[PathBuf::from("locked/inside")]).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("locked"), "{msg}");
        assert!(msg.contains("denied"), "{msg}");
        let _ = std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755));
    }

    #[test]
    fn a_resolution_past_its_budget_is_cut_off_and_names_the_path() {
        // A wedged mount cannot be produced portably, so the bound is proven
        // the way `library.rs` proves bounded_op's own: a budget too small to
        // cover even the thread spawn plus one component walk. The margin is
        // several orders of magnitude, so there is no timing to flake on.
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("a");
        std::fs::create_dir_all(&dir).unwrap();
        let start = std::time::Instant::now();
        let err = resolve_budgeted(&dir, Duration::from_nanos(1)).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("did not respond"), "{msg}");
        assert!(msg.contains(&dir.display().to_string()), "{msg}");
        assert!(start.elapsed() < Duration::from_secs(2));
    }
}
