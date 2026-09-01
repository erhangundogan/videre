//! The `videre gallery` HTTP server: the axum router, its handlers, and the
//! face-labeling API. The rendering it shares with `dedupe --html` and
//! `search --html` lives in `crate::render`; this file is the HTTP layer only.

use crate::render::*;
use axum::extract::{Json as AxumJson, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, patch, post, put};
use axum::Router;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::{Arc, Mutex};
use videre_api::{ClusterDetail, FacesData, PersonDetail};

fn json_response(body: String) -> axum::response::Response {
    use axum::response::IntoResponse;
    (
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        body,
    )
        .into_response()
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

    pub const FACES_CSS: &str = include_str!("../../../static/faces.css");
    pub const FACES_JS: &str = include_str!("../../../static/faces.js");
    pub const CLUSTER_CSS: &str = include_str!("../../../static/cluster.css");
    pub const CLUSTER_JS: &str = include_str!("../../../static/cluster.js");
    pub const PERSON_CSS: &str = include_str!("../../../static/person.css");
    pub const PERSON_JS: &str = include_str!("../../../static/person.js");
}

fn api_status(e: videre_api::Error) -> StatusCode {
    match e {
        videre_api::Error::NotFound => StatusCode::NOT_FOUND,
        videre_api::Error::Invalid => StatusCode::BAD_REQUEST,
        videre_api::Error::Db(_) => StatusCode::INTERNAL_SERVER_ERROR,
        videre_api::Error::Other(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
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
    /// Bound at startup, like `model_id`, so a request cannot retarget the
    /// server at another library. `search::run_json` opens its own connection
    /// from this, which is what keeps a ranking query off the shared one.
    db: std::path::PathBuf,
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
) -> impl axum::response::IntoResponse {
    render_live(&state, false, true, false, Some(Section::Date))
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
            // Both bound at startup, so a request cannot retarget the server.
            db: Some(state.db.clone()),
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
        crate::commands::search::run_json(
            &args,
            &crate::commands::search::CachedEmbedder(&state.embedder),
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
    let name = videre_core::location::location_name(q.lat, q.lon);
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
        _ => query_files_page(&conn, view, q.date.as_deref(), offset, limit)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
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
        let lookup = {
            let conn = state
                .conn
                .lock()
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            videre_api::face_lookup(&conn, face_id).map_err(|_| StatusCode::NOT_FOUND)?
        };
        videre_api::face_bytes_from_lookup(&lookup, face_id).map_err(|_| StatusCode::NOT_FOUND)
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)??;
    Ok(([(axum::http::header::CONTENT_TYPE, "image/jpeg")], bytes))
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
}

#[derive(Deserialize)]
struct RawFileQuery {
    /// Optional max width/height in pixels, only meaningful for HEIC
    /// (which always needs QuickLook conversion) so the caller can request a
    /// small thumbnail (240px in the grid) or a larger version (1200px in
    /// the lightbox) without paying to decode/re-encode a huge image for a
    /// tiny `<img>`. Ignored for formats served as raw bytes.
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
) -> Result<impl axum::response::IntoResponse, StatusCode> {
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

    // videre watch's `--heic` stage may have already pre-converted and cached
    // a thumbnail for this file's content hash at this exact size, serve
    // that directly instead of paying for a live qlmanage conversion.
    if ext == "heic" {
        if let Some(size) = q.size {
            let cached_path = videre_core::thumb_cache::thumb_path(&hash, size);
            if let Ok(bytes) = tokio::fs::read(&cached_path).await {
                return Ok(([(axum::http::header::CONTENT_TYPE, "image/jpeg")], bytes));
            }
        }
    }

    let size = q.size;
    let (content_type, bytes) =
        tokio::task::spawn_blocking(move || -> Option<(&'static str, Vec<u8>)> {
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
                Some((mime_for_ext(&ext), bytes))
            }
        })
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    Ok(([(axum::http::header::CONTENT_TYPE, content_type)], bytes))
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
            let lookup = {
                let conn = state
                    .conn
                    .lock()
                    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
                videre_api::original_lookup(&conn, face_id).map_err(|_| StatusCode::NOT_FOUND)?
            };
            videre_api::original_bytes_from_lookup(&lookup, face_id)
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
    port: u16,
    browse: bool,
}

async fn serve_faces_async(
    db: &Path,
    opts: ServeOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    let conn = videre_core::db::open_wal(db)?;
    videre_core::location::ensure_location_column(&conn);
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
        if let Err(e) = videre_core::embeddings_db::attach_for_read(&conn, db, &opts.model_id) {
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
        db: db.to_path_buf(),
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
        .route("/date", get(handle_gallery_date))
        .route("/map", get(handle_not_yet))
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
    db: &Path,
    model_id: String,
    port: u16,
    browse: bool,
) -> anyhow::Result<()> {
    let opts = ServeOptions {
        serve_faces_ui: true,
        report_all: true,
        report_heic: false,
        report_heic_original: false,
        model_id,
        gallery: true,
        port,
        browse,
    };
    serve_faces(db, opts).map_err(|e| anyhow::anyhow!("{e}"))
}

fn serve_faces(db: &Path, opts: ServeOptions) -> Result<(), Box<dyn std::error::Error>> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(serve_faces_async(db, opts))
}
