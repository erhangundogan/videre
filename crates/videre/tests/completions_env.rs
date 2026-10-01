//! Slice 2: library-sourced dynamic candidates under clap_complete's
//! CompleteEnv protocol. The process runs with its working directory in the
//! seeded library, exactly the default resolution the candidates use.

use std::path::PathBuf;

use common::TestLibrary;

mod common;

#[test]
fn person_candidates_come_from_the_seeded_library() {
    let lib = TestLibrary::new();
    lib.init_db();
    lib.copy_fixture("sample_with_exif.jpg", "çiçek.jpg");
    lib.conn()
        .execute_batch(
            "INSERT INTO people (name, full_name) VALUES ('ayse_demirtas', 'Ayşe Demirtaş');",
        )
        .unwrap();

    // `videre search --person <cursor>` under COMPLETE=bash: the completer
    // answers on stdout.
    let output = std::process::Command::new(env_binary())
        .current_dir(lib.context().paths.root.clone())
        .env("COMPLETE", "bash")
        .env("_CLAP_COMPLETE_INDEX", "3")
        .env("_CLAP_COMPLETE_SPACE", "false")
        .args(["videre", "--", "videre", "search", "--person", "Ay"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "status: {:?}\nstdout: {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let out = String::from_utf8_lossy(&output.stdout);
    // The value is the space-free identity key: bash inserts a completion
    // unquoted, so a display name with a space would split the line.
    assert!(out.lines().any(|l| l.starts_with("ayse_demirtas")), "{out}");
    assert!(
        out.lines().all(|l| !l.starts_with("Ayşe")),
        "the display name must not be a completion value: {out}"
    );
}

#[test]
fn person_candidates_honor_an_invocation_library_flag() {
    let lib = TestLibrary::new();
    lib.init_db();
    lib.copy_fixture("sample_with_exif.jpg", "çiçek.jpg");
    lib.conn()
        .execute_batch(
            "INSERT INTO people (name, full_name) VALUES ('ayse_demirtas', 'Ayşe Demirtaş');",
        )
        .unwrap();

    // The completer runs from an unrelated directory; the invocation's own
    // --library flag names the library whose people complete.
    let elsewhere = tempfile::tempdir().unwrap();
    let library_root = lib.context().paths.root.clone();
    let output = std::process::Command::new(env_binary())
        .current_dir(elsewhere.path())
        .env("COMPLETE", "bash")
        .env("_CLAP_COMPLETE_INDEX", "5")
        .env("_CLAP_COMPLETE_SPACE", "false")
        .args([
            "videre",
            "--",
            "videre",
            "--library",
            library_root.to_string_lossy().as_ref(),
            "search",
            "--person",
            "ay",
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let out = String::from_utf8_lossy(&output.stdout);
    assert!(out.contains("ayse_demirtas"), "{out}");
}

#[test]
fn no_library_yields_no_candidates_and_a_zero_exit() {
    let dir = tempfile::tempdir().unwrap();
    let output = std::process::Command::new(env_binary())
        .current_dir(dir.path())
        .env("COMPLETE", "bash")
        .env("_CLAP_COMPLETE_INDEX", "3")
        .env("_CLAP_COMPLETE_SPACE", "false")
        .args(["videre", "--", "videre", "search", "--person", "Ay"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(
        output.stdout.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

fn env_binary() -> std::path::PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_videre"))
}

#[test]
fn config_set_model_completes_the_provided_models() {
    let lib = TestLibrary::new();
    lib.init_db();
    let output = std::process::Command::new(env_binary())
        .current_dir(lib.context().paths.root.clone())
        .env("COMPLETE", "bash")
        .env("_CLAP_COMPLETE_INDEX", "4")
        .env("_CLAP_COMPLETE_SPACE", "false")
        .args([
            "videre",
            "--",
            "videre",
            "config",
            "set",
            "model",
            "google/sig",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let out = String::from_utf8_lossy(&output.stdout);
    for model in [
        "google/siglip-base-patch16-224",
        "google/siglip2-base-patch16-384",
        "google/siglip-so400m-patch14-384",
    ] {
        assert!(
            out.lines().any(|l| l.starts_with(model)),
            "{model} in {out}"
        );
    }
}
