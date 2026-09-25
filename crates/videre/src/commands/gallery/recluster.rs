//! The People toolbar's recluster: read the effective clustering parameters,
//! preview a pass without writing, and apply one.
//!
//! Apply is the same work as `videre faces --recluster` and watch's repair,
//! under the same locks and recorded as a `face-recluster` run, and it saves
//! the parameters to the library's `gallery.json`, where `faces` and watch
//! read them (`commands::cluster_settings`). Both run on their own connection
//! off the async workers, so a long pass does not hold the server's shared
//! connection or stall other requests.

use super::server::{guard_operation, internal, poisoned, settings_file, AppState};
use crate::commands::cluster_settings;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use rusqlite::Connection;
use serde::Serialize;
use serde_json::{json, Value};
use std::sync::Arc;
use videre_ml::cluster_params::{ClusteringParameters, PartialClusteringParameters};

/// Counts before or after a pass, over unlabeled faces only: labeled faces
/// are never regrouped.
#[derive(Debug, Serialize, PartialEq)]
pub(crate) struct Grouping {
    pub cluster_count: usize,
    pub singletons: usize,
}

#[derive(Debug, Serialize)]
pub(crate) struct ReclusterSummary {
    pub params: ClusteringParameters,
    pub total_faces: usize,
    pub clustered_faces: usize,
    pub cluster_count: usize,
    pub singletons: usize,
    pub held_out: usize,
    pub before: Grouping,
    /// Apply only: whether the parameters were saved to `gallery.json`, and
    /// why not when they were not.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub saved: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub settings_error: Option<String>,
}

const UNLABELED: &str = "NOT (confirmed = 1 AND person_label IS NOT NULL)";

fn current_grouping(conn: &Connection) -> rusqlite::Result<Grouping> {
    let clusters: i64 = conn.query_row(
        &format!(
            "SELECT count(DISTINCT cluster_id) FROM faces WHERE cluster_id IS NOT NULL AND {UNLABELED}"
        ),
        [],
        |r| r.get(0),
    )?;
    let singletons: i64 = conn.query_row(
        &format!("SELECT count(*) FROM faces WHERE cluster_id IS NULL AND {UNLABELED}"),
        [],
        |r| r.get(0),
    )?;
    Ok(Grouping {
        cluster_count: clusters as usize,
        singletons: singletons as usize,
    })
}

fn summary(
    params: ClusteringParameters,
    before: Grouping,
    outcome: Option<&videre_ml::pipeline::ClusterOutcome>,
) -> ReclusterSummary {
    let (total_faces, clustered_faces, cluster_count, held_out) = match outcome {
        Some(o) => {
            let r = o.summarize();
            (
                r.total_faces,
                r.clustered_faces,
                r.cluster_count,
                r.held_out,
            )
        }
        None => (0, 0, 0, 0),
    };
    ReclusterSummary {
        params,
        total_faces,
        clustered_faces,
        cluster_count,
        singletons: total_faces - clustered_faces,
        held_out,
        before,
        saved: None,
        settings_error: None,
    }
}

/// Why a request was refused, turned into its response at the handler.
enum Refusal {
    BadParameter(&'static str),
    Busy,
    Status(StatusCode),
}

impl From<StatusCode> for Refusal {
    fn from(status: StatusCode) -> Self {
        Self::Status(status)
    }
}

impl IntoResponse for Refusal {
    fn into_response(self) -> Response {
        match self {
            Self::BadParameter(field) => (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "invalid_parameter", "field": field })),
            )
                .into_response(),
            Self::Busy => {
                (StatusCode::CONFLICT, Json(json!({ "error": "faces_busy" }))).into_response()
            }
            Self::Status(status) => status.into_response(),
        }
    }
}

fn failed<E: Into<anyhow::Error>>(e: E) -> Refusal {
    Refusal::Status(internal(e))
}

/// A request's fields over the saved override over the built-in set.
fn requested(
    state: &AppState,
    body: &PartialClusteringParameters,
) -> Result<ClusteringParameters, Refusal> {
    let saved = cluster_settings::saved(&state.context.library.paths.state);
    let mut params =
        cluster_settings::resolve(&PartialClusteringParameters::default(), &saved.partial).params;
    body.apply_to(&mut params);
    params.validate().map_err(Refusal::BadParameter)?;
    Ok(params)
}

/// GET /api/faces/cluster-params
pub(crate) async fn handle_cluster_params(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, StatusCode> {
    let learning = {
        let conn = state.conn.lock().map_err(poisoned)?;
        videre_api::face_learning_status(&conn)
            .map(|s| s.summary)
            .unwrap_or_default()
    };
    let state_dir = state.context.library.paths.state.clone();
    tokio::task::spawn_blocking(move || {
        let saved = cluster_settings::saved(&state_dir);
        let effective =
            cluster_settings::resolve(&PartialClusteringParameters::default(), &saved.partial)
                .params;
        Json(json!({
            "effective": effective,
            "saved": saved.partial,
            "defaults": ClusteringParameters::default(),
            "warnings": saved.warnings,
            "learning": learning,
        }))
    })
    .await
    .map_err(internal)
}

/// POST /api/faces/recluster/preview: the pass `Apply` would run, computed
/// and reported, nothing written.
pub(crate) async fn handle_preview(
    State(state): State<Arc<AppState>>,
    Json(body): Json<PartialClusteringParameters>,
) -> Response {
    let params = match requested(&state, &body) {
        Ok(p) => p,
        Err(refusal) => return refusal.into_response(),
    };
    let result = tokio::task::spawn_blocking(move || -> Result<ReclusterSummary, Refusal> {
        let _activity = guard_operation(&state)?;
        let conn =
            videre_core::library_db::open_existing(&state.context.library).map_err(failed)?;
        let before = current_grouping(&conn).map_err(failed)?;
        let outcome =
            videre_ml::pipeline::compute_clustering(&conn, &params, true).map_err(failed)?;
        Ok(summary(params, before, outcome.as_ref()))
    })
    .await;
    match result {
        Ok(Ok(s)) => Json(s).into_response(),
        Ok(Err(refusal)) => refusal.into_response(),
        Err(e) => internal(e).into_response(),
    }
}

/// POST /api/faces/recluster: regroup the unlabeled faces, save the
/// parameters for this library, refresh the pending questions.
pub(crate) async fn handle_apply(
    State(state): State<Arc<AppState>>,
    Json(body): Json<PartialClusteringParameters>,
) -> Response {
    let params = match requested(&state, &body) {
        Ok(p) => p,
        Err(refusal) => return refusal.into_response(),
    };
    let result = tokio::task::spawn_blocking(move || apply(&state, params)).await;
    match result {
        Ok(Ok(s)) => Json(s).into_response(),
        Ok(Err(refusal)) => refusal.into_response(),
        Err(e) => internal(e).into_response(),
    }
}

fn apply(state: &AppState, params: ClusteringParameters) -> Result<ReclusterSummary, Refusal> {
    use videre_core::library_locks::{try_activity, try_command, ActivityMode};
    let library = &state.context.library;
    library
        .ensure_root_identity()
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    // The same locks, in the same order, as watch's repair pass: the run's
    // identity, then activity, then the faces lock that detection and the
    // standalone command hold, so none of them can overlap this.
    let _identity = try_command(library, "face-recluster").map_err(|_| Refusal::Busy)?;
    let _activity = try_activity(library, ActivityMode::Shared).map_err(|_| Refusal::Busy)?;
    let guard = try_command(library, "faces").map_err(|_| Refusal::Busy)?;
    let conn = videre_core::library_db::open_existing(library).map_err(failed)?;
    let before = current_grouping(&conn).map_err(failed)?;

    let outcome = videre_core::pipeline_runs::track_in_as(
        &conn,
        library,
        &guard,
        "faces",
        "face-recluster",
        || {
            let outcome = videre_ml::pipeline::compute_clustering(&conn, &params, true)?;
            if let Some(outcome) = &outcome {
                videre_core::face_db::update_cluster_assignments(&conn, &outcome.assignments)?;
            }
            videre_core::face_db::advance_recluster_watermark(&conn)?;
            Ok(outcome)
        },
    )
    .map_err(failed)?;

    let mut result = summary(params, before, outcome.as_ref());
    tracing::info!(
        "videre gallery: reclustered {} face(s) into {} cluster(s), {} singleton(s), eps {:.2}",
        result.total_faces,
        result.cluster_count,
        result.singletons,
        params.eps
    );
    match save_parameters(state, &params) {
        Ok(()) => result.saved = Some(true),
        Err(error) => {
            tracing::warn!(
                "videre gallery: recluster applied but its settings were not saved: {error}"
            );
            result.saved = Some(false);
            result.settings_error = Some(error);
        }
    }
    // New groups and singletons are new subjects for the identity questions.
    match state.conn.lock() {
        Ok(conn) => {
            if let Err(e) = videre_api::refresh_identity_questions(
                &conn,
                &videre_core::face_learning::QuestionSelectionConfig::default(),
            ) {
                tracing::warn!("videre gallery: questions not refreshed after recluster: {e}");
            }
        }
        Err(e) => {
            poisoned(e);
        }
    }
    Ok(result)
}

/// Write `faces.clustering` (only what differs from the built-in set; the
/// whole override removed when nothing does) under the settings lock. An
/// unreadable `gallery.json` is never overwritten: it may hold hand edits.
fn save_parameters(state: &AppState, params: &ClusteringParameters) -> Result<(), String> {
    use super::settings::{load, merge_patch, save, SaveError, Stored};
    let _held = state
        .settings_lock
        .lock()
        .map_err(|_| "the settings lock is poisoned".to_string())?;
    let path = settings_file(state);
    let mut overrides = match load(&path) {
        Stored::Absent => Value::Object(Default::default()),
        Stored::Valid(v) => v,
        Stored::Invalid(error) => return Err(error),
    };
    merge_patch(
        &mut overrides,
        &json!({ (cluster_settings::SECTION): { (cluster_settings::KEY): cluster_settings::saved_value(params) } }),
    );
    if let Some(map) = overrides.as_object_mut() {
        if map
            .get(cluster_settings::SECTION)
            .and_then(Value::as_object)
            .is_some_and(|s| s.is_empty())
        {
            map.remove(cluster_settings::SECTION);
        }
    }
    save(&path, &overrides).map_err(|e| match e {
        SaveError::TooLarge => "gallery.json would be too large".to_string(),
        SaveError::Io(e) => format!("{e:#}"),
    })
}
