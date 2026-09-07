mod common;
use common::{shared_cache_guard, TestLibrary};

/// An initialized but empty library (no scanned files): faces has nothing to
/// detect, so it must return before loading any model.
fn empty_library() -> TestLibrary {
    let lib = TestLibrary::new();
    lib.init_db();
    lib
}

#[test]
fn exits_zero_on_empty_db() {
    let _serial = shared_cache_guard();
    let lib = empty_library();
    let status = lib
        .cmd()
        .args(["faces", "--silent"])
        .status()
        .expect("failed to run videre faces");
    assert!(status.success());
}

#[test]
fn creates_faces_table() {
    let _serial = shared_cache_guard();
    let lib = empty_library();
    lib.cmd().args(["faces", "--silent"]).status().unwrap();
    let n: i64 = lib
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='faces'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1);
}

/// Regression guard: `videre faces` on a library with nothing to process must
/// not touch the model cache.
///
/// `commands/faces.rs` returns at the `to_process.is_empty()` branch before
/// loading anything, so this stays ungated. Moving the model load earlier would
/// silently turn this into a ~200 MB download on every cold CI runner. The
/// library's private `HF_HOME` (under its home) is where any real download
/// would land and be visible.
#[test]
fn faces_on_an_empty_db_touches_no_model_cache() {
    let lib = empty_library();

    let status = lib
        .cmd()
        .args(["faces", "--silent"])
        .status()
        .expect("failed to run videre faces");
    assert!(status.success());

    let hf = lib.home.join(".cache/huggingface");
    let mut found = Vec::new();
    let mut stack = vec![hf];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.filter_map(Result::ok) {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                found.push(p);
            }
        }
    }
    assert!(
        found.is_empty(),
        "videre faces downloaded into a cold cache on an empty db: {found:?}"
    );
}
