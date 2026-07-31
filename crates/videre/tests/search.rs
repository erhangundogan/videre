use std::fs;
use std::process::Command;
use tempfile::tempdir;

fn videre_bin() -> std::path::PathBuf {
    let mut path = std::env::current_exe().unwrap();
    path.pop(); // deps/
    path.pop(); // debug/
    path.push("videre");
    path
}

// Preflight check: `videre search` on a scanned-but-not-embedded db must fail
// fast with a "run videre embed first" message rather than loading the SigLIP
// model or silently returning zero results.
#[test]
fn text_search_errors_with_run_embed_first_when_no_embeddings_exist() {
    let scan_dir = tempdir().unwrap();
    let out_dir = tempdir().unwrap();
    let db_path = out_dir.path().join("hashes.db");

    fs::write(scan_dir.path().join("a.jpg"), b"content").unwrap();

    let status = Command::new(videre_bin())
        .arg("scan")
        .arg("--silent")
        .arg("--output-sqlite")
        .arg(&db_path)
        .arg(scan_dir.path())
        .status()
        .expect("failed to run videre scan");
    assert!(status.success());

    let out = Command::new(videre_bin())
        .arg("search")
        .arg("--db")
        .arg(&db_path)
        .arg("sunset on beach")
        .output()
        .expect("failed to run videre search");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("run videre embed first"), "{stderr}");

    let json_out = Command::new(videre_bin())
        .arg("search")
        .arg("--db")
        .arg(&db_path)
        .arg("--json")
        .arg("sunset on beach")
        .output()
        .expect("failed to run videre search --json");
    assert!(!json_out.status.success());
    let doc: serde_json::Value = serde_json::from_slice(&json_out.stdout)
        .expect("stdout must be one valid JSON error object");
    assert!(
        doc["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("run videre embed first"),
        "{doc}"
    );
}
