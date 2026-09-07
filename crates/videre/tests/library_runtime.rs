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

#[test]
fn prune_is_excluded_while_a_shared_operation_holds_the_library() {
    // A held shared lease (a reader or ordinary writer) must lock out prune's
    // exclusive maintenance: prune removes rows the shared operation may be
    // acting on. Proven across processes: the in-process lease is a real flock
    // the spawned `videre prune` contends with.
    let lib = TestLibrary::new();
    std::fs::write(lib.root.join("a.jpg"), b"content").unwrap();
    lib.scan();

    let ctx = lib.context();
    let held = library_locks::try_activity(&ctx, ActivityMode::Shared).unwrap();

    let out = lib.cmd().args(["prune", "--silent"]).output().unwrap();
    assert!(
        !out.status.success(),
        "prune must not run while a shared lease is held"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("in use"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    drop(held);
    // Once the lease is released, prune runs.
    let out = lib.cmd().args(["prune", "--silent"]).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn a_reader_is_excluded_while_exclusive_maintenance_holds_the_library() {
    // The converse: while prune-style exclusive maintenance holds the library,
    // an ordinary shared operation (stats) is refused rather than reading a
    // library mid-rewrite.
    let lib = TestLibrary::new();
    std::fs::write(lib.root.join("a.jpg"), b"content").unwrap();
    lib.scan();

    let ctx = lib.context();
    let held = library_locks::try_activity(&ctx, ActivityMode::Exclusive).unwrap();

    let out = lib.cmd().args(["stats"]).output().unwrap();
    assert!(
        !out.status.success(),
        "stats must not read during exclusive maintenance"
    );

    drop(held);
    let out = lib.cmd().args(["stats"]).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
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

#[test]
fn concurrent_publication_never_exposes_mixed_bytes() {
    // Each publish writes its own temp file and atomically renames it into
    // place, so a reader after the race sees exactly one writer's bytes, never
    // an interleaving of both.
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("result.bin");
    std::thread::scope(|scope| {
        for byte in [17u8, 91u8] {
            let path = target.clone();
            scope.spawn(move || {
                videre_core::atomic_file::publish(&path, |file| {
                    use std::io::Write;
                    file.write_all(&vec![byte; 8192])?;
                    Ok(())
                })
                .unwrap();
            });
        }
    });
    let actual = std::fs::read(&target).unwrap();
    assert!(
        actual == vec![17u8; 8192] || actual == vec![91u8; 8192],
        "the published file must be exactly one writer's bytes"
    );
}

#[test]
fn a_failed_publication_leaves_the_previous_bytes_intact() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("result.bin");
    videre_core::atomic_file::publish(&target, |file| {
        use std::io::Write;
        file.write_all(b"original")?;
        Ok(())
    })
    .unwrap();

    // A writer that fails mid-write must not replace the destination.
    let result =
        videre_core::atomic_file::publish(&target, |_file| anyhow::bail!("the write failed"));
    assert!(result.is_err());
    assert_eq!(
        std::fs::read(&target).unwrap(),
        b"original",
        "a failed publish must leave the previously published bytes in place"
    );
}

#[test]
fn a_face_crop_key_changes_with_its_geometry() {
    // The cached crop's identity includes the bbox and size, so re-cropping the
    // same face at a different geometry cannot return a stale cached image.
    let lib = TestLibrary::new();
    let cache = lib.context().cache;
    let base =
        videre_core::thumb_cache::face_thumb_path_in(&cache, "hash", 1, [1.0, 2.0, 3.0, 4.0], 140);
    let moved =
        videre_core::thumb_cache::face_thumb_path_in(&cache, "hash", 1, [1.0, 2.0, 3.0, 5.0], 140);
    let resized =
        videre_core::thumb_cache::face_thumb_path_in(&cache, "hash", 1, [1.0, 2.0, 3.0, 4.0], 280);
    assert_ne!(base, moved, "a changed bbox must change the cache key");
    assert_ne!(base, resized, "a changed size must change the cache key");
}
