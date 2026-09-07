//! Runtime isolation: locking, liveness, interruption and root binding under
//! concurrent access to one library and across independent libraries.
//!
//! These exercise the library-scoped lock and pipeline-run primitives directly
//! against real `LibraryContext`s under temp roots. File locks contend within a
//! single process (flock is associated with the open file, not the thread), so
//! a held guard and a second acquisition attempt prove contention without a
//! second process.

mod common;
use common::TestLibrary;
use videre_core::library_locks::{self, ActivityMode};
use videre_core::{library_db, pipeline_runs};

#[test]
fn contended_command_does_not_overwrite_the_active_run() {
    let a = TestLibrary::new();
    let ctx = a.context();
    let conn = library_db::initialize(&ctx).unwrap();
    let held = library_locks::try_command(&ctx, "scan").unwrap();
    pipeline_runs::start_run(&conn, "scan").unwrap();
    let before: String = conn
        .query_row(
            "SELECT started_at FROM pipeline_runs WHERE command='scan'",
            [],
            |r| r.get(0),
        )
        .unwrap();

    // A second contender must be refused, and must not have touched the row.
    assert!(library_locks::try_command(&ctx, "scan").is_err());
    let after: String = conn
        .query_row(
            "SELECT started_at FROM pipeline_runs WHERE command='scan'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(before, after);

    drop(held);
    assert!(!library_locks::command_locked(&ctx, "scan").unwrap());
}

#[test]
fn separate_libraries_do_not_contend_for_a_command() {
    let a = TestLibrary::new();
    let b = TestLibrary::new();
    let ca = a.context();
    let cb = b.context();
    drop(library_db::initialize(&ca).unwrap());
    drop(library_db::initialize(&cb).unwrap());

    let _held = library_locks::try_command(&ca, "scan").unwrap();
    // The same command in an unrelated library is a different lock file.
    let other = library_locks::try_command(&cb, "scan").unwrap();
    drop(other);
}

#[test]
fn a_dropped_command_lock_leaves_no_live_holder() {
    let a = TestLibrary::new();
    let ctx = a.context();
    drop(library_db::initialize(&ctx).unwrap());
    {
        let _held = library_locks::try_command(&ctx, "faces").unwrap();
        assert!(library_locks::command_locked(&ctx, "faces").unwrap());
    }
    // The lock file survives on disk, but nothing holds it, so it is not live.
    assert!(!library_locks::command_locked(&ctx, "faces").unwrap());
}

#[test]
fn track_in_records_failure_without_masking_the_error() {
    let a = TestLibrary::new();
    let ctx = a.context();
    let conn = library_db::initialize(&ctx).unwrap();
    let guard = library_locks::try_command(&ctx, "faces").unwrap();

    let result: anyhow::Result<()> = pipeline_runs::track_in(&conn, &ctx, &guard, "faces", || {
        anyhow::bail!("the work itself failed")
    });
    let message = format!("{:#}", result.unwrap_err());
    assert!(message.contains("the work itself failed"), "{message}");

    let status: String = conn
        .query_row(
            "SELECT status FROM pipeline_runs WHERE command='faces'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(status, "failed", "a failed run must be recorded as failed");
}

#[test]
fn track_in_refuses_a_guard_taken_for_another_command() {
    let a = TestLibrary::new();
    let ctx = a.context();
    let conn = library_db::initialize(&ctx).unwrap();
    let scan_guard = library_locks::try_command(&ctx, "scan").unwrap();
    // Bookkeeping under a mismatched guard would record one command's run
    // against another; track_in must refuse before writing anything.
    let result: anyhow::Result<()> =
        pipeline_runs::track_in(&conn, &ctx, &scan_guard, "faces", || Ok(()));
    assert!(result.is_err());
    let has_faces_row: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM pipeline_runs WHERE command='faces'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .unwrap()
        > 0;
    assert!(!has_faces_row, "no run row may be written under a mismatch");
}

#[test]
fn activity_shared_coexists_but_exclusive_is_local_and_nonblocking() {
    let a = TestLibrary::new();
    let b = TestLibrary::new();
    let ca = a.context();
    let cb = b.context();
    drop(library_db::initialize(&ca).unwrap());
    drop(library_db::initialize(&cb).unwrap());

    let one = library_locks::try_activity(&ca, ActivityMode::Shared).unwrap();
    let two = library_locks::try_activity(&ca, ActivityMode::Shared).unwrap();
    // Exclusive maintenance cannot start while shared readers hold the library.
    assert!(library_locks::try_activity(&ca, ActivityMode::Exclusive).is_err());
    // A different library is unaffected.
    let other = library_locks::try_activity(&cb, ActivityMode::Exclusive).unwrap();

    drop(one);
    drop(two);
    assert!(library_locks::try_activity(&ca, ActivityMode::Exclusive).is_ok());
    drop(other);
}

#[cfg(unix)]
#[test]
fn root_alias_and_explicit_selection_share_the_same_command_lock() {
    let a = TestLibrary::new();
    let ctx = a.context();
    drop(library_db::initialize(&ctx).unwrap());
    let alias = a.home.join("alias");
    std::os::unix::fs::symlink(&a.root, &alias).unwrap();
    let other = videre_core::library::LibraryContext::new(&alias, &a.home.join(".cache")).unwrap();

    let _held = library_locks::try_command(&ctx, "scan").unwrap();
    // The alias canonicalizes to the same root, so it is the same lock.
    assert!(library_locks::try_command(&other, "scan").is_err());
}
