use crate::state::DbState;
use tauri::http::Request;
use tauri::{Manager, UriSchemeContext, UriSchemeResponder};

/// Parse the face id from a `videre-face://<id>` or `videre-original://<id>`
/// URI. Tauri routes the `<id>` into the host on some platforms and the path on
/// others, so accept both.
fn parse_id(uri: &tauri::http::Uri) -> Option<i64> {
    if let Some(host) = uri.host() {
        if let Ok(id) = host.parse::<i64>() {
            return Some(id);
        }
    }
    uri.path().trim_matches('/').parse::<i64>().ok()
}

fn respond<R: tauri::Runtime>(
    ctx: UriSchemeContext<'_, R>,
    req: Request<Vec<u8>>,
    responder: UriSchemeResponder,
    original: bool,
) {
    let app = ctx.app_handle().clone();
    let uri = req.uri().clone();
    std::thread::spawn(move || {
        let id = match parse_id(&uri) {
            Some(id) => id,
            None => {
                responder.respond(
                    tauri::http::Response::builder()
                        .status(400)
                        .body(Vec::new())
                        .unwrap(),
                );
                return;
            }
        };
        let db = app.state::<DbState>();
        // Lock only for the cheap single-row lookup, then release it before
        // the expensive decode/crop/resize/encode work below - holding the
        // shared connection lock across that work would serialize every
        // thumbnail request in the app behind one mutex (the actual cause of
        // multi-second-per-thumbnail rendering in a library with thousands
        // of faces).
        let result = if original {
            let lookup = {
                let conn = match db.0.lock() {
                    Ok(c) => c,
                    Err(_) => {
                        responder.respond(
                            tauri::http::Response::builder()
                                .status(500)
                                .body(Vec::new())
                                .unwrap(),
                        );
                        return;
                    }
                };
                videre_api::original_lookup(&conn, id)
            };
            lookup.and_then(|l| videre_api::original_bytes_from_lookup(&l, id)).map(|(ct, bytes)| (ct.to_string(), bytes))
        } else {
            let lookup = {
                let conn = match db.0.lock() {
                    Ok(c) => c,
                    Err(_) => {
                        responder.respond(
                            tauri::http::Response::builder()
                                .status(500)
                                .body(Vec::new())
                                .unwrap(),
                        );
                        return;
                    }
                };
                videre_api::face_lookup(&conn, id)
            };
            lookup.and_then(|l| videre_api::face_bytes_from_lookup(&l, id)).map(|b| ("image/jpeg".to_string(), b))
        };
        let resp = match result {
            Ok((content_type, bytes)) => tauri::http::Response::builder()
                .header(tauri::http::header::CONTENT_TYPE, content_type)
                .body(bytes)
                .unwrap(),
            Err(_) => tauri::http::Response::builder()
                .status(404)
                .body(Vec::new())
                .unwrap(),
        };
        responder.respond(resp);
    });
}

pub fn face<R: tauri::Runtime>(
    ctx: UriSchemeContext<'_, R>,
    req: Request<Vec<u8>>,
    responder: UriSchemeResponder,
) {
    respond(ctx, req, responder, false);
}

pub fn original<R: tauri::Runtime>(
    ctx: UriSchemeContext<'_, R>,
    req: Request<Vec<u8>>,
    responder: UriSchemeResponder,
) {
    respond(ctx, req, responder, true);
}
