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
    assert!(out.contains("Ayşe Demirtaş"), "{out}");
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
