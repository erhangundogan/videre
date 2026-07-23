# Tauri Desktop App: Scaffold, Commands & Image Protocols - Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stand up a Tauri v2 desktop app whose Rust backend exposes the `videre-api` faces operations as commands and serves face/original images via custom URI protocols, backed by the shared SQLite library - producing a running window that can fetch and display real data (proving the whole facade -> command -> UI pipeline). The React/shadcn UI itself is Plan 3; this plan delivers the backend surface plus a minimal smoke-test frontend.

**Architecture:** A new `app/` directory holds a Tauri v2 project (`app/src-tauri` = Rust, `app/src` = Vite/React/TS). `src-tauri` depends on `videre-api`, `videre-core`, and `videre-ml` by path. It holds an `Arc<Mutex<rusqlite::Connection>>` in Tauri managed state (opened via `videre_core::db::open_wal(resolve_db(None))`), exposes one thin `#[tauri::command]` per facade operation, and registers `videre-face://<id>` and `videre-original://<id>` protocol handlers that stream JPEG bytes. Those image bytes come from two new `videre-api` functions (`face_image_bytes`, `original_image_bytes`) extracted from the existing axum handlers - the piece Plan 1 deferred.

**Tech Stack:** Tauri v2, Rust (`rusqlite`, `image`), Node + Vite + React + TypeScript, `@tauri-apps/api`. Depends on Plan 1 (the `videre-api` facade) being merged. This is Plan 2 of 3 (see `docs/superpowers/specs/2026-07-23-desktop-app-design.md`).

**Prerequisites the implementer must have:** a Node.js toolchain (npm), the Tauri v2 system prerequisites for macOS (Xcode command-line tools; WKWebView is system-provided), and the Rust toolchain already used by this workspace.

---

## File Structure

- Modify: `crates/videre-api/Cargo.toml` - add `image` + `kamadak-exif` deps
- Modify: `crates/videre-api/src/lib.rs` - export the two image-bytes fns + a small `images` module
- Create: `crates/videre-api/src/images.rs` - `face_image_bytes`, `original_image_bytes`, and the moved image helpers
- Modify: `crates/videre/src/commands/report.rs` - `handle_face_image`/`handle_original_image` delegate to `videre-api`; remove the moved helpers
- Create: `app/` - Tauri v2 project (scaffolded)
- Modify: `app/src-tauri/Cargo.toml` - add videre crate path deps
- Modify: `app/src-tauri/tauri.conf.json` - app identity, register protocols
- Create: `app/src-tauri/src/state.rs` - managed DB state
- Create: `app/src-tauri/src/commands.rs` - the 11 command wrappers
- Create: `app/src-tauri/src/protocols.rs` - the two image protocol handlers
- Modify: `app/src-tauri/src/lib.rs` (or `main.rs`) - wire state, commands, protocols into the builder
- Modify: `app/src/App.tsx` - minimal smoke-test UI (invoke `faces_list`, render counts + one face image)
- Modify: root `Cargo.toml` - DO NOT add `app/src-tauri` to the workspace members (it is a separate cargo project; keeping it out avoids forcing the whole workspace to build Tauri/GUI deps)

---

## Task 1: Extract image-bytes operations into videre-api

The Tauri protocol handlers and the axum image handlers must share one implementation. Move the face-crop/HEIC/caching logic out of `report.rs` (a bin, not importable) into `videre-api`.

**Files:**
- Modify: `crates/videre-api/Cargo.toml`
- Create: `crates/videre-api/src/images.rs`
- Modify: `crates/videre-api/src/lib.rs`

- [ ] **Step 1: Add image deps to videre-api**

In `crates/videre-api/Cargo.toml`, under `[dependencies]`, add (matching the versions used elsewhere in the workspace - check `crates/videre/Cargo.toml` for the exact `image` and `kamadak-exif` version strings and copy them):

```toml
image = { version = "0.25", default-features = false, features = ["jpeg", "png", "gif", "webp", "bmp", "tiff"] }
kamadak-exif = "0.5"
```

- [ ] **Step 2: Create the images module**

Create `crates/videre-api/src/images.rs`. It exposes two synchronous functions returning JPEG (or raw) bytes, plus the private helpers moved verbatim from `report.rs` (`make_face_thumb`, `crop_face_square`, `apply_exif_orientation`, `read_exif_orientation`, `mime_for_ext`). Read those five helpers' current bodies from `crates/videre/src/commands/report.rs` and paste them into this module unchanged (they use only the `image` crate, `std`, and `videre_core::{heic, thumb_cache}` - all available here). Then add the two public entry points:

```rust
use crate::error::{Error, Result};
use rusqlite::Connection;

const FACE_THUMB_SIZE: u32 = 140;

/// JPEG bytes for a single aligned face thumbnail (140px), reading the disk
/// cache first and converting from the source image (HEIC via QuickLook) on a
/// miss, writing through to the cache. Returns `Error::NotFound` if the face id
/// is unknown or the crop cannot be produced. Synchronous: callers that need
/// async should run this on a blocking thread.
pub fn face_image_bytes(conn: &Connection, face_id: i64) -> Result<Vec<u8>> {
    let (bbox_json, file_path, hash): (String, String, String) = conn
        .query_row(
            "SELECT f.bbox, fh.path, f.hash FROM faces f \
             JOIN file_hashes fh ON f.hash = fh.hash WHERE f.id = ?1 LIMIT 1",
            [face_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .map_err(|_| Error::NotFound)?;

    let cache = videre_core::thumb_cache::face_thumb_path(&hash, face_id, FACE_THUMB_SIZE);
    if videre_core::thumb_cache::face_thumb_exists(&hash, face_id, FACE_THUMB_SIZE) {
        if let Ok(bytes) = std::fs::read(&cache) {
            return Ok(bytes);
        }
    }

    let parts: Vec<f32> = bbox_json.split(',').filter_map(|s| s.trim().parse().ok()).collect();
    if parts.len() != 4 {
        return Err(Error::NotFound);
    }
    let bbox = [parts[0], parts[1], parts[0] + parts[2], parts[1] + parts[3]];
    let thumb = make_face_thumb(&file_path, bbox, face_id).ok_or(Error::NotFound)?;
    let mut buf = Vec::new();
    thumb
        .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Jpeg)
        .map_err(|_| Error::NotFound)?;

    // Best-effort write-through (a cache-write failure must not fail the read).
    if let Some(parent) = cache.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let tmp = cache.with_extension(format!("tmp{}", std::process::id()));
    if std::fs::write(&tmp, &buf).is_ok() {
        let _ = std::fs::rename(&tmp, &cache);
    }
    Ok(buf)
}

/// Bytes for the full original image behind a face (raw for common formats,
/// QuickLook-converted JPEG for HEIC, with the HEIC result cached). Returns the
/// MIME type alongside the bytes. `Error::NotFound` if the id is unknown or the
/// file cannot be read/converted. Synchronous.
pub fn original_image_bytes(conn: &Connection, face_id: i64) -> Result<(&'static str, Vec<u8>)> {
    let (file_path, hash): (String, String) = conn
        .query_row(
            "SELECT fh.path, f.hash FROM faces f \
             JOIN file_hashes fh ON f.hash = fh.hash WHERE f.id = ?1 LIMIT 1",
            [face_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|_| Error::NotFound)?;

    let ext = std::path::Path::new(&file_path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    if ext == "heic" {
        if let Ok(bytes) = std::fs::read(videre_core::thumb_cache::original_path(&hash)) {
            return Ok(("image/jpeg", bytes));
        }
        let img = videre_core::heic::heic_via_quicklook(&file_path, &format!("orig{face_id}"))
            .ok_or(Error::NotFound)?;
        let mut buf = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Jpeg)
            .map_err(|_| Error::NotFound)?;
        let final_path = videre_core::thumb_cache::original_path(&hash);
        if let Some(parent) = final_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let tmp = final_path.with_extension(format!("tmp{}", std::process::id()));
        if std::fs::write(&tmp, &buf).is_ok() {
            let _ = std::fs::rename(&tmp, &final_path);
        }
        Ok(("image/jpeg", buf))
    } else {
        let bytes = std::fs::read(&file_path).map_err(|_| Error::NotFound)?;
        Ok((mime_for_ext(&ext), bytes))
    }
}
```

- [ ] **Step 3: Export from lib.rs**

In `crates/videre-api/src/lib.rs`, add `mod images;` and extend the public exports with `pub use images::{face_image_bytes, original_image_bytes};`.

- [ ] **Step 4: Build**

Run: `cargo build -p videre-api`
Expected: compiles. Fix any missing `use` from the moved helpers (the compiler names them).

- [ ] **Step 5: Smoke test the not-found path**

Add to `crates/videre-api/src/images.rs` a test module that only checks the id-not-found path (image decoding needs real files, out of scope for a unit test):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_face_id_is_not_found() {
        let conn = Connection::open_in_memory().unwrap();
        videre_core::face_db::create_faces_table(&conn).unwrap();
        conn.execute_batch("CREATE TABLE file_hashes (hash TEXT PRIMARY KEY, path TEXT);").unwrap();
        assert!(matches!(face_image_bytes(&conn, 999), Err(Error::NotFound)));
        assert!(matches!(original_image_bytes(&conn, 999), Err(Error::NotFound)));
    }
}
```

Run: `cargo test -p videre-api images`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/videre-api/Cargo.toml crates/videre-api/src/images.rs crates/videre-api/src/lib.rs
git commit -m "feat(videre-api): face_image_bytes/original_image_bytes with moved image helpers"
```

---

## Task 2: Reconcile the axum image handlers onto videre-api

**Files:**
- Modify: `crates/videre/src/commands/report.rs`

- [ ] **Step 1: Delegate handle_face_image**

Replace the body of `handle_face_image` in `report.rs` so it calls the facade on a blocking thread (preserving async), mapping errors to the same status codes:

```rust
async fn handle_face_image(
    axum::extract::Path(face_id): axum::extract::Path<i64>,
    State(state): State<Arc<AppState>>,
) -> Result<impl axum::response::IntoResponse, StatusCode> {
    let conn = state.conn.clone();
    let bytes = tokio::task::spawn_blocking(move || {
        let conn = conn.lock().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        videre_api::face_image_bytes(&conn, face_id).map_err(|_| StatusCode::NOT_FOUND)
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)??;
    Ok(([(axum::http::header::CONTENT_TYPE, "image/jpeg")], bytes))
}
```

Note: this assumes `state.conn` is an `Arc<Mutex<Connection>>` that is `Clone` (it is - `AppState.conn` is already shared). If `spawn_blocking` cannot capture it because `AppState` is not `Clone`-friendly, instead lock briefly outside is NOT possible (the guard is not `Send` across await); keep the lock inside the closure as shown.

- [ ] **Step 2: Delegate handle_original_image**

```rust
async fn handle_original_image(
    axum::extract::Path(face_id): axum::extract::Path<i64>,
    State(state): State<Arc<AppState>>,
) -> Result<impl axum::response::IntoResponse, StatusCode> {
    let conn = state.conn.clone();
    let (content_type, bytes) = tokio::task::spawn_blocking(move || {
        let conn = conn.lock().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        videre_api::original_image_bytes(&conn, face_id).map_err(|_| StatusCode::NOT_FOUND)
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)??;
    Ok(([(axum::http::header::CONTENT_TYPE, content_type)], bytes))
}
```

- [ ] **Step 3: Remove the now-duplicated helpers from report.rs**

Delete `make_face_thumb`, `crop_face_square`, `apply_exif_orientation`, `read_exif_orientation`, and `mime_for_ext` from `report.rs` (they now live in `videre-api`). Use the compiler to find any remaining caller. NOTE: `mime_for_ext` may still be used by `handle_raw_file` (the `/api/raw` endpoint). If so, do NOT delete `mime_for_ext`; instead leave a thin `fn mime_for_ext(ext: &str) -> &'static str { videre_api::mime_for_ext(ext) }` - which requires exporting `mime_for_ext` from videre-api too (add it to the `pub use images::{…}` list in Task 1 Step 3 if this is the case). Report which approach you took.

- [ ] **Step 4: Build + test + clippy**

Run: `cargo build --workspace`
Run: `cargo test -p videre --test faces_server`  (Expected: 5 pass)
Run: `cargo test -p videre --bins`  (Expected: pass)
Run: `cargo clippy --workspace --all-targets`  (Expected: no NEW warnings)

- [ ] **Step 5: Commit**

```bash
git add crates/videre/src/commands/report.rs crates/videre-api/src/lib.rs
git commit -m "refactor(videre): image handlers delegate to videre-api"
```

---

## Task 3: Scaffold the Tauri v2 app

**Files:**
- Create: `app/` (via scaffolding tool)
- Modify: root `Cargo.toml` (verify `app/src-tauri` is NOT added to workspace members)

- [ ] **Step 1: Scaffold with the official tool**

From the repo root, run the Tauri v2 scaffolder into `app/`, choosing: TypeScript, React, Vite, npm. Non-interactive form:

```bash
npm create tauri-app@latest app -- --template react-ts --manager npm --yes
```

If the flags differ for the installed version, run `npm create tauri-app@latest` interactively and answer: project name `app`, frontend language TypeScript, package manager npm, UI template React, bundler Vite. Confirm it created `app/src-tauri/` (Rust) and `app/src/` (React).

- [ ] **Step 2: Keep app/src-tauri out of the Rust workspace**

Open the root `Cargo.toml`. Confirm `members` does NOT include `app/src-tauri`. If the scaffolder or cargo tries to auto-include it (it will, since it is a Cargo project under the workspace root), add an explicit exclude:

```toml
[workspace]
members = [ /* existing crates */ ]
exclude = ["app/src-tauri"]
```

Rationale: the GUI/Tauri dependency tree should not be pulled into `cargo build --workspace` for the CLI/library crates.

- [ ] **Step 3: Install JS deps and verify the scaffold builds**

Run: `cd app && npm install`
Run (from repo root): `cd app/src-tauri && cargo build`
Expected: both succeed (a blank Tauri app compiles). Do NOT run `npm run tauri dev` in this step (it needs a display; the implementer verifies build, not GUI launch, unless a display is available).

- [ ] **Step 4: Commit the scaffold**

```bash
git add app Cargo.toml
git commit -m "chore(app): scaffold Tauri v2 + React/TS app"
```

Note: if `app/` has its own `.gitignore` (scaffolder adds one for `node_modules`, `dist`, `target`), keep it - do not commit `node_modules` or build artifacts.

---

## Task 4: DB state + facade commands

**Files:**
- Modify: `app/src-tauri/Cargo.toml`
- Create: `app/src-tauri/src/state.rs`
- Create: `app/src-tauri/src/commands.rs`
- Modify: `app/src-tauri/src/lib.rs`

- [ ] **Step 1: Add videre crate dependencies**

In `app/src-tauri/Cargo.toml` under `[dependencies]`, add path deps (relative to `app/src-tauri`):

```toml
videre-api = { path = "../../crates/videre-api" }
videre-core = { path = "../../crates/videre-core" }
rusqlite = { version = "0.32", features = ["bundled"] }
serde = { version = "1", features = ["derive"] }
```

- [ ] **Step 2: Managed DB state**

Create `app/src-tauri/src/state.rs`:

```rust
use std::sync::Mutex;

/// The app's single open connection to the videre library database, shared
/// across all command invocations. Opened once at startup from the same
/// default-db resolution the CLI uses.
pub struct DbState(pub Mutex<rusqlite::Connection>);

impl DbState {
    pub fn open() -> anyhow::Result<Self> {
        let path = videre_core::home::resolve_db(None)?;
        if !path.exists() {
            anyhow::bail!(
                "no database found at {}; run 'videre scan <dir>' first",
                path.display()
            );
        }
        let conn = videre_core::db::open_wal(&path)?;
        Ok(DbState(Mutex::new(conn)))
    }
}
```

Add `anyhow = "1"` to `app/src-tauri/Cargo.toml` if the scaffold did not already include it.

- [ ] **Step 3: The command wrappers**

Create `app/src-tauri/src/commands.rs`. Each command locks the connection, calls the matching `videre_api` function, and maps `videre_api::Error` to a `String` error the frontend can show:

```rust
use crate::state::DbState;
use tauri::State;
use videre_api::{ClusterDetail, FacesData, PersonDetail};

fn err(e: videre_api::Error) -> String {
    e.to_string()
}

#[tauri::command]
pub fn faces_list(db: State<DbState>) -> Result<FacesData, String> {
    let conn = db.0.lock().map_err(|_| "db lock poisoned".to_string())?;
    videre_api::faces_list(&conn).map_err(err)
}

#[tauri::command]
pub fn cluster_detail(db: State<DbState>, cluster_id: i64) -> Result<ClusterDetail, String> {
    let conn = db.0.lock().map_err(|_| "db lock poisoned".to_string())?;
    videre_api::cluster_detail(&conn, cluster_id).map_err(err)
}

#[tauri::command]
pub fn person_detail(db: State<DbState>, name: String) -> Result<PersonDetail, String> {
    let conn = db.0.lock().map_err(|_| "db lock poisoned".to_string())?;
    videre_api::person_detail(&conn, &name).map_err(err)
}

#[tauri::command]
pub fn search_person(db: State<DbState>, name: String) -> Result<Vec<String>, String> {
    let conn = db.0.lock().map_err(|_| "db lock poisoned".to_string())?;
    videre_api::search_person(&conn, &name).map_err(err)
}

#[tauri::command]
pub fn assign(db: State<DbState>, face_ids: Vec<i64>, person_label: String) -> Result<(), String> {
    let conn = db.0.lock().map_err(|_| "db lock poisoned".to_string())?;
    videre_api::assign(&conn, &face_ids, &person_label).map_err(err)
}

#[tauri::command]
pub fn new_person(db: State<DbState>, face_ids: Vec<i64>, label: String) -> Result<(), String> {
    let conn = db.0.lock().map_err(|_| "db lock poisoned".to_string())?;
    videre_api::new_person(&conn, &face_ids, &label).map_err(err)
}

#[tauri::command]
pub fn remove_face(db: State<DbState>, face_id: i64) -> Result<(), String> {
    let conn = db.0.lock().map_err(|_| "db lock poisoned".to_string())?;
    videre_api::remove_face(&conn, face_id).map_err(err)
}

#[tauri::command]
pub fn dissolve_cluster(db: State<DbState>, cluster_id: i64) -> Result<(), String> {
    let conn = db.0.lock().map_err(|_| "db lock poisoned".to_string())?;
    videre_api::dissolve_cluster(&conn, cluster_id).map_err(err)
}

#[tauri::command]
pub fn delete_person(db: State<DbState>, label: String) -> Result<(), String> {
    let conn = db.0.lock().map_err(|_| "db lock poisoned".to_string())?;
    videre_api::delete_person(&conn, &label).map_err(err)
}

#[tauri::command]
pub fn set_primary(db: State<DbState>, face_id: i64, person_label: String) -> Result<(), String> {
    let conn = db.0.lock().map_err(|_| "db lock poisoned".to_string())?;
    videre_api::set_primary(&conn, face_id, &person_label).map_err(err)
}

#[tauri::command]
pub fn rename_person(db: State<DbState>, old_label: String, new_label: String) -> Result<(), String> {
    let conn = db.0.lock().map_err(|_| "db lock poisoned".to_string())?;
    videre_api::rename_person(&conn, &old_label, &new_label).map_err(err)
}
```

- [ ] **Step 4: Wire state + commands into the builder**

In `app/src-tauri/src/lib.rs` (Tauri v2's `run()` entry point that `main.rs` calls), register the modules, manage the state, and add the invoke handler:

```rust
mod commands;
mod protocols; // added in Task 5
mod state;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let db = state::DbState::open().expect("failed to open videre database");
    tauri::Builder::default()
        .manage(db)
        .invoke_handler(tauri::generate_handler![
            commands::faces_list,
            commands::cluster_detail,
            commands::person_detail,
            commands::search_person,
            commands::assign,
            commands::new_person,
            commands::remove_face,
            commands::dissolve_cluster,
            commands::delete_person,
            commands::set_primary,
            commands::rename_person,
        ])
        // .register_uri_scheme_protocol(...) added in Task 5
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
```

If Task 5 is not yet done, temporarily comment out `mod protocols;` and the protocol registration so this compiles.

- [ ] **Step 5: Build**

Run (from `app/src-tauri`): `cargo build`
Expected: compiles.

- [ ] **Step 6: Commit**

```bash
git add app/src-tauri
git commit -m "feat(app): DB state + facade commands"
```

---

## Task 5: Image protocols

**Files:**
- Create: `app/src-tauri/src/protocols.rs`
- Modify: `app/src-tauri/src/lib.rs`

- [ ] **Step 1: Protocol handlers**

Create `app/src-tauri/src/protocols.rs`. Two handlers parse the id from the URI host/path and stream bytes. Uses Tauri v2's `register_uri_scheme_protocol` responder signature.

```rust
use crate::state::DbState;
use tauri::{Manager, UriSchemeContext, UriSchemeResponder};
use tauri::http::Request;

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
                let _ = responder.respond(tauri::http::Response::builder().status(400).body(Vec::new()).unwrap());
                return;
            }
        };
        let db = app.state::<DbState>();
        let result = {
            let conn = match db.0.lock() {
                Ok(c) => c,
                Err(_) => { let _ = responder.respond(tauri::http::Response::builder().status(500).body(Vec::new()).unwrap()); return; }
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
            Err(_) => tauri::http::Response::builder().status(404).body(Vec::new()).unwrap(),
        };
        let _ = responder.respond(resp);
    });
}

pub fn face<R: tauri::Runtime>(ctx: UriSchemeContext<'_, R>, req: Request<Vec<u8>>, responder: UriSchemeResponder) {
    respond(ctx, req, responder, false);
}

pub fn original<R: tauri::Runtime>(ctx: UriSchemeContext<'_, R>, req: Request<Vec<u8>>, responder: UriSchemeResponder) {
    respond(ctx, req, responder, true);
}
```

Note: the exact `UriSchemeContext`/`UriSchemeResponder`/`Request` types and the async-responder signature are Tauri v2 APIs; if the installed Tauri version's signature differs, adjust to the version's `register_uri_scheme_protocol` async form (consult `app/src-tauri`'s `Cargo.lock` for the exact tauri 2.x version and its docs). The behavior to preserve: parse id, call the facade on a non-blocking thread, respond with bytes + content-type or a 404/400/500 status.

- [ ] **Step 2: Register the protocols in the builder**

In `app/src-tauri/src/lib.rs`, uncomment `mod protocols;` and add before `.run(...)`:

```rust
        .register_asynchronous_uri_scheme_protocol("videre-face", protocols::face)
        .register_asynchronous_uri_scheme_protocol("videre-original", protocols::original)
```

- [ ] **Step 3: Allow the schemes in tauri.conf.json**

In `app/src-tauri/tauri.conf.json`, ensure the app CSP (under `app.security.csp`) permits `img-src` from the custom schemes. Set (merging with any existing CSP):

```json
"csp": "default-src 'self'; img-src 'self' videre-face: videre-original: data: asset: http://asset.localhost; style-src 'self' 'unsafe-inline'"
```

- [ ] **Step 4: Build**

Run (from `app/src-tauri`): `cargo build`
Expected: compiles.

- [ ] **Step 5: Commit**

```bash
git add app/src-tauri
git commit -m "feat(app): videre-face/videre-original image protocols"
```

---

## Task 6: Minimal smoke-test frontend + end-to-end verification

Prove the pipeline: the window loads, invokes `faces_list`, shows counts, and renders one real face image via the protocol.

**Files:**
- Modify: `app/src/App.tsx`

- [ ] **Step 1: Replace App.tsx with a smoke test**

```tsx
import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

type FacesData = {
  people: { label: string; representative_id: number }[];
  clusters: { cluster_id: number; face_ids: number[] }[];
  singletons: { face_id: number; hash: string }[];
};

export default function App() {
  const [data, setData] = useState<FacesData | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    invoke<FacesData>("faces_list").then(setData).catch((e) => setError(String(e)));
  }, []);

  if (error) return <pre style={{ color: "crimson", padding: 16 }}>Error: {error}</pre>;
  if (!data) return <p style={{ padding: 16 }}>Loading…</p>;

  const firstFace =
    data.people[0]?.representative_id ??
    data.clusters[0]?.face_ids[0] ??
    data.singletons[0]?.face_id;

  return (
    <main style={{ padding: 16, fontFamily: "sans-serif" }}>
      <h1>videre (smoke test)</h1>
      <p>
        {data.people.length} people · {data.clusters.length} clusters ·{" "}
        {data.singletons.length} singletons
      </p>
      {firstFace != null && (
        <img
          src={`videre-face://${firstFace}`}
          width={140}
          height={140}
          alt={`face ${firstFace}`}
          style={{ borderRadius: 8, background: "#ddd" }}
        />
      )}
    </main>
  );
}
```

- [ ] **Step 2: Type-check the frontend**

Run: `cd app && npm run build`
Expected: Vite/tsc build succeeds (compiles the TS; does not need Tauri).

- [ ] **Step 3: End-to-end run (if a display is available)**

Run (from `app`): `npm run tauri dev`
Expected: a window opens showing the counts line and one face thumbnail rendered via `videre-face://`. If the host has no display/GUI, SKIP the launch and instead report DONE_WITH_CONCERNS noting that GUI launch was not verifiable in this environment - the `cargo build` (Tasks 4-5) and `npm run build` (this task) having passed is the achievable verification here. Do NOT block on GUI launch in a headless environment.

- [ ] **Step 4: Commit**

```bash
git add app/src/App.tsx
git commit -m "feat(app): smoke-test UI invoking faces_list + face image protocol"
```

---

## Self-Review (completed while writing)

- **Spec coverage:** managed `Connection` state via `open_wal(resolve_db(None))` (Task 4); one thin command per the 11 facade ops (Task 4); `videre-face://`/`videre-original://` protocols streaming JPEG bytes (Task 5); the deferred `face_image_bytes`/`original_image_bytes` extracted into `videre-api` and the axum handlers reconciled onto them (Tasks 1-2); `app/` as a separate cargo project kept out of the workspace (Task 3); a running smoke test proving facade -> command -> protocol -> UI (Task 6). The `VidereClient` interface and the real labeling UI are explicitly Plan 3, not here.
- **Placeholder scan:** none. Two steps intentionally instruct reading current code (the five image helpers in Task 1 Step 2; the exact tauri 2.x protocol signature in Task 5 Step 1) rather than transcribing it, because those are verbatim moves / version-specific APIs where the source of truth is the code, not a guess - each says exactly what to read and what shape to preserve.
- **Type consistency:** command return types (`FacesData`/`ClusterDetail`/`PersonDetail`) match the `videre-api` exports from Plan 1; the smoke-test TS `FacesData` shape matches the serde field names (`people`/`clusters`/`singletons`, `representative_id`, `cluster_id`, `face_ids`, `face_id`, `hash`); protocol scheme names match between `protocols.rs`, the builder registration, the CSP, and the `<img src>`.
- **Risk note:** Task 5's protocol API is the least certain (Tauri 2.x has evolved the URI-scheme responder signature across minor versions). The task flags this and pins the behavior to preserve; if an implementer hits a signature mismatch they should adapt to the installed version rather than force the snippet.
