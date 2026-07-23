# videre desktop app (Tauri) - design

Date: 2026-07-23
Status: approved for planning
Scope: first vertical slice = the faces labeling UI, plus the reusable facade
and app scaffolding it needs.

## 1. Overview

Build a cross-platform desktop app for videre with Tauri v2 and a React +
shadcn/ui frontend, reusing the existing Rust engine. The app replaces the
current "local axum server + browser" interaction model for interactive work
with a real desktop application.

This is the promotion of the desktop road from "direction, no driver" to active
work (see memory `architecture-multiplatform-ui`, `project-roadmap`). The same
React/shadcn UI and its swappable data layer are intended to become the
foundation for a later web / on-premise management dashboard, so the design
optimizes for that reuse from the start.

### Goals

- A Tauri desktop app that does the faces labeling workflow at feature parity
  with today's `videre report --faces` page.
- A `videre-api` facade crate that is the single source of truth for app-level
  operations, called by both the Tauri commands and the existing axum server.
- A frontend architected so the same components can later target a web/on-prem
  HTTP backend by swapping one client implementation.

### Non-goals (this slice)

- Other screens: dedupe review, semantic/library search, all-files gallery,
  by-date, watch/embed controls.
- The `HttpClient` implementation, and the web/on-prem dashboard itself.
- A library/database picker UI (reuse existing default-db resolution).
- Auth, multi-user, multi-tenancy.
- Mobile (iOS/Android) - see `architecture-mobile-portability`.
- Splitting `app/` into its own repository (deferred until the facade API
  stabilizes).

## 2. Workspace layout

```
videre/ (existing Cargo workspace)
  crates/
    videre-core     (unchanged)
    videre-ml       (unchanged)
    videre          (CLI + axum server; handlers rewritten to delegate)
    videre-api      NEW - facade crate
  app/              NEW - Tauri v2 app (not a workspace member of the Rust
                    crates; src-tauri is its own cargo project depending on the
                    videre crates by path)
```

`app/src-tauri` depends on `videre-api`, `videre-core`, and `videre-ml` by
relative path. Keeping the app in the same repo lets facade + UI changes land in
one PR while the API churns; the repo split happens later once the contract is
stable.

## 3. `videre-api` facade crate

Plain Rust functions over an open `&rusqlite::Connection`, returning serde
`Serialize` data structs and a shared error type. No axum, no clap, no Tauri
types. This is where the logic currently living inside the axum handlers in
`videre/src/commands/report.rs` moves to.

### Operations (faces slice)

Each maps 1:1 to an existing route/handler:

| videre-api fn | replaces handler | today's route |
|---|---|---|
| `faces_list(&conn) -> FacesData` | `handle_get_faces` | GET `/api/faces` |
| `cluster_detail(&conn, id) -> ClusterDetail` | `handle_cluster_api` | GET `/api/cluster/{id}` |
| `person_detail(&conn, name) -> PersonDetail` | `handle_person_api` | GET `/api/person/{name}` |
| `search_person(&conn, query) -> Vec<String>` | `handle_search_person` | GET `/api/search/person` |
| `assign(&conn, face_ids, label) -> Result<()>` | `handle_assign` | POST `/api/assign` |
| `new_person(&conn, face_ids, label) -> Result<()>` | `handle_new_person` | POST `/api/new-person` |
| `remove_face(&conn, id) -> Result<()>` | `handle_remove_face` | POST `/api/remove-face` |
| `set_primary(&conn, id, label) -> Result<()>` | `handle_set_primary` | POST `/api/set-primary` |
| `dissolve_cluster(&conn, id) -> Result<()>` | `handle_dissolve_cluster` | POST `/api/dissolve-cluster` |
| `delete_person(&conn, label) -> Result<()>` | `handle_delete_person` | POST `/api/delete-person` |
| `rename_person(&conn, old, new) -> Result<()>` | `handle_rename_person` | POST `/api/rename-person` |
| `face_image_bytes(&conn, id, size) -> Result<Vec<u8>>` | `handle_face_image` | GET `/api/face-image/{id}` |
| `original_image_bytes(&conn, id) -> Result<Vec<u8>>` | `handle_original_image` | GET `/api/original-image/{id}` |

`sanitize_person_label` moves into `videre-api` so both consumers share one
implementation (it currently lives in report.rs and is unit-tested there; the
tests move with it).

Server-only concerns that do NOT move to the facade: `handle_quit` (the Tauri
app closes its window natively), and the HTML-serving handlers
(`handle_report`, `handle_root`, `handle_cluster_page`, `handle_person_page`) -
those are replaced by React views.

### Types

Reuse the shapes the frontend already consumes so the contract is unchanged:
`FacesData { people, clusters, singletons }`, `PersonFaceData` (includes
`is_primary`, added this session), `ClusterDetail`, `PersonDetail`. These move
from report.rs into videre-api.

### Errors

A `videre_api::Error` enum (e.g. `NotFound`, `Conflict` for rename collision,
`Db`, `Invalid`). Each consumer maps it to its own convention: axum handlers map
to `StatusCode` (preserving current 404/409 behavior), Tauri commands map to a
serializable error the frontend can branch on (notably the rename-collision
case the UI already handles).

### Database resolution / connection

Reuse `videre_core::home::resolve_db` (default library db). The caller owns the
connection: the axum server passes its `AppState` connection; the Tauri app
holds an `Arc<Mutex<Connection>>` in Tauri managed state and passes it in. No
new resolution logic. A library/db picker is a future slice.

## 4. axum reconciliation

As each operation is extracted, its axum handler is rewritten to a thin wrapper
that opens/locks the connection, calls `videre-api`, and maps the result to a
response. This keeps `videre report --faces` working unchanged for users, proves
the facade against two real consumers, and prevents desktop/server drift. The
existing `faces_server` integration tests must keep passing.

## 5. Tauri app (`app/src-tauri`)

- Tauri v2, one window, loads the React build.
- **Managed state:** an `Arc<Mutex<rusqlite::Connection>>` opened at startup via
  `videre_core::db::open_wal(resolve_db(None))`. WAL mode already supports the
  app running alongside a background `videre watch` (see CLAUDE.md).
- **Commands:** one thin `#[tauri::command]` per facade operation, (de)serializing
  args/results and mapping `videre_api::Error` to a frontend-facing error.
- **Image protocols:** register two custom URI schemes,
  `videre-face://<id>[?size=N]` and `videre-original://<id>`, whose handlers call
  `videre_api::face_image_bytes` / `original_image_bytes` and stream JPEG bytes.
  Used directly in `<img src>` for lazy loading and native caching, mirroring the
  current `/api/face-image` and `/api/original-image`.

## 6. Frontend (`app/src`)

### Stack

Vite + React + TypeScript, Tailwind CSS + shadcn/ui, TanStack Query for
data-fetching/caching/invalidation, HTML5 drag-and-drop (dnd-kit only if native
DnD proves insufficient).

### Swappable data layer

- A typed `VidereClient` TypeScript interface declaring every operation
  (`facesList()`, `clusterDetail(id)`, `personDetail(name)`, `assign()`,
  `newPerson()`, `removeFace()`, `setPrimary()`, `dissolveCluster()`,
  `deletePerson()`, `renamePerson()`, `searchPerson()`), plus URL helpers
  `faceImageUrl(id, size?)` and `originalImageUrl(id)`.
- `TauriClient implements VidereClient` using `@tauri-apps/api` `invoke`, and
  returns `videre-face://` / `videre-original://` URLs for images.
- A `useClient()` React context provides the active client. **Components never
  call `invoke` directly.** A future `HttpClient` (fetch against the axum/web
  server, returning `/api/...` URLs) is a drop-in swap for the web/on-prem
  dashboard.
- TanStack Query hooks (`useFacesList`, `usePersonDetail`, mutation hooks for
  assign/new-person/etc.) call the client and own cache invalidation, replacing
  the current manual `loadFaces()` reloads.

### Views (feature parity with the current labeling page)

- **Labeling view** (`/`): People / Unassigned Clusters / Singletons sections;
  drag-assign a cluster/singleton onto a person; singleton multi-select with a
  bulk action bar (New Person / assign selection); New Person inline; the
  top-vs-right People placement toggle persisted in localStorage; name-sorted
  People. (All three behaviors shipped to the current page this session are
  reproduced.)
- **Cluster detail** (`/cluster/:id`): faces grid, per-face remove/assign,
  Assign cluster, Dissolve cluster.
- **Person detail** (`/person/:name`): faces grid, per-face remove, Set Default
  (with the "Default" badge), rename, delete person.

Client-side routing (React Router) replaces the server's page routes.

## 7. Testing

- **`videre-api`:** Rust unit tests per operation against an in-memory SQLite db
  (seeded faces/file_hashes). This is a genuine coverage gain - the logic is only
  integration-tested through the server today. The moved `sanitize_person_label`
  tests come along. The `set_primary` one-primary-per-person invariant and
  `remove_face` resetting `is_primary` (already tested in report.rs) move here.
- **axum:** existing `faces_server` integration tests must stay green after the
  handlers delegate.
- **Tauri commands:** thin wrappers; minimal/no dedicated tests.
- **Frontend:** component-level tests where valuable; primary validation is
  driving the running app (the same live-verification loop used this session).

## 8. Open questions / future

- `HttpClient` + the web/on-prem dashboard: separate later slice; the interface
  boundary is being put in now so it is a swap, not a rewrite.
- Repo split: move `app/` to its own repository once the `videre-api` contract
  stabilizes.
- Additional desktop slices (dedupe, search, gallery, by-date, watch controls)
  each reuse `videre-api` + the client interface.
- Auth/tenancy for the enterprise dashboard road: keep the component layer
  auth-agnostic so gated views can wrap it later; do not build now.
