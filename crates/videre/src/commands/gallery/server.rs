//! The `videre gallery` HTTP server: the axum router, its handlers, and the
//! face-labeling API. The rendering it shares with `dedupe --html` and
//! `search --html` lives in `crate::render`; this file is the HTTP layer only.

use crate::render::*;
use axum::body::Body;
use axum::extract::{Json as AxumJson, Query, State};
use axum::http::Request;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post, put};
use axum::Json;
use axum::Router;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::{Arc, Mutex};
use videre_api::{ClusterDetail, FacesData, PersonDetail};

fn json_response(body: String) -> axum::response::Response {
    (
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        body,
    )
        .into_response()
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum InitialDateFilter {
    Home,
    Prefix(String),
    Range {
        from: Option<String>,
        to: Option<String>,
    },
}

fn route_date_filter(
    year: &str,
    month: Option<&str>,
    day: Option<&str>,
) -> Result<InitialDateFilter, StatusCode> {
    let prefix = match (month, day) {
        (None, None) => {
            if year.len() == 4 && year.chars().all(|c| c.is_ascii_digit()) {
                year.to_string()
            } else {
                return Err(StatusCode::NOT_FOUND);
            }
        }
        (Some(month), None) => {
            let raw = format!("{year}-{month}");
            validate_partial_date(&raw).ok_or(StatusCode::NOT_FOUND)?;
            raw
        }
        (Some(month), Some(day)) => {
            let raw = format!("{year}-{month}-{day}");
            validate_partial_date(&raw).ok_or(StatusCode::NOT_FOUND)?;
            raw
        }
        (None, Some(_)) => return Err(StatusCode::NOT_FOUND),
    };
    Ok(InitialDateFilter::Prefix(prefix))
}

fn validate_partial_date(raw: &str) -> Option<()> {
    match raw.len() {
        4 => raw
            .chars()
            .all(|c| c.is_ascii_digit())
            .then_some(())
            .and_then(|_| raw.parse::<i32>().ok().map(|_| ())),
        7 => {
            if raw.as_bytes().get(4) != Some(&b'-') {
                return None;
            }
            let year = raw[0..4].parse::<i32>().ok()?;
            let month = raw[5..7].parse::<u32>().ok()?;
            chrono::NaiveDate::from_ymd_opt(year, month, 1).map(|_| ())
        }
        10 => {
            if raw.as_bytes().get(4) != Some(&b'-') || raw.as_bytes().get(7) != Some(&b'-') {
                return None;
            }
            let year = raw[0..4].parse::<i32>().ok()?;
            let month = raw[5..7].parse::<u32>().ok()?;
            let day = raw[8..10].parse::<u32>().ok()?;
            chrono::NaiveDate::from_ymd_opt(year, month, day).map(|_| ())
        }
        _ => None,
    }
}

fn normalize_date_range(
    from: Option<&str>,
    to: Option<&str>,
    today: chrono::NaiveDate,
) -> Result<InitialDateFilter, StatusCode> {
    if from.is_none() && to.is_none() {
        return Ok(InitialDateFilter::Home);
    }

    let from = from
        .filter(|s| !s.is_empty())
        .map(expand_bound_start)
        .transpose()
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    let to = match to.filter(|s| !s.is_empty()) {
        Some(raw) => Some(expand_bound_start(raw).map_err(|_| StatusCode::BAD_REQUEST)?),
        None if from.is_some() => Some((today + chrono::Days::new(1)).to_string()),
        None => None,
    };

    if let (Some(from), Some(to)) = (&from, &to) {
        if from >= to {
            return Err(StatusCode::BAD_REQUEST);
        }
    }

    Ok(InitialDateFilter::Range { from, to })
}

fn expand_bound_start(raw: &str) -> Result<String, ()> {
    match raw.len() {
        4 => {
            let year = raw.parse::<i32>().map_err(|_| ())?;
            chrono::NaiveDate::from_ymd_opt(year, 1, 1)
                .map(|d| d.to_string())
                .ok_or(())
        }
        7 => {
            if raw.as_bytes().get(4) != Some(&b'-') {
                return Err(());
            }
            let year = raw[0..4].parse::<i32>().map_err(|_| ())?;
            let month = raw[5..7].parse::<u32>().map_err(|_| ())?;
            chrono::NaiveDate::from_ymd_opt(year, month, 1)
                .map(|d| d.to_string())
                .ok_or(())
        }
        10 => {
            if raw.as_bytes().get(4) != Some(&b'-') || raw.as_bytes().get(7) != Some(&b'-') {
                return Err(());
            }
            let year = raw[0..4].parse::<i32>().map_err(|_| ())?;
            let month = raw[5..7].parse::<u32>().map_err(|_| ())?;
            let day = raw[8..10].parse::<u32>().map_err(|_| ())?;
            chrono::NaiveDate::from_ymd_opt(year, month, day)
                .map(|d| d.to_string())
                .ok_or(())
        }
        _ => Err(()),
    }
}

#[cfg(test)]
mod date_filter_tests {
    use super::*;
    use chrono::NaiveDate;

    #[test]
    fn date_route_segments_normalize_to_prefixes() {
        assert_eq!(
            route_date_filter("2025", None, None).unwrap(),
            InitialDateFilter::Prefix("2025".to_string())
        );
        assert_eq!(
            route_date_filter("2025", Some("06"), None).unwrap(),
            InitialDateFilter::Prefix("2025-06".to_string())
        );
        assert_eq!(
            route_date_filter("2025", Some("06"), Some("03")).unwrap(),
            InitialDateFilter::Prefix("2025-06-03".to_string())
        );
    }

    #[test]
    fn invalid_date_route_segments_are_not_found() {
        for (year, month, day) in [
            ("abcd", None, None),
            ("2025", Some("6"), None),
            ("2025", Some("13"), None),
            ("2025", Some("02"), Some("31")),
            ("2025", Some("06"), Some("xx")),
        ] {
            let err = route_date_filter(year, month, day).unwrap_err();
            assert_eq!(err, StatusCode::NOT_FOUND);
        }
    }

    #[test]
    fn date_query_range_normalizes_partial_bounds() {
        let today = NaiveDate::from_ymd_opt(2026, 9, 2).unwrap();

        assert_eq!(
            normalize_date_range(Some("2025"), None, today).unwrap(),
            InitialDateFilter::Range {
                from: Some("2025-01-01".to_string()),
                to: Some("2026-09-03".to_string()),
            }
        );
        assert_eq!(
            normalize_date_range(Some("2016-05"), Some("2017"), today).unwrap(),
            InitialDateFilter::Range {
                from: Some("2016-05-01".to_string()),
                to: Some("2017-01-01".to_string()),
            }
        );
        assert_eq!(
            normalize_date_range(None, Some("2018"), today).unwrap(),
            InitialDateFilter::Range {
                from: None,
                to: Some("2018-01-01".to_string()),
            }
        );
    }

    #[test]
    fn invalid_date_query_ranges_are_bad_requests() {
        let today = NaiveDate::from_ymd_opt(2026, 9, 2).unwrap();

        for (from, to) in [
            (Some("banana"), None),
            (Some("2025-13"), None),
            (Some("2017"), Some("2016")),
            (Some("2025-05-10"), Some("2025-05-10")),
            (Some("2025-02-31"), None),
        ] {
            let err = normalize_date_range(from, to, today).unwrap_err();
            assert_eq!(err, StatusCode::BAD_REQUEST);
        }
    }
}

#[cfg(test)]
mod location_cluster_tests {
    use super::*;
    use axum::body::{to_bytes, Body};
    use axum::http::Request;
    use tower::ServiceExt;

    pub(super) fn gallery_state(root: &Path) -> Arc<AppState> {
        let conn = Connection::open_in_memory().unwrap();
        videre_core::location_cluster::register_haversine_sql_function(&conn).unwrap();
        let library = Arc::new(
            videre_core::library::LibraryContext::new(root, &root.join("cache"))
                .expect("test library context should be valid"),
        );
        let context = Arc::new(crate::command_context::CommandContext {
            library,
            invocation_dir: root.to_path_buf(),
            source: crate::command_context::LibrarySource::Cwd,
        });
        let (shutdown_tx, _shutdown_rx) = tokio::sync::oneshot::channel();
        Arc::new(AppState {
            conn: Arc::new(Mutex::new(conn)),
            events_cache: Mutex::new(None),
            learning: None,
            shutdown_tx: Mutex::new(Some(shutdown_tx)),
            model_id: String::new(),
            report_heic: false,
            report_heic_original: false,
            serve_faces_ui: true,
            gallery: true,
            context,
            embedder: Mutex::new(None),
            basemap_downloading: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            settings_lock: Arc::new(Mutex::new(())),
            settings_warned: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        })
    }

    fn seed_map_locations(conn: &Connection) {
        videre_core::location_cluster::ensure_location_clusters_table(conn).unwrap();
        conn.execute_batch(
            "INSERT INTO location_clusters
                (id, centroid_lat, centroid_lon, name, photo_count, radius_km, created_at)
             VALUES
                (1, 41.01, 28.97, 'İstanbul', 5, 20.0, CURRENT_TIMESTAMP),
                (2, 41.02, 28.98, 'istanbul', 20, 25.0, CURRENT_TIMESTAMP),
                (3, 52.52, 13.405, 'Berlin', 10, 20.0, CURRENT_TIMESTAMP),
                (4, 52.51, 13.40, 'Berlin', 10, 30.0, CURRENT_TIMESTAMP),
                (5, 40.71, -74.01, NULL, 2, 15.0, CURRENT_TIMESTAMP),
                (6, 40.72, -74.02, NULL, 8, 35.0, CURRENT_TIMESTAMP);",
        )
        .unwrap();
    }

    #[test]
    fn map_location_resolver_normalizes_and_prefers_the_largest_cluster() {
        let conn = Connection::open_in_memory().unwrap();
        seed_map_locations(&conn);

        assert_eq!(
            resolve_map_location(&conn, "istanbul").unwrap().unwrap().id,
            2
        );
        assert_eq!(
            resolve_map_location(&conn, "berlin").unwrap().unwrap().id,
            3
        );
        assert_eq!(
            resolve_map_location(&conn, "unnamed_location")
                .unwrap()
                .unwrap()
                .id,
            6
        );
        assert!(resolve_map_location(&conn, "!!!").unwrap().is_none());
    }

    #[tokio::test]
    async fn map_location_route_bootstraps_the_resolved_cluster() {
        let dir = tempfile::tempdir().unwrap();
        let state = gallery_state(dir.path());
        seed_map_locations(&state.conn.lock().unwrap());
        let app = Router::new()
            .route("/map/location/{name}", get(handle_map_location))
            .with_state(state);

        for (uri, radius) in [
            ("/map/location/berlin", "20.0"),
            ("/map/location/berlin?radius=25", "25.0"),
        ] {
            let response = app
                .clone()
                .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{uri}");
            let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            let body = String::from_utf8(body.to_vec()).unwrap();
            assert!(
                body.contains(&format!(
                    "var GLOC={{\"kind\":\"location\",\"name\":\"berlin\",\"radius\":{radius}}};"
                )),
                "{uri} did not carry the expected bootstrap: {body}"
            );
        }

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/map/location/berlin?radius=0")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn unknown_map_location_renders_an_honest_200_state() {
        let dir = tempfile::tempdir().unwrap();
        let state = gallery_state(dir.path());
        seed_map_locations(&state.conn.lock().unwrap());
        let app = Router::new()
            .route("/map/location/{name}", get(handle_map_location))
            .with_state(state);

        // An unknown name is a working 200 page whatever the radius query is:
        // an invalid radius must not turn it into a 400 for a place the map
        // never had.
        for uri in [
            "/map/location/not-a-place",
            "/map/location/not-a-place?radius=0",
        ] {
            let response = app
                .clone()
                .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{uri}");
            let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            let body = String::from_utf8(body.to_vec()).unwrap();
            assert!(
                body.contains("var GLOC={\"kind\":\"unknown\",\"name\":\"notaplace\"};"),
                "{uri}"
            );
            assert!(!body.contains("\"latitude\""));
            assert!(!body.contains("\"longitude\""));
            assert!(!body.contains("\"cluster_id\""));
        }
    }

    #[test]
    fn nearest_map_location_picks_the_closest_centroid() {
        let conn = Connection::open_in_memory().unwrap();
        seed_map_locations(&conn);
        // A point in Berlin resolves to a Berlin cluster, not İstanbul or NYC.
        assert_eq!(
            nearest_map_location(&conn, 52.50, 13.41)
                .unwrap()
                .unwrap()
                .display_name,
            "Berlin"
        );
        // A point near New York resolves to the (unnamed) NYC cluster.
        assert_eq!(
            nearest_map_location(&conn, 40.70, -74.00)
                .unwrap()
                .unwrap()
                .id,
            5
        );
    }

    #[test]
    fn parse_lat_lon_accepts_a_pair_and_rejects_junk() {
        assert_eq!(parse_lat_lon("52.52,13.405"), Some((52.52, 13.405)));
        assert_eq!(parse_lat_lon(" -33.9 , 18.4 "), Some((-33.9, 18.4)));
        assert!(parse_lat_lon("91,0").is_none()); // latitude out of range
        assert!(parse_lat_lon("0,181").is_none()); // longitude out of range
        assert!(parse_lat_lon("nope").is_none());
        assert!(parse_lat_lon("1").is_none());
    }

    #[tokio::test]
    async fn map_near_query_bootstraps_the_nearest_cluster() {
        let dir = tempfile::tempdir().unwrap();
        let state = gallery_state(dir.path());
        seed_map_locations(&state.conn.lock().unwrap());
        let app = Router::new()
            .route("/map", get(handle_map))
            .with_state(state);

        // A photo's own coordinates resolve to the nearest cluster's route name
        // and its default radius (cluster 4 at 52.51,13.40 is closest here), which
        // the client then canonicalises to /map/location/<name>.
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/map?near=52.50,13.41")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        assert!(
            body.contains("var GLOC={\"kind\":\"location\",\"name\":\"berlin\",\"radius\":30.0};"),
            "near query did not resolve to the nearest Berlin cluster: {body}"
        );

        // A plain /map (no near) stays unselected.
        let response = app
            .oneshot(Request::builder().uri("/map").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        assert!(body.contains("var GLOC=null;"), "{body}");
    }

    #[tokio::test]
    async fn location_clusters_endpoint_returns_rows_ordered_and_shaped() {
        let dir = tempfile::tempdir().unwrap();
        let state = gallery_state(dir.path());
        {
            let conn = state.conn.lock().unwrap();
            videre_core::location_cluster::ensure_location_clusters_table(&conn).unwrap();
            conn.execute_batch(
                "INSERT INTO location_clusters
                    (centroid_lat, centroid_lon, name, photo_count, radius_km, created_at)
                 VALUES
                    (52.52, 13.405, 'Berlin', 5, 20.0, CURRENT_TIMESTAMP),
                    (35.68, 139.69, 'Tokyo', 20, 20.0, CURRENT_TIMESTAMP),
                    (40.71, -74.01, NULL, 10, 20.0, CURRENT_TIMESTAMP),
                    (-33.87, 151.21, 'Sydney', 1, 20.0, CURRENT_TIMESTAMP);",
            )
            .unwrap();
        }
        let app = Router::new()
            .route("/api/location-clusters", get(handle_location_clusters))
            .with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/location-clusters")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let rows: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let rows = rows.as_array().unwrap();
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0]["name"], "Tokyo");
        assert_eq!(rows[0]["route_name"], "tokyo");
        assert_eq!(rows[0]["continent"], "Asia");
        assert_eq!(rows[1]["name"], "Unnamed location");
        assert_eq!(rows[3]["continent"], "Oceania");
        assert!(rows[0]["centroid_lat"].is_number());
        assert!(rows[0]["radius_km"].is_number());
    }

    #[tokio::test]
    async fn location_clusters_endpoint_on_an_empty_library_returns_an_empty_array() {
        let dir = tempfile::tempdir().unwrap();
        let state = gallery_state(dir.path());
        let app = Router::new()
            .route("/api/location-clusters", get(handle_location_clusters))
            .with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/location-clusters")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(body.as_ref(), b"[]");
    }

    #[tokio::test]
    async fn files_endpoint_filters_by_exact_proximity_with_correct_total() {
        let dir = tempfile::tempdir().unwrap();
        let state = gallery_state(dir.path());
        {
            let conn = state.conn.lock().unwrap();
            conn.execute_batch(
                "CREATE TABLE file_hashes (
                    path TEXT PRIMARY KEY,
                    hash TEXT NOT NULL,
                    size_bytes INTEGER,
                    ext TEXT,
                    created_at TEXT,
                    modified_at TEXT,
                    exif_date TEXT,
                    gps_lat REAL,
                    gps_lon REAL,
                    width INTEGER,
                    height INTEGER,
                    location_cluster_id INTEGER
                 );
                 INSERT INTO file_hashes
                    (path, hash, size_bytes, ext, gps_lat, gps_lon)
                 VALUES
                    ('/a-center.jpg', 'aaa', 1, 'jpg', 52.52, 13.405),
                    ('/b-axis.jpg', 'bbb', 2, 'jpg', 52.60, 13.405),
                    ('/c-corner.jpg', 'ccc', 3, 'jpg', 52.70, 13.69),
                    ('/d-outside.jpg', 'ddd', 4, 'jpg', 53.00, 13.405),
                    ('/e-no-gps.jpg', 'eee', 5, 'jpg', NULL, NULL);",
            )
            .unwrap();
        }
        let app = Router::new()
            .route("/api/files", get(handle_files))
            .with_state(state);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/files?view=all&lat=52.52&lon=13.405&radius=25&limit=1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["total"], 2);
        assert_eq!(json["files"].as_array().unwrap().len(), 1);
        assert_eq!(json["files"][0]["hash"], "aaa");

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/files?view=all&lat=52.52&lon=13.405&radius=25&offset=1&limit=1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["total"], 2);
        assert_eq!(json["files"].as_array().unwrap().len(), 1);
        assert_eq!(json["files"][0]["hash"], "bbb");
    }

    #[tokio::test]
    async fn files_endpoint_rejects_partial_or_invalid_proximity() {
        let dir = tempfile::tempdir().unwrap();
        let state = gallery_state(dir.path());
        state
            .conn
            .lock()
            .unwrap()
            .execute_batch(
                "CREATE TABLE file_hashes (
                    path TEXT PRIMARY KEY, hash TEXT NOT NULL, size_bytes INTEGER,
                    ext TEXT, created_at TEXT, modified_at TEXT, exif_date TEXT,
                    gps_lat REAL, gps_lon REAL, width INTEGER, height INTEGER,
                    location_cluster_id INTEGER
                );",
            )
            .unwrap();
        let app = Router::new()
            .route("/api/files", get(handle_files))
            .with_state(state);

        for uri in [
            "/api/files?lat=52.52",
            "/api/files?lat=52.52&lon=13.405",
            "/api/files?lat=91&lon=13.405&radius=20",
            "/api/files?lat=52.52&lon=181&radius=20",
            "/api/files?lat=52.52&lon=13.405&radius=0",
            "/api/files?lat=52.52&lon=13.405&radius=-1",
        ] {
            let response = app
                .clone()
                .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{uri}");
        }
    }

    #[tokio::test]
    async fn date_view_ignores_proximity_parameters() {
        let dir = tempfile::tempdir().unwrap();
        let state = gallery_state(dir.path());
        state
            .conn
            .lock()
            .unwrap()
            .execute_batch(
                "CREATE TABLE file_hashes (
                    path TEXT PRIMARY KEY, hash TEXT NOT NULL, size_bytes INTEGER,
                    ext TEXT, created_at TEXT, modified_at TEXT, exif_date TEXT,
                    gps_lat REAL, gps_lon REAL, width INTEGER, height INTEGER,
                    location_cluster_id INTEGER
                );
                INSERT INTO file_hashes
                    (path, hash, size_bytes, ext, exif_date, gps_lat, gps_lon)
                VALUES ('/a.jpg', 'aaa', 1, 'jpg', '2025-01-01', 52.52, 13.405);",
            )
            .unwrap();
        let app = Router::new()
            .route("/api/files", get(handle_files))
            .with_state(state);

        let mut responses = Vec::new();
        for uri in ["/api/files?view=date", "/api/files?view=date&lat=999"] {
            let response = app
                .clone()
                .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{uri}");
            let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            responses.push(serde_json::from_slice::<serde_json::Value>(&body).unwrap());
        }
        assert_eq!(responses[0]["total"], responses[1]["total"]);
        assert_eq!(responses[0]["files"], responses[1]["files"]);
    }
}

#[cfg(test)]
mod basemap_tests {
    use super::location_cluster_tests::gallery_state;
    use super::*;
    use axum::body::{to_bytes, Body};
    use axum::http::header;
    use axum::http::Request;
    use tower::ServiceExt;

    /// The archive path the handlers resolve for a given test state: the
    /// shared geo cache under the library context, not a bare tempdir join.
    fn archive_for(state: &AppState) -> std::path::PathBuf {
        videre_core::basemap::archive_path(
            &state.context.library.cache.geo,
            &state.context.library.paths.state,
        )
    }

    #[tokio::test]
    async fn basemap_tile_endpoint_serves_ranges_and_absent_404() {
        let dir = tempfile::tempdir().unwrap();
        let state = gallery_state(dir.path());

        // No archive yet: 404.
        let app = Router::new()
            .route("/tiles/basemap.pmtiles", get(handle_basemap_tiles))
            .with_state(state.clone());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/tiles/basemap.pmtiles")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        // An archive on disk: a Range returns exactly those bytes with 206.
        let archive = archive_for(&state);
        std::fs::create_dir_all(archive.parent().unwrap()).unwrap();
        std::fs::write(&archive, b"0123456789").unwrap();
        let app = Router::new()
            .route("/tiles/basemap.pmtiles", get(handle_basemap_tiles))
            .with_state(state);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/tiles/basemap.pmtiles")
                    .header(header::RANGE, "bytes=4-7")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            response.headers().get(header::ACCEPT_RANGES).unwrap(),
            "bytes"
        );
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(body.as_ref(), b"4567");
    }

    #[tokio::test]
    async fn basemap_tile_endpoint_409s_while_a_download_is_in_flight() {
        let dir = tempfile::tempdir().unwrap();
        let state = gallery_state(dir.path());
        // A `.part` sibling with no final archive is the in-flight state.
        let archive = archive_for(&state);
        std::fs::create_dir_all(archive.parent().unwrap()).unwrap();
        std::fs::write(archive.with_extension("pmtiles.part"), b"half").unwrap();
        let app = Router::new()
            .route("/tiles/basemap.pmtiles", get(handle_basemap_tiles))
            .with_state(state);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/tiles/basemap.pmtiles")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert!(String::from_utf8_lossy(&body).contains("\"partial\""));
    }

    #[tokio::test]
    async fn basemap_status_reports_absent_then_ready() {
        let dir = tempfile::tempdir().unwrap();
        let state = gallery_state(dir.path());
        let app = Router::new()
            .route("/api/basemap/status", get(handle_basemap_status))
            .with_state(state.clone());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/basemap/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert!(String::from_utf8_lossy(&body).contains("\"absent\""));

        let archive = archive_for(&state);
        std::fs::create_dir_all(archive.parent().unwrap()).unwrap();
        std::fs::write(&archive, b"abc").unwrap();
        let app = Router::new()
            .route("/api/basemap/status", get(handle_basemap_status))
            .with_state(state);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/basemap/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let text = String::from_utf8_lossy(&body);
        assert!(text.contains("\"ready\""));
        assert!(text.contains("\"bytes\":3"));
    }
}

#[cfg(test)]
mod files_sort_tests {
    use super::location_cluster_tests::gallery_state;
    use super::*;
    use axum::body::{to_bytes, Body};
    use axum::http::Request;
    use tower::ServiceExt;

    async fn body_of(app: &Router, uri: &str) -> String {
        let response = app
            .clone()
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        String::from_utf8(body.to_vec()).unwrap()
    }

    fn sorted_files_app() -> (tempfile::TempDir, Router) {
        let dir = tempfile::tempdir().unwrap();
        let state = gallery_state(dir.path());
        {
            let conn = state.conn.lock().unwrap();
            videre_core::library_db::ensure_scan_schema(&conn).unwrap();
            videre_core::marks::ensure_marks_table(&conn).unwrap();
            conn.execute_batch(
                "INSERT INTO file_hashes (path, hash, size_bytes, ext, mime, modified_at) VALUES
                    ('/lib/a_old.jpg', 'ha', 30, 'jpg', 'image/jpeg', '2021-01-01T00:00:00'),
                    ('/lib/b_new.jpg', 'hb', 20, 'jpg', 'image/jpeg', '2022-01-01T00:00:00');
                INSERT INTO marks (hash, rating, liked, updated_at) VALUES
                    ('ha', 5, 0, '2026-01-01T00:00:00');",
            )
            .unwrap();
        }
        let app = Router::new()
            .route("/api/files", get(handle_files))
            .with_state(state);
        (dir, app)
    }

    #[tokio::test]
    async fn files_page_carries_the_sort_control_and_events_page_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let state = gallery_state(dir.path());
        {
            let conn = state.conn.lock().unwrap();
            videre_core::library_db::ensure_scan_schema(&conn).unwrap();
            videre_core::marks::ensure_marks_table(&conn).unwrap();
        }
        let app = Router::new()
            .route("/", get(handle_gallery_all))
            .route("/events", get(handle_events))
            .with_state(state);

        let home = body_of(&app, "/").await;
        assert!(home.contains("class=\"sort-select\""), "{home}");
        assert!(home.contains("class=\"sort-dir-btn\""), "{home}");

        let events = body_of(&app, "/events").await;
        assert!(events.contains("view-mode-select"), "{events}"); // the page has a head
                                                                  // Match the attribute, not the bare class name: the inlined gallery.js
                                                                  // mentions .sort-select in its querySelectorAll calls on every page.
        assert!(!events.contains("class=\"sort-select\""), "{events}");
        assert!(!events.contains("class=\"sort-dir-btn\""), "{events}");
    }

    #[test]
    fn static_export_rows_carry_marks_so_offline_sorts_work() {
        let dir = tempfile::tempdir().unwrap();
        let state = gallery_state(dir.path());
        let out = dir.path().join("out.html");
        // query_all_files drops rows whose path does not exist, so the scanned
        // row needs a real file behind it.
        let real = dir.path().join("a.jpg");
        std::fs::write(&real, b"x").unwrap();
        let rows = {
            let conn = state.conn.lock().unwrap();
            videre_core::library_db::ensure_scan_schema(&conn).unwrap();
            videre_core::marks::ensure_marks_table(&conn).unwrap();
            conn.execute(
                "INSERT INTO file_hashes (path, hash, size_bytes, ext, mime, modified_at) \
                 VALUES (?1, 'ha', 30, 'jpg', 'image/jpeg', '2021-01-01T00:00:00')",
                rusqlite::params![real.to_string_lossy().as_ref()],
            )
            .unwrap();
            videre_core::marks::set(
                &conn,
                &["ha".to_string()],
                &videre_core::marks::change_from_parts(Some(5), None, None, None),
            )
            .unwrap();
            crate::render::query_all_files(&conn)
        };
        let conn = state.conn.lock().unwrap();
        crate::render::write_static_page(&conn, &out, &[], Some(&rows)).unwrap();
        let html = std::fs::read_to_string(&out).unwrap();
        assert!(html.contains("var ALLFILES=["), "{html}");
        assert!(html.contains("\"rating\":5"), "{html}");
    }

    #[tokio::test]
    async fn files_endpoint_sorts_by_the_query_parameters_and_falls_back_on_unknowns() {
        let (_dir, app) = sorted_files_app();

        // Default (date, desc): b_new (2022) before a_old (2021).
        let body = body_of(&app, "/api/files?view=all").await;
        assert!(body.find("\"hash\":\"hb\"").unwrap() < body.find("\"hash\":\"ha\"").unwrap());

        // Name asc: a_old before b_new.
        let body = body_of(&app, "/api/files?view=all&sort=name&dir=asc").await;
        assert!(body.find("\"hash\":\"ha\"").unwrap() < body.find("\"hash\":\"hb\"").unwrap());

        // Size desc: a_old (30) before b_new (20).
        let body = body_of(&app, "/api/files?view=all&sort=size&dir=desc").await;
        assert!(body.find("\"hash\":\"ha\"").unwrap() < body.find("\"hash\":\"hb\"").unwrap());

        // Unknown values fall back to the default, without erroring.
        let body = body_of(&app, "/api/files?view=all&sort=bogus&dir=sideways").await;
        assert!(body.find("\"hash\":\"hb\"").unwrap() < body.find("\"hash\":\"ha\"").unwrap());

        // Marks ride along for the sorts that need them.
        let body = body_of(&app, "/api/files?view=all&sort=rating&dir=desc").await;
        assert!(body.contains("\"rating\":5"));
        assert!(body.find("\"hash\":\"ha\"").unwrap() < body.find("\"hash\":\"hb\"").unwrap());
    }
}

#[cfg(test)]
mod events_tests {
    use super::location_cluster_tests::gallery_state;
    use super::*;
    use axum::body::{to_bytes, Body};
    use axum::http::Request;
    use tower::ServiceExt;

    #[test]
    fn trip_snapshot_uses_capture_date_only_and_rejects_invalid_gps() {
        let conn = Connection::open_in_memory().unwrap();
        videre_core::library_db::ensure_scan_schema(&conn).unwrap();
        conn.execute_batch(
            "INSERT INTO file_hashes (path, hash, ext, modified_at, gps_lat, gps_lon) VALUES
             ('/p/no-date.jpg','no-date','jpg','2020-03-12T09:30:00+02:00',52.52,13.4),
             ('/p/bad-lat.jpg','bad-lat','jpg','2020-03-12T09:30:00+02:00',91,13.4),
             ('/p/bad-lon.jpg','bad-lon','jpg','2020-03-12T09:30:00+02:00',52.52,181),
             ('/p/infinite.jpg','infinite','jpg','2020-03-12T09:30:00+02:00',1e999,13.4);",
        )
        .unwrap();
        let rows = load_trip_rows(&conn).unwrap();
        assert_eq!(rows.len(), 4);
        assert!(rows.iter().all(|row| row.capture.is_none()));
        assert_eq!(
            rows.iter().find(|row| row.hash == "no-date").unwrap().gps,
            Some((52.52, 13.4))
        );
        assert!(rows
            .iter()
            .filter(|row| row.hash.starts_with("bad-") || row.hash == "infinite")
            .all(|row| row.gps.is_none()));
    }

    #[test]
    fn trip_snapshot_prefers_usable_duplicate_metadata_in_any_insert_order() {
        fn snapshot(reverse: bool) -> Vec<super::super::events::TripRow> {
            let conn = Connection::open_in_memory().unwrap();
            videre_core::library_db::ensure_scan_schema(&conn).unwrap();
            let good = "INSERT INTO file_hashes (path, hash, ext, exif_date, gps_lat, gps_lon) VALUES ('/p/z.jpg','same','jpg','2020-03-12T09:30:00',47.5,19.0)";
            let stale = "INSERT INTO file_hashes (path, hash, ext, modified_at) VALUES ('/p/a.jpg','same','jpg','2020-03-12T09:30:00+02:00')";
            if reverse {
                conn.execute(stale, []).unwrap();
                conn.execute(good, []).unwrap();
            } else {
                conn.execute(good, []).unwrap();
                conn.execute(stale, []).unwrap();
            }
            load_trip_rows(&conn).unwrap()
        }
        let rows = snapshot(false);
        assert_eq!(rows, snapshot(true));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].path, "/p/z.jpg");
        assert_eq!(
            rows[0].capture,
            Some(super::super::events::parse_capture("2020-03-12T09:30:00").unwrap())
        );
        assert_eq!(rows[0].gps, Some((47.5, 19.0)));
    }

    fn seed_events(conn: &Connection) {
        videre_core::library_db::ensure_scan_schema(conn).unwrap();
        let mut add = conn.prepare("INSERT INTO file_hashes (path,hash,size_bytes,ext,mime,exif_date,modified_at,gps_lat,gps_lon,width,height) VALUES (?1,?2,100,?3,?4,?5,'2020-03-12T11:00:00+00:00',?6,?7,4000,3000)").unwrap();
        let mut insert_media =
            |i: usize, ext: &str, when: Option<String>, gps: Option<(f64, f64)>| {
                add.execute(rusqlite::params![
                    format!("/p/{i}.{ext}"),
                    format!("{i:064x}"),
                    ext,
                    if ext == "mp4" {
                        "video/mp4"
                    } else {
                        "image/jpeg"
                    },
                    when,
                    gps.map(|p| p.0),
                    gps.map(|p| p.1)
                ])
                .unwrap();
            };
        for i in 1..=13 {
            insert_media(
                i,
                "jpg",
                Some(format!("2020-01-01T08:{i:02}:00")),
                Some((52.52, 13.405)),
            );
        }
        insert_media(
            101,
            "jpg",
            Some("2020-03-12T10:00:00".into()),
            Some((47.4979, 19.0402)),
        );
        insert_media(
            102,
            "jpg",
            Some("2020-03-12T11:00:00".into()),
            Some((47.4980, 19.0410)),
        );
        insert_media(
            103,
            "jpg",
            Some("2020-03-12T12:00:00".into()),
            Some((47.4981, 19.0420)),
        );
        for i in 0..8 {
            insert_media(
                104 + i,
                "jpg",
                Some(format!("2020-03-12T10:{:02}:00", 10 + i * 5)),
                None,
            );
        }
        insert_media(112, "mp4", Some("2020-03-12T11:40:00".into()), None);
        insert_media(113, "jpg", None, None);
    }

    #[test]
    fn events_cache_reuses_clean_snapshot_and_invalidates_same_connection_edits() {
        let dir = tempfile::tempdir().unwrap();
        let state = gallery_state(dir.path());
        let conn = state.conn.lock().unwrap();
        seed_events(&conn);
        let first = cached_events(&state, &conn).unwrap();
        assert_eq!(first.events.len(), 1);
        let second = cached_events(&state, &conn).unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        conn.execute(
            "UPDATE file_hashes SET exif_date=NULL WHERE hash=?1",
            [format!("{:064x}", 101)],
        )
        .unwrap();
        let changed = cached_events(&state, &conn).unwrap();
        assert!(!Arc::ptr_eq(&first, &changed));
        assert!(changed.events.is_empty());
    }

    #[test]
    fn events_cache_invalidates_other_connection_commits_and_never_caches_a_transaction() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = gallery_state(dir.path());
        let db_path = dir.path().join("events.db");
        let first_conn = Connection::open(&db_path).unwrap();
        seed_events(&first_conn);
        Arc::get_mut(&mut state).unwrap().conn = Arc::new(Mutex::new(first_conn));
        let second_conn = Connection::open(&db_path).unwrap();
        let conn = state.conn.lock().unwrap();
        let first = cached_events(&state, &conn).unwrap();
        second_conn
            .execute(
                "UPDATE file_hashes SET exif_date=NULL WHERE hash=?1",
                [format!("{:064x}", 101)],
            )
            .unwrap();
        let external = cached_events(&state, &conn).unwrap();
        assert!(!Arc::ptr_eq(&first, &external));
        assert!(external.events.is_empty());
        second_conn
            .execute(
                "UPDATE file_hashes SET exif_date='2020-03-12T10:00:00' WHERE hash=?1",
                [format!("{:064x}", 101)],
            )
            .unwrap();
        let restored = cached_events(&state, &conn).unwrap();
        assert_eq!(restored.events.len(), 1);
        conn.execute_batch("BEGIN; UPDATE file_hashes SET exif_date=NULL WHERE hash=(SELECT hash FROM file_hashes WHERE path='/p/101.jpg');").unwrap();
        let uncommitted = cached_events(&state, &conn).unwrap();
        assert!(uncommitted.events.is_empty());
        assert!(!Arc::ptr_eq(&restored, &uncommitted));
        conn.execute_batch("ROLLBACK").unwrap();
        let rolled_back = cached_events(&state, &conn).unwrap();
        assert_eq!(rolled_back.events.len(), 1);
    }

    #[tokio::test]
    async fn api_events_serves_substantial_trip_not_singletons() {
        let dir = tempfile::tempdir().unwrap();
        let state = gallery_state(dir.path());
        {
            let conn = state.conn.lock().unwrap();
            videre_core::face_db::create_faces_table(&conn).unwrap();
            seed_events(&conn);
            for i in 201..=210 {
                conn.execute("INSERT INTO file_hashes (path,hash,size_bytes,ext,mime,exif_date,gps_lat,gps_lon) VALUES (?1,?2,100,'jpg','image/jpeg',?3,48.21,16.37)",
                    rusqlite::params![format!("/p/{i}.jpg"),format!("{i:064x}"),format!("2020-04-01T10:{:02}:00", i-201)]).unwrap();
            }
        }
        let app = Router::new()
            .route("/api/events", get(handle_events_api))
            .with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/events")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let events = json["events"].as_array().unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0]["start"], "2020-04-01 10:00:00");
        assert_eq!(events[0]["count"], 10);
        assert_eq!(events[1]["kind"], "trip");
        assert_eq!(events[1]["start"], "2020-03-12 10:00:00");
        assert_eq!(events[1]["count"], 12);
        assert_eq!(events[1]["key"], format!("20200312T100000-{:064x}", 101));
        let place = events[1]["place"].as_str().unwrap();
        assert!(place.ends_with(", HU"), "unexpected place: {place}");
        assert!(events[1]["title"]
            .as_str()
            .unwrap()
            .ends_with("March 2020 Trip"));
        assert_eq!(events[1]["sample"]["hash"], format!("{:064x}", 101));
        assert!(json["empty_reason"].is_null());
    }

    #[tokio::test]
    async fn api_events_reports_specific_empty_reasons() {
        let dir = tempfile::tempdir().unwrap();
        let state = gallery_state(dir.path());
        {
            let conn = state.conn.lock().unwrap();
            videre_core::library_db::ensure_scan_schema(&conn).unwrap();
        }
        let app = Router::new()
            .route("/api/events", get(handle_events_api))
            .with_state(state.clone());
        async fn reason(app: Router, expected: &str) {
            let response = app
                .oneshot(
                    Request::builder()
                        .uri("/api/events")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(json["events"].as_array().unwrap().len(), 0);
            assert_eq!(json["empty_reason"], expected);
        }
        reason(app.clone(), "no_media").await;
        {
            let conn = state.conn.lock().unwrap();
            conn.execute("INSERT INTO file_hashes (path,hash,ext,mime) VALUES ('/p/a.jpg','a','jpg','image/jpeg')", []).unwrap();
        }
        reason(app.clone(), "no_capture_dates").await;
        {
            let conn = state.conn.lock().unwrap();
            conn.execute(
                "UPDATE file_hashes SET exif_date='2020-03-12T10:00:00' WHERE hash='a'",
                [],
            )
            .unwrap();
        }
        reason(app.clone(), "insufficient_location_evidence").await;
        {
            let conn = state.conn.lock().unwrap();
            conn.execute(
                "UPDATE file_hashes SET gps_lat=52.52,gps_lon=13.4 WHERE hash='a'",
                [],
            )
            .unwrap();
            conn.execute("INSERT INTO file_hashes (path,hash,ext,mime,exif_date,gps_lat,gps_lon) VALUES ('/p/b.jpg','b','jpg','image/jpeg','2020-03-12T11:00:00',47.5,19.0)", []).unwrap();
        }
        reason(app, "no_qualifying_trips").await;
    }

    #[tokio::test]
    async fn trip_title_falls_back_when_offline_place_cache_is_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        let state = gallery_state(dir.path());
        {
            let conn = state.conn.lock().unwrap();
            seed_events(&conn);
        }
        let geo = &state.context.library.cache.geo;
        std::fs::create_dir_all(geo.parent().unwrap()).unwrap();
        std::fs::write(geo, b"not a directory").unwrap();
        let app = Router::new()
            .route("/api/events", get(handle_events_api))
            .with_state(state);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/events")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["events"][0]["title"], "Trip, March 2020");
        assert!(json["events"][0]["place"].is_null());
    }

    #[tokio::test]
    async fn trip_title_preserves_offline_utf8_place_name() {
        let dir = tempfile::tempdir().unwrap();
        let state = gallery_state(dir.path());
        {
            let conn = state.conn.lock().unwrap();
            seed_events(&conn);
            for i in 201..=210 {
                conn.execute("INSERT INTO file_hashes (path,hash,size_bytes,ext,mime,exif_date,gps_lat,gps_lon) VALUES (?1,?2,100,'jpg','image/jpeg',?3,41.02274,29.01366)",
                    rusqlite::params![format!("/p/{i}.jpg"),format!("{i:064x}"),format!("2020-04-01T10:{:02}:00", i-201)]).unwrap();
            }
        }
        let app = Router::new()
            .route("/api/events", get(handle_events_api))
            .with_state(state);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/events")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(json["events"][0]["title"]
            .as_str()
            .unwrap()
            .starts_with("Üsküdar, April 2020 Trip"));
    }

    #[tokio::test]
    async fn api_event_files_returns_only_that_events_members() {
        let dir = tempfile::tempdir().unwrap();
        let state = gallery_state(dir.path());
        {
            let conn = state.conn.lock().unwrap();
            videre_core::face_db::create_faces_table(&conn).unwrap();
            seed_events(&conn);
        }
        let app = Router::new()
            .route("/api/events/{key}/files", get(handle_events_files))
            .with_state(state);

        // The trip includes exact dated members but no Berlin or undated file.
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/events/20200312T100000-{:064x}/files", 101))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let files = json["files"].as_array().unwrap();
        assert_eq!(files.len(), 12);
        assert_eq!(files[0]["hash"], format!("{:064x}", 101));
        assert!(files.iter().all(|f| f["hash"] != format!("{:064x}", 113)));

        // An unknown key is a 404, not an empty grid.
        let missing = app
            .oneshot(
                Request::builder()
                    .uri("/api/events/20000101T000000/files")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn event_files_surface_a_row_error_instead_of_dropping_the_photo() {
        let dir = tempfile::tempdir().unwrap();
        let state = gallery_state(dir.path());
        let conn = state.conn.lock().unwrap();
        seed_events(&conn);
        // A size that cannot decode as an integer: the row must not vanish
        // from the event silently.
        conn.execute(
            "UPDATE file_hashes SET size_bytes = 'x' WHERE hash = ?1",
            [format!("{:064x}", 101)],
        )
        .unwrap();
        assert!(crate::render::query_event_files(&conn, &[format!("{:064x}", 101)]).is_err());
    }

    #[tokio::test]
    async fn events_pages_carry_the_right_globals_and_404_unknown_keys() {
        let dir = tempfile::tempdir().unwrap();
        let state = gallery_state(dir.path());
        {
            let conn = state.conn.lock().unwrap();
            videre_core::face_db::create_faces_table(&conn).unwrap();
            seed_events(&conn);
        }
        let app = Router::new()
            .route("/events", get(handle_events))
            .route("/events/{key}", get(handle_events_key))
            .with_state(state);

        // Overview: events view, no specific event.
        let overview = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/events")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(overview.status(), StatusCode::OK);
        let body = to_bytes(overview.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        assert!(body.contains("var GVIEW=\"events\";"), "{body}");
        assert!(body.contains("var GEVENT=null;"), "{body}");
        // The Events nav link is highlighted.
        assert!(body.contains("href=\"/events\" class=\"on\""));
        assert!(!body.contains("id=\"gallery-more\""), "{body}");

        // Leaf: the matching event's range travels in GEVENT.
        let leaf = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/events/20200312T100000-{:064x}", 101))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(leaf.status(), StatusCode::OK);
        let body = to_bytes(leaf.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        assert!(body.contains("var GVIEW=\"events\";"), "{body}");
        assert!(
            body.contains(&format!("\"key\":\"20200312T100000-{:064x}\"", 101)),
            "{body}"
        );
        // Events are one-shot: the paged gallery's Show more button (whose
        // pager would fetch /api/files) must not be on the page at all.
        assert!(!body.contains("id=\"gallery-more\""), "{body}");
        assert!(body.contains("\"from\":\"2020-03-12 10:00:00\""), "{body}");
        assert!(body.contains("March 2020 Trip"), "{body}");

        // Unknown key is a 404.
        let missing = app
            .oneshot(
                Request::builder()
                    .uri("/events/20000101T000000")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    }
}

// ---- Faces labeling server ----

/// Maps a `videre-api` facade error to the HTTP status code the axum
/// handlers return, preserving the exact 400/404/409/500 behavior the
/// handlers had before delegating to the facade.
/// The three server pages. Markup lives in `crates/videre/templates/`, CSS and
/// JavaScript in `crates/videre/static/`, all compiled into the binary.
///
/// They were 866 lines of string literal in this file until 0.18.0. Askama
/// checks the templates against these structs at compile time, so a renamed
/// field fails the build rather than rendering a blank page.
mod pages {
    use askama::Template;

    #[derive(Template)]
    #[template(path = "faces.html")]
    pub struct Faces {
        /// Gallery settings for `templates/nav.html`. See `settings::page_script`.
        pub settings_script: String,
        /// The chrome every videre page shares. See `static/chrome.css`.
        pub chrome: &'static str,
        pub css: &'static str,
        pub js: &'static str,
        /// Pre-escaped by `esc`, so the template must not escape it again.
        pub db: String,
        pub generated_at: String,
        pub total_files: i64,
        /// The current section, or `None` when there is nowhere to navigate to.
        /// See `templates/nav.html`.
        pub nav: Option<super::Section>,
        /// Where this server's labeling sub-pages live. See `people_root`.
        pub people_root: &'static str,
    }

    #[derive(Template)]
    #[template(path = "cluster.html")]
    pub struct Cluster {
        pub settings_script: String,
        pub css: &'static str,
        pub js: &'static str,
        pub cluster_id: i64,
        /// Where the labeling UI lives on this server. See `people_root`.
        pub back_href: &'static str,
        pub back_label: &'static str,
        /// Highlighted section for the shared nav strip. See `templates/nav.html`.
        pub nav: Option<super::Section>,
    }

    #[derive(Template)]
    #[template(path = "person.html")]
    pub struct Person {
        pub settings_script: String,
        pub css: &'static str,
        pub js: &'static str,
        pub faces_ui_enabled: bool,
        /// Where the labeling UI lives on this server. See `people_root`.
        pub back_href: &'static str,
        pub back_label: &'static str,
        /// Highlighted section for the shared nav strip. See `templates/nav.html`.
        pub nav: Option<super::Section>,
    }

    #[derive(Template)]
    #[template(path = "map.html")]
    pub struct Map {
        pub settings_script: String,
        pub chrome: &'static str,
        pub gallery_css: &'static str,
        pub css: &'static str,
        pub justified_js: &'static str,
        pub gallery_js: &'static str,
        pub js: &'static str,
        pub basemap_style: &'static str,
        pub vendor_version: &'static str,
        pub globals: String,
        pub nav: Option<super::Section>,
    }

    #[derive(Template)]
    #[template(path = "settings.html")]
    pub struct Settings {
        pub settings_script: String,
        pub chrome: &'static str,
        pub js: &'static str,
        /// The library's `gallery.json`, shown so hand editing is findable.
        pub path: String,
        pub nav: Option<super::Section>,
    }

    pub const SETTINGS_PAGE_JS: &str = include_str!("../../../static/settings-page.js");
    pub const FACES_CSS: &str = include_str!("../../../static/faces.css");
    /// The People script, with the shared multi-select component ahead of it.
    pub const FACES_JS: &str = concat!(
        include_str!("../../../static/selection.js"),
        "\n",
        include_str!("../../../static/faces.js")
    );
    pub const CLUSTER_CSS: &str = include_str!("../../../static/cluster.css");
    pub const CLUSTER_JS: &str = include_str!("../../../static/cluster.js");
    pub const PERSON_CSS: &str = include_str!("../../../static/person.css");
    pub const PERSON_JS: &str = include_str!("../../../static/person.js");
    pub const MAP_CSS: &str = include_str!("../../../static/map.css");
    pub const MAP_JS: &str = include_str!("../../../static/map.js");
    /// The geometry-only MapLibre style, checked in and injected into the map
    /// page as a JS global so it needs no HTTP route of its own.
    pub const BASEMAP_STYLE: &str = include_str!("../../../static/basemap-style.json");
    /// MapLibre GL JS and the pmtiles protocol plugin, vendored and served on
    /// the Map page only through `/vendor/{version}/{asset}` (never inlined: ~1 MB).
    pub const MAPLIBRE_JS: &str = include_str!("../../../static/maplibre-gl.js");
    pub const MAPLIBRE_CSS: &str = include_str!("../../../static/maplibre-gl.css");
    pub const PMTILES_JS: &str = include_str!("../../../static/pmtiles.js");
}

pub(super) fn api_status(e: videre_api::Error) -> StatusCode {
    match e {
        videre_api::Error::NotFound => StatusCode::NOT_FOUND,
        videre_api::Error::Invalid => StatusCode::BAD_REQUEST,
        videre_api::Error::Conflict => StatusCode::CONFLICT,
        videre_api::Error::Unavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
        e @ (videre_api::Error::Db(_) | videre_api::Error::Other(_)) => internal(e),
    }
}

/// A poisoned connection lock: a handler panicked while holding it. The
/// poison error carries the guard, which cannot cross threads, so only the
/// fact is logged.
pub(super) fn poisoned<T>(_: std::sync::PoisonError<T>) -> StatusCode {
    internal(anyhow::anyhow!(
        "the database connection lock is poisoned by an earlier panic"
    ))
}

/// Every 500 goes through here, so a server-side failure is logged once
/// instead of being discarded by `map_err(|_| ...)`.
pub(super) fn internal<E: Into<anyhow::Error>>(e: E) -> StatusCode {
    videre_core::error_log::report(tracing::Level::ERROR, &e.into(), None);
    StatusCode::INTERNAL_SERVER_ERROR
}

/// The per-operation guard every blocking server operation runs first: verify
/// the startup-bound library's root still names that library, then take its
/// shared activity lease. A replaced root or a library held by exclusive
/// maintenance yields a retryable 503 rather than serving from, or mutating,
/// whatever now sits at the path. The returned guard must be held for the whole
/// operation (decode and cache publication included), so callers bind it.
pub(super) fn guard_operation(
    state: &AppState,
) -> Result<videre_core::library_locks::ActivityGuard, StatusCode> {
    let library = &state.context.library;
    library
        .ensure_root_identity()
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    videre_core::library_locks::try_activity(
        library,
        videre_core::library_locks::ActivityMode::Shared,
    )
    .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)
}

pub(super) fn settings_file(state: &AppState) -> std::path::PathBuf {
    super::settings::path(&state.context.library.paths.state)
}

/// Where the gallery opens: the library's saved route when it is a safe
/// local page, otherwise `/`. See `settings::resume_route`. Resuming at `/`
/// prints the bare address, exactly as before resume existed, so anything
/// reading the port off that line keeps working.
fn startup_url(addr: &str, state_dir: &Path) -> String {
    let snapshot = super::settings::snapshot(&super::settings::path(state_dir));
    match super::settings::resume_route(&snapshot.effective).as_str() {
        "/" => format!("http://{addr}"),
        route => format!("http://{addr}{route}"),
    }
}

/// The settings script every served page inlines through `nav.html`. Read
/// from disk on each render, so a hand edit applies on the next page load, and
/// on a blocking thread, so a stalled library drive cannot tie up the async
/// workers that serve every other request.
async fn page_settings(state: &AppState) -> String {
    let path = settings_file(state);
    let warned = state.settings_warned.clone();
    let rendered = tokio::task::spawn_blocking(move || {
        let snapshot = super::settings::snapshot(&path);
        if super::settings::should_warn(&warned, snapshot.error.as_deref()) {
            tracing::warn!(
                "gallery settings not loaded, using defaults and saving nothing until it is fixed or deleted: {}",
                snapshot.error.as_deref().unwrap_or_default()
            );
        }
        super::settings::page_script(&snapshot, true)
    })
    .await;
    rendered.unwrap_or_else(|e| {
        internal(e);
        super::settings::default_page_script(true)
    })
}

fn settings_json(s: super::settings::Snapshot) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "effective": s.effective,
        "overrides": s.overrides,
        "ignored": s.ignored,
        "error": s.error,
        "path": s.path.display().to_string(),
    }))
}

/// `/settings`: import, export and reset of the library's gallery settings,
/// reached from the nav's `...` menu.
async fn handle_settings_page(State(state): State<Arc<AppState>>) -> axum::response::Html<String> {
    use askama::Template;
    let page = pages::Settings {
        settings_script: page_settings(&state).await,
        chrome: CHROME_CSS,
        js: pages::SETTINGS_PAGE_JS,
        path: settings_file(&state).display().to_string(),
        nav: Some(Section::Settings),
    };
    axum::response::Html(page.render().expect("settings template"))
}

/// `GET /api/settings`: the effective settings, the stored overrides, and
/// which overrides were ignored for having the wrong type.
async fn handle_get_settings(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let path = settings_file(&state);
    tokio::task::spawn_blocking(move || settings_json(super::settings::snapshot(&path)))
        .await
        .map_err(internal)
}

/// Read-modify-write of the overrides under one lock, from disk every time,
/// so a hand edit made while the server runs is never overwritten by a stale
/// copy in memory.
async fn write_settings(
    state: Arc<AppState>,
    change: impl FnOnce(&mut serde_json::Value) + Send + 'static,
) -> Result<Json<serde_json::Value>, StatusCode> {
    use super::settings::{load, save, snapshot, SaveError, Stored};
    tokio::task::spawn_blocking(move || {
        let _guard = guard_operation(&state)?;
        let _held = state.settings_lock.lock().map_err(poisoned)?;
        let path = settings_file(&state);
        let mut overrides = match load(&path) {
            Stored::Absent => serde_json::Value::Object(Default::default()),
            Stored::Valid(v) => v,
            // Never overwrite a file that could not be read: it may hold hand
            // edits. The page shows why, and the user fixes or deletes it.
            Stored::Invalid(_) => return Err(StatusCode::CONFLICT),
        };
        change(&mut overrides);
        super::settings::prune_defaults(&mut overrides, &super::settings::defaults());
        match save(&path, &overrides) {
            Ok(()) => Ok(settings_json(snapshot(&path))),
            Err(SaveError::TooLarge) => Err(StatusCode::PAYLOAD_TOO_LARGE),
            Err(SaveError::Io(e)) => Err(internal(e.context("save gallery settings"))),
        }
    })
    .await
    .map_err(internal)?
}

/// `PATCH /api/settings`: an RFC 7396 merge patch over the stored overrides.
/// `null` removes a key, which reverts it to its default.
async fn handle_patch_settings(
    State(state): State<Arc<AppState>>,
    AxumJson(patch): AxumJson<serde_json::Value>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    if !patch.is_object() {
        return Err(StatusCode::BAD_REQUEST);
    }
    write_settings(state, move |o| super::settings::merge_patch(o, &patch)).await
}

/// `PUT /api/settings`: import. Replaces `routes` wholesale and keeps the
/// rest, so importing another library's settings never moves where this one
/// resumes.
async fn handle_put_settings(
    State(state): State<Arc<AppState>>,
    AxumJson(body): AxumJson<serde_json::Value>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let Some(routes) = body.get("routes").filter(|r| r.is_object()).cloned() else {
        return Err(StatusCode::BAD_REQUEST);
    };
    write_settings(state, move |o| {
        o.as_object_mut()
            .expect("stored overrides are an object")
            .insert("routes".into(), routes);
    })
    .await
}

#[derive(Deserialize)]
struct NewPersonRequest {
    face_ids: Vec<i64>,
    name: String,
}

/// Faces to attach to a person: `PUT /api/people/{name}/faces`. The person comes
/// from the path.
#[derive(Deserialize)]
struct AssignFacesBody {
    face_ids: Vec<i64>,
}

/// Changing what a person is shown as, without touching their identity. The
/// identity comes from the path (`PATCH /api/people/{name}`).
#[derive(Deserialize)]
struct SetFullNameBody {
    full_name: String,
}

/// Which person a face is the primary for: `PATCH /api/faces/{id}`. The face id
/// comes from the path.
#[derive(Deserialize)]
struct SetPrimaryBody {
    person_label: String,
}

#[derive(Deserialize)]
struct PersonSearchQuery {
    name: String,
}

/// The body of `PATCH /api/files/{hash}`. Every field is optional: only those present are
/// changed, matching the partial-update semantics of `videre mark`. `rating` 0
/// clears; `pick`/`label` `"none"` clears.
#[derive(Deserialize)]
struct MarkBody {
    rating: Option<i64>,
    pick: Option<String>,
    label: Option<String>,
    liked: Option<bool>,
}

pub(crate) struct AppState {
    pub(super) conn: Arc<Mutex<Connection>>,
    events_cache: Mutex<Option<EventsCacheEntry>>,
    /// Background face-learning worker; present whenever people pages are
    /// served. Teaching mutations notify it after their commit.
    pub(super) learning: Option<super::learning::LearningCoordinator>,
    shutdown_tx: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    pub(super) model_id: String,
    report_heic: bool,
    report_heic_original: bool,
    serve_faces_ui: bool,
    /// `videre gallery` rather than a report or labeling server. The one
    /// configuration where `/`, `/date` and `/people` all exist, so the one
    /// where a section strip can link to them.
    gallery: bool,
    /// The startup-bound library context. A ranking search runs through
    /// `search::run_json_in` against this, and a location lookup geocodes into
    /// this library's cache, so no request can retarget the server at another
    /// library.
    pub(super) context: Arc<crate::command_context::CommandContext>,
    /// Loaded on the first ranking search and kept for the process's life. Empty
    /// until then: a gallery whose library nobody searches never loads a model.
    embedder: Mutex<Option<videre_ml::model::Embedder>>,
    /// Set while a basemap download is in flight, so a second `POST
    /// /api/basemap/ensure` returns the current status instead of launching a
    /// second download of the same once-per-machine archive.
    basemap_downloading: Arc<std::sync::atomic::AtomicBool>,
    /// Serializes read-modify-write of `.videre/gallery.json`, so two saves
    /// from two tabs cannot interleave and drop one of them.
    pub(super) settings_lock: Arc<Mutex<()>>,
    /// Whether the current unreadable-settings error has been logged, so a
    /// broken `gallery.json` warns once rather than on every page render.
    settings_warned: Arc<std::sync::atomic::AtomicBool>,
}

struct EventsCacheEntry {
    version: (i64, i64),
    detection: Arc<super::events::Detection>,
}

/// The labeling UI. Served as `/people` under `videre gallery`, and as `/` on a
/// labeling-only server, which is why the nav strip depends on state rather than
/// on the route: only the former has anywhere to navigate to.
async fn handle_root(State(state): State<Arc<AppState>>) -> impl axum::response::IntoResponse {
    use askama::Template;
    use chrono::Utc;
    // The same header the other pages carry. It had none, so the labeling page
    // announced itself in a bare toolbar while every other route showed which
    // library it was looking at.
    let (db_path, total_files) = {
        let conn = state.conn.lock().unwrap();
        let total = conn
            .query_row("SELECT COUNT(*) FROM file_hashes", [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap_or(0);
        (
            conn.path().map(|p| p.to_string()).unwrap_or_default(),
            total,
        )
    };
    let page = pages::Faces {
        settings_script: page_settings(&state).await,
        chrome: CHROME_CSS,
        css: pages::FACES_CSS,
        js: pages::FACES_JS,
        db: esc(&db_path),
        generated_at: Utc::now().format("%Y-%m-%d %H:%M UTC").to_string(),
        total_files,
        nav: state.gallery.then_some(Section::People),
        people_root: people_root(state.gallery),
    };
    axum::response::Html(page.render().expect("faces template"))
}

/// `videre gallery`'s `/`: every file, with in-page similarity search.
async fn handle_gallery_all(
    State(state): State<Arc<AppState>>,
) -> impl axum::response::IntoResponse {
    render_live(
        &state,
        true,
        false,
        false,
        Some(Section::All),
        page_settings(&state).await,
    )
}

/// `videre gallery`'s `/duplicates`: duplicate groups, the review
/// `dedupe --html` writes to a file.
async fn handle_gallery_duplicates(
    State(state): State<Arc<AppState>>,
) -> impl axum::response::IntoResponse {
    render_live(
        &state,
        false,
        false,
        true,
        Some(Section::Duplicates),
        page_settings(&state).await,
    )
}

/// `videre gallery`'s `/date`: the year/month/day drill-down over KEEP files.
async fn handle_gallery_date(
    State(state): State<Arc<AppState>>,
    Query(q): Query<DatePageQuery>,
) -> Result<axum::response::Response, StatusCode> {
    let filter = normalize_date_range(
        q.from.as_deref(),
        q.to.as_deref(),
        chrono::Local::now().date_naive(),
    )?;
    render_live_date(&state, filter, page_settings(&state).await)
}

#[derive(Deserialize)]
struct DatePageQuery {
    from: Option<String>,
    to: Option<String>,
}

async fn handle_gallery_date_year(
    axum::extract::Path(year): axum::extract::Path<String>,
    State(state): State<Arc<AppState>>,
) -> Result<axum::response::Response, StatusCode> {
    let filter = route_date_filter(&year, None, None)?;
    render_live_date(&state, filter, page_settings(&state).await)
}

async fn handle_gallery_date_month(
    axum::extract::Path((year, month)): axum::extract::Path<(String, String)>,
    State(state): State<Arc<AppState>>,
) -> Result<axum::response::Response, StatusCode> {
    let filter = route_date_filter(&year, Some(&month), None)?;
    render_live_date(&state, filter, page_settings(&state).await)
}

async fn handle_gallery_date_day(
    axum::extract::Path((year, month, day)): axum::extract::Path<(String, String, String)>,
    State(state): State<Arc<AppState>>,
) -> Result<axum::response::Response, StatusCode> {
    let filter = route_date_filter(&year, Some(&month), Some(&day))?;
    render_live_date(&state, filter, page_settings(&state).await)
}

#[derive(Debug, Clone, PartialEq)]
struct ResolvedMapLocation {
    id: i64,
    route_name: String,
    display_name: String,
    centroid_lat: f64,
    centroid_lon: f64,
    photo_count: i64,
    radius_km: f64,
}

fn map_locations(conn: &Connection) -> rusqlite::Result<Vec<ResolvedMapLocation>> {
    videre_core::location_cluster::ensure_location_clusters_table(conn)?;
    let mut stmt = conn.prepare(
        "SELECT id, COALESCE(name, 'Unnamed location'), centroid_lat, centroid_lon,
                photo_count, radius_km
         FROM location_clusters ORDER BY photo_count DESC, id ASC",
    )?;
    let rows = stmt
        .query_map([], |row| {
            let display_name = row.get::<_, String>(1)?;
            let route_name = videre_core::person::normalize(&display_name)
                .unwrap_or_else(|| "unnamed_location".to_string());
            Ok(ResolvedMapLocation {
                id: row.get(0)?,
                route_name,
                display_name,
                centroid_lat: row.get(2)?,
                centroid_lon: row.get(3)?,
                photo_count: row.get(4)?,
                radius_km: row.get(5)?,
            })
        })?
        .collect();
    rows
}

fn resolve_map_location(
    conn: &Connection,
    segment: &str,
) -> rusqlite::Result<Option<ResolvedMapLocation>> {
    let Some(wanted) = videre_core::person::normalize(segment) else {
        return Ok(None);
    };
    Ok(map_locations(conn)?
        .into_iter()
        .find(|location| location.route_name == wanted))
}

/// The location cluster nearest a point, by haversine distance to its centroid.
///
/// The lightbox forwards a photo to the map by its own coordinates, not by a
/// place name: a photo's reverse-geocoded name (`Schöneberg, DE`) is finer than
/// any cluster's name (`Berlin`), so matching names never resolves. Resolving to
/// the nearest cluster here turns the photo's point into a cluster the map can
/// actually select. `None` only when the library has no clusters at all.
fn nearest_map_location(
    conn: &Connection,
    lat: f64,
    lon: f64,
) -> rusqlite::Result<Option<ResolvedMapLocation>> {
    Ok(map_locations(conn)?.into_iter().min_by(|a, b| {
        let da =
            videre_core::location_cluster::haversine_km(lat, lon, a.centroid_lat, a.centroid_lon);
        let db =
            videre_core::location_cluster::haversine_km(lat, lon, b.centroid_lat, b.centroid_lon);
        da.total_cmp(&db)
    }))
}

/// Parse a `"lat,lon"` pair as the lightbox writes it into `/map?near=`. Rejects
/// anything that is not two finite numbers in range.
fn parse_lat_lon(value: &str) -> Option<(f64, f64)> {
    let (lat, lon) = value.split_once(',')?;
    let lat: f64 = lat.trim().parse().ok()?;
    let lon: f64 = lon.trim().parse().ok()?;
    if lat.is_finite()
        && lon.is_finite()
        && (-90.0..=90.0).contains(&lat)
        && (-180.0..=180.0).contains(&lon)
    {
        Some((lat, lon))
    } else {
        None
    }
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum MapLocationBootstrap {
    Location { name: String, radius: f64 },
    Unknown { name: String },
}

#[derive(Deserialize)]
struct MapPageQuery {
    radius: Option<f64>,
}

fn render_map(
    state: &AppState,
    location_json: &str,
    settings_script: String,
) -> axum::response::Html<String> {
    use askama::Template;

    // Compute HAS_EMBEDDINGS exactly as the `/` handler does, so the Similar
    // button (and the nav search box) gate identically: an embedded library must
    // offer them on the grid under the map too, not just on the files page.
    let has_embeddings = {
        let conn = state.conn.lock().unwrap();
        query_embedded_count(&conn, &state.model_id).is_some_and(|n| n > 0)
    };
    let globals = format!(
        "var LIVE_SERVER=true;\nvar HAS_EMBEDDINGS={};\nvar VIDEO_POSTERS={};\n\
         var GVIEW=\"all\";\nvar PEOPLE_ROOT=\"/people\";\nvar GDATE=null;\nvar GLOC={location_json};\nvar GROUPS=[];",
        has_embeddings,
        cfg!(target_os = "macos")
    );
    let page = pages::Map {
        settings_script,
        chrome: CHROME_CSS,
        gallery_css: include_str!("../../../static/gallery.css"),
        css: pages::MAP_CSS,
        justified_js: include_str!("../../../static/justified-layout.js"),
        gallery_js: crate::render::GALLERY_JS,
        js: pages::MAP_JS,
        basemap_style: pages::BASEMAP_STYLE,
        vendor_version: env!("CARGO_PKG_VERSION"),
        globals,
        nav: Some(Section::Map),
    };
    axum::response::Html(page.render().expect("map template"))
}

#[derive(Deserialize)]
struct MapHomeQuery {
    /// A `"lat,lon"` pair the lightbox forwards from a photo. Resolved to the
    /// nearest cluster server-side, so the client bootstraps as if that cluster
    /// were selected and canonicalises the URL to `/map/location/<name>`.
    near: Option<String>,
}

async fn handle_map(
    Query(query): Query<MapHomeQuery>,
    State(state): State<Arc<AppState>>,
) -> axum::response::Html<String> {
    let json = match query.near.as_deref().and_then(parse_lat_lon) {
        Some((lat, lon)) => {
            let resolved = {
                match state.conn.lock() {
                    Ok(conn) => nearest_map_location(&conn, lat, lon).ok().flatten(),
                    Err(_) => None,
                }
            };
            match resolved {
                Some(location) => serde_json::to_string(&MapLocationBootstrap::Location {
                    name: location.route_name,
                    radius: location.radius_km,
                })
                .unwrap_or_else(|_| "null".to_string()),
                None => "null".to_string(),
            }
        }
        None => "null".to_string(),
    };
    render_map(&state, &json, page_settings(&state).await)
}

async fn handle_map_location(
    axum::extract::Path(name): axum::extract::Path<String>,
    Query(query): Query<MapPageQuery>,
    State(state): State<Arc<AppState>>,
) -> Result<axum::response::Html<String>, StatusCode> {
    let resolved = {
        let conn = state.conn.lock().map_err(poisoned)?;
        resolve_map_location(&conn, &name).map_err(internal)?
    };
    let bootstrap = match resolved {
        // The radius only matters for a resolved location, so validate it here:
        // an unknown name renders the honest 200 page whatever the radius is,
        // rather than turning an invalid radius into a 400 for a place the map
        // never had.
        Some(location) => {
            let radius = match query.radius {
                Some(radius) if radius.is_finite() && radius > 0.0 => Some(radius),
                Some(_) => return Err(StatusCode::BAD_REQUEST),
                None => None,
            };
            MapLocationBootstrap::Location {
                name: location.route_name,
                radius: radius.unwrap_or(location.radius_km),
            }
        }
        None => MapLocationBootstrap::Unknown {
            name: videre_core::person::normalize(&name).unwrap_or_default(),
        },
    };
    let json = serde_json::to_string(&bootstrap).map_err(internal)?;
    Ok(render_map(&state, &json, page_settings(&state).await))
}

#[derive(Deserialize)]
struct SearchQuery {
    /// A text query, ranked semantically.
    q: Option<String>,
    /// A hash already in this library: "more like this one".
    like: Option<String>,
    limit: Option<usize>,
}

/// Rank the library, and return only a ranking.
///
/// :warning: **Rows are deliberately not returned here.** The client already
/// fetches rows by hash through `/api/files?hashes=`, so returning them would be
/// a second row-shaping path to keep in step with the first. Search answers
/// "which, in what order"; `/api/files` answers "what are they".
///
/// This is the third caller of the seam `commands/mcp.rs` opened: build a
/// `SearchArgs`, hand it to `search::run_json`. Nothing about search is
/// implemented here, which is the point - the CLI, MCP and the gallery rank
/// identically because they run the same code.
async fn handle_search(
    State(state): State<Arc<AppState>>,
    Query(sq): Query<SearchQuery>,
) -> Result<axum::response::Response, StatusCode> {
    const MAX_LIMIT: usize = 200;

    if sq.q.is_none() == sq.like.is_none() {
        return Err(StatusCode::BAD_REQUEST);
    }
    let top_k = sq.limit.unwrap_or(24).clamp(1, MAX_LIMIT);
    // Kept out of the closure: needed again below to drop the example from its
    // own results.
    let example = sq.like.clone();

    // :warning: `spawn_blocking`, because this is the one handler that can take
    // ~900ms: the first ranking query loads the model. Every other handler here
    // does milliseconds of SQLite and gets away with blocking the runtime; this
    // one would stall every concurrent request, thumbnails included.
    let hits = tokio::task::spawn_blocking(move || {
        let args = crate::commands::search::SearchArgs {
            html: None,
            media: crate::commands::selection_args::MediaArgs::default(),
            paths: crate::commands::selection_args::PathArgs::default(),
            presence: crate::commands::selection_args::PresenceArgs::default(),
            marks: crate::commands::selection_args::MarkArgs::default(),
            tags: Default::default(),
            // Bound at startup, so a request cannot retarget the server.
            model: Some(state.model_id.clone()),
            query: sq.q.clone(),
            image: None,
            like: sq.like.clone(),
            person: None,
            category: None,
            location: None,
            radius: 20.0,
            after: None,
            before: None,
            date: None,
            sort: None,
            top_k,
            scores: false,
            json: true,
        };
        crate::commands::search::run_json_in(
            &args,
            &crate::commands::search::CachedEmbedder(&state.embedder),
            &state.context,
        )
    })
    .await
    .map_err(internal)?;

    let hits = match hits {
        Ok(h) => h,
        Err(e) => {
            return Err(internal(anyhow::anyhow!("/api/search failed: {e}")));
        }
    };

    // :warning: An example never appears among its own neighbours. Ranking a
    // stored vector against the corpus it came from scores it ~1.0 against
    // itself, and "things like this one" that begins with this one wastes the
    // first and best slot. The in-page version skipped its own index for the
    // same reason; the UI shows the query separately.
    let results: Vec<_> = hits
        .results
        .iter()
        .filter(|h| h.hash.as_deref() != example.as_deref())
        .collect();

    let mut out = format!("{{\"total\":{},\"results\":[", hits.total_matches);
    for (i, hit) in results.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&format!(
            "{{\"hash\":{},\"score\":{}}}",
            json_str(hit.hash.as_deref().unwrap_or("")),
            hit.score.unwrap_or(0.0)
        ));
    }
    out.push_str("]}");
    Ok(json_response(out))
}

/// Reserved so the shape of the command is visible before the views exist.
async fn handle_not_yet() -> impl axum::response::IntoResponse {
    (
        StatusCode::NOT_FOUND,
        axum::response::Html(
            "<!DOCTYPE html><html><head><meta charset=\"UTF-8\"><title>videre</title></head>\
             <body style=\"font-family:-apple-system,sans-serif;padding:48px\">\
             <h1>Not built yet</h1>\
             <p>This view is reserved. See <a href=\"/\">all files</a>.</p>\
             </body></html>",
        ),
    )
}

/// Which sections a live page renders is a property of the route, not of the
/// server, so `/` and `/date` can differ within one `gallery`.
///
/// `nav` is `Some` only under `videre gallery`, which is the one configuration
/// that routes `/`, `/date` and `/people` together. The other servers reach this
/// through `/` alone, where a strip of links to routes that do not exist would
/// 404 on every entry but the one you are on.
fn render_live(
    state: &Arc<AppState>,
    all: bool,
    by_date: bool,
    with_groups: bool,
    nav: Option<Section>,
    settings_script: String,
) -> axum::response::Html<String> {
    render_live_with_date(
        state,
        all,
        by_date,
        with_groups,
        nav,
        "null",
        settings_script,
    )
}

fn render_live_date(
    state: &Arc<AppState>,
    filter: InitialDateFilter,
    settings_script: String,
) -> Result<axum::response::Response, StatusCode> {
    use axum::response::IntoResponse;

    Ok(render_live_with_date(
        state,
        false,
        true,
        false,
        Some(Section::Date),
        &initial_date_filter_json(&filter),
        settings_script,
    )
    .into_response())
}

fn initial_date_filter_json(filter: &InitialDateFilter) -> String {
    match filter {
        InitialDateFilter::Home => "null".to_string(),
        InitialDateFilter::Prefix(value) => {
            format!("{{\"kind\":\"prefix\",\"value\":{}}}", json_str(value))
        }
        InitialDateFilter::Range { from, to } => {
            let from = from
                .as_deref()
                .map(json_str)
                .unwrap_or_else(|| "null".to_string());
            let to = to
                .as_deref()
                .map(json_str)
                .unwrap_or_else(|| "null".to_string());
            format!("{{\"kind\":\"range\",\"from\":{from},\"to\":{to}}}")
        }
    }
}

fn render_live_with_date(
    state: &Arc<AppState>,
    all: bool,
    by_date: bool,
    with_groups: bool,
    nav: Option<Section>,
    date_filter_json: &str,
    settings_script: String,
) -> axum::response::Html<String> {
    let conn = state.conn.lock().unwrap();
    let stats = query_stats(&conn);
    // :warning: Only `/duplicates` builds duplicate groups. `/` used to carry them
    // as well, and they are inlined into the page rather than fetched, so the
    // default route grew with the number of duplicates in the library. That is
    // the same fault the file list had, on the one page everybody lands on.
    let groups = if with_groups {
        query_groups(&conn)
    } else {
        Vec::new()
    };
    // `all_files` and `keep_files` came from different queries; the view picks
    // one. `with_groups`, `all` and `by_date` are mutually exclusive per route.
    let items = if all {
        query_all_files(&conn)
    } else if by_date {
        query_keep_files(&conn)
    } else {
        Vec::new()
    };
    let view = if with_groups {
        View::Duplicates
    } else if by_date {
        View::Date
    } else {
        View::All
    };
    let faces_by_hash = videre_core::face_db::labeled_faces_by_hash(&conn).unwrap_or_default();
    let embedded = if all {
        query_embedded_count(&conn, &state.model_id)
    } else {
        None
    };
    let db_path = conn.path().map(|p| p.to_string()).unwrap_or_default();
    drop(conn);
    let set = RenderSet {
        stats,
        items,
        groups,
        faces_by_hash,
        // Live pages inline no rows, so the marks map is never read.
        marks_by_hash: Default::default(),
        nav,
        view,
        options: RenderOptions {
            live: true,
            heic: state.report_heic,
            heic_original: state.report_heic_original,
            embedded,
            db_path,
            date_filter_json: date_filter_json.to_string(),
            event_json: "null".to_string(),
            settings_script,
        },
    };
    axum::response::Html(render(&set))
}

#[derive(Deserialize)]
struct LocationQuery {
    lat: f64,
    lon: f64,
}

#[derive(Serialize)]
struct LocationResponse {
    name: Option<String>,
}

async fn handle_location(
    Query(q): Query<LocationQuery>,
    State(state): State<Arc<AppState>>,
) -> Result<AxumJson<LocationResponse>, StatusCode> {
    // Reads the cache, geocodes into it and writes location_name back, so it
    // takes the same per-operation guard as the image routes.
    let _guard = guard_operation(&state)?;
    let conn = state.conn.lock().map_err(poisoned)?;
    // `q.lat`/`q.lon` arrive rounded to 6 decimal places (the precision
    // `file_to_json_with_faces` bakes into `meta.location` client-side), but
    // `file_hashes.gps_lat`/`gps_lon` are stored at full EXIF precision, an
    // exact float comparison would never match, silently breaking the cache
    // on every real coordinate. Round the stored value to the same precision
    // before comparing, both when reading and when writing back the cache.
    let cached: Option<String> = conn
        .query_row(
            "SELECT location_name FROM file_hashes \
             WHERE ROUND(gps_lat, 6) = ?1 AND ROUND(gps_lon, 6) = ?2 \
             AND location_name IS NOT NULL LIMIT 1",
            rusqlite::params![q.lat, q.lon],
            |r| r.get(0),
        )
        .optional()
        .map_err(internal)?;
    if let Some(name) = cached {
        return Ok(AxumJson(LocationResponse { name: Some(name) }));
    }
    // Best-effort, like the old ambient lookup: a geocoder failure yields no
    // name and no cache write rather than a request error.
    let name = videre_core::location::location_name_in(&state.context.library.cache, q.lat, q.lon)
        .ok()
        .flatten();
    if let Some(ref n) = name {
        let _ = conn.execute(
            "UPDATE file_hashes SET location_name = ?1 \
             WHERE ROUND(gps_lat, 6) = ?2 AND ROUND(gps_lon, 6) = ?3",
            rusqlite::params![n, q.lat, q.lon],
        );
    }
    Ok(AxumJson(LocationResponse { name }))
}

/// One page of the file list, for a live gallery that fetches rather than
/// carries its rows.
///
/// :warning: `limit` is capped. An unbounded `limit` would let a client ask for
/// the whole library in one request, which is the exact page this endpoint
/// exists to stop being built.
async fn handle_files(
    State(state): State<Arc<AppState>>,
    Query(q): Query<FilesQuery>,
) -> Result<axum::response::Response, StatusCode> {
    const MAX_LIMIT: i64 = 500;

    let view = q.view.as_deref().unwrap_or("all");
    let offset = q.offset.unwrap_or(0).max(0);
    let limit = q.limit.unwrap_or(200).clamp(1, MAX_LIMIT);

    let conn = state.conn.lock().map_err(poisoned)?;
    let faces_by_hash = videre_core::face_db::labeled_faces_by_hash(&conn).unwrap_or_default();
    let (rows, total) = match q.hashes.as_deref() {
        Some(h) if !h.is_empty() => query_files_by_hash(&conn, h).map_err(internal)?,
        _ => {
            let date_filter = if let Some(date) = q.date.as_deref().filter(|s| !s.is_empty()) {
                validate_partial_date(date).ok_or(StatusCode::BAD_REQUEST)?;
                Some(FileDateFilter::Prefix(date.to_string()))
            } else if q.from.is_some() || q.to.is_some() {
                match normalize_date_range(
                    q.from.as_deref(),
                    q.to.as_deref(),
                    chrono::Local::now().date_naive(),
                )? {
                    InitialDateFilter::Range { from, to } => {
                        Some(FileDateFilter::Range { from, to })
                    }
                    InitialDateFilter::Prefix(value) => Some(FileDateFilter::Prefix(value)),
                    InitialDateFilter::Home => None,
                }
            } else {
                None
            };
            let location = files_location_filter(&q, view)?;
            let sort = FileSort::from_query(q.sort.as_deref(), q.dir.as_deref());
            query_files_page(
                &conn,
                view,
                date_filter.as_ref(),
                offset,
                limit,
                location.as_ref(),
                sort,
            )
            .map_err(internal)?
        }
    };

    Ok(files_json_response(
        &conn,
        &rows,
        total,
        offset,
        &faces_by_hash,
    ))
}

/// Build the `{total,offset,files:[...]}` body shared by `/api/files` and the
/// per-event files endpoint, splicing each row's duplicate count and marks in.
fn files_json_response(
    conn: &Connection,
    rows: &[(FileRow, i64)],
    total: i64,
    offset: i64,
    faces_by_hash: &videre_core::face_db::LabeledFacesByHash,
) -> axum::response::Response {
    let page_hashes: Vec<String> = rows.iter().map(|(r, _)| r.hash.clone()).collect();
    let marks = videre_core::marks::get_many(conn, &page_hashes).unwrap_or_default();

    let mut out = String::from("{\"total\":");
    out.push_str(&total.to_string());
    out.push_str(",\"offset\":");
    out.push_str(&offset.to_string());
    out.push_str(",\"files\":[");
    for (i, (row, copies)) in rows.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        // Same row shape the page inlines today, so the client renders a fetched
        // row and an inlined one with one code path.
        let mut obj = file_to_json_with_faces(
            row,
            false,
            false,
            faces_by_hash
                .get(&row.hash)
                .map(|v| v.as_slice())
                .unwrap_or(&[]),
            true,
        );
        // Splice `copies` and the marks in rather than widening FileRow, which
        // the static export also builds and has no use for either.
        if obj.ends_with('}') {
            obj.truncate(obj.len() - 1);
            obj.push_str(&format!(",\"copies\":{copies}"));
            obj.push_str(&mark_fields_json(marks.get(&row.hash)));
            obj.push('}');
        }
        out.push_str(&obj);
    }
    out.push_str("]}");

    json_response(out)
}

/// `GET /api/events/{key}/files`: the rows of one event, by its exact members.
async fn handle_events_files(
    axum::extract::Path(key): axum::extract::Path<String>,
    State(state): State<Arc<AppState>>,
) -> Result<axum::response::Response, StatusCode> {
    let conn = state.conn.lock().map_err(poisoned)?;
    let detection = cached_events(&state, &conn)?;
    let event = detection
        .events
        .iter()
        .find(|e| e.key() == key)
        .ok_or(StatusCode::NOT_FOUND)?;
    let (files, total) = query_event_files(&conn, &event.members).map_err(internal)?;
    let faces_by_hash = videre_core::face_db::labeled_faces_by_hash(&conn).unwrap_or_default();
    Ok(files_json_response(&conn, &files, total, 0, &faces_by_hash))
}

/// Returns every persisted location cluster with its derived continent,
/// ordered largest first for the map legend and world overview.
async fn handle_location_clusters(State(state): State<Arc<AppState>>) -> Response {
    let conn = match state.conn.lock() {
        Ok(conn) => conn,
        Err(e) => return poisoned(e).into_response(),
    };
    let rows = match map_locations(&conn) {
        Ok(rows) => rows,
        Err(e) => return internal(e).into_response(),
    };

    let mut out = String::from("[");
    for (index, row) in rows.into_iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        let continent =
            videre_core::location_cluster::continent_of(row.centroid_lat, row.centroid_lon);
        out.push_str(&format!(
            "{{\"cluster_id\":{},\"name\":{},\"route_name\":{},\"centroid_lat\":{},\
             \"centroid_lon\":{},\"photo_count\":{},\"radius_km\":{},\
             \"continent\":{}}}",
            row.id,
            json_str(&row.display_name),
            json_str(&row.route_name),
            row.centroid_lat,
            row.centroid_lon,
            row.photo_count,
            row.radius_km,
            json_str(continent),
        ));
    }
    out.push(']');
    json_response(out)
}

/// `GET /vendor/{version}/{asset}`: the vendored map libraries (MapLibre GL JS,
/// its CSS, and the pmtiles protocol plugin), compiled into the binary and
/// served only here so `map.html` can load ~1 MB of script off the page rather
/// than inline it into every gallery view. The response is immutable and
/// long-lived; the `{version}` segment (videre's own version, emitted by
/// `map.html`) is a cache buster, so an upgrade fetches fresh bytes on the same
/// port instead of reusing a year-old bundle. The segment is not validated: the
/// bytes are identical for any value.
async fn handle_vendor_asset(
    axum::extract::Path((_version, asset)): axum::extract::Path<(String, String)>,
) -> Response {
    let (body, content_type): (&'static str, &'static str) = match asset.as_str() {
        "maplibre-gl.js" => (pages::MAPLIBRE_JS, "text/javascript; charset=utf-8"),
        "maplibre-gl.css" => (pages::MAPLIBRE_CSS, "text/css; charset=utf-8"),
        "pmtiles.js" => (pages::PMTILES_JS, "text/javascript; charset=utf-8"),
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    (
        [
            (axum::http::header::CONTENT_TYPE, content_type),
            (
                axum::http::header::CACHE_CONTROL,
                "public, max-age=31536000, immutable",
            ),
        ],
        body,
    )
        .into_response()
}

/// Delete every cached preview for one content hash, so the next request
/// re-renders it. All caches (HEIC thumbnail, raster preview, video poster,
/// full conversion, face crops) are named `<hash>_...` in the one thumbnails
/// directory, so a prefix sweep covers them.
fn invalidate_thumb_cache(cache: &videre_core::library::CachePaths, hash: &str) {
    let prefix = format!("{hash}_");
    if let Ok(entries) = std::fs::read_dir(&cache.thumbnails) {
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().starts_with(&prefix) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

#[derive(Deserialize)]
struct RotateQuery {
    /// `cw` (default) or `ccw`: which way to turn. The lightbox has a button for
    /// each. Anything else is treated as `cw`.
    dir: Option<String>,
}

/// `POST /api/files/{hash}/rotate`: rotate one photo 90 degrees (clockwise by
/// default, counter-clockwise with `?dir=ccw`) by bumping its EXIF Orientation
/// tag in place, then drop its cached previews so the grid and lightbox
/// re-render. Refuses a format that carries no EXIF orientation (video, HEIC,
/// and the like) with 415, matching the button the gallery only shows for
/// supported images.
async fn handle_rotate_file(
    axum::extract::Path(hash): axum::extract::Path<String>,
    Query(query): Query<RotateQuery>,
    State(state): State<Arc<AppState>>,
) -> Response {
    let ccw = query.dir.as_deref() == Some("ccw");
    let hash_for_task = hash.clone();
    let result = tokio::task::spawn_blocking(move || rotate_one(&state, &hash_for_task, ccw)).await;
    match result {
        Ok(Ok(RotateOutcome::Rotated(orientation))) => {
            json_response(format!("{{\"orientation\":{orientation}}}"))
        }
        Ok(Ok(RotateOutcome::NotFound)) => StatusCode::NOT_FOUND.into_response(),
        Ok(Ok(RotateOutcome::Unsupported)) => (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "rotation is only supported for EXIF-bearing images",
        )
            .into_response(),
        Ok(Err(e)) => internal(anyhow::anyhow!("rotate failed for {hash}: {e}")).into_response(),
        Err(e) => internal(e).into_response(),
    }
}

pub(super) enum RotateOutcome {
    Rotated(u16),
    NotFound,
    /// Not an image that carries an EXIF orientation (videos, DNG, ...).
    Unsupported,
}

/// Turn one photo a quarter turn: the EXIF orientation, its face geometry and
/// its cached thumbnails. Blocking; shared by the lightbox's rotate and the
/// selection bar's.
pub(super) fn rotate_one(state: &AppState, hash: &str, ccw: bool) -> anyhow::Result<RotateOutcome> {
    let path: Option<String> = {
        let conn = state
            .conn
            .lock()
            .map_err(|_| anyhow::anyhow!("the database connection lock is poisoned"))?;
        conn.query_row(
            "SELECT path FROM file_hashes WHERE hash = ?1 LIMIT 1",
            [hash],
            |r| r.get::<_, String>(0),
        )
        .optional()?
    };
    let Some(path) = path else {
        return Ok(RotateOutcome::NotFound);
    };
    let ext = std::path::Path::new(&path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    if !super::rotate::supports_exif_orientation(&ext) {
        return Ok(RotateOutcome::Unsupported);
    }
    let source = std::path::Path::new(&path);
    // The stored face bboxes/landmarks are in the display canvas as it
    // decodes now; capture that canvas's dimensions before the turn so the
    // geometry can be mapped onto the post-rotation canvas. Read them up
    // front: the rotation bumps the EXIF the dimensions depend on.
    let dims = super::rotate::current_display_dimensions(source);
    let orientation = if ccw {
        super::rotate::rotate_ccw_in_place(source, &ext)?
    } else {
        super::rotate::rotate_cw_in_place(source, &ext)?
    };
    // Turn the display-canvas face geometry with the photo, so a rotated
    // photo's face crops stay on their faces and keep their people labels
    // instead of cropping the pre-rotation region of the new canvas.
    if let Some((display_w, display_h)) = dims {
        rotate_faces_geometry(state, hash, ccw, display_w as i32, display_h as i32);
    }
    invalidate_thumb_cache(&state.context.library.cache, hash);
    Ok(RotateOutcome::Rotated(orientation))
}

/// Turn every face row for `hash` 90 degrees to
/// match one rotation of the photo: clockwise on a canvas of height `display_h`,
/// or counter-clockwise on a canvas of width `display_w` when `ccw`. Best-effort:
/// a row whose bbox or landmark will not parse is left untouched rather than
/// corrupted, and a DB error is logged, since the rotation itself has already
/// succeeded and the caller reports that.
fn rotate_faces_geometry(state: &AppState, hash: &str, ccw: bool, display_w: i32, display_h: i32) {
    // The file itself was rotated already, so a failure here leaves face
    // boxes stale: logged at error, where the primary log keeps it.
    let failed = |what: String, e: anyhow::Error| {
        videre_core::error_log::report(tracing::Level::ERROR, &e.context(what), None)
    };
    let conn = match state.conn.lock() {
        Ok(conn) => conn,
        Err(_) => {
            failed(
                format!("videre gallery: rotating face geometry for {hash}"),
                anyhow::anyhow!("the database connection lock is poisoned by an earlier panic"),
            );
            return;
        }
    };
    let rows: Vec<(i64, String, Option<String>)> = {
        let mut stmt = match conn.prepare("SELECT id, bbox, landmark FROM faces WHERE hash = ?1") {
            Ok(stmt) => stmt,
            Err(e) => {
                failed(
                    format!("videre gallery: reading faces for rotate {hash}"),
                    e.into(),
                );
                return;
            }
        };
        let mapped = stmt.query_map([hash], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
            ))
        });
        match mapped {
            // A row that cannot be read keeps its old geometry: say so, once
            // per row, and carry on with the rest.
            Ok(iter) => iter
                .filter_map(|r| match r {
                    Ok(row) => Some(row),
                    Err(e) => {
                        failed(
                            format!("videre gallery: reading faces for rotate {hash}"),
                            e.into(),
                        );
                        None
                    }
                })
                .collect(),
            Err(e) => {
                failed(
                    format!("videre gallery: reading faces for rotate {hash}"),
                    e.into(),
                );
                return;
            }
        }
    };
    for (id, bbox, landmark) in rows {
        let new_bbox = if ccw {
            super::rotate::rotate_bbox_ccw(&bbox, display_w)
        } else {
            super::rotate::rotate_bbox_cw(&bbox, display_h)
        };
        let Some(new_bbox) = new_bbox else {
            continue;
        };
        // Keep the original landmark if it cannot be transformed, rather than
        // nulling a column the crop does not use but clustering does.
        let new_landmark = landmark.as_deref().map(|l| {
            let turned = if ccw {
                super::rotate::rotate_landmark_ccw(l, display_w as f32)
            } else {
                super::rotate::rotate_landmark_cw(l, display_h as f32)
            };
            turned.unwrap_or_else(|| l.to_string())
        });
        if let Err(e) = conn.execute(
            "UPDATE faces SET bbox = ?1, landmark = ?2 WHERE id = ?3",
            rusqlite::params![new_bbox, new_landmark, id],
        ) {
            failed(
                format!("videre gallery: updating face {id} geometry for rotate"),
                e.into(),
            );
        }
    }
}

/// The basemap archive path this server resolves: the per-library override
/// when present (how tests seed one), else the machine-shared geo cache.
fn basemap_archive_path(state: &AppState) -> std::path::PathBuf {
    videre_core::basemap::archive_path(
        &state.context.library.cache.geo,
        &state.context.library.paths.state,
    )
}

/// The `{"state":..,"bytes":n}` body the client polls, mapped straight from
/// the on-disk archive status.
fn basemap_status_json(path: &Path) -> String {
    match videre_core::basemap::status(path) {
        videre_core::basemap::BasemapStatus::Absent => {
            "{\"state\":\"absent\",\"bytes\":0}".to_string()
        }
        videre_core::basemap::BasemapStatus::Partial { bytes } => {
            format!("{{\"state\":\"partial\",\"bytes\":{bytes}}}")
        }
        videre_core::basemap::BasemapStatus::Ready { bytes } => {
            format!("{{\"state\":\"ready\",\"bytes\":{bytes}}}")
        }
    }
}

/// `GET /tiles/basemap.pmtiles`: the local, Range-capable basemap endpoint
/// MapLibre reads through the pmtiles protocol. Absent -> 404 (the client
/// then POSTs ensure); a download in flight -> 409 with the status body (the
/// client keeps polling); ready -> the file, served with full Range support
/// by the same `ServeFile` the raw-video route uses.
async fn handle_basemap_tiles(
    State(state): State<Arc<AppState>>,
    request: Request<Body>,
) -> Response {
    let path = basemap_archive_path(&state);
    match videre_core::basemap::status(&path) {
        videre_core::basemap::BasemapStatus::Absent => StatusCode::NOT_FOUND.into_response(),
        videre_core::basemap::BasemapStatus::Partial { .. } => (
            StatusCode::CONFLICT,
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            basemap_status_json(&path),
        )
            .into_response(),
        videre_core::basemap::BasemapStatus::Ready { .. } => {
            match tower_http::services::ServeFile::new(&path)
                .try_call(request)
                .await
            {
                Ok(response) => response.map(Body::new),
                Err(e) => internal(e).into_response(),
            }
        }
    }
}

/// `GET /api/basemap/status`: absent / partial / ready plus a byte count.
async fn handle_basemap_status(State(state): State<Arc<AppState>>) -> Response {
    json_response(basemap_status_json(&basemap_archive_path(&state)))
}

/// `POST /api/basemap/ensure`: start the once-per-machine download if the
/// archive is absent and none is already running, then return the current
/// status immediately. The blocking `ureq` download runs on a blocking task;
/// the guard flag keeps a second POST from launching a duplicate.
async fn handle_basemap_ensure(State(state): State<Arc<AppState>>) -> Response {
    use std::sync::atomic::Ordering;
    let path = basemap_archive_path(&state);
    let ready = matches!(
        videre_core::basemap::status(&path),
        videre_core::basemap::BasemapStatus::Ready { .. }
    );
    if !ready
        && state
            .basemap_downloading
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    {
        let geo = state.context.library.cache.geo.clone();
        let state_dir = state.context.library.paths.state.clone();
        let flag = state.basemap_downloading.clone();
        tokio::task::spawn_blocking(move || {
            if let Err(e) = videre_core::basemap::ensure_downloaded(&geo, &state_dir, |_, _| {}) {
                tracing::error!("videre gallery: basemap download failed: {e}");
            }
            flag.store(false, Ordering::SeqCst);
        });
    }
    json_response(basemap_status_json(&path))
}

/// The year/month/day tree, as counts with one representative row per bucket.
///
/// :warning: **A page of rows cannot build this.** Grouping 200 files by year
/// shows a tree that grows as you scroll, which is worse than no tree. The
/// counts have to come from the whole library, so they come from SQL.
///
/// One level at a time, so a response is at most a few dozen buckets: about
/// fifteen years, twelve months, thirty-one days. The representative row is
/// included because each card shows a thumbnail, and fetching those separately
/// would be one request per bucket.
async fn handle_dates(
    State(state): State<Arc<AppState>>,
    Query(q): Query<DatesQuery>,
) -> Result<axum::response::Response, StatusCode> {
    let (len, parent_len) = match q.level.as_deref() {
        Some("month") => (7, 4),
        Some("day") => (10, 7),
        _ => (4, 0),
    };
    let effective = videre_core::query::EFFECTIVE_DATE_SQL;
    let keep = keep_set_sql();

    let mut sql = format!(
        "SELECT substr({effective}, 1, {len}) AS k, COUNT(*) AS n, \
                path, hash, COALESCE(ext,''), width, height, MIN(path) \
         FROM {keep} AS f \
         WHERE {effective} IS NOT NULL AND {effective} NOT LIKE '0000%'"
    );
    let mut params: Vec<String> = Vec::new();
    if parent_len > 0 {
        if let Some(p) = q.parent.as_deref().filter(|p| !p.is_empty()) {
            sql.push_str(&format!(" AND substr({effective}, 1, {parent_len}) = ?1"));
            params.push(p.to_string());
        }
    }
    sql.push_str(" GROUP BY k ORDER BY k DESC");

    let conn = state.conn.lock().map_err(poisoned)?;
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| internal(anyhow::anyhow!("/api/dates query failed: {e}\n  {sql}")))?;
    // SQLite picks the bare columns from the row matching MIN(path), which is
    // what makes the representative deterministic rather than arbitrary.
    let rows = stmt
        .query_map(rusqlite::params_from_iter(params.iter()), |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, Option<i32>>(5)?,
                r.get::<_, Option<i32>>(6)?,
            ))
        })
        .map_err(internal)?
        .filter_map(|r| r.ok())
        .collect::<Vec<_>>();
    drop(stmt);
    drop(conn);

    let mut out = String::from("{\"buckets\":[");
    for (i, (key, n, path, hash, ext, w, h)) in rows.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&format!(
            "{{\"key\":{},\"count\":{n},\"sample\":{{\"path\":{},\"hash\":{},\"ext\":{},\"w\":{},\"h\":{}}}}}",
            json_str(key),
            json_str(path),
            json_str(hash),
            json_str(ext),
            w.map(|v| v.to_string()).unwrap_or("null".into()),
            h.map(|v| v.to_string()).unwrap_or("null".into()),
        ));
    }
    out.push_str("]}");
    Ok(json_response(out))
}

/// A capture-only snapshot for travel inference, including undated files for
/// home evidence. A content hash counts once even when several paths exist.
fn load_trip_rows(conn: &Connection) -> rusqlite::Result<Vec<super::events::TripRow>> {
    use super::events::{MediaKind, TripRow};
    use std::collections::BTreeMap;

    let mut stmt = conn.prepare(
        "SELECT hash, exif_date, gps_lat, gps_lon, path, COALESCE(ext,''), mime, width, height \
         FROM file_hashes ORDER BY hash, path",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, Option<String>>(1)?,
            r.get::<_, Option<f64>>(2)?,
            r.get::<_, Option<f64>>(3)?,
            r.get::<_, String>(4)?,
            r.get::<_, String>(5)?,
            r.get::<_, Option<String>>(6)?,
            r.get::<_, Option<i32>>(7)?,
            r.get::<_, Option<i32>>(8)?,
        ))
    })?;
    let mut by_hash: BTreeMap<String, TripRow> = BTreeMap::new();
    for row in rows {
        let (hash, capture, lat, lon, path, ext, mime, width, height) = row?;
        let ext = ext.to_ascii_lowercase();
        let Some(effective) = videre_core::mime_probe::effective_mime(mime.as_deref(), &ext) else {
            continue;
        };
        let media = if effective.starts_with("image/") {
            MediaKind::Photo
        } else if videre_core::mime_probe::is_video_mime(effective) {
            MediaKind::Video
        } else {
            continue;
        };
        let gps = match (lat, lon) {
            (Some(lat), Some(lon))
                if lat.is_finite()
                    && lon.is_finite()
                    && (-90.0..=90.0).contains(&lat)
                    && (-180.0..=180.0).contains(&lon) =>
            {
                Some((lat, lon))
            }
            _ => None,
        };
        let candidate = TripRow {
            hash: hash.clone(),
            capture: capture.as_deref().and_then(super::events::parse_capture),
            gps,
            media,
            path,
            ext,
            width,
            height,
        };
        let better = by_hash.get(&hash).is_none_or(|prior| {
            (candidate.capture.is_some(), candidate.gps.is_some())
                > (prior.capture.is_some(), prior.gps.is_some())
                || ((candidate.capture.is_some(), candidate.gps.is_some())
                    == (prior.capture.is_some(), prior.gps.is_some())
                    && candidate.path < prior.path)
        });
        if better {
            by_hash.insert(hash, candidate);
        }
    }
    Ok(by_hash.into_values().collect())
}

/// The lock caller holds on `conn` also covers the snapshot version check and
/// reader, so all Events routes share one result until this DB changes.
fn cached_events(
    state: &AppState,
    conn: &Connection,
) -> Result<Arc<super::events::Detection>, StatusCode> {
    if !conn.is_autocommit() {
        let rows = load_trip_rows(conn).map_err(internal)?;
        return Ok(Arc::new(super::events::detect(&rows)));
    }
    let version = (
        conn.query_row("PRAGMA data_version", [], |row| row.get::<_, i64>(0))
            .map_err(internal)?,
        conn.query_row("SELECT total_changes()", [], |row| row.get::<_, i64>(0))
            .map_err(internal)?,
    );
    let mut cache = state.events_cache.lock().map_err(poisoned)?;
    if let Some(entry) = cache.as_ref().filter(|entry| entry.version == version) {
        return Ok(entry.detection.clone());
    }
    let rows = load_trip_rows(conn).map_err(internal)?;
    let detection = Arc::new(super::events::detect(&rows));
    *cache = Some(EventsCacheEntry {
        version,
        detection: detection.clone(),
    });
    Ok(detection)
}

fn trip_place_and_title(
    cache: &videre_core::library::CachePaths,
    event: &super::events::Trip,
) -> (Option<String>, String) {
    let (lat, lon) = event.dominant_stop;
    let place = videre_core::location::location_name_in(cache, lat, lon)
        .ok()
        .flatten();
    let month = event.anchor_when.format("%B %Y");
    let title = match place.as_deref().and_then(|p| p.split(',').next()) {
        Some(city) if !city.trim().is_empty() => format!("{}, {month} Trip", city.trim()),
        _ => format!("Trip, {month}"),
    };
    (place, title)
}

/// `GET /api/events`: the event overview, newest first.
async fn handle_events_api(
    State(state): State<Arc<AppState>>,
) -> Result<axum::response::Response, StatusCode> {
    let detection = {
        let conn = state.conn.lock().map_err(poisoned)?;
        cached_events(&state, &conn)?
    };
    let cache = &state.context.library.cache;
    let mut out = String::from("{\"events\":[");
    for (i, event) in detection.events.iter().rev().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let key = event.key();
        let (place, title) = trip_place_and_title(cache, event);
        out.push_str(&format!(
            "{{\"key\":{},\"kind\":\"trip\",\"title\":{},\"start\":{},\"end\":{},\"count\":{},\"place\":{},\
              \"sample\":{{\"path\":{},\"hash\":{},\"ext\":{},\"w\":{},\"h\":{}}}}}",
            json_str(&key),
            json_str(&title),
            json_str(&event.start.format("%Y-%m-%d %H:%M:%S").to_string()),
            json_str(&event.end.format("%Y-%m-%d %H:%M:%S").to_string()),
            event.members.len(),
            place
                .as_deref()
                .map(json_str)
                .unwrap_or_else(|| "null".to_string()),
            json_str(&event.sample.path),
            json_str(&event.sample.hash),
            json_str(&event.sample.ext),
            event
                .sample
                .width
                .map(|v| v.to_string())
                .unwrap_or_else(|| "null".to_string()),
            event
                .sample
                .height
                .map(|v| v.to_string())
                .unwrap_or_else(|| "null".to_string()),
        ));
    }
    out.push_str("],\"empty_reason\":");
    out.push_str(
        &detection
            .empty_reason
            .map(|reason| json_str(reason.as_str()))
            .unwrap_or_else(|| "null".to_owned()),
    );
    out.push('}');
    Ok(json_response(out))
}

fn render_live_events(
    state: &Arc<AppState>,
    event_json: &str,
    settings_script: String,
) -> axum::response::Html<String> {
    let conn = state.conn.lock().unwrap();
    let stats = query_stats(&conn);
    let faces_by_hash = videre_core::face_db::labeled_faces_by_hash(&conn).unwrap_or_default();
    let db_path = conn.path().map(|p| p.to_string()).unwrap_or_default();
    drop(conn);
    let set = RenderSet {
        stats,
        items: Vec::new(),
        groups: Vec::new(),
        faces_by_hash,
        marks_by_hash: Default::default(),
        nav: Some(Section::Events),
        view: View::Events,
        options: RenderOptions {
            live: true,
            heic: state.report_heic,
            heic_original: state.report_heic_original,
            embedded: None,
            db_path,
            date_filter_json: "null".to_string(),
            event_json: event_json.to_string(),
            settings_script,
        },
    };
    axum::response::Html(render(&set))
}

/// `videre gallery`'s `/events`: the travel-trip overview.
async fn handle_events(State(state): State<Arc<AppState>>) -> axum::response::Html<String> {
    render_live_events(&state, "null", page_settings(&state).await)
}

/// `videre gallery`'s `/events/{key}`: one event's photos.
async fn handle_events_key(
    axum::extract::Path(key): axum::extract::Path<String>,
    State(state): State<Arc<AppState>>,
) -> Result<axum::response::Response, StatusCode> {
    use axum::response::IntoResponse;
    let event_json = {
        let conn = state.conn.lock().map_err(poisoned)?;
        let detection = cached_events(&state, &conn)?;
        let event = detection
            .events
            .iter()
            .find(|e| e.key() == key)
            .ok_or(StatusCode::NOT_FOUND)?;
        let (place, title) = trip_place_and_title(&state.context.library.cache, event);
        format!(
            "{{\"key\":{},\"kind\":\"trip\",\"title\":{},\"from\":{},\"to\":{},\"place\":{}}}",
            json_str(&key),
            json_str(&title),
            json_str(&event.start.format("%Y-%m-%d %H:%M:%S").to_string()),
            json_str(&event.end.format("%Y-%m-%d %H:%M:%S").to_string()),
            place
                .as_deref()
                .map(json_str)
                .unwrap_or_else(|| "null".to_string()),
        )
    };
    Ok(render_live_events(&state, &event_json, page_settings(&state).await).into_response())
}

async fn handle_get_faces(
    State(state): State<Arc<AppState>>,
) -> Result<AxumJson<FacesData>, StatusCode> {
    let conn = state.conn.lock().map_err(poisoned)?;
    videre_api::faces_list(&conn)
        .map(AxumJson)
        .map_err(api_status)
}

async fn handle_assign(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(name): axum::extract::Path<String>,
    AxumJson(req): AxumJson<AssignFacesBody>,
) -> Result<Json<videre_api::LearningAcknowledgement>, StatusCode> {
    let acknowledgement = {
        let conn = state.conn.lock().map_err(poisoned)?;
        let context = teaching_context(&conn, &state);
        // The route keeps its historical meaning: naming a cluster after a
        // person who does not exist yet creates that person.
        let exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM people WHERE name = ?1)",
                [&name],
                |row| row.get(0),
            )
            .map_err(internal)?;
        let result = if exists {
            videre_api::assign_with_learning(&conn, &req.face_ids, &name, &context)
        } else {
            videre_api::new_person_with_learning(&conn, &req.face_ids, &name, &context)
        };
        result.map_err(api_status)?
    };
    if let Some(learning) = &state.learning {
        learning.notify();
    }
    Ok(Json(acknowledgement))
}

/// Set marks on one photo from the gallery. Goes through the same
/// `videre_core::marks` writer and the same parts-to-change mapping as
/// `videre mark`, so the CLI and the gallery behave identically.
async fn handle_set_mark(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(hash): axum::extract::Path<String>,
    AxumJson(body): AxumJson<MarkBody>,
) -> Result<StatusCode, StatusCode> {
    let change = videre_core::marks::change_from_parts(
        body.rating,
        body.pick.as_deref(),
        body.label.as_deref(),
        body.liked,
    );
    if !change.any() {
        return Ok(StatusCode::OK);
    }
    let conn = state.conn.lock().map_err(poisoned)?;
    videre_core::marks::set(&conn, std::slice::from_ref(&hash), &change)
        .map(|_| StatusCode::OK)
        .map_err(|e| internal(anyhow::anyhow!("/api/mark failed: {e}")))
}

async fn handle_new_person(
    State(state): State<Arc<AppState>>,
    AxumJson(req): AxumJson<NewPersonRequest>,
) -> Result<Json<videre_api::LearningAcknowledgement>, StatusCode> {
    let acknowledgement = {
        let conn = state.conn.lock().map_err(poisoned)?;
        videre_api::new_person_with_learning(
            &conn,
            &req.face_ids,
            &req.name,
            &teaching_context(&conn, &state),
        )
        .map_err(api_status)?
    };
    if let Some(learning) = &state.learning {
        learning.notify();
    }
    Ok(Json(acknowledgement))
}

pub(super) fn teaching_context(conn: &Connection, state: &AppState) -> videre_api::TeachingContext {
    videre_api::TeachingContext {
        embedding_model_id: state.model_id.clone(),
        active_profile_id: videre_core::face_learning::active_profile(conn)
            .ok()
            .flatten()
            .map(|profile| profile.id),
    }
}

async fn handle_remove_face(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<i64>,
) -> Result<Json<videre_api::LearningAcknowledgement>, StatusCode> {
    let acknowledgement = {
        let conn = state.conn.lock().map_err(poisoned)?;
        videre_api::remove_face_with_learning(&conn, id, &teaching_context(&conn, &state))
            .map_err(api_status)?
    };
    if let Some(learning) = &state.learning {
        learning.notify();
    }
    Ok(Json(acknowledgement))
}

async fn handle_delete_person(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> Result<Json<videre_api::LearningAcknowledgement>, StatusCode> {
    let acknowledgement = {
        let conn = state.conn.lock().map_err(poisoned)?;
        videre_api::delete_person_with_learning(&conn, &name)
            .map_err(api_status)?
            .unwrap_or_else(|| videre_api::LearningAcknowledgement {
                generation: videre_core::face_learning::learning_state(&conn)
                    .map(|state| state.generation)
                    .unwrap_or(0),
                event_ids: Vec::new(),
                message_key: "person_deleted_without_learning".into(),
            })
    };
    if let Some(learning) = &state.learning {
        learning.notify();
    }
    Ok(Json(acknowledgement))
}

async fn handle_set_full_name(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(name): axum::extract::Path<String>,
    AxumJson(req): AxumJson<SetFullNameBody>,
) -> Result<StatusCode, StatusCode> {
    let conn = state.conn.lock().map_err(poisoned)?;
    videre_api::set_full_name(&conn, &name, &req.full_name)
        .map(|_| StatusCode::OK)
        .map_err(api_status)
}

async fn handle_dissolve_cluster(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<i64>,
) -> Result<Json<videre_api::LearningAcknowledgement>, StatusCode> {
    let acknowledgement = {
        let conn = state.conn.lock().map_err(poisoned)?;
        videre_api::dissolve_cluster_with_learning(&conn, id, &teaching_context(&conn, &state))
            .map_err(api_status)?
    };
    if let Some(learning) = &state.learning {
        learning.notify();
    }
    Ok(Json(acknowledgement))
}

async fn handle_set_primary(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<i64>,
    AxumJson(req): AxumJson<SetPrimaryBody>,
) -> Result<StatusCode, StatusCode> {
    let conn = state.conn.lock().map_err(poisoned)?;
    videre_api::set_primary(&conn, id, &req.person_label)
        .map(|_| StatusCode::OK)
        .map_err(api_status)
}

async fn handle_search_person(
    State(state): State<Arc<AppState>>,
    Query(q): Query<PersonSearchQuery>,
) -> Result<AxumJson<Vec<String>>, StatusCode> {
    let conn = state.conn.lock().map_err(poisoned)?;
    videre_api::search_person(&conn, &q.name)
        .map(AxumJson)
        .map_err(api_status)
}

async fn handle_quit(State(state): State<Arc<AppState>>) -> StatusCode {
    if let Ok(mut lock) = state.shutdown_tx.lock() {
        if let Some(tx) = lock.take() {
            let _ = tx.send(());
        }
    }
    if let Some(learning) = &state.learning {
        learning.shutdown();
    }
    StatusCode::OK
}

async fn handle_cluster_page(
    axum::extract::Path(cluster_id): axum::extract::Path<i64>,
    State(state): State<Arc<AppState>>,
) -> impl axum::response::IntoResponse {
    use askama::Template;
    let page = pages::Cluster {
        settings_script: page_settings(&state).await,
        css: pages::CLUSTER_CSS,
        js: pages::CLUSTER_JS,
        cluster_id,
        back_href: people_root(state.gallery),
        back_label: people_back_label(state.gallery),
        nav: state.gallery.then_some(Section::People),
    };
    axum::response::Html(page.render().expect("cluster template"))
}

async fn handle_cluster_api(
    axum::extract::Path(cluster_id): axum::extract::Path<i64>,
    State(state): State<Arc<AppState>>,
) -> Result<AxumJson<ClusterDetail>, StatusCode> {
    let conn = state.conn.lock().map_err(poisoned)?;
    videre_api::cluster_detail(&conn, cluster_id)
        .map(AxumJson)
        .map_err(api_status)
}

async fn handle_person_page(State(state): State<Arc<AppState>>) -> axum::response::Html<String> {
    use askama::Template;
    let page = pages::Person {
        settings_script: page_settings(&state).await,
        css: pages::PERSON_CSS,
        js: pages::PERSON_JS,
        faces_ui_enabled: state.serve_faces_ui,
        back_href: people_root(state.gallery),
        back_label: people_back_label(state.gallery),
        nav: state.gallery.then_some(Section::People),
    };
    axum::response::Html(page.render().expect("person template"))
}

async fn handle_person_api(
    axum::extract::Path(name): axum::extract::Path<String>,
    State(state): State<Arc<AppState>>,
) -> Result<AxumJson<PersonDetail>, StatusCode> {
    let conn = state.conn.lock().map_err(poisoned)?;
    videre_api::person_detail(&conn, &name)
        .map(AxumJson)
        .map_err(api_status)
}

async fn handle_face_image(
    axum::extract::Path(face_id): axum::extract::Path<i64>,
    State(state): State<Arc<AppState>>,
) -> Result<impl axum::response::IntoResponse, StatusCode> {
    let state = state.clone();
    // Lock only for the cheap single-row lookup, then release it before the
    // expensive decode/crop/resize/encode work, holding the shared
    // connection lock across that work serializes every thumbnail request
    // behind one mutex, which is the actual cause of multi-second-per-thumbnail
    // rendering in a library with thousands of faces.
    let bytes = tokio::task::spawn_blocking(move || -> Result<Vec<u8>, StatusCode> {
        // Held across the DB lookup and the decode/crop/cache-publish below, so
        // a root replaced mid-request is refused and exclusive maintenance is
        // excluded from the cache write.
        let _guard = guard_operation(&state)?;
        let lookup = {
            let conn = state.conn.lock().map_err(poisoned)?;
            videre_api::face_lookup(&conn, face_id).map_err(|_| StatusCode::NOT_FOUND)?
        };
        videre_api::face_bytes_from_lookup(&lookup, face_id, &state.context.library.cache)
            .map_err(|_| StatusCode::NOT_FOUND)
    })
    .await
    .map_err(internal)??;
    Ok(([(axum::http::header::CONTENT_TYPE, "image/jpeg")], bytes))
}

/// Formats the gallery grid renders as `<img>` and that the `image` crate can
/// decode, so a downscaled JPEG thumbnail can stand in for the full original.
/// HEIC has its own QuickLook path; video, DNG and TIFF are not thumbnailed.
fn is_thumbnailable_raster(ext: &str) -> bool {
    matches!(ext, "jpg" | "jpeg" | "png" | "gif" | "webp" | "bmp")
}

/// Decode `bytes`, downscale so the longest edge is at most `max_px` (never
/// upscaling), and re-encode as JPEG. Returns `None` if the bytes do not
/// decode, so the caller can fall back to serving the original untouched
/// rather than a broken tile. Generating this once and caching it per
/// (hash, size) is what stops the grid from streaming full multi-megabyte
/// originals off a slow drive for every tile. Orientation is read through the
/// decoder from these already-loaded bytes, so it adds no second source read.
fn render_raster_thumbnail(bytes: &[u8], max_px: u32) -> Option<Vec<u8>> {
    let img = videre_core::image_decode::decode_oriented_bytes(bytes)?;
    let img = if img.width() > max_px || img.height() > max_px {
        img.resize(max_px, max_px, image::imageops::FilterType::Triangle)
    } else {
        img
    };
    let mut buf = Vec::new();
    img.write_to(
        &mut std::io::Cursor::new(&mut buf),
        image::ImageFormat::Jpeg,
    )
    .ok()?;
    Some(buf)
}

fn mime_for_ext(ext: &str) -> &'static str {
    videre_api::mime_for_ext(ext)
}

#[derive(Deserialize)]
struct DatesQuery {
    /// `year`, `month` or `day`. Anything else is treated as `year`.
    level: Option<String>,
    /// The bucket being drilled into: a year for `month`, a year-month for
    /// `day`. Absent at the top level.
    parent: Option<String>,
}

#[derive(Deserialize)]
struct FilesQuery {
    /// `all` (default) or `date`. Anything else is treated as `all` rather than
    /// rejected: an unknown view is a client bug, and a 400 here would render as
    /// an empty gallery with no explanation.
    view: Option<String>,
    offset: Option<i64>,
    limit: Option<i64>,
    /// Comma-separated hashes. Used by in-page similarity to resolve the rows
    /// behind its results without holding every row to look them up in.
    hashes: Option<String>,
    /// A date prefix, `YYYY`, `YYYY-MM` or `YYYY-MM-DD`. The date view's leaf
    /// asks for one day; the prefix form means the same endpoint answers a
    /// month or a year without a second parameter.
    date: Option<String>,
    /// Inclusive lower bound for date range filters.
    from: Option<String>,
    /// Exclusive upper bound for date range filters.
    to: Option<String>,
    /// Map drill-down center latitude. Geographic parameters are applied only
    /// when all three are present on the `all` view.
    lat: Option<f64>,
    /// Map drill-down center longitude.
    lon: Option<f64>,
    /// Map drill-down radius in positive kilometers.
    radius: Option<f64>,
    /// Which field the files are ordered by: `date`, `name`, `size`,
    /// `rating`, `liked` or `type`. Unknown values fall back to the default,
    /// the same rule `view` follows.
    sort: Option<String>,
    /// `asc` or `desc`. Anything else means `desc`.
    dir: Option<String>,
}

fn files_location_filter(
    query: &FilesQuery,
    view: &str,
) -> Result<Option<LocationFilter>, StatusCode> {
    if view == "date" {
        return Ok(None);
    }
    match (query.lat, query.lon, query.radius) {
        (None, None, None) => Ok(None),
        (Some(lat), Some(lon), Some(radius_km))
            if lat.is_finite()
                && lon.is_finite()
                && radius_km.is_finite()
                && (-90.0..=90.0).contains(&lat)
                && (-180.0..=180.0).contains(&lon)
                && radius_km > 0.0 =>
        {
            Ok(Some(LocationFilter {
                lat,
                lon,
                radius_km,
            }))
        }
        _ => Err(StatusCode::BAD_REQUEST),
    }
}

#[derive(Deserialize)]
struct RawFileQuery {
    /// Optional max width/height in pixels for browser-raster images and HEIC.
    /// The caller can request a thumbnail (480px in the grid/tiles, sharp on a
    /// 2x display) or a larger version (1200px in the lightbox) without paying
    /// to transfer a huge image for a small `<img>`. Ignored for formats served
    /// as raw bytes.
    size: Option<u32>,
}

/// Streams a file already recorded in `file_hashes.path` over HTTP, added
/// so the live report (server mode, --show-faces) can point thumbnail
/// `<img>`/`<video>` tags and the lightbox at an http:// URL instead of
/// `file://`, which browsers refuse to load as a subresource of an
/// http://-served page. Only paths that exist as a `file_hashes.path` value
/// are served, this is a deliberate allowlist, not a general file server,
/// so a client can't request arbitrary paths off the filesystem.
///
/// HEIC is converted to JPEG on demand via QuickLook (same
/// `videre_core::heic::heic_via_quicklook` helper used elsewhere), one file per request,
/// lazily as the browser requests each thumbnail/lightbox image, NOT
/// eagerly for the whole report up front, which is what made server mode
/// unusably slow on a collection with many HEIC files before this endpoint
/// existed (`generate_html` used to call `heic_to_b64` synchronously for
/// every HEIC file before returning any response).
async fn handle_raw_file(
    axum::extract::Path(hash): axum::extract::Path<String>,
    Query(q): Query<RawFileQuery>,
    State(state): State<Arc<AppState>>,
    request: Request<Body>,
) -> Result<Response, StatusCode> {
    let path = {
        let conn = state.conn.lock().map_err(poisoned)?;
        conn.query_row(
            "SELECT path FROM file_hashes WHERE hash = ?1 LIMIT 1",
            [&hash],
            |r| r.get::<_, String>(0),
        )
        .optional()
        .map_err(internal)?
        .ok_or(StatusCode::NOT_FOUND)?
    };
    let ext = std::path::Path::new(&path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    // Browsers fetch video metadata with byte ranges. Serving those requests
    // through a streaming file service is essential: returning a full MP4 for
    // every `<video preload="metadata">` ties up Firefox's per-origin HTTP/1.1
    // connections and prevents the image thumbnails behind them from loading.
    if q.size.is_none() && matches!(ext.as_str(), "mov" | "mp4") {
        let response = tower_http::services::ServeFile::new(path)
            .try_call(request)
            .await
            .map_err(internal)?;
        return Ok(response.map(Body::new));
    }

    // A thumbnail may already be cached for this content hash at this size:
    // videre watch's `--heic` stage pre-converts HEIC, and the raster branch
    // below caches whatever it renders. Serve the cached JPEG directly and skip
    // both a QuickLook conversion and re-reading the full original off disk.
    if let Some(size) = q.size {
        let cached_path = if ext == "heic" {
            Some(videre_core::thumb_cache::thumb_path_in(
                &state.context.library.cache,
                &hash,
                size,
            ))
        } else if is_thumbnailable_raster(&ext) {
            Some(videre_core::thumb_cache::raster_thumb_path_in(
                &state.context.library.cache,
                &hash,
                size,
            ))
        } else if matches!(ext.as_str(), "mov" | "mp4") {
            // A sized video request wants the oriented poster frame, cached like
            // the HEIC and raster thumbnails below.
            Some(videre_core::thumb_cache::video_poster_path_in(
                &state.context.library.cache,
                &hash,
                size,
            ))
        } else {
            None
        };
        if let Some(cached_path) = cached_path {
            if let Ok(bytes) = tokio::fs::read(&cached_path).await {
                return Ok(
                    ([(axum::http::header::CONTENT_TYPE, "image/jpeg")], bytes).into_response()
                );
            }
        }
    }

    let size = q.size;
    let is_heic = ext == "heic";
    // A sized video request produces its poster through the same QuickLook path.
    let is_video_poster = matches!(ext.as_str(), "mov" | "mp4") && size.is_some();
    let uses_quicklook = is_heic || is_video_poster;

    // A file whose QuickLook conversion has repeatedly failed (corrupt, hanging,
    // or QuickLook unavailable) is not re-attempted: the conversion is the
    // expensive, possibly-hanging step, and only successes are cached, so it
    // otherwise re-pays the timeout on every tile request. A file at the
    // two-strike threshold is refused outright. The lock is taken and dropped
    // here, never held across the conversion below.
    if uses_quicklook {
        let conn = state.conn.lock().map_err(poisoned)?;
        let _ = videre_core::decode_failures::ensure_table(&conn);
        let failed = videre_core::decode_failures::failed_hashes(
            &conn,
            videre_core::decode_failures::STAGE_THUMBNAIL,
            videre_core::decode_failures::FAILURE_THRESHOLD,
        )
        .unwrap_or_default();
        if failed.contains(&hash) {
            return Err(StatusCode::NOT_FOUND);
        }
    }

    // Where a freshly rendered raster thumbnail should be cached (atomic tmp ->
    // rename), computed here so only owned paths cross into the blocking task.
    // `None` unless this is a sized request for a thumbnailable raster format
    // (a full-size original, a video, or HEIC does not take this path).
    let raster_cache = match (is_thumbnailable_raster(&ext), size) {
        (true, Some(px)) => Some((
            px,
            state.context.library.cache.thumbnails.clone(),
            videre_core::thumb_cache::raster_thumb_tmp_path_in(
                &state.context.library.cache,
                &hash,
                px,
            ),
            videre_core::thumb_cache::raster_thumb_path_in(&state.context.library.cache, &hash, px),
        )),
        _ => None,
    };
    // Where a freshly rendered video poster should be cached, so repeated tile
    // requests do not each re-run QuickLook. Owned paths only, for the task.
    let video_poster_cache = match (is_video_poster, size) {
        (true, Some(px)) => Some((
            state.context.library.cache.thumbnails.clone(),
            videre_core::thumb_cache::video_poster_tmp_path_in(
                &state.context.library.cache,
                &hash,
                px,
            ),
            videre_core::thumb_cache::video_poster_path_in(&state.context.library.cache, &hash, px),
        )),
        _ => None,
    };
    let converted = tokio::task::spawn_blocking(move || -> Option<(&'static str, Vec<u8>)> {
        if ext == "heic" {
            // `size` doubles as the qlmanage render cap: when Some, this
            // caller downscales to it below anyway; when None, the caller
            // wants the true original (no downscale applied), which is
            // exactly heic_via_quicklook(..., None)'s full-resolution
            // behavior too. See its safety note.
            let img = videre_core::heic::heic_via_quicklook(
                &path,
                &format!("raw{}", size.unwrap_or(0)),
                size,
            )?;
            let img = match size {
                Some(max_px) if img.width() > max_px || img.height() > max_px => {
                    img.resize(max_px, max_px, image::imageops::FilterType::Triangle)
                }
                _ => img,
            };
            let mut buf = Vec::new();
            img.write_to(
                &mut std::io::Cursor::new(&mut buf),
                image::ImageFormat::Jpeg,
            )
            .ok()?;
            Some(("image/jpeg", buf))
        } else if matches!(ext.as_str(), "mov" | "mp4") && size.is_some() {
            // Video poster: QuickLook renders the frame display-oriented, so the
            // rotation these files carry (which browsers apply unreliably) is
            // baked into the image videre serves. Cached so later tile requests
            // for the same content reuse it instead of re-running QuickLook.
            let px = size.unwrap();
            let img =
                videre_core::heic::heic_via_quicklook(&path, &format!("vposter{px}"), Some(px))?;
            let img = if img.width() > px || img.height() > px {
                img.resize(px, px, image::imageops::FilterType::Triangle)
            } else {
                img
            };
            let mut buf = Vec::new();
            img.write_to(
                &mut std::io::Cursor::new(&mut buf),
                image::ImageFormat::Jpeg,
            )
            .ok()?;
            if let Some((dir, tmp, final_)) = &video_poster_cache {
                let _ = std::fs::create_dir_all(dir);
                if std::fs::write(tmp, &buf).is_ok() {
                    let _ = std::fs::rename(tmp, final_);
                }
            }
            Some(("image/jpeg", buf))
        } else {
            let timeout_path = path.clone();
            let bytes = match videre_core::io_timeout::run_with_timeout(
                videre_core::io_timeout::DEFAULT_IO_TIMEOUT,
                move || std::fs::read(&timeout_path),
            ) {
                Ok(Ok(bytes)) => bytes,
                Ok(Err(e)) => {
                    tracing::warn!("raw file unavailable for {path}: {e}; skipping");
                    return None;
                }
                Err(_) => {
                    tracing::warn!(
                        "timed out reading {path} \
                         (file may be unreachable - is its drive connected?); skipping"
                    );
                    return None;
                }
            };
            // A raster grid tile is served as a small cached JPEG, not the
            // full original: rendering a downscaled thumbnail once (and
            // caching it) is what keeps a large library on a slow drive from
            // saturating the browser's connection pool with multi-megabyte
            // transfers for every tile. If the bytes do not decode, fall
            // back to the original untouched so nothing regresses.
            if let Some((px, dir, tmp, final_)) = &raster_cache {
                if let Some(buf) = render_raster_thumbnail(&bytes, *px) {
                    let _ = std::fs::create_dir_all(dir);
                    if std::fs::write(tmp, &buf).is_ok() {
                        let _ = std::fs::rename(tmp, final_);
                    }
                    return Some(("image/jpeg", buf));
                }
            }
            Some((mime_for_ext(&ext), bytes))
        }
    })
    .await
    .map_err(internal)?;

    // Record a failed QuickLook conversion so it reaches the two-strike
    // threshold and stops being re-attempted; clear on success so a file that
    // converts (or recovers) is served normally again. A brief lock, never
    // across the work.
    if uses_quicklook {
        if let Ok(conn) = state.conn.lock() {
            match &converted {
                Some(_) => {
                    let _ = videre_core::decode_failures::clear(
                        &conn,
                        &hash,
                        videre_core::decode_failures::STAGE_THUMBNAIL,
                    );
                }
                None => {
                    let _ = videre_core::decode_failures::record(
                        &conn,
                        &hash,
                        videre_core::decode_failures::STAGE_THUMBNAIL,
                        "gallery HEIC conversion failed",
                    );
                }
            }
        }
    }

    let (content_type, bytes) = converted.ok_or(StatusCode::NOT_FOUND)?;
    Ok(([(axum::http::header::CONTENT_TYPE, content_type)], bytes).into_response())
}

/// Serve the full, uncropped original image for a face's source file.
///
/// Browsers refuse to navigate from an http:// page to a file:// URL for
/// security reasons ("Not allowed to load local resource"), so the
/// original can't be linked to directly, it has to be read and served
/// over HTTP like everything else.
async fn handle_original_image(
    axum::extract::Path(face_id): axum::extract::Path<i64>,
    State(state): State<Arc<AppState>>,
) -> Result<impl axum::response::IntoResponse, StatusCode> {
    let state = state.clone();
    let (content_type, bytes) =
        tokio::task::spawn_blocking(move || -> Result<(&'static str, Vec<u8>), StatusCode> {
            let _guard = guard_operation(&state)?;
            let lookup = {
                let conn = state.conn.lock().map_err(poisoned)?;
                videre_api::original_lookup(&conn, face_id).map_err(|_| StatusCode::NOT_FOUND)?
            };
            videre_api::original_bytes_from_lookup(&lookup, face_id, &state.context.library.cache)
                .map_err(|_| StatusCode::NOT_FOUND)
        })
        .await
        .map_err(internal)??;
    Ok(([(axum::http::header::CONTENT_TYPE, content_type)], bytes))
}

/// Options threaded from `main()`'s CLI args into the live server, since
/// server mode renders the report on-demand per request instead of once to a
/// static file.
struct ServeOptions {
    serve_faces_ui: bool,
    report_all: bool,
    report_heic: bool,
    report_heic_original: bool,
    model_id: String,
    /// `videre gallery`: serve every view on its own route rather than one page
    /// whose content depends on which flags started the server.
    gallery: bool,
    /// The startup-bound library context, stored in `AppState` for search.
    context: Arc<crate::command_context::CommandContext>,
    port: u16,
    /// True only when `port` is the default (no `--port` given): a taken port
    /// then advances to the next free one. An explicit `--port` is exact.
    port_fallback: bool,
    browse: bool,
}

/// The port `videre gallery` uses when `--port` is omitted.
const DEFAULT_GALLERY_PORT: u16 = 7878;

/// Bind `127.0.0.1:port`. When `allow_fallback` is set, a port already in use
/// advances to the next one so a second gallery does not have to be told a free
/// port by hand; without it the requested port is exact and a clash is an error
/// (an explicit `--port` means that port and no other).
///
/// `port` 0 is left to the OS, which picks a free port itself, so there is
/// nothing to scan. Any error other than "address in use" is returned as is,
/// and the scan stops at 65535 rather than wrapping back to a privileged port.
fn bind_with_fallback(port: u16, allow_fallback: bool) -> std::io::Result<std::net::TcpListener> {
    if port == 0 {
        return std::net::TcpListener::bind(("127.0.0.1", 0));
    }
    let mut port = port;
    loop {
        match std::net::TcpListener::bind(("127.0.0.1", port)) {
            Ok(listener) => return Ok(listener),
            Err(error) if allow_fallback && error.kind() == std::io::ErrorKind::AddrInUse => {
                match port.checked_add(1) {
                    Some(next) => port = next,
                    None => return Err(error),
                }
            }
            Err(error) => return Err(error),
        }
    }
}

#[cfg(test)]
mod bind_tests {
    use super::bind_with_fallback;
    use std::net::TcpListener;

    #[test]
    fn the_default_port_advances_past_an_occupied_one() {
        // Hold a port for the duration, then ask the binder for it with
        // fallback on (the no-`--port` default): it must skip to a higher one.
        let occupied = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let taken = occupied.local_addr().unwrap().port();

        let listener = bind_with_fallback(taken, true).unwrap();
        assert!(
            listener.local_addr().unwrap().port() > taken,
            "a taken default port must advance to a higher free port"
        );
    }

    #[test]
    fn an_explicit_port_does_not_fall_back_and_errors_when_taken() {
        // Fallback off is the explicit-`--port` case: a clash is an error, not
        // a silent move to a different port.
        let occupied = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let taken = occupied.local_addr().unwrap().port();

        let error = bind_with_fallback(taken, false).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::AddrInUse);
    }

    #[test]
    fn port_zero_lets_the_os_pick_a_free_port() {
        let listener = bind_with_fallback(0, false).unwrap();
        assert_ne!(
            listener.local_addr().unwrap().port(),
            0,
            "port 0 must resolve to a concrete OS-chosen port"
        );
    }
}

async fn serve_faces_async(
    db: &Path,
    opts: ServeOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    let conn = videre_core::db::open_wal(db)?;
    videre_core::location_cluster::register_haversine_sql_function(&conn)?;
    // Thumbnail decode-failure records are the one skip with no --reprocess
    // hatch, so clear them once per server run: a HEIC that hit two transient
    // QuickLook failures (a wedged agent, Spotlight contention) is retried on
    // the next start rather than staying a permanently broken tile. Within a
    // run the gate still spares repeated timeouts once a file has failed twice.
    let _ = videre_core::decode_failures::clear_stage(
        &conn,
        videre_core::decode_failures::STAGE_THUMBNAIL,
    );
    // The labeling server writes person labels, so it is a writer and migrates
    // like the other writers do. Without this, a user who only ever labels
    // through the UI would keep the old mixed-case labels and never get the
    // case-insensitive behaviour.
    match videre_core::face_db::migrate_person_labels(&conn) {
        Ok((people, merged)) if merged > 0 => {
            tracing::info!(
                "Merged {merged} name(s) differing only in spelling; {people} people now"
            );
        }
        Ok(_) => {}
        Err(e) => tracing::warn!("could not migrate person names: {e}"),
    }
    if opts.serve_faces_ui {
        // The first questions request can precede the status request or the
        // worker's first promoted profile. Make that resource ready at startup.
        // The learning tables too: an older library gains columns here (the
        // waiting ask), before the worker or any handler reads the state.
        videre_core::face_learning::ensure_learning_tables(&conn)?;
        videre_core::face_learning::ensure_question_tables(&conn)?;
    }
    // Only --all needs vectors. A missing model database disables the
    // similarity search with a note rather than failing the whole report,
    // which works perfectly well without embeddings.
    if opts.report_all {
        if let Err(e) = videre_core::embeddings_db::attach_for_read_in(
            &conn,
            &opts.context.library,
            &opts.model_id,
        ) {
            tracing::info!("note: similarity search disabled ({e})");
        }
    }
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let conn = Arc::new(Mutex::new(conn));
    let learning = opts.serve_faces_ui.then(|| {
        super::learning::spawn(super::learning::LearningDeps {
            conn: conn.clone(),
            embedding_model_id: opts.model_id.clone(),
            config: videre_core::face_learning::TrainingConfig::default(),
            gates: videre_core::face_learning::PromotionGates::shipped(),
            questions: videre_core::face_learning::QuestionSelectionConfig::default(),
            train: Arc::new(|snapshot, config, gates| {
                videre_core::face_learning::train_and_select_candidate(snapshot, config, gates)
            }),
        })
    });
    let state = Arc::new(AppState {
        learning,
        conn,
        events_cache: Mutex::new(None),
        shutdown_tx: Mutex::new(Some(shutdown_tx)),
        model_id: opts.model_id.clone(),
        report_heic: opts.report_heic,
        report_heic_original: opts.report_heic_original,
        serve_faces_ui: opts.serve_faces_ui,
        gallery: opts.gallery,
        context: opts.context.clone(),
        embedder: Mutex::new(None),
        basemap_downloading: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        settings_lock: Arc::new(Mutex::new(())),
        settings_warned: Arc::new(std::sync::atomic::AtomicBool::new(false)),
    });

    // `videre gallery` is the only server configuration (labeling-only went away
    // with `videre report` in 0.20.0, `serve_faces_ui` is always true), so every
    // route is registered unconditionally. The API is resource-oriented REST;
    // ids live in the path, methods carry intent. The cluster and person pages
    // live under `/people`.
    let router = Router::new()
        // files / media
        .route("/api/files", get(handle_files))
        .route("/api/files/{hash}", patch(handle_set_mark))
        .route("/api/files/marks", post(super::bulk::handle_marks))
        .route("/api/files/tags", post(super::bulk::handle_tags))
        .route("/api/files/rotate", post(super::bulk::handle_rotate))
        .route("/api/tags", get(super::bulk::handle_list_tags))
        .route("/api/files/{hash}/raw", get(handle_raw_file))
        .route("/api/files/{hash}/rotate", post(handle_rotate_file))
        .route("/api/dates", get(handle_dates))
        .route(
            "/api/settings",
            get(handle_get_settings)
                .patch(handle_patch_settings)
                .put(handle_put_settings),
        )
        .route("/api/events", get(handle_events_api))
        .route("/api/events/{key}/files", get(handle_events_files))
        .route("/api/search", get(handle_search))
        .route("/api/locations", get(handle_location))
        .route("/api/location-clusters", get(handle_location_clusters))
        // basemap: the offline PMTiles archive and its download lifecycle
        .route("/tiles/basemap.pmtiles", get(handle_basemap_tiles))
        .route("/api/basemap/status", get(handle_basemap_status))
        .route("/api/basemap/ensure", post(handle_basemap_ensure))
        .route("/vendor/{version}/{asset}", get(handle_vendor_asset))
        // people
        .route(
            "/api/people",
            get(handle_search_person).post(handle_new_person),
        )
        .route(
            "/api/people/{name}",
            get(handle_person_api)
                .patch(handle_set_full_name)
                .delete(handle_delete_person),
        )
        .route("/api/people/{name}/faces", put(handle_assign))
        // faces
        .route("/api/faces", get(handle_get_faces))
        .route(
            "/api/faces/{id}",
            patch(handle_set_primary).delete(handle_remove_face),
        )
        .route("/api/faces/{id}/image", get(handle_face_image))
        .route(
            "/api/faces/cluster-params",
            get(super::recluster::handle_cluster_params),
        )
        .route(
            "/api/faces/recluster/preview",
            post(super::recluster::handle_preview),
        )
        .route("/api/faces/recluster", post(super::recluster::handle_apply))
        .route("/api/faces/{id}/original", get(handle_original_image))
        // clusters
        .route(
            "/api/clusters/{id}",
            get(handle_cluster_api).delete(handle_dissolve_cluster),
        )
        // face learning
        .route(
            "/api/face-learning/status",
            get(super::learning::handle_learning_status),
        )
        .route(
            "/api/face-learning/questions",
            get(super::learning::handle_learning_questions),
        )
        .route(
            "/api/face-learning/questions/{id}/answer",
            post(super::learning::handle_learning_answer),
        )
        .route(
            "/api/face-learning/events",
            get(super::learning::handle_learning_events),
        )
        .route(
            "/api/face-learning/events/{id}",
            get(super::learning::handle_learning_event_detail),
        )
        // control
        .route("/api/quit", post(handle_quit))
        // pages
        .route("/people/cluster/{id}", get(handle_cluster_page))
        .route("/people/person/{name}", get(handle_person_page))
        .route("/", get(handle_gallery_all))
        .route("/duplicates", get(handle_gallery_duplicates))
        .route("/people", get(handle_root))
        .route("/date/{year}/{month}/{day}", get(handle_gallery_date_day))
        .route("/date/{year}/{month}", get(handle_gallery_date_month))
        .route("/date/{year}", get(handle_gallery_date_year))
        .route("/date", get(handle_gallery_date))
        .route("/map/location/{name}", get(handle_map_location))
        .route("/map", get(handle_map))
        .route("/settings", get(handle_settings_page))
        .route("/events/{key}", get(handle_events_key))
        .route("/events", get(handle_events))
        .route("/smart", get(handle_not_yet));

    let app = router.with_state(state);

    let requested = format!("127.0.0.1:{}", opts.port);
    // Bind synchronously so an occupied port can be skipped, then hand the
    // socket to tokio. Running two galleries at once is normal (one per
    // library), so the default port advances to the next free one rather than
    // failing; an explicit `--port` stays exact.
    let std_listener = bind_with_fallback(opts.port, opts.port_fallback).map_err(|e| {
        // With fallback on, an address-in-use error here means the scan ran
        // out of ports at the top of the range; any other failure names the
        // port that was actually requested.
        if opts.port_fallback && e.kind() == std::io::ErrorKind::AddrInUse {
            format!("Cannot bind a port at or above {requested}: {e}")
        } else {
            format!("Cannot bind to {requested}: {e}")
        }
    })?;
    std_listener
        .set_nonblocking(true)
        .map_err(|e| format!("Cannot prepare the gallery socket: {e}"))?;
    let listener = tokio::net::TcpListener::from_std(std_listener)
        .map_err(|e| format!("Cannot prepare the gallery socket: {e}"))?;

    // :warning: Report the address that was BOUND, not the one requested. With
    // `--port 0` the OS picks a free port, and printing the request meant the
    // server announced `http://127.0.0.1:0`, which is unreachable. Anyone using
    // 0 to avoid choosing a port then had no way to find the server, and
    // `--browse` opened the same dead address. Fallback makes this true a second
    // way: the bound port is often not the one the flag named.
    let addr = listener
        .local_addr()
        .map(|a| a.to_string())
        .unwrap_or(requested);
    let addr = addr.as_str();

    // The gallery reopens where it was last left: the printed URL and the
    // `--browse` target both carry the saved route, so resuming works with or
    // without `--browse`. `/` itself never redirects.
    let url = if opts.gallery {
        startup_url(addr, &opts.context.library.paths.state)
    } else {
        format!("http://{addr}")
    };
    if opts.gallery {
        tracing::info!("videre gallery: {url}");
    } else {
        tracing::info!("Faces labeling server: {url}");
    }
    if opts.browse {
        // After the listener binds, or the browser races it and lands on a
        // connection refused.
        let _ = std::process::Command::new("open").arg(&url).spawn();
    }
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = shutdown_rx.await;
        })
        .await?;
    Ok(())
}

/// Entry point for `videre gallery`: the same server, every view on its own
/// route.
///
/// This module is the HTTP layer only. The renderer it shares with
/// `dedupe --html` and `search --html` lives in `crate::render`.
pub(crate) fn serve_gallery(
    ctx: &crate::command_context::CommandContext,
    model_id: String,
    port: Option<u16>,
    browse: bool,
) -> anyhow::Result<()> {
    let db = ctx.library.paths.db.clone();
    // No `--port`: start at the default and let a busy port advance to the next
    // free one, so a second `videre gallery` lands on 7879 by itself. An
    // explicit `--port` is used exactly, taken or not.
    let (port, port_fallback) = match port {
        Some(port) => (port, false),
        None => (DEFAULT_GALLERY_PORT, true),
    };
    let opts = ServeOptions {
        serve_faces_ui: true,
        report_all: true,
        report_heic: false,
        report_heic_original: false,
        model_id,
        gallery: true,
        context: Arc::new(ctx.clone()),
        port,
        port_fallback,
        browse,
    };
    serve_faces(&db, opts).map_err(|e| anyhow::anyhow!("{e}"))
}

fn serve_faces(db: &Path, opts: ServeOptions) -> Result<(), Box<dyn std::error::Error>> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(serve_faces_async(db, opts))
}

#[cfg(test)]
mod thumbnail_tests {
    use super::*;
    use axum::body::{to_bytes, Body};
    use axum::http::{header, Request};
    use tower::ServiceExt;

    /// The pre-refactor body of `render_raster_thumbnail`, kept as a
    /// reference oracle: the shared-helper refactor must stay byte-identical
    /// so the `raster-v1` cache entries remain valid.
    fn render_raster_thumbnail_reference(bytes: &[u8], max_px: u32) -> Option<Vec<u8>> {
        use image::ImageDecoder;

        let reader = image::ImageReader::new(std::io::Cursor::new(bytes))
            .with_guessed_format()
            .ok()?;
        let mut decoder = reader.into_decoder().ok()?;
        let orientation = decoder
            .orientation()
            .unwrap_or(image::metadata::Orientation::NoTransforms);
        let mut img = image::DynamicImage::from_decoder(decoder).ok()?;
        img.apply_orientation(orientation);
        let img = if img.width() > max_px || img.height() > max_px {
            img.resize(max_px, max_px, image::imageops::FilterType::Triangle)
        } else {
            img
        };
        let mut buf = Vec::new();
        img.write_to(
            &mut std::io::Cursor::new(&mut buf),
            image::ImageFormat::Jpeg,
        )
        .ok()?;
        Some(buf)
    }

    #[test]
    fn refactored_renderer_matches_reference_byte_for_byte() {
        let base = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/");
        for name in [
            "ai-generated-couple.jpg",
            "ai-generated-couple_o6.jpg",
            "tiny.jpg",
        ] {
            let bytes = std::fs::read(format!("{base}{name}")).unwrap();
            let a = render_raster_thumbnail(&bytes, 240).unwrap();
            let b = render_raster_thumbnail_reference(&bytes, 240).unwrap();
            assert_eq!(
                a, b,
                "{name}: refactored renderer must stay byte-identical to the pre-refactor body, \
                 otherwise raster-v1 cache entries are silently invalidated"
            );
        }
    }

    fn raw_file_test_state(
        root: &std::path::Path,
        cache: &std::path::Path,
        file: &std::path::Path,
    ) -> Arc<AppState> {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE file_hashes (path TEXT, hash TEXT);")
            .unwrap();
        conn.execute(
            "INSERT INTO file_hashes (path, hash) VALUES (?1, 'video-hash')",
            [file.to_str().unwrap()],
        )
        .unwrap();
        let library = Arc::new(
            videre_core::library::LibraryContext::new(root, cache)
                .expect("test library context should be valid"),
        );
        let context = Arc::new(crate::command_context::CommandContext {
            library,
            invocation_dir: root.to_path_buf(),
            source: crate::command_context::LibrarySource::Cwd,
        });
        let (shutdown_tx, _shutdown_rx) = tokio::sync::oneshot::channel();
        Arc::new(AppState {
            conn: Arc::new(Mutex::new(conn)),
            events_cache: Mutex::new(None),
            learning: None,
            shutdown_tx: Mutex::new(Some(shutdown_tx)),
            model_id: String::new(),
            report_heic: false,
            report_heic_original: false,
            serve_faces_ui: true,
            gallery: true,
            context,
            embedder: Mutex::new(None),
            basemap_downloading: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            settings_lock: Arc::new(Mutex::new(())),
            settings_warned: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        })
    }

    #[tokio::test]
    async fn raw_video_supports_full_and_single_range_responses() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("clip.mp4");
        std::fs::write(&file, b"0123456789").unwrap();
        let state = raw_file_test_state(dir.path(), &dir.path().join("cache"), &file);
        let app = Router::new()
            .route("/api/files/{hash}/raw", get(handle_raw_file))
            .with_state(state);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/files/video-hash/raw")
                    .header(header::RANGE, "bytes=2-5")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            response.headers().get(header::ACCEPT_RANGES).unwrap(),
            "bytes"
        );
        assert_eq!(
            response.headers().get(header::CONTENT_RANGE).unwrap(),
            "bytes 2-5/10"
        );
        assert_eq!(response.headers().get(header::CONTENT_LENGTH).unwrap(), "4");
        assert_eq!(
            to_bytes(response.into_body(), usize::MAX).await.unwrap(),
            b"2345"[..]
        );

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/files/video-hash/raw")
                    .header(header::RANGE, "bytes=20-30")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::RANGE_NOT_SATISFIABLE);
        assert_eq!(
            response.headers().get(header::CONTENT_RANGE).unwrap(),
            "bytes */10"
        );

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/files/video-hash/raw")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::ACCEPT_RANGES).unwrap(),
            "bytes"
        );
        assert_eq!(
            to_bytes(response.into_body(), usize::MAX).await.unwrap(),
            b"0123456789"[..]
        );
    }

    #[tokio::test]
    async fn sized_raster_request_ignores_legacy_unoriented_cache() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("oriented.jpg");
        std::fs::copy(
            concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/tiny.jpg"),
            &file,
        )
        .unwrap();
        let state = raw_file_test_state(dir.path(), &dir.path().join("cache"), &file);
        let legacy_cache = videre_core::thumb_cache::thumb_path_in(
            &state.context.library.cache,
            "video-hash",
            240,
        );
        let raster_cache = videre_core::thumb_cache::raster_thumb_path_in(
            &state.context.library.cache,
            "video-hash",
            240,
        );
        std::fs::create_dir_all(legacy_cache.parent().unwrap()).unwrap();
        std::fs::write(&legacy_cache, b"legacy sideways thumbnail").unwrap();
        let app = Router::new()
            .route("/api/files/{hash}/raw", get(handle_raw_file))
            .with_state(state);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/files/video-hash/raw?size=240")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_ne!(bytes.as_ref(), b"legacy sideways thumbnail");
        let decoded = image::load_from_memory(&bytes).expect("thumbnail must decode");
        assert_eq!((decoded.width(), decoded.height()), (12, 16));

        assert_eq!(std::fs::read(&raster_cache).unwrap(), bytes.as_ref());
        std::fs::remove_file(&file).unwrap();

        let cached_response = app
            .oneshot(
                Request::builder()
                    .uri("/api/files/video-hash/raw?size=240")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(cached_response.status(), StatusCode::OK);
        let cached_bytes = to_bytes(cached_response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(cached_bytes.as_ref(), bytes.as_ref());
    }

    #[test]
    fn render_raster_thumbnail_downscales_and_encodes_jpeg() {
        // A grid tile must be served as a small JPEG, not the full original, or
        // a large library on a slow drive starves the browser's connection pool
        // and most tiles never load. Encode an 800x600 source, ask for a 240px
        // thumbnail, and assert it comes back as a JPEG bounded to 240px.
        let mut src = Vec::new();
        image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            800,
            600,
            image::Rgb([120, 130, 140]),
        ))
        .write_to(&mut std::io::Cursor::new(&mut src), image::ImageFormat::Png)
        .unwrap();

        let out = render_raster_thumbnail(&src, 240).expect("should render a thumbnail");
        assert_eq!(&out[..2], &[0xFF, 0xD8], "JPEG SOI marker expected");
        let decoded = image::load_from_memory(&out).expect("thumbnail must decode");
        assert!(
            decoded.width() <= 240 && decoded.height() <= 240,
            "thumbnail must be bounded to 240px, got {}x{}",
            decoded.width(),
            decoded.height()
        );
        assert_eq!(decoded.width(), 240, "the long edge should scale to 240");
    }

    #[test]
    fn render_raster_thumbnail_applies_exif_orientation_before_resizing() {
        let source = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/tiny.jpg"
        ));

        let out = render_raster_thumbnail(source, 240).expect("should render a thumbnail");
        let decoded = image::load_from_memory(&out).expect("thumbnail must decode");

        assert_eq!(
            (decoded.width(), decoded.height()),
            (12, 16),
            "EXIF Orientation 6 should rotate the 16x12 source into portrait pixels"
        );
    }

    #[test]
    fn render_raster_thumbnail_rejects_non_image_bytes() {
        assert!(render_raster_thumbnail(b"not an image", 240).is_none());
    }

    #[test]
    fn only_browser_raster_formats_are_thumbnailed() {
        for e in ["jpg", "jpeg", "png", "gif", "webp", "bmp"] {
            assert!(is_thumbnailable_raster(e), "{e} should be thumbnailed");
        }
        for e in ["heic", "mp4", "mov", "dng", "tiff", ""] {
            assert!(
                !is_thumbnailable_raster(e),
                "{e} must not go through raster thumbnailing"
            );
        }
    }
}

#[cfg(test)]
mod rotate_geometry_tests {
    use super::*;

    #[test]
    fn an_unreadable_face_row_is_logged_and_the_others_still_rotate() {
        #[derive(Clone, Default)]
        struct Buf(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
        impl std::io::Write for Buf {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(b);
                Ok(b.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let temp = tempfile::tempdir().unwrap();
        let state = location_cluster_tests::gallery_state(temp.path());
        {
            let conn = state.conn.lock().unwrap();
            conn.execute_batch(
                "CREATE TABLE faces (id INTEGER PRIMARY KEY, hash TEXT, bbox TEXT,
                     landmark TEXT, oriented INTEGER);
                 INSERT INTO faces VALUES (1, 'çağla', '10,20,30,40', NULL, 1);
                 INSERT INTO faces VALUES (2, 'çağla', X'00', NULL, 1);",
            )
            .unwrap();
        }
        let buf = Buf::default();
        let w = buf.clone();
        let sub = tracing_subscriber::fmt()
            .with_writer(move || w.clone())
            .finish();
        tracing::subscriber::with_default(sub, || {
            rotate_faces_geometry(&state, "çağla", false, 100, 80)
        });

        let logged = String::from_utf8_lossy(&buf.0.lock().unwrap()).to_string();
        assert!(
            logged.contains("ERROR") && logged.contains("reading faces for rotate"),
            "{logged}"
        );
        let conn = state.conn.lock().unwrap();
        let rotated: String = conn
            .query_row("SELECT bbox FROM faces WHERE id = 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rotated, "20,10,40,30", "the readable face still turns");
    }
}

#[cfg(test)]
mod settings_api_tests {
    use super::*;
    use axum::body::{to_bytes, Body};
    use axum::http::{header, Request};
    use serde_json::{json, Value};
    use tower::ServiceExt;

    /// A library whose state exists, as every served library's does: the
    /// write path takes the activity lock, which needs `.videre/locks`.
    fn gallery_state(root: &Path) -> Arc<AppState> {
        std::fs::create_dir_all(root.join(".videre/locks")).unwrap();
        super::location_cluster_tests::gallery_state(root)
    }

    fn app(state: Arc<AppState>) -> Router {
        Router::new()
            .route(
                "/api/settings",
                get(handle_get_settings)
                    .patch(handle_patch_settings)
                    .put(handle_put_settings),
            )
            .with_state(state)
    }

    async fn send(app: &Router, method: &str, ctype: &str, body: &str) -> (StatusCode, Value) {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri("/api/settings")
                    .header(header::CONTENT_TYPE, ctype)
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    async fn get_settings(app: &Router) -> Value {
        let (status, body) = send(app, "GET", "application/json", "").await;
        assert_eq!(status, StatusCode::OK);
        body
    }

    async fn patch_settings(app: &Router, body: Value) -> (StatusCode, Value) {
        send(
            app,
            "PATCH",
            "application/merge-patch+json",
            &body.to_string(),
        )
        .await
    }

    fn file(root: &Path) -> std::path::PathBuf {
        root.join(".videre").join("gallery.json")
    }

    #[tokio::test]
    async fn a_fresh_library_has_the_defaults_and_no_overrides() {
        let dir = tempfile::tempdir().unwrap();
        let app = app(gallery_state(dir.path()));
        let body = get_settings(&app).await;
        assert_eq!(body["effective"], super::super::settings::defaults());
        assert_eq!(body["overrides"], json!({}));
        assert_eq!(body["ignored"], json!([]));
        assert_eq!(body["error"], Value::Null);
    }

    #[tokio::test]
    async fn a_patch_is_stored_sparse_and_null_reverts_it() {
        let dir = tempfile::tempdir().unwrap();
        let app = app(gallery_state(dir.path()));

        let (status, _) =
            patch_settings(&app, json!({"routes": {"files": {"view": "list"}}})).await;
        assert_eq!(status, StatusCode::OK);
        let on_disk: Value =
            serde_json::from_str(&std::fs::read_to_string(file(dir.path())).unwrap()).unwrap();
        assert_eq!(on_disk, json!({"routes": {"files": {"view": "list"}}}));
        assert_eq!(
            get_settings(&app).await["effective"]["routes"]["files"]["view"],
            "list"
        );

        let (status, body) =
            patch_settings(&app, json!({"routes": {"files": {"view": null}}})).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["effective"]["routes"]["files"]["view"], "tile");
    }

    #[tokio::test]
    async fn choosing_the_default_again_removes_the_override() {
        // Switching List back to Tile must not store "tile": a stored copy of
        // a default would stop a later changed default reaching this library.
        let dir = tempfile::tempdir().unwrap();
        let app = app(gallery_state(dir.path()));
        patch_settings(&app, json!({"routes": {"files": {"view": "list"}}})).await;
        let (status, body) =
            patch_settings(&app, json!({"routes": {"files": {"view": "tile"}}})).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["overrides"], json!({}));
        let on_disk: Value =
            serde_json::from_str(&std::fs::read_to_string(file(dir.path())).unwrap()).unwrap();
        assert_eq!(on_disk, json!({}));

        // An import that repeats defaults stores only what differs.
        let (_, body) = send(
            &app,
            "PUT",
            "application/json",
            r#"{"routes":{"files":{"view":"tile","pageSize":50}}}"#,
        )
        .await;
        assert_eq!(
            body["overrides"],
            json!({"routes": {"files": {"pageSize": 50}}})
        );
    }

    #[tokio::test]
    async fn a_hand_edit_shows_up_without_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let app = app(gallery_state(dir.path()));
        get_settings(&app).await;
        std::fs::create_dir_all(dir.path().join(".videre")).unwrap();
        std::fs::write(file(dir.path()), r#"{"routes":{"people":{"align":"top"}}}"#).unwrap();
        assert_eq!(
            get_settings(&app).await["effective"]["routes"]["people"]["align"],
            "top"
        );
    }

    #[tokio::test]
    async fn a_wrong_type_is_stored_but_ignored_and_reported() {
        let dir = tempfile::tempdir().unwrap();
        let app = app(gallery_state(dir.path()));
        let (status, body) =
            patch_settings(&app, json!({"routes": {"files": {"pageSize": "x"}}})).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["ignored"], json!(["routes.files.pageSize"]));
        assert_eq!(body["effective"]["routes"]["files"]["pageSize"], 200);
    }

    #[tokio::test]
    async fn an_unreadable_file_is_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let app = app(gallery_state(dir.path()));
        std::fs::create_dir_all(dir.path().join(".videre")).unwrap();
        std::fs::write(file(dir.path()), "{oops").unwrap();

        let (status, _) =
            patch_settings(&app, json!({"routes": {"files": {"view": "list"}}})).await;
        assert_eq!(status, StatusCode::CONFLICT);
        let (status, _) = send(&app, "PUT", "application/json", r#"{"routes":{}}"#).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(std::fs::read_to_string(file(dir.path())).unwrap(), "{oops");

        let body = get_settings(&app).await;
        assert_eq!(body["effective"], super::super::settings::defaults());
        assert!(body["error"].as_str().unwrap().contains("not valid JSON"));
    }

    #[tokio::test]
    async fn import_replaces_routes_and_keeps_where_the_library_resumes() {
        let dir = tempfile::tempdir().unwrap();
        let app = app(gallery_state(dir.path()));
        patch_settings(
            &app,
            json!({"resume": {"route": "/map"}, "routes": {"files": {"view": "list"}}}),
        )
        .await;
        let (status, body) = send(
            &app,
            "PUT",
            "application/json",
            r#"{"routes":{"map":{"radiusKm":5}},"resume":{"route":"/date"}}"#,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body["overrides"],
            json!({"resume": {"route": "/map"}, "routes": {"map": {"radiusKm": 5}}})
        );
    }

    #[tokio::test]
    async fn malformed_bodies_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let app = app(gallery_state(dir.path()));
        for body in [r#"{"routes":3}"#, "[]", "{}"] {
            let (status, _) = send(&app, "PUT", "application/json", body).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        }
        let (status, _) = send(&app, "PATCH", "application/json", "[1]").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        // A content type that is not CORS-simple is what keeps a cross-site
        // page from writing without a preflight; plain text must not get in.
        let (status, _) = send(&app, "PATCH", "text/plain", r#"{"routes":{}}"#).await;
        assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
        assert!(!file(dir.path()).exists());
    }

    #[tokio::test]
    async fn an_oversized_write_is_refused_and_leaves_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let app = app(gallery_state(dir.path()));
        patch_settings(&app, json!({"routes": {"files": {"view": "list"}}})).await;
        let before = std::fs::read_to_string(file(dir.path())).unwrap();
        let (status, _) = patch_settings(&app, json!({"big": "a".repeat(70_000)})).await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(std::fs::read_to_string(file(dir.path())).unwrap(), before);
    }

    async fn page(app: &Router, uri: &str) -> String {
        let response = app
            .clone()
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{uri}");
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[tokio::test]
    async fn the_settings_page_offers_import_export_and_reset() {
        let dir = tempfile::tempdir().unwrap();
        let app = Router::new()
            .route("/settings", get(handle_settings_page))
            .with_state(gallery_state(dir.path()));
        let html = page(&app, "/settings").await;
        for id in ["settings-export", "settings-import", "settings-reset"] {
            assert!(html.contains(&format!("id=\"{id}\"")), "{id}");
        }
        assert!(html.contains(".videre/gallery.json"), "shows the file path");
        assert!(html.contains("id=\"secnav-more\""), "carries the nav menu");
    }

    #[tokio::test]
    async fn gallery_pages_have_the_menu_with_a_settings_item() {
        // The menu lives in the shared `nav.html` include, so two pages built
        // on different templates are enough to show every page carries it.
        let dir = tempfile::tempdir().unwrap();
        let state = gallery_state(dir.path());
        state
            .conn
            .lock()
            .unwrap()
            .execute_batch("CREATE TABLE file_hashes (path TEXT, hash TEXT);")
            .unwrap();
        let app = Router::new()
            .route("/people", get(handle_root))
            .route("/map", get(handle_map))
            .with_state(state);
        for uri in ["/people", "/map"] {
            let html = page(&app, uri).await;
            assert!(html.contains("id=\"secnav-more\""), "{uri}");
            assert!(
                html.contains("role=\"menuitem\" href=\"/settings\""),
                "{uri}"
            );
        }
    }

    #[test]
    fn the_gallery_opens_at_the_saved_route_when_it_is_safe() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join(".videre");
        let addr = "127.0.0.1:7878";
        assert_eq!(startup_url(addr, &state), "http://127.0.0.1:7878");

        std::fs::create_dir_all(&state).unwrap();
        let saved = |route: &str| {
            std::fs::write(
                state.join("gallery.json"),
                json!({"resume": {"route": route}}).to_string(),
            )
            .unwrap();
        };
        saved("/map/location/berlin?radius=25");
        assert_eq!(
            startup_url(addr, &state),
            "http://127.0.0.1:7878/map/location/berlin?radius=25"
        );
        saved("/api/quit");
        assert_eq!(startup_url(addr, &state), "http://127.0.0.1:7878");
        std::fs::write(state.join("gallery.json"), "{oops").unwrap();
        assert_eq!(startup_url(addr, &state), "http://127.0.0.1:7878");
    }

    #[tokio::test]
    async fn two_libraries_keep_separate_settings() {
        let one = tempfile::tempdir().unwrap();
        let two = tempfile::tempdir().unwrap();
        let app_one = app(gallery_state(one.path()));
        let app_two = app(gallery_state(two.path()));
        patch_settings(&app_one, json!({"routes": {"files": {"view": "list"}}})).await;
        assert_eq!(
            get_settings(&app_one).await["effective"]["routes"]["files"]["view"],
            "list"
        );
        assert_eq!(
            get_settings(&app_two).await["effective"]["routes"]["files"]["view"],
            "tile"
        );
    }
}

#[cfg(test)]
mod recluster_api_tests {
    use super::*;
    use axum::body::{to_bytes, Body};
    use axum::http::{header, Request};
    use serde_json::{json, Value};
    use tower::ServiceExt;

    /// A real library on disk (the handlers open their own connection), with
    /// four unlabeled faces: three identical, which group, and one apart.
    fn library(root: &Path) -> Arc<AppState> {
        let state = super::location_cluster_tests::gallery_state(root);
        let conn = videre_core::library_db::initialize(&state.context.library).unwrap();
        let unit = |axis: usize| -> Vec<u8> {
            (0..512)
                .flat_map(|i| {
                    let v = if i == axis { 1.0f32 } else { 0.0 };
                    half::f16::from_f32(v).to_le_bytes()
                })
                .collect()
        };
        for (id, axis) in [(1, 0), (2, 0), (3, 0), (4, 1)] {
            conn.execute(
                "INSERT INTO faces (id, hash, bbox, embedding, blur) VALUES (?1, ?2, '0,0,100,100', ?3, 500.0)",
                rusqlite::params![id, format!("h{id}"), unit(axis)],
            )
            .unwrap();
        }
        state
    }

    fn app(state: Arc<AppState>) -> Router {
        Router::new()
            .route(
                "/api/faces/cluster-params",
                get(super::super::recluster::handle_cluster_params),
            )
            .route(
                "/api/faces/recluster/preview",
                post(super::super::recluster::handle_preview),
            )
            .route(
                "/api/faces/recluster",
                post(super::super::recluster::handle_apply),
            )
            .with_state(state)
    }

    async fn call(app: &Router, method: &str, uri: &str, body: Value) -> (StatusCode, Value) {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(if method == "GET" {
                        Body::empty()
                    } else {
                        Body::from(body.to_string())
                    })
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    fn clustered(state: &AppState) -> Vec<(i64, Option<i64>)> {
        let conn = videre_core::library_db::open_existing(&state.context.library).unwrap();
        let mut stmt = conn
            .prepare("SELECT id, cluster_id FROM faces ORDER BY id")
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    fn settings(root: &Path) -> Option<Value> {
        std::fs::read_to_string(root.join(".videre/gallery.json"))
            .ok()
            .map(|t| serde_json::from_str(&t).unwrap())
    }

    #[tokio::test]
    async fn preview_reports_the_grouping_and_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let state = library(dir.path());
        let app = app(state.clone());
        let (status, body) = call(&app, "POST", "/api/faces/recluster/preview", json!({})).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["cluster_count"], 1);
        assert_eq!(body["clustered_faces"], 3);
        assert_eq!(body["singletons"], 1);
        assert_eq!(
            body["before"],
            json!({ "cluster_count": 0, "singletons": 4 })
        );
        assert!(clustered(&state).iter().all(|(_, c)| c.is_none()));
        assert_eq!(settings(dir.path()), None);
    }

    #[tokio::test]
    async fn apply_regroups_records_the_run_and_saves_only_what_differs() {
        let dir = tempfile::tempdir().unwrap();
        let state = library(dir.path());
        let app = app(state.clone());
        let (status, body) =
            call(&app, "POST", "/api/faces/recluster", json!({ "eps": 0.7 })).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["saved"], true);
        let rows = clustered(&state);
        let cluster = rows[0].1.expect("the identical faces group");
        assert_eq!(rows[1].1, Some(cluster));
        assert_eq!(rows[3].1, None);
        assert_eq!(
            settings(dir.path()).unwrap(),
            json!({ "faces": { "clustering": { "eps": 0.7f32 } } })
        );
        let conn = videre_core::library_db::open_existing(&state.context.library).unwrap();
        let runs: i64 = conn
            .query_row(
                "SELECT count(*) FROM pipeline_runs WHERE command = 'face-recluster' AND status = 'success'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(runs, 1);

        let (_, params) = call(&app, "GET", "/api/faces/cluster-params", Value::Null).await;
        assert_eq!(params["saved"], json!({ "eps": 0.7f32 }));
        assert_eq!(params["effective"]["eps"], json!(0.7f32));
        assert_eq!(params["defaults"]["eps"], json!(0.6f32));
    }

    #[tokio::test]
    async fn applying_the_defaults_removes_the_override_and_keeps_other_settings() {
        let dir = tempfile::tempdir().unwrap();
        let state = library(dir.path());
        std::fs::write(
            dir.path().join(".videre/gallery.json"),
            r#"{ "faces": { "clustering": { "eps": 0.7 } }, "routes": { "people": { "align": "top" } } }"#,
        )
        .unwrap();
        let app = app(state);
        let (status, _) = call(&app, "POST", "/api/faces/recluster", json!({ "eps": 0.6 })).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            settings(dir.path()).unwrap(),
            json!({ "routes": { "people": { "align": "top" } } })
        );
    }

    #[tokio::test]
    async fn apply_is_refused_while_a_faces_run_holds_the_lock() {
        let dir = tempfile::tempdir().unwrap();
        let state = library(dir.path());
        let _held =
            videre_core::library_locks::try_command(&state.context.library, "faces").unwrap();
        let (status, body) = call(
            &app(state.clone()),
            "POST",
            "/api/faces/recluster",
            json!({}),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["error"], "faces_busy");
        assert!(clustered(&state).iter().all(|(_, c)| c.is_none()));
    }

    #[tokio::test]
    async fn an_out_of_range_parameter_is_named() {
        let dir = tempfile::tempdir().unwrap();
        let app = app(library(dir.path()));
        for uri in ["/api/faces/recluster/preview", "/api/faces/recluster"] {
            let (status, body) = call(&app, "POST", uri, json!({ "min_cluster_size": 0 })).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}");
            assert_eq!(
                body,
                json!({ "error": "invalid_parameter", "field": "min_cluster_size" })
            );
        }
    }
}

#[cfg(test)]
mod bulk_api_tests {
    use super::*;
    use axum::body::{to_bytes, Body};
    use axum::http::{header, Request};
    use serde_json::{json, Value};
    use tower::ServiceExt;

    /// A real library holding `files` (name, hash); photos are the tiny JPEG
    /// fixture so rotate has an image to turn.
    fn library(root: &Path, files: &[(&str, &str)]) -> Arc<AppState> {
        let state = super::location_cluster_tests::gallery_state(root);
        let disk = videre_core::library_db::initialize(&state.context.library).unwrap();
        let fixture = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/tiny.jpg");
        for (name, hash) in files {
            let path = root.join(name);
            std::fs::copy(fixture, &path).unwrap();
            let ext = name.rsplit('.').next().unwrap();
            disk.execute(
                "INSERT INTO file_hashes (path, hash, ext) VALUES (?1, ?2, ?3)",
                rusqlite::params![path.to_string_lossy(), hash, ext],
            )
            .unwrap();
        }
        drop(disk);
        // The handlers use the server's shared connection: point it at the library.
        *state.conn.lock().unwrap() =
            videre_core::library_db::open_existing(&state.context.library).unwrap();
        state
    }

    fn app(state: Arc<AppState>) -> Router {
        Router::new()
            .route("/api/files/marks", post(super::super::bulk::handle_marks))
            .route("/api/files/tags", post(super::super::bulk::handle_tags))
            .route("/api/files/rotate", post(super::super::bulk::handle_rotate))
            .route("/api/tags", get(super::super::bulk::handle_list_tags))
            .with_state(state)
    }

    async fn call(app: &Router, method: &str, uri: &str, body: Value) -> (StatusCode, Value) {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(if method == "GET" {
                        Body::empty()
                    } else {
                        Body::from(body.to_string())
                    })
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    #[tokio::test]
    async fn marks_apply_to_every_selected_item_and_keep_toggles() {
        let dir = tempfile::tempdir().unwrap();
        let state = library(dir.path(), &[("a.jpg", "ha"), ("b.jpg", "hb")]);
        let app = app(state.clone());
        let (status, body) = call(
            &app,
            "POST",
            "/api/files/marks",
            json!({ "hashes": ["ha", "hb"], "rating": 4, "liked": true }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["updated"], 2);
        {
            let conn = state.conn.lock().unwrap();
            for h in ["ha", "hb"] {
                let m = videre_core::marks::get(&conn, h).unwrap();
                assert_eq!((m.rating, m.liked), (Some(4), true));
            }
        }
        let (_, first) = call(
            &app,
            "POST",
            "/api/files/marks",
            json!({ "hashes": ["ha", "hb"], "pick": "keep" }),
        )
        .await;
        assert_eq!(first["pick"], "keep");
        let (_, second) = call(
            &app,
            "POST",
            "/api/files/marks",
            json!({ "hashes": ["ha", "hb"], "pick": "keep" }),
        )
        .await;
        assert_eq!(second["pick"], "none", "Keep on items all kept clears it");
        let conn = state.conn.lock().unwrap();
        assert_eq!(videre_core::marks::get(&conn, "ha").unwrap().pick, None);
    }

    #[tokio::test]
    async fn an_unknown_empty_or_changeless_request_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let app = app(library(dir.path(), &[("a.jpg", "ha")]));
        let (s, b) = call(
            &app,
            "POST",
            "/api/files/marks",
            json!({ "hashes": ["ha", "nope"], "liked": true }),
        )
        .await;
        assert_eq!(
            (s, b["error"].as_str()),
            (StatusCode::BAD_REQUEST, Some("unknown_hash"))
        );
        let (s, b) = call(
            &app,
            "POST",
            "/api/files/marks",
            json!({ "hashes": [], "liked": true }),
        )
        .await;
        assert_eq!(
            (s, b["field"].as_str()),
            (StatusCode::BAD_REQUEST, Some("hashes"))
        );
        let (s, b) = call(
            &app,
            "POST",
            "/api/files/marks",
            json!({ "hashes": ["ha"] }),
        )
        .await;
        assert_eq!(
            (s, b["error"].as_str()),
            (StatusCode::BAD_REQUEST, Some("no_change"))
        );
        let (s, _) = call(
            &app,
            "POST",
            "/api/files/rotate",
            json!({ "hashes": ["ha"], "direction": "up" }),
        )
        .await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn tags_add_and_remove_and_the_list_counts_them() {
        let dir = tempfile::tempdir().unwrap();
        let app = app(library(dir.path(), &[("a.jpg", "ha"), ("b.jpg", "hb")]));
        let (s, _) = call(
            &app,
            "POST",
            "/api/files/tags",
            json!({ "hashes": ["ha", "hb"], "add": [" İstanbul "] }),
        )
        .await;
        assert_eq!(s, StatusCode::OK);
        let (_, tags) = call(&app, "GET", "/api/tags", Value::Null).await;
        assert_eq!(tags, json!([{ "tag": "İstanbul", "count": 2 }]));
        call(
            &app,
            "POST",
            "/api/files/tags",
            json!({ "hashes": ["hb"], "remove": ["İstanbul"] }),
        )
        .await;
        let (_, tags) = call(&app, "GET", "/api/tags", Value::Null).await;
        assert_eq!(tags, json!([{ "tag": "İstanbul", "count": 1 }]));
    }

    #[tokio::test]
    async fn rotate_turns_photos_and_skips_what_cannot_turn() {
        let dir = tempfile::tempdir().unwrap();
        let app = app(library(dir.path(), &[("a.jpg", "ha"), ("clip.mp4", "hv")]));
        let (s, b) = call(
            &app,
            "POST",
            "/api/files/rotate",
            json!({ "hashes": ["ha", "hv"], "direction": "cw" }),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{b}");
        assert_eq!(b, json!({ "rotated": 1, "skipped": 1, "failed": 0 }));
    }
}
