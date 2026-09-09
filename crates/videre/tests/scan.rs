mod common;

use common::{permissions_are_enforced, stderr_without_library_noise, TestLibrary};
use rusqlite::OptionalExtension;

fn scan(library: &TestLibrary, args: &[&str]) -> std::process::Output {
    let mut command = library.cmd();
    command.arg("scan").args(args).output().unwrap()
}

#[test]
fn scan_writes_local_records_with_correct_hashes() {
    let library = TestLibrary::new();
    let path = library.copy_fixture("tiny.jpg", "nested/image.jpg");
    let expected = blake3::hash(&std::fs::read(&path).unwrap())
        .to_hex()
        .to_string();

    let output = scan(&library, &["--silent"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
    let conn = library.conn();
    let row: (String, String, i64, String) = conn
        .query_row(
            "SELECT path, hash, size_bytes, ext FROM file_hashes",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(
        row.0,
        library
            .context()
            .paths
            .root
            .join("nested/image.jpg")
            .display()
            .to_string()
    );
    assert_eq!(row.1, expected);
    assert_eq!(row.2, std::fs::metadata(path).unwrap().len() as i64);
    assert_eq!(row.3, "jpg");
}

#[test]
fn exif_fields_are_populated_from_a_confined_original() {
    let library = TestLibrary::new();
    library.copy_fixture("sample_with_exif.jpg", "dated.jpg");
    assert!(scan(&library, &["--silent"]).status.success());
    let row: (
        Option<String>,
        Option<f64>,
        Option<f64>,
        Option<i64>,
        Option<i64>,
    ) = library
        .conn()
        .query_row(
            "SELECT exif_date, gps_lat, gps_lon, width, height FROM file_hashes",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .unwrap();
    assert!(row.0.is_some());
    assert!(row.1.is_some());
    assert!(row.2.is_some());
    assert!(row.3.is_some());
    assert!(row.4.is_some());
}

#[test]
fn repeated_scan_upserts_instead_of_duplicating_rows() {
    let library = TestLibrary::new();
    let path = library.copy_fixture("tiny.jpg", "image.jpg");
    assert!(scan(&library, &["--silent"]).status.success());
    std::fs::write(&path, b"changed image bytes").unwrap();
    assert!(scan(&library, &["--silent"]).status.success());
    let conn = library.conn();
    let count: i64 = conn
        .query_row("SELECT count(*) FROM file_hashes", [], |row| row.get(0))
        .unwrap();
    let hash: String = conn
        .query_row("SELECT hash FROM file_hashes", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 1);
    assert_eq!(
        hash,
        blake3::hash(b"changed image bytes").to_hex().to_string()
    );
}

#[test]
fn first_scan_creates_fixed_local_state_and_config() {
    let library = TestLibrary::new();
    library.copy_fixture("tiny.jpg", "image.jpg");
    assert!(scan(&library, &["--silent"]).status.success());
    assert!(library.db().exists());
    let config = std::fs::read_to_string(library.root.join(".videre/config.toml")).unwrap();
    assert!(config.contains("db = \"hashes.db\""), "{config}");
    assert!(config.contains("jsonl = \"hashes.jsonl\""), "{config}");
    assert!(
        config.contains("default_model = \"google/siglip-base-patch16-224\""),
        "{config}"
    );
    assert!(config.contains("xmp_precedence = \"db\""), "{config}");
    assert!(config.contains("export_xmp_on_watch = false"), "{config}");
}

#[test]
fn silent_scan_keeps_both_output_streams_clean() {
    let library = TestLibrary::new();
    library.copy_fixture("tiny.jpg", "image.jpg");
    let output = scan(&library, &["--silent"]);
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert_eq!(
        stderr_without_library_noise(&String::from_utf8_lossy(&output.stderr)),
        ""
    );
}

#[test]
fn unreadable_files_are_skipped_and_reported() {
    use std::os::unix::fs::PermissionsExt;

    let library = TestLibrary::new();
    library.copy_fixture("tiny.jpg", "good.jpg");
    let unreadable = library.copy_fixture("tiny.jpg", "unreadable.jpg");
    std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o000)).unwrap();
    if !permissions_are_enforced(&unreadable) {
        return;
    }
    let output = scan(&library, &[]);
    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("1 skipped"), "{stderr}");
    let count: i64 = library
        .conn()
        .query_row("SELECT count(*) FROM file_hashes", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn json_error_shape_is_preserved_for_an_invalid_library() {
    let launch = TestLibrary::new();
    let missing = launch.root.join("missing");
    let output = launch
        .cmd()
        .arg("--library")
        .arg(&missing)
        .args(["scan", "--json"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["schema_version"], 1);
    assert!(json["error"]["message"]
        .as_str()
        .unwrap()
        .contains("does not exist"));
}

#[test]
fn json_mode_reports_the_fixed_sqlite_destination() {
    let library = TestLibrary::new();
    library.copy_fixture("tiny.jpg", "image.jpg");
    let output = scan(&library, &["--silent", "--json"]);
    assert!(output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["schema_version"], 1);
    assert_eq!(json["total_files"], 1);
    assert_eq!(json["output"]["kind"], "sqlite");
    assert_eq!(
        json["output"]["path"],
        library.context().paths.db.display().to_string()
    );
}

#[test]
fn scan_records_a_successful_pipeline_run() {
    let library = TestLibrary::new();
    library.copy_fixture("tiny.jpg", "image.jpg");
    assert!(scan(&library, &["--silent"]).status.success());
    let row: (String, String) = library
        .conn()
        .query_row(
            "SELECT command, status FROM pipeline_runs WHERE command = 'scan'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(row, ("scan".into(), "success".into()));
}

#[test]
fn retry_incomplete_skips_rows_with_a_known_type() {
    let library = TestLibrary::new();
    library.copy_fixture("tiny.jpg", "image.jpg");
    assert!(scan(&library, &["--silent"]).status.success());
    let output = scan(&library, &["--retry-incomplete"]);
    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("0 incomplete; 0 processed"), "{stderr}");
}

#[test]
fn retry_incomplete_processes_only_unknown_rows_and_new_files() {
    let library = TestLibrary::new();
    library.copy_fixture("tiny.jpg", "first.jpg");
    assert!(scan(&library, &["--silent"]).status.success());
    let conn = library.conn();
    conn.execute("UPDATE file_hashes SET mime = NULL", [])
        .unwrap();
    drop(conn);
    library.copy_fixture("tiny.jpg", "second.jpg");

    let output = scan(&library, &["--retry-incomplete", "--silent"]);
    assert!(output.status.success());
    let conn = library.conn();
    let count: i64 = conn
        .query_row("SELECT count(*) FROM file_hashes", [], |row| row.get(0))
        .unwrap();
    let nulls: i64 = conn
        .query_row(
            "SELECT count(*) FROM file_hashes WHERE mime IS NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 2);
    assert_eq!(nulls, 0);
}

#[test]
fn unidentifiable_files_receive_a_sentinel_and_are_not_retried() {
    let library = TestLibrary::new();
    let path = library.root.join("odd.jpg");
    std::fs::write(&path, b"not an image").unwrap();
    assert!(scan(&library, &["--silent"]).status.success());
    let conn = library.conn();
    let mime: Option<String> = conn
        .query_row("SELECT mime FROM file_hashes", [], |row| row.get(0))
        .optional()
        .unwrap()
        .flatten();
    assert_eq!(mime.as_deref(), Some(videre_core::mime_probe::UNKNOWN_MIME));
    drop(conn);
    let output = scan(&library, &["--retry-incomplete"]);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("0 incomplete; 0 processed"));
}

#[test]
fn similar_mode_stores_a_perceptual_hash() {
    let library = TestLibrary::new();
    library.copy_fixture("tiny.jpg", "image.jpg");
    assert!(scan(&library, &["--silent", "--similar"]).status.success());
    let phash: Option<i64> = library
        .conn()
        .query_row("SELECT phash FROM file_hashes", [], |row| row.get(0))
        .unwrap();
    assert!(phash.is_some());
}

#[test]
fn xmp_precedence_and_keywords_are_applied_from_confined_sidecars() {
    let library = TestLibrary::new();
    library.copy_fixture("tiny.jpg", "image.jpg");
    std::fs::write(
        library.root.join("image.jpg.xmp"),
        r#"<x:xmpmeta xmlns:x="adobe:ns:meta/">
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
<rdf:Description xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmp:Rating="4"
 xmlns:dc="http://purl.org/dc/elements/1.1/">
<dc:subject><rdf:Bag><rdf:li>holiday</rdf:li></rdf:Bag></dc:subject>
</rdf:Description></rdf:RDF></x:xmpmeta>"#,
    )
    .unwrap();
    assert!(scan(&library, &["--silent"]).status.success());
    let conn = library.conn();
    let hash: String = conn
        .query_row("SELECT hash FROM file_hashes", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        videre_core::marks::get(&conn, &hash).unwrap().rating,
        Some(4)
    );
    assert_eq!(
        videre_core::tags::tags_for_hash(&conn, &hash).unwrap(),
        vec!["holiday"]
    );
    videre_core::marks::set(
        &conn,
        std::slice::from_ref(&hash),
        &videre_core::marks::change_from_parts(Some(1), None, None, None),
    )
    .unwrap();
    drop(conn);

    assert!(scan(&library, &["--silent"]).status.success());
    assert_eq!(
        videre_core::marks::get(&library.conn(), &hash)
            .unwrap()
            .rating,
        Some(1)
    );
    assert!(scan(&library, &["--silent", "--xmp", "file"])
        .status
        .success());
    assert_eq!(
        videre_core::marks::get(&library.conn(), &hash)
            .unwrap()
            .rating,
        Some(4)
    );
}

#[test]
fn scan_announces_the_metadata_phase_after_hashing() {
    // After the hash bar reaches 100%, scan still reads XMP for every row in
    // the library (the whole-library reconcile), which on a large library
    // looks frozen because it ran silently. A non-silent scan must announce
    // that phase so the user knows work is still happening.
    let library = TestLibrary::new();
    library.copy_fixture("tiny.jpg", "a.jpg");
    let output = scan(&library, &[]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Reading metadata for"),
        "expected a metadata-phase message so the user knows to wait; got: {stderr}"
    );
}
