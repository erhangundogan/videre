//! Writes for a selection of items (content hashes) from the gallery's
//! selection bar: marks, tags and rotation. Every request names 1 to
//! `MAX_HASHES` hashes the library knows; anything else is refused whole, so
//! a stale page never believes it changed something it did not.

use super::refusal::{failed, Refusal};
use super::server::{guard_operation, poisoned, rotate_one, AppState, RotateOutcome};
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::Json;
use rusqlite::Connection;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

pub(crate) const MAX_HASHES: usize = 10_000;

/// Deduplicate `hashes`, and refuse an empty or oversized list or one naming a
/// hash the library does not have.
pub(super) fn known_hashes(conn: &Connection, hashes: &[String]) -> Result<Vec<String>, Refusal> {
    let mut unique: Vec<String> = hashes.to_vec();
    unique.sort();
    unique.dedup();
    if unique.is_empty() || unique.len() > MAX_HASHES {
        return Err(Refusal::BadParameter("hashes"));
    }
    let mut stmt = conn
        .prepare_cached("SELECT 1 FROM file_hashes WHERE hash = ?1 LIMIT 1")
        .map_err(failed)?;
    for hash in &unique {
        if !stmt.exists([hash]).map_err(failed)? {
            return Err(Refusal::Invalid("unknown_hash"));
        }
    }
    Ok(unique)
}

#[derive(Deserialize)]
pub(crate) struct MarksBody {
    hashes: Vec<String>,
    rating: Option<i64>,
    pick: Option<String>,
    label: Option<String>,
    liked: Option<bool>,
}

/// POST /api/files/marks. `pick` toggles: asking for the pick every selected
/// item already has clears it, so Keep pressed twice undoes itself.
pub(crate) async fn handle_marks(
    State(state): State<Arc<AppState>>,
    Json(body): Json<MarksBody>,
) -> Response {
    let result = tokio::task::spawn_blocking(move || -> Result<Value, Refusal> {
        let _activity = guard_operation(&state)?;
        let conn = state.conn.lock().map_err(poisoned)?;
        let hashes = known_hashes(&conn, &body.hashes)?;
        let mut pick = body.pick.clone();
        if let Some(wanted) = pick.as_deref().filter(|p| *p == "keep" || *p == "reject") {
            let current = videre_core::marks::get_many(&conn, &hashes).map_err(failed)?;
            let all_have = hashes.iter().all(|h| {
                current
                    .get(h)
                    .and_then(|m| m.pick)
                    .is_some_and(|p| (p == videre_core::marks::Pick::Keep) == (wanted == "keep"))
            });
            if all_have {
                pick = Some("none".into());
            }
        }
        let change = videre_core::marks::change_from_parts(
            body.rating,
            pick.as_deref(),
            body.label.as_deref(),
            body.liked,
        );
        if !change.any() {
            return Err(Refusal::Invalid("no_change"));
        }
        videre_core::marks::set(&conn, &hashes, &change).map_err(failed)?;
        Ok(json!({ "updated": hashes.len(), "pick": pick }))
    })
    .await;
    respond(result)
}

#[derive(Deserialize)]
pub(crate) struct TagsBody {
    hashes: Vec<String>,
    #[serde(default)]
    add: Vec<String>,
    #[serde(default)]
    remove: Vec<String>,
}

/// POST /api/files/tags: add, then remove, on every selected item.
pub(crate) async fn handle_tags(
    State(state): State<Arc<AppState>>,
    Json(body): Json<TagsBody>,
) -> Response {
    let result = tokio::task::spawn_blocking(move || -> Result<Value, Refusal> {
        let add: Vec<String> = body
            .add
            .iter()
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect();
        let remove: Vec<String> = body
            .remove
            .iter()
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect();
        if add.is_empty() && remove.is_empty() {
            return Err(Refusal::Invalid("no_change"));
        }
        let _activity = guard_operation(&state)?;
        let conn = state.conn.lock().map_err(poisoned)?;
        let hashes = known_hashes(&conn, &body.hashes)?;
        let tx = conn.unchecked_transaction().map_err(failed)?;
        videre_core::tags::set_tags(&tx, &hashes, &add).map_err(failed)?;
        videre_core::tags::remove_tags(&tx, &hashes, &remove).map_err(failed)?;
        tx.commit().map_err(failed)?;
        Ok(json!({ "updated": hashes.len() }))
    })
    .await;
    respond(result)
}

/// GET /api/tags: every tag in the library, most used first, for suggestions.
pub(crate) async fn handle_list_tags(State(state): State<Arc<AppState>>) -> Response {
    let result = tokio::task::spawn_blocking(move || -> Result<Value, Refusal> {
        let conn = state.conn.lock().map_err(poisoned)?;
        videre_core::tags::ensure_photo_tags_table(&conn).map_err(failed)?;
        let mut stmt = conn
            .prepare(
                "SELECT tag, count(*) FROM photo_tags GROUP BY tag ORDER BY count(*) DESC, tag",
            )
            .map_err(failed)?;
        let rows = stmt
            .query_map([], |r| {
                Ok(json!({ "tag": r.get::<_, String>(0)?, "count": r.get::<_, i64>(1)? }))
            })
            .map_err(failed)?
            .collect::<rusqlite::Result<Vec<Value>>>()
            .map_err(failed)?;
        Ok(Value::Array(rows))
    })
    .await;
    respond(result)
}

#[derive(Deserialize)]
pub(crate) struct RotateBody {
    hashes: Vec<String>,
    direction: String,
}

/// POST /api/files/rotate: a quarter turn for every selected photo; items
/// that cannot carry an orientation (videos, DNG) are skipped and counted.
pub(crate) async fn handle_rotate(
    State(state): State<Arc<AppState>>,
    Json(body): Json<RotateBody>,
) -> Response {
    let result = tokio::task::spawn_blocking(move || -> Result<Value, Refusal> {
        let ccw = match body.direction.as_str() {
            "cw" => false,
            "ccw" => true,
            _ => return Err(Refusal::BadParameter("direction")),
        };
        let _activity = guard_operation(&state)?;
        let hashes = {
            let conn = state.conn.lock().map_err(poisoned)?;
            known_hashes(&conn, &body.hashes)?
        };
        let (mut rotated, mut skipped, mut failed_count) = (0usize, 0usize, 0usize);
        for hash in &hashes {
            match rotate_one(&state, hash, ccw) {
                Ok(RotateOutcome::Rotated(_)) => rotated += 1,
                Ok(RotateOutcome::Unsupported) | Ok(RotateOutcome::NotFound) => skipped += 1,
                Err(e) => {
                    tracing::warn!("videre gallery: could not rotate {hash}: {e:#}");
                    failed_count += 1;
                }
            }
        }
        Ok(json!({ "rotated": rotated, "skipped": skipped, "failed": failed_count }))
    })
    .await;
    respond(result)
}

#[derive(Deserialize)]
pub(crate) struct DeleteBody {
    hashes: Vec<String>,
    #[serde(default)]
    dry_run: bool,
}

/// POST /api/files/delete: move every file of the selected items to the
/// system Trash (all copies of a hash: leaving one would bring the item
/// straight back) and forget their rows. `dry_run` only counts, for the
/// confirmation dialog. Needs the library to itself, like `dedupe --remove`.
pub(crate) async fn handle_delete(
    State(state): State<Arc<AppState>>,
    Json(body): Json<DeleteBody>,
) -> Response {
    let result = tokio::task::spawn_blocking(move || -> Result<Value, Refusal> {
        let library = &state.context.library;
        library
            .ensure_root_identity()
            .map_err(|_| axum::http::StatusCode::SERVICE_UNAVAILABLE)?;
        let mode = if body.dry_run {
            videre_core::library_locks::ActivityMode::Shared
        } else {
            videre_core::library_locks::ActivityMode::Exclusive
        };
        let _activity = videre_core::library_locks::try_activity(library, mode)
            .map_err(|_| Refusal::Busy("library_busy"))?;
        let conn = state.conn.lock().map_err(poisoned)?;
        let hashes = known_hashes(&conn, &body.hashes)?;
        let mut files: Vec<(String, String, String)> = Vec::new();
        {
            let mut stmt = conn
                .prepare_cached("SELECT path, hash, lower(coalesce(ext, '')) FROM file_hashes WHERE hash = ?1 ORDER BY path")
                .map_err(failed)?;
            for hash in &hashes {
                let rows = stmt
                    .query_map([hash], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                    .map_err(failed)?;
                for row in rows {
                    files.push(row.map_err(failed)?);
                }
            }
        }
        let is_video = |ext: &str| videre_core::embeddings::is_video_ext(ext);
        let videos = hashes
            .iter()
            .filter(|h| files.iter().any(|(_, fh, ext)| fh == *h && is_video(ext)))
            .count();
        let mut report = json!({
            "items": hashes.len(),
            "files": files.len(),
            "photos": hashes.len() - videos,
            "videos": videos,
            "extra_copies": files.len() - hashes.len(),
        });
        if body.dry_run {
            return Ok(report);
        }
        let paths: Vec<std::path::PathBuf> =
            files.iter().map(|(p, _, _)| std::path::PathBuf::from(p)).collect();
        let results = crate::removal::trash_and_forget(&conn, &paths).map_err(failed)?;
        let failed_paths: Vec<Value> = results
            .iter()
            .filter_map(|(path, r)| {
                r.as_ref()
                    .err()
                    .map(|e| json!({ "path": path.to_string_lossy(), "error": e }))
            })
            .collect();
        // An item is gone when none of its files is left.
        let kept: std::collections::HashSet<&str> = results
            .iter()
            .filter(|(_, r)| r.is_err())
            .filter_map(|(path, _)| {
                files
                    .iter()
                    .find(|(p, _, _)| std::path::Path::new(p) == path)
                    .map(|(_, h, _)| h.as_str())
            })
            .collect();
        let trashed: Vec<&String> = hashes.iter().filter(|h| !kept.contains(h.as_str())).collect();
        tracing::info!(
            "videre gallery: moved {} file(s) of {} item(s) to the trash, {} failed",
            results.len() - failed_paths.len(),
            trashed.len(),
            failed_paths.len()
        );
        report["trashed"] = json!(trashed);
        report["failed"] = Value::Array(failed_paths);
        Ok(report)
    })
    .await;
    respond(result)
}

fn respond(result: Result<Result<Value, Refusal>, tokio::task::JoinError>) -> Response {
    match result {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(refusal)) => refusal.into_response(),
        Err(e) => failed(e).into_response(),
    }
}
