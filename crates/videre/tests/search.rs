mod common;

use common::TestLibrary;
use rusqlite::params;
use serde_json::Value;

/// Paths a search printed, made relative to the selected library root so the
/// assertions read the same whatever temp directory the library landed in.
fn search_rel(lib: &TestLibrary, args: &[&str]) -> Vec<String> {
    let out = lib
        .cmd()
        .arg("search")
        .args(args)
        .output()
        .expect("failed to run videre search");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let prefix = format!("{}/", lib.context().paths.root.to_string_lossy());
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| l.starts_with('/'))
        .map(|l| l.strip_prefix(&prefix).unwrap_or(l).to_string())
        .collect()
}

/// Two dated files under the library root: enough to prove a date predicate
/// narrows. Seeded directly so the dates are the fixture rather than whatever
/// mtime the filesystem gave the temp files.
fn dates_library() -> TestLibrary {
    let lib = TestLibrary::new();
    let root = lib.context().paths.root;
    let conn = lib.init_db();
    for (rel, hash, date) in [
        ("may.jpg", "h1", "2025-05-14T10:00:00"),
        ("jun.jpg", "h2", "2025-06-14T10:00:00"),
    ] {
        let path = root.join(rel);
        conn.execute(
            "INSERT INTO file_hashes (path, hash, size_bytes, modified_at, exif_date, ext, mime)
             VALUES (?1, ?2, 10, ?3, ?3, 'jpg', 'image/jpeg')",
            params![path.to_string_lossy().as_ref(), hash, date],
        )
        .unwrap();
    }
    lib
}

/// The dated fixture plus two confirmed faces, both labelled Alice.
fn people_library() -> TestLibrary {
    let lib = dates_library();
    lib.conn()
        .execute_batch(
            "INSERT INTO faces (hash, bbox, embedding, person_label, confirmed)
             VALUES ('h1','[]',x'00','alice',1), ('h2','[]',x'00','alice',1);",
        )
        .unwrap();
    lib
}

/// Mixed media, so the 0.15.0 axes have something to discriminate: two photos,
/// two videos, one HEIC, across two folders and two months.
fn mixed_media_library() -> TestLibrary {
    let lib = TestLibrary::new();
    let root = lib.context().paths.root;
    let conn = lib.init_db();
    for (rel, hash, date, ext, mime) in [
        (
            "a/photo1.jpg",
            "p1",
            "2025-05-01T10:00:00",
            "jpg",
            "image/jpeg",
        ),
        (
            "a/photo2.heic",
            "p2",
            "2025-05-02T10:00:00",
            "heic",
            "image/heic",
        ),
        (
            "b/clip1.mov",
            "v1",
            "2025-06-01T10:00:00",
            "mov",
            "video/quicktime",
        ),
        (
            "b/clip2.mp4",
            "v2",
            "2025-06-02T10:00:00",
            "mp4",
            "video/mp4",
        ),
    ] {
        let path = root.join(rel);
        conn.execute(
            "INSERT INTO file_hashes (path, hash, size_bytes, modified_at, exif_date, ext, mime)
             VALUES (?1, ?2, 10, ?3, ?3, ?4, ?5)",
            params![path.to_string_lossy().as_ref(), hash, date, ext, mime],
        )
        .unwrap();
    }
    lib
}

fn presence_library() -> TestLibrary {
    let lib = TestLibrary::new();
    let root = lib.context().paths.root;
    let conn = lib.init_db();
    let rows: [(&str, &str, Option<&str>, Option<f64>, Option<f64>); 4] = [
        (
            "presence/complete.jpg",
            "complete",
            Some("2025-05-01T10:00:00"),
            Some(52.5),
            Some(13.4),
        ),
        (
            "presence/missing-lat.jpg",
            "missing_lat",
            Some("2025-05-02T10:00:00"),
            None,
            Some(13.4),
        ),
        (
            "presence/missing-lon.jpg",
            "missing_lon",
            Some("2025-05-03T10:00:00"),
            Some(52.5),
            None,
        ),
        (
            "presence/missing-date.jpg",
            "missing_date",
            None,
            Some(52.5),
            Some(13.4),
        ),
    ];
    for (rel, hash, date, lat, lon) in rows {
        let path = root.join(rel);
        conn.execute(
            "INSERT INTO file_hashes
                 (path, hash, size_bytes, modified_at, exif_date, ext, mime, gps_lat, gps_lon)
             VALUES (?1, ?2, 10, ?3, ?3, 'jpg', 'image/jpeg', ?4, ?5)",
            params![path.to_string_lossy().as_ref(), hash, date, lat, lon],
        )
        .unwrap();
    }
    lib
}

/// Seed a place into the local geocode cache so a `--location` search resolves
/// without a network call.
fn seed_geocode(lib: &TestLibrary, query: &str, lat: f64, lon: f64) {
    lib.conn()
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS geocode_cache
                 (query TEXT PRIMARY KEY, lat REAL NOT NULL, lon REAL NOT NULL, resolved_at TEXT NOT NULL);",
        )
        .unwrap();
    lib.conn()
        .execute(
            "INSERT OR REPLACE INTO geocode_cache (query, lat, lon, resolved_at)
             VALUES (?1, ?2, ?3, '2026-01-01')",
            params![query, lat, lon],
        )
        .unwrap();
}

/// A relative `--path` filter resolves inside the selected library, not the
/// launch directory, and a search launched elsewhere never creates a library
/// where it was launched.
#[test]
fn search_path_is_relative_to_explicit_library() {
    let a = TestLibrary::new();
    let c = TestLibrary::new();
    a.copy_fixture("tiny.jpg", "Trips/a.jpg");
    a.scan();
    a.conn()
        .execute("UPDATE file_hashes SET gps_lat=NULL, gps_lon=NULL", [])
        .unwrap();
    let out = a
        .from(&c.root)
        .args(["search", "--missing", "gps", "--path", "Trips", "--json"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let doc: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(doc["count"], 1);
    assert!(!c.db().exists());
}

// Preflight check: `videre search` on a scanned-but-not-embedded library must
// fail fast with a "no embeddings" message rather than loading the model or
// silently returning zero results.
#[test]
fn text_search_errors_with_run_embed_first_when_no_embeddings_exist() {
    let lib = dates_library();

    let out = lib
        .cmd()
        .args(["search", "sunset on beach"])
        .output()
        .expect("failed to run videre search");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("no embeddings"), "{stderr}");
    assert!(stderr.contains("videre embed --model"), "{stderr}");

    let json_out = lib
        .cmd()
        .args(["search", "--json", "sunset on beach"])
        .output()
        .expect("failed to run videre search --json");
    assert!(!json_out.status.success());
    let doc: Value = serde_json::from_slice(&json_out.stdout)
        .expect("stdout must be one valid JSON error object");
    let msg = doc["error"]["message"].as_str().unwrap_or_default();
    assert!(msg.contains("no embeddings"), "{doc}");
    assert!(msg.contains("videre embed --model"), "{doc}");
}

#[test]
fn location_search_returns_nearby_photos_sorted_by_distance() {
    let lib = TestLibrary::new();
    let path = lib.context().paths.root.join("a.jpg");
    lib.init_db()
        .execute(
            "INSERT INTO file_hashes (path, hash, ext, gps_lat, gps_lon)
             VALUES (?1, 'ha', 'jpg', 48.8566, 2.3522)",
            [path.to_string_lossy().as_ref()],
        )
        .unwrap();
    seed_geocode(&lib, "paris, france", 48.8566, 2.3522);

    let out = lib
        .cmd()
        .args(["search", "--location", "Paris, France", "--json"])
        .output()
        .expect("failed to run videre search --location");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let doc: Value = serde_json::from_slice(&out.stdout).unwrap();
    let results = doc["results"].as_array().unwrap();
    assert_eq!(results.len(), 1);
    assert!(results[0]["distance_km"].as_f64().unwrap() < 1.0);
}

#[test]
fn location_search_excludes_photos_outside_radius() {
    let lib = TestLibrary::new();
    let path = lib.context().paths.root.join("a.jpg");
    // Tokyo, far from Paris.
    lib.init_db()
        .execute(
            "INSERT INTO file_hashes (path, hash, ext, gps_lat, gps_lon)
             VALUES (?1, 'ha', 'jpg', 35.6762, 139.6503)",
            [path.to_string_lossy().as_ref()],
        )
        .unwrap();
    seed_geocode(&lib, "paris, france", 48.8566, 2.3522);

    let out = lib
        .cmd()
        .args([
            "search",
            "--location",
            "Paris, France",
            "--radius",
            "50",
            "--json",
        ])
        .output()
        .expect("failed to run videre search --location");
    assert!(out.status.success());
    let doc: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(doc["results"].as_array().unwrap().len(), 0);
}

#[test]
fn location_and_radius_conflict_with_other_search_modes() {
    // A fresh library with no database: both requests fail before any work,
    // and --radius without --location is rejected by the parser outright.
    let lib = TestLibrary::new();
    let out = lib
        .cmd()
        .args([
            "search",
            "--location",
            "Berlin, Germany",
            "--person",
            "Alice",
        ])
        .output()
        .expect("failed to run videre search");
    assert!(!out.status.success());

    let out = lib
        .cmd()
        .args(["search", "--radius", "10"])
        .output()
        .expect("failed to run videre search");
    assert!(
        !out.status.success(),
        "--radius without --location must be rejected"
    );
}

#[test]
fn location_search_truncates_to_top_k_closest() {
    let lib = TestLibrary::new();
    let root = lib.context().paths.root;
    let conn = lib.init_db();
    // Three distinct photos at increasing distance from the geocoded "Paris,
    // France" center (48.8566, 2.3522): a closest, c furthest, all in radius.
    for (rel, hash, lat) in [
        ("a.jpg", "ha", 48.8566),
        ("b.jpg", "hb", 48.9000),
        ("c.jpg", "hc", 49.0000),
    ] {
        let path = root.join(rel);
        conn.execute(
            "INSERT INTO file_hashes (path, hash, ext, gps_lat, gps_lon)
             VALUES (?1, ?2, 'jpg', ?3, 2.3522)",
            params![path.to_string_lossy().as_ref(), hash, lat],
        )
        .unwrap();
    }
    seed_geocode(&lib, "paris, france", 48.8566, 2.3522);

    let out = lib
        .cmd()
        .args(["search", "--location", "Paris, France", "-k", "2", "--json"])
        .output()
        .expect("failed to run videre search --location");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let doc: Value = serde_json::from_slice(&out.stdout).unwrap();
    let results = doc["results"].as_array().unwrap();
    assert_eq!(
        results.len(),
        2,
        "-k 2 must truncate 3 in-radius matches down to 2"
    );
    assert!(
        results[0]["path"].as_str().unwrap().ends_with("a.jpg"),
        "{doc}"
    );
    assert!(
        results[1]["path"].as_str().unwrap().ends_with("b.jpg"),
        "{doc}"
    );
}

#[test]
fn date_filter_narrows_results() {
    let lib = dates_library();
    let out = lib
        .cmd()
        .args(["search", "--date", "2025-05", "--json"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let doc: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        doc["count"], 1,
        "only the May 2025 file should match: {doc}"
    );
    assert!(doc["results"][0]["path"]
        .as_str()
        .unwrap()
        .ends_with("may.jpg"));
}

#[test]
fn filters_compose_and_narrow_further() {
    let lib = dates_library();
    let out = lib
        .cmd()
        .args([
            "search",
            "--category",
            "document",
            "--date",
            "2025-05",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let doc: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(doc["count"], 0, "no document in May 2025 in the fixture");
}

#[test]
fn person_and_date_compose() {
    let lib = people_library();
    let alone = lib
        .cmd()
        .args(["search", "--person", "Alice", "--json"])
        .output()
        .unwrap();
    let alone: Value = serde_json::from_slice(&alone.stdout).unwrap();
    assert_eq!(alone["count"], 2, "{alone}");

    let composed = lib
        .cmd()
        .args(["search", "--person", "Alice", "--date", "2025-05", "--json"])
        .output()
        .unwrap();
    let composed: Value = serde_json::from_slice(&composed.stdout).unwrap();
    assert_eq!(
        composed["count"], 1,
        "the date must narrow Alice: {composed}"
    );
}

#[test]
fn top_k_now_applies_to_person_search() {
    let lib = people_library();
    let out = lib
        .cmd()
        .args(["search", "--person", "Alice", "-k", "1", "--json"])
        .output()
        .unwrap();
    let doc: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(doc["results"].as_array().unwrap().len(), 1, "{doc}");
}

#[test]
fn hits_carry_the_effective_date() {
    let lib = dates_library();
    let out = lib
        .cmd()
        .args(["search", "--date", "2025-05", "--json"])
        .output()
        .unwrap();
    let doc: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        doc["results"][0]["date"], "2025-05-14T10:00:00",
        "every hit should report its effective date: {doc}"
    );
}

#[test]
fn explicit_sort_reorders_and_scores_prepend_the_primary_key() {
    let lib = dates_library();
    let newest = lib
        .cmd()
        .args(["search", "--date", "2025"])
        .output()
        .unwrap();
    let newest = String::from_utf8_lossy(&newest.stdout);
    let newest: Vec<&str> = newest.lines().collect();
    assert_eq!(newest.len(), 2);
    assert!(
        newest[0].ends_with("jun.jpg"),
        "date descending is the default: {newest:?}"
    );
    assert!(newest[1].ends_with("may.jpg"), "{newest:?}");

    let oldest = lib
        .cmd()
        .args(["search", "--date", "2025", "--sort", "date:asc", "--scores"])
        .output()
        .unwrap();
    let oldest = String::from_utf8_lossy(&oldest.stdout);
    let oldest: Vec<&str> = oldest.lines().collect();
    assert_eq!(oldest.len(), 2);
    assert!(
        oldest[0].starts_with("2025-05-14T10:00:00\t") && oldest[0].ends_with("may.jpg"),
        "--scores prepends the primary sort key, here the date: {oldest:?}"
    );
    assert!(
        oldest[1].starts_with("2025-06-14T10:00:00\t") && oldest[1].ends_with("jun.jpg"),
        "{oldest:?}"
    );
}

#[test]
fn sort_distance_without_location_is_rejected() {
    let lib = dates_library();
    let out = lib
        .cmd()
        .args(["search", "--sort", "distance"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("--location"),
        "the error must say what is missing: {err}"
    );

    let json = lib
        .cmd()
        .args(["search", "--sort", "distance", "--json"])
        .output()
        .unwrap();
    assert!(!json.status.success());
    let doc: Value = serde_json::from_slice(&json.stdout).unwrap();
    assert!(
        doc["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("--location"),
        "{doc}"
    );
}

#[test]
fn sort_relevance_without_a_query_is_rejected() {
    let lib = dates_library();
    let out = lib
        .cmd()
        .args(["search", "--date", "2025", "--sort", "relevance"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("--image"), "{err}");
}

#[test]
fn a_bad_sort_spec_fails_before_any_query_work() {
    let lib = dates_library();
    let out = lib
        .cmd()
        .args(["search", "--date", "2025", "--sort", "bogus"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("relevance"), "{err}");
}

#[test]
fn a_bad_date_is_rejected_with_the_accepted_forms() {
    let lib = dates_library();
    let out = lib
        .cmd()
        .args(["search", "--date", "May 2025"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("YYYY"), "{err}");
}

/// Truncation must be visible. A filter-only query has no ranker, so `-k` cuts
/// an arbitrary slice of a larger set; without a count the user reads the short
/// list as the whole answer.
#[test]
fn truncated_results_report_the_total_on_stderr_and_in_json() {
    let lib = dates_library();

    let out = lib
        .cmd()
        .args(["search", "--date", "2025", "-k", "1"])
        .output()
        .expect("failed to run videre search");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(stdout.lines().count(), 1, "one result was requested");
    assert!(
        stderr.contains("showing 1 of 2"),
        "the dropped result must be reported: {stderr}"
    );
    assert!(
        !stdout.contains("showing"),
        "the notice belongs on stderr so a piped stdout stays a bare path list"
    );

    let full = lib
        .cmd()
        .args(["search", "--date", "2025", "-k", "50"])
        .output()
        .unwrap();
    let full_err = String::from_utf8_lossy(&full.stderr);
    assert!(
        !full_err.contains("showing"),
        "no notice when nothing was truncated: {full_err}"
    );

    let js = lib
        .cmd()
        .args(["search", "--date", "2025", "-k", "1", "--json"])
        .output()
        .unwrap();
    let doc: Value = serde_json::from_slice(&js.stdout).unwrap();
    assert_eq!(doc["count"], 1);
    assert_eq!(doc["total_matches"], 2);
    assert!(
        String::from_utf8_lossy(&js.stderr).is_empty(),
        "json mode keeps stderr clean for agents"
    );
}

#[test]
fn search_filters_by_missing_gps() {
    let lib = presence_library();
    let mut got = search_rel(&lib, &["--missing", "gps"]);
    got.sort();
    assert_eq!(
        got,
        vec![
            "presence/missing-lat.jpg".to_string(),
            "presence/missing-lon.jpg".to_string()
        ]
    );

    let out = lib
        .cmd()
        .args(["search", "--missing", "gps", "--json"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(json["query"]["kind"], "filter");
    assert!(json["query"]["value"]
        .as_str()
        .unwrap()
        .contains("missing=gps"));
}

#[test]
fn search_accepts_comma_separated_presence_fields() {
    let lib = presence_library();
    let got = search_rel(&lib, &["--has", "gps,date"]);
    assert_eq!(got, vec!["presence/complete.jpg".to_string()]);
}

#[test]
fn mime_selects_one_exact_type() {
    let lib = mixed_media_library();
    let got = search_rel(&lib, &["--mime", "video/quicktime"]);
    assert_eq!(
        got,
        vec!["b/clip1.mov"],
        "an exact mime must not match its neighbours"
    );

    let got = search_rel(&lib, &["--mime", "image/heic"]);
    assert_eq!(got, vec!["a/photo2.heic"]);
}

#[test]
fn mime_is_repeatable_and_comma_separated() {
    let lib = mixed_media_library();
    let a = search_rel(&lib, &["--mime", "video/quicktime,video/mp4"]);
    let b = search_rel(&lib, &["--mime", "video/quicktime", "--mime", "video/mp4"]);
    assert_eq!(a.len(), 2);
    assert_eq!(a, b, "a comma list and repeated flags are the same request");
}

#[test]
fn type_covers_a_family_that_no_single_extension_does() {
    let lib = mixed_media_library();
    let vids = search_rel(&lib, &["--type", "video"]);
    assert_eq!(vids.len(), 2, "mov and mp4 are both video");
    let imgs = search_rel(&lib, &["--type", "image"]);
    assert_eq!(imgs.len(), 2, "jpeg and heic are both image");
}

#[test]
fn ext_is_narrower_than_type() {
    let lib = mixed_media_library();
    assert_eq!(search_rel(&lib, &["--ext", "mov"]), vec!["b/clip1.mov"]);
    assert_eq!(search_rel(&lib, &["--ext", "mov,mp4"]).len(), 2);
}

#[test]
fn path_restricts_to_a_subtree() {
    let lib = mixed_media_library();
    let got = search_rel(&lib, &["--path", "b"]);
    assert_eq!(got.len(), 2, "only the b subtree");
    assert!(got.iter().all(|p| p.starts_with("b/")));
}

#[test]
fn the_new_axes_compose_with_the_old_ones() {
    let lib = mixed_media_library();
    assert_eq!(
        search_rel(&lib, &["--type", "video", "--date", "2025-06"]).len(),
        2
    );
    assert!(search_rel(&lib, &["--type", "video", "--date", "2025-05"]).is_empty());
    assert_eq!(
        search_rel(&lib, &["--type", "video", "--ext", "mov", "--path", "b"]),
        vec!["b/clip1.mov"]
    );
    assert!(search_rel(&lib, &["--type", "image", "--ext", "mov"]).is_empty());
}

#[test]
fn a_media_filter_reports_its_total_in_json() {
    let lib = mixed_media_library();
    let out = lib
        .cmd()
        .args(["search", "--type", "video", "--json", "-k", "1"])
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).expect("json output must parse");
    assert_eq!(v["total_matches"], 2, "total is before truncation");
    assert_eq!(v["results"].as_array().unwrap().len(), 1, "-k truncates");
}

#[test]
fn a_media_filter_is_named_rather_than_mislabeled_as_a_date_query() {
    let lib = mixed_media_library();
    let out = lib
        .cmd()
        .args(["search", "--type", "video", "--json"])
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_ne!(
        v["query"]["kind"], "date",
        "a --type query is not a date query: {}",
        v["query"]
    );
}

/// A search with neither a ranking query nor any filter must refuse, and its
/// message must name every filter it accepts, including the mark/tag ones that
/// were folded in when search moved onto the shared row-selection assembler.
/// Before that, the hard-coded message omitted them.
#[test]
fn empty_selection_error_names_the_mark_and_tag_filters() {
    let lib = dates_library();
    let out = lib.cmd().arg("search").output().unwrap();
    assert!(
        !out.status.success(),
        "a search with no query and no filter must error"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    for flag in ["--rating", "--pick", "--label", "--like", "--tag"] {
        assert!(
            stderr.contains(flag),
            "the empty-selection message must name {flag}: {stderr}"
        );
    }
}

/// The mark and tag filters narrow `search` end-to-end, just like every other
/// predicate: `--label` returns only the labelled file, and `--tag` intersected
/// with `--rating` returns a file only when it satisfies both.
#[test]
fn mark_and_tag_filters_narrow_search() {
    let lib = dates_library();
    let conn = lib.conn();
    videre_core::marks::ensure_marks_table(&conn).unwrap();
    videre_core::marks::set(
        &conn,
        &["h1".to_string()],
        &videre_core::marks::change_from_parts(Some(5), None, Some("Green"), None),
    )
    .unwrap();
    videre_core::tags::ensure_photo_tags_table(&conn).unwrap();
    videre_core::tags::set_tags(&conn, &["h1".to_string()], &["beach".to_string()]).unwrap();
    drop(conn);

    // --label alone returns only the labelled file (h1 -> may.jpg).
    assert_eq!(search_rel(&lib, &["--label", "Green"]), vec!["may.jpg"]);

    // --tag intersected with --rating still returns h1, which has both.
    assert_eq!(
        search_rel(&lib, &["--tag", "beach", "--rating", "5"]),
        vec!["may.jpg"]
    );

    // A tag no file carries intersects to nothing, even paired with a rating
    // some file meets.
    assert!(search_rel(&lib, &["--rating", "1", "--tag", "sea"]).is_empty());
}
