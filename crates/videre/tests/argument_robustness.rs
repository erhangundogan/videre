//! Nonsense arguments must produce errors, never panics.
//!
//! Three of the bugs fixed in 0.15.2 were the same shape: ordinary CLI input
//! reaching a `panic!` because a guard sat at one of several entrances to the
//! same function. `videre embed --model foo` aborted with a Rust backtrace, and
//! `videre faces --batch 0` reached `slice::chunks(0)`. Both looked like
//! unfinished software to anyone who typed a typo.
//!
//! This sweeps the argument surface so that class cannot come back quietly. It
//! asserts the weakest useful property - **no panic, ever** - rather than
//! pinning exact messages, so it stays true as wording changes.

mod common;
use common::videre_bin;
use std::fs;
use std::process::Command;
use tempfile::{tempdir, TempDir};

/// A scanned library, so commands get past their "no database" guard and the
/// arguments themselves are what gets exercised.
///
/// The one file is a **`.dng` on purpose**. This file spawns `videre embed`
/// and `videre classify` to check their argument handling, and with an
/// embeddable file present both get past their "nothing to do" branch and load
/// SigLIP - which downloads 777MB on a cold cache, breaking the invariant that
/// tests never download. `.dng` is scanned and stored like any other file, so
/// the row these tests need still exists, but it is explicitly vetoed as
/// non-embeddable (it reports `image/tiff` yet cannot be decoded), so both
/// commands return before loading any model.
///
/// This is not hypothetical tidiness. With `a.jpg` here, CI's model cache
/// picked up SigLIP weights it is not supposed to have on Linux, which made
/// `cpu_batch_matches_single_image_baseline` stop skipping and start running
/// for real - taking the Ubuntu job past 35 minutes.
fn library() -> (TempDir, std::path::PathBuf) {
    let dir = tempdir().unwrap();
    let pics = dir.path().join("pics");
    fs::create_dir_all(&pics).unwrap();
    fs::write(pics.join("a.dng"), b"a").unwrap();
    let db = dir.path().join("t.db");
    let ok = Command::new(videre_bin())
        .env("VIDERE_HOME", dir.path())
        .args(["scan"])
        .arg(&pics)
        .arg("--db")
        .arg(&db)
        .arg("--silent")
        .status()
        .unwrap();
    assert!(ok.success());
    (dir, db)
}

fn run(home: &std::path::Path, db: &std::path::Path, args: &[&str]) -> (bool, String) {
    let needs_db = matches!(
        args[0],
        "search"
            | "embed"
            | "faces"
            | "classify"
            | "dedupe"
            | "prune"
            | "stats"
            | "locations"
            | "mark"
            | "tag"
            | "export"
    );
    let mut c = Command::new(videre_bin());
    // Point the weights cache at the temp dir too. Nothing here should reach a
    // model load, and the guard below asserts it; this makes a regression cost
    // a failing test rather than silently filling the developer's real cache
    // (and, on CI, the cached artifact that decides whether the slow
    // CPU-inference test skips).
    c.env("VIDERE_HOME", home)
        .env("HF_HOME", home.join("hf"))
        .args(args);
    if needs_db && !args.contains(&"--db") {
        c.arg("--db").arg(db);
    }
    let out = c.output().unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    (out.status.success(), text)
}

fn assert_no_panic(label: &str, text: &str) {
    assert!(
        !text.contains("panicked"),
        "{label} panicked instead of erroring:\n{text}"
    );
    assert!(
        !text.contains("RUST_BACKTRACE"),
        "{label} surfaced a backtrace hint, so it panicked:\n{text}"
    );
}

#[test]
fn an_unknown_flag_is_rejected_by_every_subcommand() {
    let (dir, db) = library();
    for cmd in [
        "scan",
        "search",
        "embed",
        "faces",
        "classify",
        "dedupe",
        "prune",
        "gallery",
        "import",
        "stats",
        "locations",
        "fix-dates",
        "watch",
        "config",
        "mcp",
    ] {
        let (ok, text) = run(dir.path(), &db, &[cmd, "--definitely-not-a-flag"]);
        assert_no_panic(cmd, &text);
        assert!(!ok, "{cmd} accepted an unknown flag");
    }
}

#[test]
fn a_flag_a_command_cannot_answer_fails_to_parse() {
    // The gaps in the selection layer are deliberate: a walk has not opened the
    // file, so scan and watch cannot answer --date or --location, and embed and
    // faces refuse --person and --category because both are derived from the
    // data those commands produce.
    let (dir, db) = library();
    let pics = dir.path().join("pics");
    let p = pics.to_str().unwrap();
    for args in [
        vec!["scan", p, "--person", "Alice"],
        vec!["scan", p, "--date", "2024"],
        vec!["watch", p, "--location", "Berlin"],
        vec!["watch", p, "--category", "screenshot"],
        vec!["embed", "--person", "Alice"],
        vec!["embed", "--category", "screenshot"],
        vec!["faces", "--person", "Alice"],
        vec!["faces", "--category", "screenshot"],
        vec!["locations", "--type", "video"],
    ] {
        let label = format!("{} {}", args[0], args[args.len() - 2]);
        let (ok, text) = run(dir.path(), &db, &args);
        assert_no_panic(&label, &text);
        assert!(!ok, "{label} must not parse: the command cannot answer it");
    }
}

#[test]
fn path_backed_commands_reject_presence_filters() {
    let (dir, db) = library();
    let pics = dir.path().join("pics");
    let p = pics.to_str().unwrap();
    for args in [
        vec!["scan", p, "--missing", "gps"],
        vec!["scan", p, "--has", "date"],
        vec!["watch", p, "--missing", "gps"],
        vec!["watch", p, "--has", "date"],
        vec!["locations", "--missing", "gps"],
    ] {
        let label = args.join(" ");
        let (ok, text) = run(dir.path(), &db, &args);
        assert_no_panic(&label, &text);
        assert!(!ok, "{label} must not parse: the command cannot answer it");
    }
}

#[test]
fn row_backed_commands_accept_presence_filters() {
    let (dir, db) = library();
    for args in [
        vec!["embed", "--has", "gps", "--batch", "0"],
        vec!["faces", "--missing", "date", "--dry-run"],
        vec!["classify", "--has", "gps", "--model", "no-slash"],
        vec!["mark", "--missing", "gps", "--rating", "1", "--dry-run"],
        vec!["tag", "--missing", "gps", "--add", "needs-gps"],
        vec!["export", "--missing", "gps", "--xmp", "--dry-run"],
    ] {
        let label = args.join(" ");
        let (_, text) = run(dir.path(), &db, &args);
        assert_no_panic(&label, &text);
        assert!(
            !text.contains("unexpected argument"),
            "{label} should parse presence filters before reaching command validation:\n{text}"
        );
    }
}

#[test]
fn degenerate_values_error_or_clamp_but_never_panic() {
    // `--batch 0` reached slice::chunks(0) before 0.15.2; `--model foo` reached
    // split_once('/').expect(). Both are here permanently.
    let (dir, db) = library();
    for args in [
        vec!["embed", "--batch", "0"],
        vec!["faces", "--batch", "0"],
        vec!["embed", "--batch", "18446744073709551615"],
        vec!["embed", "--model", "no-slash"],
        vec!["search", "x", "--model", "no-slash"],
        vec!["classify", "--model", "no-slash"],
        vec!["search", "x", "--top-k", "0"],
        vec!["search", "x", "--date", "not-a-date"],
        vec!["search", "x", "--sort", "bogus"],
        vec!["search", "x", "--type", "sideways"],
        vec!["search", "x", "--location", "Berlin", "--radius", "-5"],
        vec!["faces", "--eps", "-1"],
        vec!["classify", "--margin", "-5"],
    ] {
        let label = args.join(" ");
        let (_, text) = run(dir.path(), &db, &args);
        assert_no_panic(&label, &text);
    }
}

#[test]
fn conflicting_flags_are_refused_rather_than_silently_resolved() {
    let (dir, db) = library();
    let pics = dir.path().join("pics");
    let p = pics.to_str().unwrap();
    let dbs = db.to_str().unwrap();
    for args in [
        vec!["scan", p, "--db", dbs, "--output", "x.jsonl"],
        vec!["scan", p, "--output", "o.jsonl", "--retry-incomplete"],
        vec!["search", "text", "--image", "/tmp/nope.jpg"],
        vec!["search", "--date", "2024", "--after", "2020-01-01"],
        vec!["search", "x", "--radius", "5"], // --radius needs --location
    ] {
        let label = args.join(" ");
        let (ok, text) = run(dir.path(), &db, &args);
        assert_no_panic(&label, &text);
        assert!(!ok, "{label} should be refused");
    }
}

#[test]
fn hostile_strings_do_not_reach_the_database_as_sql() {
    // Every filter is a parameterised query, but this is the assertion that
    // proves it rather than assuming it. A bare `.execute` with format! would
    // pass every other test in the suite and fail this one.
    let (dir, db) = library();
    let inj = "'; DROP TABLE file_hashes; --";
    for args in [
        vec!["search", "x", "--person", inj],
        vec!["search", "x", "--category", inj],
        vec!["search", "x", "--ext", inj],
        vec!["search", "x", "--mime", inj],
        vec!["search", "x", "--path", inj],
        vec!["search", inj],
        vec!["config", "set", "model", inj],
    ] {
        let label = args.join(" ");
        let (_, text) = run(dir.path(), &db, &args);
        assert_no_panic(&label, &text);
    }

    let conn = rusqlite::Connection::open(&db).unwrap();
    let rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM file_hashes", [], |r| r.get(0))
        .unwrap();
    assert_eq!(rows, 1, "the library must survive every injection attempt");
}

#[test]
fn odd_but_harmless_input_is_tolerated() {
    let (dir, db) = library();
    let long = "x".repeat(5000);
    for args in [
        vec!["search", "x", "--person", "🙂"],
        vec!["search", "x", "--person", "\u{1b}[31mred\u{1b}[0m"],
        vec!["search", "x", "--ext", "JPG"], // case is normalised, not rejected
        vec!["search", &long],
        vec!["search", "x", "--top-k", "5", "--top-k", "9"], // last wins
        vec!["search", "x", "--type", "image", "--type", "video"], // repeatable
    ] {
        let label = format!(
            "{} {}",
            args[0],
            &args[args.len() - 1][..args[args.len() - 1].len().min(20)]
        );
        let (_, text) = run(dir.path(), &db, &args);
        assert_no_panic(&label, &text);
    }
}

#[test]
fn sweeping_the_argument_surface_downloads_no_model_weights() {
    // The guard for the trap this file fell into: `videre embed` on a library
    // with one embeddable file loads SigLIP, and loading it downloads 777MB.
    //
    // That is worth a test rather than a comment because the failure is
    // invisible locally - a developer's cache is already warm, so the download
    // never happens and nothing looks wrong. It only shows up on CI, as a
    // cached artifact that quietly changes which *other* tests skip.
    let (dir, db) = library();
    let hf = dir.path().join("hf");

    for args in [
        vec!["embed"],
        vec!["embed", "--batch", "96"],
        vec!["classify"],
        vec!["faces"],
        vec!["search", "anything"],
    ] {
        let label = args.join(" ");
        let (_, text) = run(dir.path(), &db, &args);
        assert_no_panic(&label, &text);
    }

    let downloaded: u64 = walk_size(&hf);
    assert_eq!(
        downloaded,
        0,
        "the argument sweep downloaded {downloaded} bytes of model weights into {}. \
         Tests never download: give library() a file the model-backed commands \
         will not process, rather than warming a cache as a side effect.",
        hf.display()
    );
}

/// Total bytes under `p`, 0 when it does not exist.
fn walk_size(p: &std::path::Path) -> u64 {
    if !p.exists() {
        return 0;
    }
    let mut total = 0;
    let mut stack = vec![p.to_path_buf()];
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
fn a_missing_required_value_errors_cleanly() {
    let (dir, db) = library();
    for args in [
        vec!["search"],
        vec!["config", "set", "model"],
        vec!["search", "x", "--ext"],
        vec!["search", "x", "--top-k"],
    ] {
        let label = args.join(" ");
        let (ok, text) = run(dir.path(), &db, &args);
        assert_no_panic(&label, &text);
        assert!(!ok, "{label} should be refused");
    }
}
