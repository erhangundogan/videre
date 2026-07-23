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
        let result = {
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
            if original {
                videre_api::original_image_bytes(&conn, id)
                    .map(|(ct, bytes)| (ct.to_string(), bytes))
            } else {
                videre_api::face_image_bytes(&conn, id).map(|b| ("image/jpeg".to_string(), b))
            }
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
