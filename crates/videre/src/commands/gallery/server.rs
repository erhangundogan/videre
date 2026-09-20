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

    fn gallery_state(root: &Path) -> Arc<AppState> {
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
            conn: Mutex::new(conn),
            shutdown_tx: Mutex::new(Some(shutdown_tx)),
            model_id: String::new(),
            report_heic: false,
            report_heic_original: false,
            serve_faces_ui: true,
            gallery: true,
            context,
            embedder: Mutex::new(None),
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

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/map/location/not-a-place")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        assert!(body.contains("var GLOC={\"kind\":\"unknown\",\"name\":\"notaplace\"};"));
        assert!(!body.contains("\"latitude\""));
        assert!(!body.contains("\"longitude\""));
        assert!(!body.contains("\"cluster_id\""));
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
        pub chrome: &'static str,
        pub gallery_css: &'static str,
        pub css: &'static str,
        pub justified_js: &'static str,
        pub gallery_js: &'static str,
        pub js: &'static str,
        pub globals: String,
        pub db: String,
        pub generated_at: String,
        pub total_files: i64,
        pub nav: Option<super::Section>,
    }

    pub const FACES_CSS: &str = include_str!("../../../static/faces.css");
    pub const FACES_JS: &str = include_str!("../../../static/faces.js");
    pub const CLUSTER_CSS: &str = include_str!("../../../static/cluster.css");
    pub const CLUSTER_JS: &str = include_str!("../../../static/cluster.js");
    pub const PERSON_CSS: &str = include_str!("../../../static/person.css");
    pub const PERSON_JS: &str = include_str!("../../../static/person.js");
    pub const MAP_CSS: &str = include_str!("../../../static/map.css");
    pub const MAP_JS: &str = include_str!("../../../static/map.js");
}

fn api_status(e: videre_api::Error) -> StatusCode {
    match e {
        videre_api::Error::NotFound => StatusCode::NOT_FOUND,
        videre_api::Error::Invalid => StatusCode::BAD_REQUEST,
        videre_api::Error::Db(_) => StatusCode::INTERNAL_SERVER_ERROR,
        videre_api::Error::Unavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
        videre_api::Error::Other(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

/// The per-operation guard every blocking server operation runs first: verify
/// the startup-bound library's root still names that library, then take its
/// shared activity lease. A replaced root or a library held by exclusive
/// maintenance yields a retryable 503 rather than serving from, or mutating,
/// whatever now sits at the path. The returned guard must be held for the whole
/// operation (decode and cache publication included), so callers bind it.
fn guard_operation(
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

struct AppState {
    conn: Mutex<Connection>,
    shutdown_tx: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    model_id: String,
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
    context: Arc<crate::command_context::CommandContext>,
    /// Loaded on the first ranking search and kept for the process's life. Empty
    /// until then: a gallery whose library nobody searches never loads a model.
    embedder: Mutex<Option<videre_ml::model::Embedder>>,
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
    render_live(&state, true, false, false, Some(Section::All))
}

/// `videre gallery`'s `/duplicates`: duplicate groups, the review
/// `dedupe --html` writes to a file.
async fn handle_gallery_duplicates(
    State(state): State<Arc<AppState>>,
) -> impl axum::response::IntoResponse {
    render_live(&state, false, false, true, Some(Section::Duplicates))
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
    render_live_date(&state, filter)
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
    render_live_date(&state, filter)
}

async fn handle_gallery_date_month(
    axum::extract::Path((year, month)): axum::extract::Path<(String, String)>,
    State(state): State<Arc<AppState>>,
) -> Result<axum::response::Response, StatusCode> {
    let filter = route_date_filter(&year, Some(&month), None)?;
    render_live_date(&state, filter)
}

async fn handle_gallery_date_day(
    axum::extract::Path((year, month, day)): axum::extract::Path<(String, String, String)>,
    State(state): State<Arc<AppState>>,
) -> Result<axum::response::Response, StatusCode> {
    let filter = route_date_filter(&year, Some(&month), Some(&day))?;
    render_live_date(&state, filter)
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

fn render_map(state: &AppState, location_json: &str) -> axum::response::Html<String> {
    use askama::Template;
    use chrono::Utc;

    let (db_path, total_files, has_embeddings) = {
        let conn = state.conn.lock().unwrap();
        let total = conn
            .query_row("SELECT COUNT(*) FROM file_hashes", [], |row| row.get(0))
            .unwrap_or(0);
        // Compute HAS_EMBEDDINGS exactly as the `/` handler does, so the Similar
        // button gates identically: an embedded library must show it on the grid
        // under the map too, not just on the files page.
        let has_embeddings = query_embedded_count(&conn, &state.model_id).is_some_and(|n| n > 0);
        (
            conn.path().map(|path| path.to_string()).unwrap_or_default(),
            total,
            has_embeddings,
        )
    };
    let globals = format!(
        "var LIVE_SERVER=true;\nvar HAS_EMBEDDINGS={};\nvar VIDEO_POSTERS={};\n\
         var GVIEW=\"all\";\nvar PEOPLE_ROOT=\"/people\";\nvar GDATE=null;\nvar GLOC={location_json};\nvar GROUPS=[];",
        has_embeddings,
        cfg!(target_os = "macos")
    );
    let page = pages::Map {
        chrome: CHROME_CSS,
        gallery_css: include_str!("../../../static/gallery.css"),
        css: pages::MAP_CSS,
        justified_js: include_str!("../../../static/justified-layout.js"),
        gallery_js: include_str!("../../../static/gallery.js"),
        js: pages::MAP_JS,
        globals,
        db: esc(&db_path),
        generated_at: Utc::now().format("%Y-%m-%d %H:%M UTC").to_string(),
        total_files,
        nav: Some(Section::Map),
    };
    axum::response::Html(page.render().expect("map template"))
}

async fn handle_map(State(state): State<Arc<AppState>>) -> axum::response::Html<String> {
    render_map(&state, "null")
}

async fn handle_map_location(
    axum::extract::Path(name): axum::extract::Path<String>,
    Query(query): Query<MapPageQuery>,
    State(state): State<Arc<AppState>>,
) -> Result<axum::response::Html<String>, StatusCode> {
    let radius = match query.radius {
        Some(radius) if radius.is_finite() && radius > 0.0 => Some(radius),
        Some(_) => return Err(StatusCode::BAD_REQUEST),
        None => None,
    };
    let resolved = {
        let conn = state
            .conn
            .lock()
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        resolve_map_location(&conn, &name).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    };
    let bootstrap = match resolved {
        Some(location) => MapLocationBootstrap::Location {
            name: location.route_name,
            radius: radius.unwrap_or(location.radius_km),
        },
        None => MapLocationBootstrap::Unknown {
            name: videre_core::person::normalize(&name).unwrap_or_default(),
        },
    };
    let json = serde_json::to_string(&bootstrap).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(render_map(&state, &json))
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
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let hits = match hits {
        Ok(h) => h,
        Err(e) => {
            eprintln!("videre gallery: /api/search failed: {e}");
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
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
) -> axum::response::Html<String> {
    render_live_with_date(state, all, by_date, with_groups, nav, "null")
}

fn render_live_date(
    state: &Arc<AppState>,
    filter: InitialDateFilter,
) -> Result<axum::response::Response, StatusCode> {
    use axum::response::IntoResponse;

    Ok(render_live_with_date(
        state,
        false,
        true,
        false,
        Some(Section::Date),
        &initial_date_filter_json(&filter),
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
        nav,
        view,
        options: RenderOptions {
            live: true,
            heic: state.report_heic,
            heic_original: state.report_heic_original,
            embedded,
            db_path,
            date_filter_json: date_filter_json.to_string(),
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
    let conn = state
        .conn
        .lock()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
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
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
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
/// The mark fields for a file object, as a leading-comma JSON fragment
/// (`,"rating":..,"pick":..,"label":..,"liked":..`). An unmarked photo reads as
/// nulls and `liked:false`, so every file has the same stable shape.
fn mark_fields_json(m: Option<&videre_core::marks::Marks>) -> String {
    use videre_core::marks::Pick;
    let rating = m
        .and_then(|m| m.rating)
        .map(|r| r.to_string())
        .unwrap_or_else(|| "null".into());
    let pick = match m.and_then(|m| m.pick) {
        Some(Pick::Keep) => "\"keep\"",
        Some(Pick::Reject) => "\"reject\"",
        None => "null",
    };
    let label = m
        .and_then(|m| m.label.as_deref())
        .and_then(|l| serde_json::to_string(l).ok())
        .unwrap_or_else(|| "null".into());
    let liked = m.map(|m| m.liked).unwrap_or(false);
    format!(",\"rating\":{rating},\"pick\":{pick},\"label\":{label},\"liked\":{liked}")
}

async fn handle_files(
    State(state): State<Arc<AppState>>,
    Query(q): Query<FilesQuery>,
) -> Result<axum::response::Response, StatusCode> {
    const MAX_LIMIT: i64 = 500;

    let view = q.view.as_deref().unwrap_or("all");
    let offset = q.offset.unwrap_or(0).max(0);
    let limit = q.limit.unwrap_or(200).clamp(1, MAX_LIMIT);

    let conn = state
        .conn
        .lock()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let faces_by_hash = videre_core::face_db::labeled_faces_by_hash(&conn).unwrap_or_default();
    let (rows, total) = match q.hashes.as_deref() {
        Some(h) if !h.is_empty() => {
            query_files_by_hash(&conn, h).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        }
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
            query_files_page(
                &conn,
                view,
                date_filter.as_ref(),
                offset,
                limit,
                location.as_ref(),
            )
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        }
    };

    let page_hashes: Vec<String> = rows.iter().map(|(r, _)| r.hash.clone()).collect();
    let marks = videre_core::marks::get_many(&conn, &page_hashes).unwrap_or_default();

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

    Ok(json_response(out))
}

/// Returns every persisted location cluster with its derived continent,
/// ordered largest first for the map legend and world overview.
async fn handle_location_clusters(State(state): State<Arc<AppState>>) -> Response {
    let conn = match state.conn.lock() {
        Ok(conn) => conn,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let rows = match map_locations(&conn) {
        Ok(rows) => rows,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
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

    let conn = state
        .conn
        .lock()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut stmt = conn.prepare(&sql).map_err(|e| {
        eprintln!("videre gallery: /api/dates query failed: {e}\n  {sql}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
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
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
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

async fn handle_get_faces(
    State(state): State<Arc<AppState>>,
) -> Result<AxumJson<FacesData>, StatusCode> {
    let conn = state
        .conn
        .lock()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    videre_api::faces_list(&conn)
        .map(AxumJson)
        .map_err(api_status)
}

async fn handle_assign(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(name): axum::extract::Path<String>,
    AxumJson(req): AxumJson<AssignFacesBody>,
) -> Result<StatusCode, StatusCode> {
    let conn = state
        .conn
        .lock()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    videre_api::assign(&conn, &req.face_ids, &name)
        .map(|_| StatusCode::OK)
        .map_err(api_status)
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
    let conn = state
        .conn
        .lock()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    videre_core::marks::set(&conn, std::slice::from_ref(&hash), &change)
        .map(|_| StatusCode::OK)
        .map_err(|e| {
            eprintln!("videre gallery: /api/mark failed: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })
}

async fn handle_new_person(
    State(state): State<Arc<AppState>>,
    AxumJson(req): AxumJson<NewPersonRequest>,
) -> Result<StatusCode, StatusCode> {
    let conn = state
        .conn
        .lock()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    videre_api::new_person(&conn, &req.face_ids, &req.name)
        .map(|_| StatusCode::OK)
        .map_err(api_status)
}

async fn handle_remove_face(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<i64>,
) -> Result<StatusCode, StatusCode> {
    let conn = state
        .conn
        .lock()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    videre_api::remove_face(&conn, id)
        .map(|_| StatusCode::OK)
        .map_err(api_status)
}

async fn handle_delete_person(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> Result<StatusCode, StatusCode> {
    let conn = state
        .conn
        .lock()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    videre_api::delete_person(&conn, &name)
        .map(|_| StatusCode::OK)
        .map_err(api_status)
}

async fn handle_set_full_name(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(name): axum::extract::Path<String>,
    AxumJson(req): AxumJson<SetFullNameBody>,
) -> Result<StatusCode, StatusCode> {
    let conn = state
        .conn
        .lock()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    videre_api::set_full_name(&conn, &name, &req.full_name)
        .map(|_| StatusCode::OK)
        .map_err(api_status)
}

async fn handle_dissolve_cluster(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<i64>,
) -> Result<StatusCode, StatusCode> {
    let conn = state
        .conn
        .lock()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    videre_api::dissolve_cluster(&conn, id)
        .map(|_| StatusCode::OK)
        .map_err(api_status)
}

async fn handle_set_primary(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<i64>,
    AxumJson(req): AxumJson<SetPrimaryBody>,
) -> Result<StatusCode, StatusCode> {
    let conn = state
        .conn
        .lock()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    videre_api::set_primary(&conn, id, &req.person_label)
        .map(|_| StatusCode::OK)
        .map_err(api_status)
}

async fn handle_search_person(
    State(state): State<Arc<AppState>>,
    Query(q): Query<PersonSearchQuery>,
) -> Result<AxumJson<Vec<String>>, StatusCode> {
    let conn = state
        .conn
        .lock()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
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
    StatusCode::OK
}

async fn handle_cluster_page(
    axum::extract::Path(cluster_id): axum::extract::Path<i64>,
    State(state): State<Arc<AppState>>,
) -> impl axum::response::IntoResponse {
    use askama::Template;
    let page = pages::Cluster {
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
    let conn = state
        .conn
        .lock()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    videre_api::cluster_detail(&conn, cluster_id)
        .map(AxumJson)
        .map_err(api_status)
}

async fn handle_person_page(State(state): State<Arc<AppState>>) -> axum::response::Html<String> {
    use askama::Template;
    let page = pages::Person {
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
    let conn = state
        .conn
        .lock()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
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
            let conn = state
                .conn
                .lock()
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            videre_api::face_lookup(&conn, face_id).map_err(|_| StatusCode::NOT_FOUND)?
        };
        videre_api::face_bytes_from_lookup(&lookup, face_id, &state.context.library.cache)
            .map_err(|_| StatusCode::NOT_FOUND)
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)??;
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
        let conn = state
            .conn
            .lock()
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        conn.query_row(
            "SELECT path FROM file_hashes WHERE hash = ?1 LIMIT 1",
            [&hash],
            |r| r.get::<_, String>(0),
        )
        .optional()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
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
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
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
        let conn = state
            .conn
            .lock()
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
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
                    eprintln!("warning: raw file unavailable for {path}: {e}; skipping");
                    return None;
                }
                Err(_) => {
                    eprintln!(
                        "warning: timed out reading {path} \
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
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

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
                let conn = state
                    .conn
                    .lock()
                    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
                videre_api::original_lookup(&conn, face_id).map_err(|_| StatusCode::NOT_FOUND)?
            };
            videre_api::original_bytes_from_lookup(&lookup, face_id, &state.context.library.cache)
                .map_err(|_| StatusCode::NOT_FOUND)
        })
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)??;
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
    browse: bool,
}

async fn serve_faces_async(
    db: &Path,
    opts: ServeOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    let conn = videre_core::db::open_wal(db)?;
    videre_core::location_cluster::register_haversine_sql_function(&conn)?;
    videre_core::location::ensure_location_column(&conn);
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
            eprintln!("Merged {merged} name(s) differing only in spelling; {people} people now");
        }
        Ok(_) => {}
        Err(e) => eprintln!("warning: could not migrate person names: {e}"),
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
            eprintln!("note: similarity search disabled ({e})");
        }
    }
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let state = Arc::new(AppState {
        conn: Mutex::new(conn),
        shutdown_tx: Mutex::new(Some(shutdown_tx)),
        model_id: opts.model_id.clone(),
        report_heic: opts.report_heic,
        report_heic_original: opts.report_heic_original,
        serve_faces_ui: opts.serve_faces_ui,
        gallery: opts.gallery,
        context: opts.context.clone(),
        embedder: Mutex::new(None),
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
        .route("/api/files/{hash}/raw", get(handle_raw_file))
        .route("/api/dates", get(handle_dates))
        .route("/api/search", get(handle_search))
        .route("/api/locations", get(handle_location))
        .route("/api/location-clusters", get(handle_location_clusters))
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
        .route("/api/faces/{id}/original", get(handle_original_image))
        // clusters
        .route(
            "/api/clusters/{id}",
            get(handle_cluster_api).delete(handle_dissolve_cluster),
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
        .route("/events", get(handle_not_yet))
        .route("/smart", get(handle_not_yet));

    let app = router.with_state(state);

    let requested = format!("127.0.0.1:{}", opts.port);
    let listener = tokio::net::TcpListener::bind(&requested)
        .await
        .map_err(|e| format!("Cannot bind to {requested}: {e}"))?;

    // :warning: Report the address that was BOUND, not the one requested. With
    // `--port 0` the OS picks a free port, and printing the request meant the
    // server announced `http://127.0.0.1:0`, which is unreachable. Anyone using
    // 0 to avoid choosing a port then had no way to find the server, and
    // `--browse` opened the same dead address.
    let addr = listener
        .local_addr()
        .map(|a| a.to_string())
        .unwrap_or(requested);
    let addr = addr.as_str();

    if opts.gallery {
        eprintln!("videre gallery: http://{addr}");
    } else {
        eprintln!("Faces labeling server: http://{addr}");
    }
    if opts.browse {
        // After the listener binds, or the browser races it and lands on a
        // connection refused.
        let _ = std::process::Command::new("open")
            .arg(format!("http://{addr}"))
            .spawn();
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
    port: u16,
    browse: bool,
) -> anyhow::Result<()> {
    let db = ctx.library.paths.db.clone();
    let opts = ServeOptions {
        serve_faces_ui: true,
        report_all: true,
        report_heic: false,
        report_heic_original: false,
        model_id,
        gallery: true,
        context: Arc::new(ctx.clone()),
        port,
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
            conn: Mutex::new(conn),
            shutdown_tx: Mutex::new(Some(shutdown_tx)),
            model_id: String::new(),
            report_heic: false,
            report_heic_original: false,
            serve_faces_ui: true,
            gallery: true,
            context,
            embedder: Mutex::new(None),
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
