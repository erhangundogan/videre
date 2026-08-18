//! What a scoped run says, and what it must not do while saying it.
//!
//! `embed`, `classify` and `faces` share one implementation of "work out what is
//! pending, narrow it by the selection, stop if nothing is left"
//! (`videre_core::work`). These are the end-to-end assertions on that shared
//! path: the exact wording users read, and the guarantee that a run with nothing
//! to do never reaches a model.
//!
//! The wording is pinned deliberately. It is a user-visible contract - people
//! grep stderr - and it was three different phrasings before the commands shared
//! an implementation, which is exactly the drift a test prevents.

mod common;
use common::videre_bin;
use std::fs;
use std::path::Path;
use std::process::Command;
use tempfile::{tempdir, TempDir};

/// A library with one real, embeddable image, so a filter has something to
/// exclude and the "N of M" denominator is not zero.
fn library() -> (TempDir, std::path::PathBuf) {
    let dir = tempdir().unwrap();
    let pics = dir.path().join("pics");
    fs::create_dir_all(&pics).unwrap();
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_with_exif.jpg"),
        pics.join("a.jpg"),
    )
    .unwrap();
    let db = dir.path().join("t.db");
    let ok = run(
        dir.path(),
        &[
            "scan",
            pics.to_str().unwrap(),
            "--db",
            db.to_str().unwrap(),
            "--silent",
        ],
    );
    assert!(ok.0, "fixture scan failed: {}", ok.1);
    (dir, db)
}

/// A library whose one file is a `.dng`: scanned and stored like any other, but
/// explicitly vetoed as non-embeddable and carrying nothing to detect faces in.
///
/// Needed because a library with a *real* image gives these commands genuine
/// work, and genuine work means loading a model - an early version of this file
/// downloaded 191MB of face models by asking `videre faces` to run on a JPEG.
/// Tests never download; this fixture is how that stays true while still
/// exercising the "nothing to do" paths.
fn empty_library() -> (TempDir, std::path::PathBuf) {
    let dir = tempdir().unwrap();
    let pics = dir.path().join("pics");
    fs::create_dir_all(&pics).unwrap();
    fs::write(pics.join("a.dng"), b"x").unwrap();
    let db = dir.path().join("t.db");
    let ok = run(
        dir.path(),
        &[
            "scan",
            pics.to_str().unwrap(),
            "--db",
            db.to_str().unwrap(),
            "--silent",
        ],
    );
    assert!(ok.0, "fixture scan failed: {}", ok.1);
    (dir, db)
}

/// Runs videre with both homes pointed at the temp dir, so a stray model load
/// lands somewhere this test can inspect rather than in the real cache.
fn run(home: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(videre_bin())
        .env("VIDERE_HOME", home)
        .env("HF_HOME", home.join("hf"))
        .args(args)
        .output()
        .unwrap();
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stderr),
            String::from_utf8_lossy(&out.stdout)
        ),
    )
}

fn downloaded_bytes(home: &Path) -> u64 {
    let hf = home.join("hf");
    if !hf.exists() {
        return 0;
    }
    let mut total = 0;
    let mut stack = vec![hf];
    while let Some(d) = stack.pop() {
        let Ok(entries) = fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let Ok(md) = e.metadata() else { continue };
            if md.is_dir() {
                stack.push(e.path());
            } else {
                total += md.len();
            }
        }
    }
    total
}

#[test]
fn a_filter_matching_nothing_reports_both_numbers_and_loads_no_model() {
    // The denominator is the point. Without it, "a filter matched nothing" and
    // "the library is empty" are indistinguishable, which is why the line says
    // `0 of 1` rather than just stopping.
    let (dir, db) = library();
    let (ok, text) = run(
        dir.path(),
        &["embed", "--db", db.to_str().unwrap(), "--type", "video"],
    );

    assert!(ok, "a filter matching nothing is not an error:\n{text}");
    assert!(
        text.contains("Embedding 0 of 1 pending item(s) (--type video)"),
        "expected the scoped count line, got:\n{text}"
    );
    assert!(
        text.contains("Nothing to embed: the selection matched nothing pending."),
        "expected the selection-specific message, got:\n{text}"
    );
    assert_eq!(
        downloaded_bytes(dir.path()),
        0,
        "a run with nothing to do must not reach a model"
    );
}

#[test]
fn the_two_empty_states_say_different_things() {
    // "You are up to date" and "your filter matched nothing" call for opposite
    // reactions from the reader, so they must not collapse into one sentence.
    // A DNG is stored but never embeddable, so the pending set is empty without
    // anything having to be processed first.
    let (empty, empty_db) = empty_library();
    let (_, up_to_date) = run(empty.path(), &["embed", "--db", empty_db.to_str().unwrap()]);
    assert!(
        up_to_date.contains("Nothing to embed: everything eligible is already done."),
        "expected the up-to-date message, got:\n{up_to_date}"
    );

    let (dir, db) = library();
    let (_, filtered) = run(
        dir.path(),
        &["embed", "--db", db.to_str().unwrap(), "--type", "video"],
    );
    assert!(
        filtered.contains("the selection matched nothing pending"),
        "expected the filtered message, got:\n{filtered}"
    );
    assert!(
        !filtered.contains("everything eligible is already done"),
        "the two states must not both be reported:\n{filtered}"
    );
    assert_eq!(downloaded_bytes(dir.path()), 0);
}

#[test]
fn every_command_uses_the_same_wording_for_the_same_idea() {
    // Before these commands shared an implementation they counted "pending
    // file(s)", "pending hash(es)" and "eligible file(s)" - three names for one
    // idea. This is the test that stops a fourth appearing.
    // The DNG library: every command has nothing to do, so none of them loads a
    // model, and all three still print their version of the line.
    let (dir, db) = empty_library();
    for cmd in ["embed", "classify", "faces"] {
        let (_, text) = run(
            dir.path(),
            &[cmd, "--db", db.to_str().unwrap(), "--type", "video"],
        );
        for stale in ["pending file(s)", "pending hash(es)", "eligible file(s)"] {
            assert!(
                !text.contains(stale),
                "{cmd} still says {stale:?}, which the shared wording replaced:\n{text}"
            );
        }
    }
    assert_eq!(downloaded_bytes(dir.path()), 0);
}

#[test]
fn silent_suppresses_the_scoped_report() {
    let (dir, db) = library();
    let (ok, text) = run(
        dir.path(),
        &[
            "embed",
            "--db",
            db.to_str().unwrap(),
            "--type",
            "video",
            "--silent",
        ],
    );
    assert!(ok, "{text}");
    assert!(
        !text.contains("pending item(s)") && !text.contains("Nothing to embed"),
        "--silent must suppress both the count line and the reason:\n{text}"
    );
}

#[test]
fn faces_reports_once_and_recluster_stays_quiet_about_detection() {
    // Regression guards for two bugs introduced while moving faces onto the
    // shared helper: it reported the same state twice in different words, and
    // --recluster started complaining about eligible files it never looks at.
    // Must be the DNG library: on a real image `faces` has work to do and would
    // correctly download face models, which a test must never cause.
    let (dir, db) = empty_library();

    let (ok, plain) = run(dir.path(), &["faces", "--db", db.to_str().unwrap()]);
    assert!(ok, "{plain}");
    let said_no_eligible = plain
        .matches("No eligible files to scan for faces.")
        .count();
    let said_all_processed = plain.matches("All hashes already processed.").count();
    assert!(
        said_no_eligible + said_all_processed <= 1,
        "faces described one state more than once:\n{plain}"
    );

    let (ok, recluster) = run(
        dir.path(),
        &["faces", "--db", db.to_str().unwrap(), "--recluster"],
    );
    assert!(ok, "{recluster}");
    assert!(
        !recluster.contains("No eligible files to scan for faces."),
        "--recluster skips detection, so it must not report on it:\n{recluster}"
    );
    assert_eq!(downloaded_bytes(dir.path()), 0);
}

#[test]
fn an_unfiltered_run_reports_no_scope_line_at_all() {
    // The "N of M" line exists to explain a filter. Printing it on an
    // unfiltered run would be noise, and would also make "N of M" stop meaning
    // "something was excluded".
    let (dir, db) = empty_library();
    let (_, text) = run(dir.path(), &["embed", "--db", db.to_str().unwrap()]);
    assert!(
        !text.contains(" of ") || !text.contains("pending item(s)"),
        "an unfiltered run should not print a scope line:\n{text}"
    );
}
