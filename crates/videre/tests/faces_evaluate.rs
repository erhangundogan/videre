mod common;

use common::{stderr_without_library_noise, TestLibrary};
use rusqlite::types::Value;
use videre_ml::evaluation::BaselineEvaluation;

fn embedding_at(angle: f32) -> Vec<u8> {
    let radians = angle.to_radians();
    [radians.cos(), radians.sin()]
        .into_iter()
        .flat_map(|value| half::f16::from_f32(value).to_le_bytes())
        .collect()
}

fn seed_faces(labels: &[(f32, Option<&str>)]) -> TestLibrary {
    let library = TestLibrary::new();
    let conn = library.init_db();
    for (_, label) in labels {
        if let Some(label) = label {
            conn.execute(
                "INSERT OR IGNORE INTO people (name, full_name) VALUES (?1, ?1)",
                [label],
            )
            .unwrap();
        }
    }
    for (index, (angle, label)) in labels.iter().enumerate() {
        let id = index as i64 + 1;
        conn.execute(
            "INSERT INTO faces (
                id, hash, bbox, embedding, cluster_id, person_label, confirmed, blur
             ) VALUES (?1, ?2, '0,0,100,100', ?3, ?4, ?5, ?6, 100.0)",
            rusqlite::params![
                id,
                format!("private-hash-{id}"),
                embedding_at(*angle),
                Some(id + 100),
                label,
                i64::from(label.is_some()),
            ],
        )
        .unwrap();
    }
    drop(conn);
    library
}

fn logical_dump(library: &TestLibrary) -> Vec<(String, Vec<Vec<Value>>)> {
    let conn = library.conn();
    let tables: Vec<String> = conn
        .prepare(
            "SELECT name FROM sqlite_master
             WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
        )
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    tables
        .into_iter()
        .map(|table| {
            let escaped = table.replace('"', "\"\"");
            let mut statement = conn
                .prepare(&format!("SELECT * FROM \"{escaped}\" ORDER BY rowid"))
                .unwrap();
            let columns = statement.column_count();
            let rows = statement
                .query_map([], |row| {
                    (0..columns)
                        .map(|index| row.get::<_, Value>(index))
                        .collect::<rusqlite::Result<Vec<_>>>()
                })
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            (table, rows)
        })
        .collect()
}

fn evaluation_args() -> [&'static str; 19] {
    [
        "faces",
        "--evaluate",
        "--eps",
        "0.10",
        "--min-cluster-size",
        "2",
        "--merge-sim",
        "1",
        "--min-face-size",
        "0",
        "--max-generic-sim",
        "1",
        "--max-landmark-error",
        "1000",
        "--min-blur",
        "0",
        "--attach-sim",
        "1",
        "--json",
    ]
}

#[test]
fn json_evaluation_is_model_free_private_and_database_read_only() {
    let library = seed_faces(&[
        (0.0, Some("alpha")),
        (3.0, Some("alpha")),
        (90.0, Some("beta")),
        (93.0, Some("beta")),
    ]);
    let before = logical_dump(&library);

    let output = library.cmd().args(evaluation_args()).output().unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        stderr_without_library_noise(&String::from_utf8_lossy(&output.stderr)),
        ""
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let report: BaselineEvaluation = serde_json::from_str(&stdout).unwrap();
    assert_eq!(report.protocol_version, 1);
    assert_eq!(report.labeled_faces, 4);
    assert!(report.evaluation.suggestions.is_none());
    for private in [
        "alpha",
        "beta",
        "private-hash",
        library.root.to_str().unwrap(),
    ] {
        assert!(!stdout.contains(private), "JSON leaked {private}: {stdout}");
    }
    assert_eq!(logical_dump(&library), before);
}

#[test]
fn human_evaluation_names_every_quality_measure() {
    let library = seed_faces(&[
        (0.0, Some("alpha")),
        (3.0, Some("alpha")),
        (90.0, Some("beta")),
        (93.0, Some("beta")),
    ]);
    let mut arguments = evaluation_args().to_vec();
    arguments.pop();
    let output = library.cmd().args(arguments).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    for label in [
        "Labeled faces",
        "Identities",
        "Pair precision",
        "Pair recall",
        "Mixed clusters",
        "Fragmented identities",
        "Unassigned rate",
    ] {
        assert!(stdout.contains(label), "missing {label}: {stdout}");
    }
}

#[test]
fn no_labels_is_an_actionable_error() {
    let library = seed_faces(&[(0.0, None)]);
    let output = library.cmd().args(evaluation_args()).output().unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no confirmed labeled faces"), "{stderr}");
}

#[test]
fn undefined_pair_metrics_are_json_null() {
    let library = seed_faces(&[(0.0, Some("alpha"))]);
    let output = library.cmd().args(evaluation_args()).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(value["evaluation"]["clustering"]["pair_precision"].is_null());
    assert!(value["evaluation"]["clustering"]["pair_recall"].is_null());
}
