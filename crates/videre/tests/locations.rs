mod common;

use common::TestLibrary;

/// A library seeded directly with GPS rows, each path under the canonical root
/// so the row-containment guard accepts the database.
fn library_with_gps(rows: &[(&str, &str, Option<(f64, f64)>)]) -> TestLibrary {
    let lib = TestLibrary::new();
    let root = lib.context().paths.root;
    let conn = lib.init_db();
    for (rel, hash, gps) in rows {
        let path = root.join(rel);
        match gps {
            Some((lat, lon)) => conn
                .execute(
                    "INSERT INTO file_hashes (path, hash, ext, gps_lat, gps_lon)
                     VALUES (?1, ?2, 'jpg', ?3, ?4)",
                    rusqlite::params![path.to_string_lossy().as_ref(), hash, lat, lon],
                )
                .unwrap(),
            None => conn
                .execute(
                    "INSERT INTO file_hashes (path, hash, ext) VALUES (?1, ?2, 'jpg')",
                    rusqlite::params![path.to_string_lossy().as_ref(), hash],
                )
                .unwrap(),
        };
    }
    lib
}

/// The stored `location_cluster_id` for one in-root relative path.
fn cluster_id(lib: &TestLibrary, rel: &str) -> Option<i64> {
    let path = lib.context().paths.root.join(rel);
    lib.conn()
        .query_row(
            "SELECT location_cluster_id FROM file_hashes WHERE path = ?1",
            [path.to_string_lossy().as_ref()],
            |r| r.get(0),
        )
        .unwrap()
}

#[test]
fn clusters_nearby_gps_rows_and_prints_json_summary() {
    let lib = library_with_gps(&[
        ("a.jpg", "ha", Some((48.8566, 2.3522))),
        ("b.jpg", "hb", Some((48.8606, 2.3376))),
        ("c.jpg", "hc", Some((51.5074, -0.1278))),
    ]);

    let out = lib
        .cmd()
        .args(["locations", "--radius", "50", "--json"])
        .output()
        .expect("failed to run videre locations");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(doc["schema_version"], 1);
    let clusters = doc["clusters"].as_array().unwrap();
    assert_eq!(clusters.len(), 2, "{doc}");
    let mut counts: Vec<i64> = clusters
        .iter()
        .map(|c| c["photo_count"].as_i64().unwrap())
        .collect();
    counts.sort();
    assert_eq!(counts, vec![1, 2]);
}

#[test]
fn zero_gps_rows_prints_empty_and_exits_zero() {
    let lib = library_with_gps(&[("a.jpg", "ha", None)]);
    let out = lib
        .cmd()
        .args(["locations", "--json"])
        .output()
        .expect("failed to run videre locations");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(doc["clusters"].as_array().unwrap().len(), 0);
}

#[test]
fn geojson_output_is_a_feature_collection_with_lon_lat_order() {
    let lib = library_with_gps(&[("a.jpg", "ha", Some((48.8566, 2.3522)))]);
    let out = lib
        .cmd()
        .args(["locations", "--geojson"])
        .output()
        .expect("failed to run videre locations");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(doc["type"], "FeatureCollection");
    let features = doc["features"].as_array().unwrap();
    assert_eq!(features.len(), 1);
    assert_eq!(features[0]["geometry"]["coordinates"][0], 2.3522);
    assert_eq!(features[0]["geometry"]["coordinates"][1], 48.8566);
}

#[test]
fn json_and_geojson_are_mutually_exclusive() {
    // Rejected by clap before anything opens a database.
    let lib = TestLibrary::new();
    let out = lib
        .cmd()
        .args(["locations", "--json", "--geojson"])
        .output()
        .expect("failed to run videre locations");
    assert!(!out.status.success());
}

#[test]
fn rerun_replaces_previous_clusters_not_appends() {
    let lib = library_with_gps(&[("a.jpg", "ha", Some((48.8566, 2.3522)))]);

    for _ in 0..2 {
        let status = lib
            .cmd()
            .args(["locations", "--silent"])
            .status()
            .expect("failed to run videre locations");
        assert!(status.success());
    }

    let count: i64 = lib
        .conn()
        .query_row("SELECT COUNT(*) FROM location_clusters", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1, "rerunning must replace, not accumulate, clusters");
}

#[test]
fn file_hashes_location_cluster_id_is_assigned_and_cleared_on_rerun() {
    let lib = library_with_gps(&[
        ("a.jpg", "ha", Some((48.8566, 2.3522))),
        ("b.jpg", "hb", Some((48.8606, 2.3376))),
    ]);

    let status = lib
        .cmd()
        .args(["locations", "--radius", "50", "--silent"])
        .status()
        .expect("failed to run videre locations");
    assert!(status.success());

    let cluster_a = cluster_id(&lib, "a.jpg");
    let cluster_b = cluster_id(&lib, "b.jpg");
    assert!(
        cluster_a.is_some(),
        "expected a.jpg to get a location_cluster_id"
    );
    assert_eq!(
        cluster_a, cluster_b,
        "both nearby photos must land in the same cluster"
    );

    // Remove b's GPS so a rerun reclusters it out of any cluster; the
    // NULL-clearing step must actually take effect.
    let b_path = lib.context().paths.root.join("b.jpg");
    lib.conn()
        .execute(
            "UPDATE file_hashes SET gps_lat = NULL, gps_lon = NULL WHERE path = ?1",
            [b_path.to_string_lossy().as_ref()],
        )
        .unwrap();

    let status = lib
        .cmd()
        .args(["locations", "--radius", "50", "--silent"])
        .status()
        .expect("failed to run videre locations");
    assert!(status.success());

    assert_eq!(
        cluster_id(&lib, "b.jpg"),
        None,
        "b.jpg's stale location_cluster_id must be cleared once it no longer has GPS"
    );
}
