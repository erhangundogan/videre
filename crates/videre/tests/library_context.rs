//! Tests for `TestLibrary`, the owned per-child temporary library used by the
//! directory-local-libraries refactor.
//!
//! These cover CLI selection and isolation without mutating the test process's
//! own directory or environment.

mod common;

use std::path::Path;

fn command(root: &Path, home: &Path) -> std::process::Command {
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_videre"));
    command
        .current_dir(root)
        .env("HOME", home)
        .env("HF_HOME", home.join(".cache/huggingface"))
        .env_remove("VIDERE_HOME");
    command
}

#[test]
fn child_context_is_explicit_without_mutating_parent_environment() {
    let before = std::env::current_dir().unwrap();
    let a = common::TestLibrary::new();
    let b = common::TestLibrary::new();
    let cmd = a.from(&b.root);
    assert_eq!(cmd.get_current_dir(), Some(b.root.as_path()));
    let args: Vec<_> = cmd.get_args().collect();
    assert_eq!(args[0], std::ffi::OsStr::new("--library"));
    assert_eq!(args[1], a.root.as_os_str());
    let envs: Vec<_> = cmd.get_envs().collect();
    assert!(envs.contains(&(std::ffi::OsStr::new("HOME"), Some(a.home.as_os_str()))));
    assert!(envs.contains(&(
        std::ffi::OsStr::new("HF_HOME"),
        Some(a.home.join(".cache/huggingface").as_os_str())
    )));
    assert!(envs.contains(&(std::ffi::OsStr::new("VIDERE_HOME"), None)));
    assert_eq!(std::env::current_dir().unwrap(), before);
    assert!(!a.db().exists());
    assert!(!b.db().exists());
}

#[test]
fn explicit_core_libraries_do_not_create_global_state() {
    let a = common::TestLibrary::new();
    let b = common::TestLibrary::new();
    let ca = videre_core::library::LibraryContext::new(&a.root, &a.home.join(".cache")).unwrap();
    let cb = videre_core::library::LibraryContext::new(&b.root, &b.home.join(".cache")).unwrap();
    drop(videre_core::library_db::initialize(&ca).unwrap());
    drop(videre_core::library_db::initialize(&cb).unwrap());
    assert!(a.db().is_file());
    assert!(b.db().is_file());
    assert!(!a.home.join(".videre").exists());
    assert!(!b.home.join(".videre").exists());
    assert_ne!(ca.cache.thumbnails, cb.cache.thumbnails);
}

#[test]
fn explicit_scan_initializes_only_the_selected_library() {
    let a = common::TestLibrary::new();
    let c = common::TestLibrary::new();
    a.copy_fixture("tiny.jpg", "x/a.jpg");

    let out = a
        .from(&c.root)
        .args(["scan", "--silent", "--json"])
        .output()
        .unwrap();

    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(doc.is_object());
    assert!(a.db().exists());
    assert!(!c.db().exists());
    assert!(!a.home.join(".videre").exists());
    let conn = videre_core::library_db::open_existing(&a.context()).unwrap();
    let count: i64 = conn
        .query_row("SELECT count(*) FROM file_hashes", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn config_edits_the_selected_library_without_initializing_its_database() {
    let a = common::TestLibrary::new();
    let c = common::TestLibrary::new();

    let set = a
        .from(&c.root)
        .args(["config", "set", "model", "owner/local-model"])
        .output()
        .unwrap();

    assert!(
        set.status.success(),
        "{}",
        String::from_utf8_lossy(&set.stderr)
    );
    assert!(!a.db().exists());
    assert!(!c.root.join(".videre").exists());
    let shown = a.from(&c.root).arg("config").output().unwrap();
    assert!(shown.status.success());
    let stdout = String::from_utf8_lossy(&shown.stdout);
    assert!(stdout.contains(&a.root.display().to_string()), "{stdout}");
    assert!(stdout.contains("owner/local-model"), "{stdout}");
}

#[test]
fn removed_scan_storage_and_path_arguments_fail_before_state_creation() {
    for args in [
        vec!["scan", "child"],
        vec!["scan", "--path", "child"],
        vec!["scan", "--db", "other.db"],
        vec!["scan", "--output", "records.jsonl"],
        vec!["scan", "--output-sqlite", "other.db"],
    ] {
        let library = common::TestLibrary::new();
        std::fs::create_dir(library.root.join("child")).unwrap();

        let out = library.cmd().args(&args).output().unwrap();

        assert!(!out.status.success(), "obsolete arguments passed: {args:?}");
        assert!(!library.root.join(".videre").exists(), "{args:?}");
        assert!(!library.home.join(".videre").exists(), "{args:?}");
    }
}

#[test]
fn library_selector_works_at_each_global_position_and_in_equals_form() {
    let launch = common::TestLibrary::new();
    for position in 0..4 {
        let library = common::TestLibrary::new();
        let root = library.root.display().to_string();
        let args = match position {
            0 => vec!["--library", &root, "config", "set", "xmp", "file"],
            1 => vec!["config", "--library", &root, "set", "xmp", "file"],
            2 => vec!["config", "set", "--library", &root, "xmp", "file"],
            _ => vec!["config", "set", "xmp", "file", "--library", &root],
        };
        let output = launch.cmd().args(args).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(library.root.join(".videre/config.toml").exists());
        assert!(!launch.root.join(".videre").exists());
    }

    let library = common::TestLibrary::new();
    let selector = format!("--library={}", library.root.display());
    let output = launch
        .cmd()
        .args([selector.as_str(), "config"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("selected by:   --library"));

    let scan_library = common::TestLibrary::new();
    let scan_root = scan_library.root.display().to_string();
    let output = launch
        .cmd()
        .args(["scan", "--library", &scan_root, "--silent"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(scan_library.db().exists());

    let relative = launch.root.join("relative");
    std::fs::create_dir(&relative).unwrap();
    let output = launch
        .cmd()
        .args(["--library", "relative", "config"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains(
        &std::fs::canonicalize(relative)
            .unwrap()
            .display()
            .to_string()
    ));
}

#[test]
fn duplicate_empty_and_missing_library_selectors_fail_without_state() {
    let launch = common::TestLibrary::new();
    let selected = common::TestLibrary::new();
    let root = selected.root.display().to_string();
    let duplicate = launch
        .cmd()
        .args(["--library", &root, "config", "--library", &root])
        .output()
        .unwrap();
    assert!(!duplicate.status.success());
    assert!(String::from_utf8_lossy(&duplicate.stderr).contains("only once"));

    for args in [vec!["--library=", "config"], vec!["config", "--library"]] {
        let output = launch.cmd().args(args).output().unwrap();
        assert!(!output.status.success());
    }
    assert!(!launch.root.join(".videre").exists());
    assert!(!selected.root.join(".videre").exists());
}

#[test]
fn option_terminator_prevents_literal_search_text_from_becoming_a_selector() {
    let library = common::TestLibrary::new();
    let output = library
        .cmd()
        .args(["search", "--", "--library=literal-query"])
        .output()
        .unwrap();
    // After `--`, the token is search text, never a second --library selector,
    // so the failure is about the query (an uninitialized library here), never
    // a duplicate-selector error.
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("only once"), "{stderr}");
    assert!(!library.root.join(".videre").exists());
}

#[test]
fn help_and_version_do_not_inspect_an_invalid_library() {
    let launch = common::TestLibrary::new();
    let missing = launch.root.join("missing");
    for trailing in [vec!["--version"], vec!["scan", "--help"]] {
        let output = launch
            .cmd()
            .arg("--library")
            .arg(&missing)
            .args(trailing)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert!(!missing.exists());
    assert!(!launch.root.join(".videre").exists());
}

#[test]
fn invalid_library_root_kinds_fail_without_creating_nested_state() {
    let launch = common::TestLibrary::new();
    let missing = launch.root.join("missing");
    let file = launch.root.join("file.jpg");
    std::fs::write(&file, b"file").unwrap();
    let state = launch.root.join("state-owner/.videre");
    std::fs::create_dir_all(&state).unwrap();

    for root in [&missing, &file, &state] {
        let output = launch
            .cmd()
            .arg("--library")
            .arg(root)
            .arg("config")
            .output()
            .unwrap();
        assert!(!output.status.success(), "{}", root.display());
    }
    assert!(!missing.exists());
    assert!(!state.join(".videre").exists());
}

#[cfg(unix)]
#[test]
fn a_non_utf8_library_selector_reaches_the_filesystem_unchanged() {
    use std::os::unix::ffi::OsStringExt;

    let launch = common::TestLibrary::new();
    let name = std::ffi::OsString::from_vec(vec![b'l', b'i', b'b', 0xff]);
    let root = launch.root.join(name);
    if std::fs::create_dir(&root).is_err() {
        // Some filesystems (APFS among them) reject a non-UTF-8 name outright;
        // there is nothing to test when the fixture cannot exist.
        return;
    }
    let output = launch
        .cmd()
        .arg("--library")
        .arg(&root)
        .args(["scan", "--silent"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(root.join(".videre/hashes.db").exists());
}

#[test]
fn parent_then_child_scans_create_independent_libraries() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let parent = temp.path().join("Photos");
    let child = parent.join("x");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&child).unwrap();
    std::fs::write(parent.join("parent.jpg"), b"parent").unwrap();
    std::fs::write(child.join("child.jpg"), b"child").unwrap();

    assert!(command(&parent, &home)
        .args(["scan", "--silent"])
        .status()
        .unwrap()
        .success());
    assert!(command(&child, &home)
        .args(["scan", "--silent"])
        .status()
        .unwrap()
        .success());

    let parent_db = rusqlite::Connection::open(parent.join(".videre/hashes.db")).unwrap();
    let child_db = rusqlite::Connection::open(child.join(".videre/hashes.db")).unwrap();
    let parent_count: i64 = parent_db
        .query_row("SELECT count(*) FROM file_hashes", [], |row| row.get(0))
        .unwrap();
    let child_count: i64 = child_db
        .query_row("SELECT count(*) FROM file_hashes", [], |row| row.get(0))
        .unwrap();
    assert_eq!(parent_count, 2);
    assert_eq!(child_count, 1);
}

#[test]
fn child_then_parent_scan_skips_child_state_and_never_adopts_it() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let parent = temp.path().join("Photos");
    let child = parent.join("x/y");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&child).unwrap();
    std::fs::write(parent.join("parent.jpg"), b"parent").unwrap();
    std::fs::write(child.join("child.jpg"), b"child").unwrap();

    assert!(command(&child, &home)
        .args(["scan", "--silent"])
        .status()
        .unwrap()
        .success());
    let child_db_path = child.join(".videre/hashes.db");
    let child_config_path = child.join(".videre/config.toml");
    let child_conn = rusqlite::Connection::open(&child_db_path).unwrap();
    let child_hash: String = child_conn
        .query_row("SELECT hash FROM file_hashes", [], |row| row.get(0))
        .unwrap();
    videre_core::marks::set(
        &child_conn,
        std::slice::from_ref(&child_hash),
        &videre_core::marks::change_from_parts(Some(5), None, None, None),
    )
    .unwrap();
    drop(child_conn);
    std::fs::write(child.join(".videre/private.jpg"), b"state media").unwrap();
    let config_before = std::fs::read(&child_config_path).unwrap();

    assert!(command(&parent, &home)
        .args(["scan", "--silent"])
        .status()
        .unwrap()
        .success());

    let parent_conn = rusqlite::Connection::open(parent.join(".videre/hashes.db")).unwrap();
    let paths: Vec<String> = parent_conn
        .prepare("SELECT path FROM file_hashes ORDER BY path")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(paths.len(), 2);
    assert!(paths.iter().all(|path| !path.contains("/.videre/")));
    let child_conn = rusqlite::Connection::open(&child_db_path).unwrap();
    assert_eq!(
        videre_core::marks::get(&child_conn, &child_hash)
            .unwrap()
            .rating,
        Some(5)
    );
    assert_eq!(std::fs::read(child_config_path).unwrap(), config_before);
}

#[test]
fn home_and_its_photos_child_can_be_scanned_independently() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("Home");
    let photos = home.join("Photos");
    std::fs::create_dir_all(&photos).unwrap();
    std::fs::write(home.join("home.jpg"), b"home").unwrap();
    std::fs::write(photos.join("photo.jpg"), b"photo").unwrap();

    assert!(command(&photos, &home)
        .args(["scan", "--silent"])
        .status()
        .unwrap()
        .success());
    assert!(command(&home, &home)
        .args(["scan", "--silent"])
        .status()
        .unwrap()
        .success());
    assert!(home.join(".videre/hashes.db").exists());
    assert!(photos.join(".videre/hashes.db").exists());
}

/// The removed `--db` override must be rejected by the parser for every
/// subcommand, and rejection must happen before any command opens or creates
/// state. The list is the full clap inventory, so a newly added command that
/// reintroduced a `--db` flag would fail here rather than silently resurrect
/// global resolution.
#[test]
fn every_command_rejects_database_overrides_without_initializing_state() {
    let commands = [
        "scan",
        "watch",
        "search",
        "embed",
        "faces",
        "classify",
        "locations",
        "fix-dates",
        "prune",
        "stats",
        "mcp",
        "dedupe",
        "gallery",
        "mark",
        "export",
        "tag",
        "import",
        "config",
    ];
    for name in commands {
        let library = common::TestLibrary::new();
        let out = library
            .cmd()
            .args([name, "--db", "foreign.db"])
            .output()
            .unwrap();
        assert_eq!(
            out.status.code(),
            Some(2),
            "{name} did not reject --db during parsing"
        );
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("unexpected argument '--db'"),
            "{name} stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(!library.root.join(".videre").exists());
        assert!(!library.home.join(".videre").exists());
    }
}
