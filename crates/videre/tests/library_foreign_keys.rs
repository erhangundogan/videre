mod common;

use common::TestLibrary;
use std::process::Stdio;
use std::time::Duration;

#[test]
fn library_upgrade_reports_repairs_once_without_polluting_json() {
    let lib = TestLibrary::new();
    drop(lib.init_db());

    // A legacy connection could write these rows before enforcement existed.
    let conn = rusqlite::Connection::open(&lib.context().paths.db).unwrap();
    conn.execute_batch(
        "PRAGMA foreign_keys = OFF;
         INSERT INTO faces
             (id, hash, bbox, embedding, cluster_id, person_label, confirmed, is_primary)
         VALUES (1, 'legacy-hash', '0,0,50,50', X'0000', 7, 'İsıl Legacy', 1, 1);
         INSERT INTO face_learning_questions
             (status, target_identity, profile_id, model_kind,
              representative_face_id, cluster_id, evidence_revision, evidence_json)
         VALUES ('pending', 'İsıl Legacy', 1, 'logistic', 1, 7, 'old-revision', '{}');
         PRAGMA user_version = 1;",
    )
    .unwrap();
    drop(conn);

    let first = lib.cmd().args(["stats", "--json"]).output().unwrap();
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let _: serde_json::Value = serde_json::from_slice(&first.stdout).unwrap();
    let stderr = String::from_utf8_lossy(&first.stderr);
    assert_eq!(
        stderr.matches("unassigned 1 face(s)").count(),
        1,
        "{stderr}"
    );
    assert!(stderr.contains("İsıl Legacy"), "{stderr}");
    assert!(
        stderr.contains("superseded 1 pending question(s)"),
        "{stderr}"
    );
    assert!(!String::from_utf8_lossy(&first.stdout).contains("İsıl Legacy"));
    // The repair changed data, so it is kept in the command's primary log at
    // the default level, not only shown once on the terminal.
    let log = std::fs::read_to_string(lib.context().paths.state.join("logs/stats.log")).unwrap();
    assert!(
        log.lines()
            .filter_map(videre_core::error_log::parse_line)
            .any(|l| l.level == videre_core::error_log::LineLevel::Warn
                && l.message.contains("İsıl Legacy")),
        "{log}"
    );

    let second = lib.cmd().args(["stats", "--json"]).output().unwrap();
    assert!(second.status.success());
    let _: serde_json::Value = serde_json::from_slice(&second.stdout).unwrap();
    let stderr = String::from_utf8_lossy(&second.stderr);
    assert!(!stderr.contains("unassigned"), "{stderr}");
    assert!(!stderr.contains("superseded"), "{stderr}");

    let conn = lib.conn();
    let violations: i64 = conn
        .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(violations, 0);
}

#[test]
fn gallery_upgrades_legacy_schema_before_serving() {
    let lib = TestLibrary::new();
    let conn = lib.init_db();
    conn.execute_batch(
        "PRAGMA foreign_keys = OFF;
         DROP TABLE faces;
         CREATE TABLE faces (
             id INTEGER PRIMARY KEY,
             hash TEXT NOT NULL,
             bbox TEXT NOT NULL,
             embedding BLOB NOT NULL,
             person_label TEXT
         );
         PRAGMA user_version = 1;",
    )
    .unwrap();
    drop(conn);

    let mut child = lib
        .cmd()
        .args(["gallery", "--port", "0"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut upgraded = false;
    for _ in 0..40 {
        std::thread::sleep(Duration::from_millis(50));
        let conn = rusqlite::Connection::open(&lib.context().paths.db).unwrap();
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        if version == 2 {
            upgraded = true;
            break;
        }
        if child.try_wait().unwrap().is_some() {
            break;
        }
    }
    child.kill().ok();
    child.wait().ok();
    assert!(upgraded, "gallery must run the v1-to-v2 upgrade on startup");

    let conn = lib.conn();
    let person_key_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_foreign_key_list('faces')
             WHERE \"from\" = 'person_label' AND \"table\" = 'people'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(person_key_count, 1);
}
