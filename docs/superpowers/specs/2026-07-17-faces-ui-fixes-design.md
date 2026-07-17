# Faces Labeling UI Fixes - Design

## Context

Manual testing of `videre report --faces` turned up seven issues, ranging from small
UX gaps to a missing cache wire-up. All of it lives in one file,
`crates/videre/src/commands/report.rs` (HTML/JS templates are Rust string constants
in the same file; there is no separate frontend build). This spec covers all seven as
one implementation slice, since they're small, independent, and share the same file.

A second, unrelated batch of issues in `videre report --all --heic --show-faces` (the
live report/lightbox, not the labeling UI) is deliberately out of scope here and will
get its own spec.

Two items originally raised for the labeling UI are explicitly **out of scope** for
this slice, per user direction: cross-cluster duplicate-face detection (no existing
logic to build on; needs its own design pass) is dropped entirely.

## Goals

1. New Person input: Enter submits, matching the existing Create button.
2. `/person/<name>`: add a "Remove person" action that returns its faces to
   unassigned.
3. `/person/<name>`: add a rename action.
4. Replace the `prompt()`-based singleton assign flow with a real modal with
   autocomplete.
5. Harden person-name sanitization against control/format characters and fix a
   client/server truncation mismatch.
6. Wire face-crop and original-image serving into `thumb_cache` (currently
   reconverts from source on every request).
7. Make the "Back" link behave differently when reached from the all-images
   lightbox vs. the labeling home page, and fix an unescaped-HTML bug found in the
   same function.

## Non-goals

- Duplicate-face-across-clusters detection (dropped, see Context).
- Any change to `--all --show-faces` (separate spec).
- Introducing a real `people` table - people remain a derived grouping of
  `faces.person_label` strings. None of the above requires a schema change.
- Re-running DBSCAN clustering after a person removal (see Goal 2 below for what
  "back to unassigned" actually means without live re-clustering).

## Current State (facts gathered during investigation)

All line numbers refer to `crates/videre/src/commands/report.rs` as of commit
`e1afbeb`.

- `sanitize_person_label` (server, ~1975-1981): collapses whitespace, rejects empty,
  truncates to 60 **chars** (`.chars().take(60)`, UTF-8-safe).
- `sanitizeName()` (client JS, duplicated at ~1375-1377 and ~1535-1537): collapses
  whitespace, truncates via `.slice(0, 60)` (UTF-16 **code-unit**-based - can split a
  surrogate pair/astral character in half, unlike the server).
- Neither side strips control (`Cc`) or bidi/format characters.
- `AssignRequest { face_ids: Vec<i64>, person_label: String }` and
  `NewPersonRequest { face_ids: Vec<i64>, label: String }` (~1773-1782) are
  functionally identical handlers: both sanitize the label, then loop
  `UPDATE faces SET person_label = ?1, confirmed = 1 WHERE id = ?2` per face id.
  There is no separate people table; "a person" is just the set of faces sharing a
  `person_label`.
- No endpoint lists distinct existing labels today. `handle_search_person`
  (~2074-2082) does a substring/LIKE search over paths, not a label list.
- `assignOne` (~1615-1628, cluster page only) uses `prompt('Assign face #' + faceId +
  ' to person:')` and POSTs to `/api/new-person` with `{face_ids: [faceId], label}`.
- `handle_face_image` (~2235-2276) and `handle_original_image` (~2382-2423) both
  reconvert/re-crop from the source file on every request - no caching.
- `videre_core::thumb_cache` (crates/videre-core/src/thumb_cache.rs) is keyed
  strictly by `(hash, size)`: `thumb_path(hash, size) -> <hash>_<size>.jpg`. It has
  no concept of a face id or bbox, so it cannot serve face crops as-is.
- `handle_raw_file` (~2318-2374) is the one existing example of the check-cache,
  else-convert-and-write pattern this spec will replicate for face images.
- Router (~2455-2487): page routes are bare nouns (`/cluster/{id}`, `/person/{name}`);
  API routes are `/api/<verb-or-noun>` in kebab-case (`new-person`, `remove-face`,
  `dissolve-cluster`, `set-primary`), all `POST` except reads. New routes in this spec
  follow the same convention and are gated behind `serve_faces_ui` like the existing
  mutating routes.
- `renderMetaPanel` (~943-971, lightbox metadata panel) builds a person link as a raw
  string with no `escHtml()` call on `fc.name`:
  `'<a href="/person/'+encodeURIComponent(fc.name)+'">'+fc.name+'</a>'` - the second
  `fc.name` (link text) is unescaped HTML.
- "Back to labeling" anchors on the cluster page (~1512) and person page (~1658) are
  static `<a href="/">&larr; Back to labeling</a>` with no JS/query-param handling.

## Design

### 1. Enter-to-submit on New Person inputs

Both copies of the New Person input (`report.rs:1375-1377` area in `FACES_HTML`, and
the equivalent in `CLUSTER_HTML` ~1520) already render with `autofocus`. Add a
`keydown` listener that calls the same submit function the Create button's `onclick`
already calls when the key is `Enter`:

```js
input.addEventListener('keydown', function(e) {
  if (e.key === 'Enter') { e.preventDefault(); submitNewPerson(cardId); }
});
```

(substitute the correct existing submit function name/args at each of the two call
sites - `submitNewPerson(cardId)` on the card flow, the cluster page's own submit
function for its `person-input` field). No new state; purely additive.

### 2. Remove person

New route, gated behind `serve_faces_ui`:

```
POST /api/delete-person
Body: { "label": "<exact person_label>" }
```

Handler `handle_delete_person`:

```rust
#[derive(Deserialize)]
struct DeletePersonRequest {
    label: String,
}

async fn handle_delete_person(
    State(state): State<AppState>,
    Json(req): Json<DeletePersonRequest>,
) -> StatusCode {
    let conn = state.conn.lock().unwrap();
    let result = conn.execute(
        "UPDATE faces SET cluster_id = NULL, person_label = NULL, confirmed = 0, is_primary = 0 WHERE person_label = ?1",
        rusqlite::params![req.label],
    );
    match result {
        Ok(_) => StatusCode::OK,
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}
```

This resets every face carrying that label back to an unassigned singleton (same
target state `handle_remove_face` puts a single face into). It does **not** trigger
re-clustering - there is no live DBSCAN re-run in this server, and adding one is out
of scope. The faces reappear under "Singletons" on the labeling home page.

UI: on `PERSON_HTML`, add a "Remove person" button to the right side of the toolbar
(`~1658` area). On click: `confirm('Remove ' + name + '? Their ' + count + ' photo(s)
will become unassigned.')`, then POST, then `location.href = '/'` on success. On
failure, `alert('Failed to remove person.')` and stay on the page.

### 3. Rename person

New route, gated behind `serve_faces_ui`:

```
POST /api/rename-person
Body: { "old_label": "<current>", "new_label": "<desired>" }
```

Handler `handle_rename_person`:

```rust
#[derive(Deserialize)]
struct RenamePersonRequest {
    old_label: String,
    new_label: String,
}

async fn handle_rename_person(
    State(state): State<AppState>,
    Json(req): Json<RenamePersonRequest>,
) -> Result<StatusCode, StatusCode> {
    let Some(sanitized) = sanitize_person_label(&req.new_label) else {
        return Err(StatusCode::BAD_REQUEST);
    };
    let conn = state.conn.lock().unwrap();
    conn.execute(
        "UPDATE faces SET person_label = ?1 WHERE person_label = ?2",
        rusqlite::params![sanitized, req.old_label],
    )
    .map(|_| StatusCode::OK)
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}
```

UI: on `PERSON_HTML`, add a text input (pre-filled with the current name, via the
existing pattern of picking the name up client-side) plus a "Save" button on the
left side of the toolbar. Sanitize client-side with the same `sanitizeName()` used
elsewhere before sending. On success, `location.href = '/person/' +
encodeURIComponent(sanitized)`. On failure (400 from empty-after-sanitize, or 500),
`alert('Rename failed.')` and stay put.

### 4. Real assign modal

`/api/assign` and `/api/new-person` are already functionally identical (see Current
State). Add one new read endpoint plus one shared modal component; do not add a new
mutating endpoint.

```
GET /api/people
Response: { "labels": ["Alice", "Bob", ...] }
```

Handler `handle_list_people`:

```rust
async fn handle_list_people(State(state): State<AppState>) -> Json<PeopleResponse> {
    let conn = state.conn.lock().unwrap();
    let mut stmt = conn
        .prepare("SELECT DISTINCT person_label FROM faces WHERE confirmed = 1 AND person_label IS NOT NULL ORDER BY person_label")
        .unwrap();
    let labels: Vec<String> = stmt
        .query_map([], |row| row.get(0))
        .unwrap()
        .filter_map(Result::ok)
        .collect();
    Json(PeopleResponse { labels })
}

#[derive(Serialize)]
struct PeopleResponse {
    labels: Vec<String>,
}
```

Modal markup (added once to `CLUSTER_HTML`, the only page that currently has the
`prompt()` flow):

```html
<div id="assignModal" class="modal-backdrop">
  <div class="modal">
    <h3>Assign to person</h3>
    <input id="assignInput" list="assign-people-list" placeholder="Person name" autofocus>
    <datalist id="assign-people-list"></datalist>
    <div class="modal-actions">
      <button onclick="submitAssignModal()">Assign</button>
      <button onclick="closeAssignModal()">Cancel</button>
    </div>
  </div>
</div>
```

```js
let assignModalFaceId = null;

function openAssignModal(faceId) {
  assignModalFaceId = faceId;
  fetch('/api/people').then(r => r.json()).then(data => {
    document.getElementById('assign-people-list').innerHTML =
      data.labels.map(l => '<option value="' + escHtml(l) + '">').join('');
  });
  document.getElementById('assignModal').classList.add('on');
  document.getElementById('assignInput').value = '';
  document.getElementById('assignInput').focus();
}

function closeAssignModal() {
  document.getElementById('assignModal').classList.remove('on');
  assignModalFaceId = null;
}

function submitAssignModal() {
  const label = sanitizeName(document.getElementById('assignInput').value);
  if (!label) return;
  fetch('/api/assign', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ face_ids: [assignModalFaceId], person_label: label }),
  }).then(r => {
    if (r.ok) { closeAssignModal(); /* existing card-removal logic from assignOne */ }
    else alert('Assign failed.');
  });
}

document.getElementById('assignInput').addEventListener('keydown', function(e) {
  if (e.key === 'Enter') { e.preventDefault(); submitAssignModal(); }
});
```

`assignOne(faceId)`'s body is replaced with `openAssignModal(faceId)`; the
`prompt()` call and its inline `fetch` are removed. Card-removal-on-success logic
that previously lived inside `assignOne`'s `.then()` moves into
`submitAssignModal()`'s success branch, parameterized on `assignModalFaceId`.

### 5. Sanitization hardening

Server (`sanitize_person_label`, ~1975-1981) - add a filter step before whitespace
collapsing that drops control and known bidi/format characters:

```rust
fn sanitize_person_label(raw: &str) -> Option<String> {
    let filtered: String = raw
        .chars()
        .filter(|c| !c.is_control() && !is_disallowed_format_char(*c))
        .collect();
    let collapsed = filtered.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return None;
    }
    Some(collapsed.chars().take(60).collect())
}

/// Zero-width and bidi-override characters that can visually spoof a name
/// (e.g. right-to-left override) without being caught by `char::is_control`.
fn is_disallowed_format_char(c: char) -> bool {
    matches!(
        c,
        '\u{200B}'..='\u{200F}' // zero-width space/joiners, LRM/RLM
        | '\u{202A}'..='\u{202E}' // LRE/RLE/PDF/LRO/RLO bidi overrides
        | '\u{2060}'..='\u{2069}' // word joiner, invisible operators, isolates
        | '\u{FEFF}' // BOM / zero-width no-break space
    )
}
```

Client (`sanitizeName()`, both copies at ~1375-1377 and ~1535-1537) - mirror the
same filter and switch truncation to code-point-safe:

```js
function sanitizeName(raw) {
  const filtered = Array.from(raw).filter(function(ch) {
    const cp = ch.codePointAt(0);
    if (cp < 0x20 || (cp >= 0x7f && cp <= 0x9f)) return false; // control chars
    if (cp >= 0x200B && cp <= 0x200F) return false;
    if (cp >= 0x202A && cp <= 0x202E) return false;
    if (cp >= 0x2060 && cp <= 0x2069) return false;
    if (cp === 0xFEFF) return false;
    return true;
  }).join('');
  return filtered.trim().replace(/\s+/g, ' ').split('').length > MAX_NAME_LEN
    ? Array.from(filtered.trim().replace(/\s+/g, ' ')).slice(0, MAX_NAME_LEN).join('')
    : filtered.trim().replace(/\s+/g, ' ');
}
```

(Simplify at implementation time if a cleaner code-point-safe truncation one-liner is
clearer - the requirement is: filter same character classes as the server, and
truncate by `Array.from(str)` code points, not `.slice()`.)

This is a pure hardening change - existing valid names (letters, digits, spaces,
normal punctuation, emoji) are unaffected.

### 6. Face-crop and original-image caching

Extend `crates/videre-core/src/thumb_cache.rs` with two new path functions, following
the existing naming/return-type conventions:

```rust
/// Cache path for a single face crop. Distinct from `thumb_path` because many
/// faces can share one source `hash` - the face id disambiguates.
pub fn face_thumb_path(hash: &str, face_id: i64, size: u32) -> PathBuf {
    cache_dir().join(format!("{hash}_face{face_id}_{size}.jpg"))
}

pub fn face_thumb_exists(hash: &str, face_id: i64, size: u32) -> bool {
    face_thumb_path(hash, face_id, size).exists()
}

/// Cache path for a full-resolution HEIC-converted original, used by
/// handle_original_image. One per hash (not per face - the original photo
/// is the same regardless of which face on it was clicked).
pub fn original_path(hash: &str) -> PathBuf {
    cache_dir().join(format!("{hash}_original.jpg"))
}

pub fn original_exists(hash: &str) -> bool {
    original_path(hash).exists()
}
```

Wire `handle_face_image` (~2235-2276) to check-then-write, mirroring
`handle_raw_file`'s existing pattern (~2342-2349):

1. Look up `hash`, `bbox`, `path` for `face_id` (unchanged).
2. Check `thumb_cache::face_thumb_exists(&hash, face_id, FACE_THUMB_SIZE)`. If
   present, read and return those bytes directly (no reconversion).
3. Otherwise, run the existing `make_face_thumb` path, then write the result to
   `thumb_cache::face_thumb_path(&hash, face_id, FACE_THUMB_SIZE)` before returning
   it (write via a `.tmp<pid>` + rename, matching the existing whole-image cache
   writer's crash-safety pattern - reuse or extract that helper if already factored
   out, otherwise inline the same tmp-then-rename sequence).

`FACE_THUMB_SIZE` is whatever fixed size `make_face_thumb` already produces today
(confirm the constant/literal at implementation time - this spec doesn't change the
rendered size, only adds caching around it).

Wire `handle_original_image` (~2382-2423) the same way using `original_path`/
`original_exists`, caching the full HEIC-converted JPEG once per hash.

Cache invalidation: none needed. Both caches are keyed by content hash (source file
content) plus, for face crops, a DB-local `face_id` - if a face is re-detected with a
different id after `--reprocess`, its crop is simply cached under the new id
(the old entry becomes an orphan, same lifecycle as the existing whole-image thumb
cache, which also has no invalidation and is documented as a straightforward cache
that regenerates on miss).

### 7. Back-link referrer + escHtml fix

**Lightbox side** (`renderMetaPanel`, ~943-971): append `?from=lightbox` to the
person link's `href`, and fix the unescaped name in the link text:

```js
parts.push(meta.faces.map(function(fc){
  return '<div class="lb-face"><img src="'+fc.thumb+'">'+
    '<a href="/person/'+encodeURIComponent(fc.name)+'?from=lightbox">'+escHtml(fc.name)+'</a></div>';
}).join(''));
```

**Person/cluster page side** (`PERSON_HTML` ~1658, `CLUSTER_HTML` ~1512): replace the
static anchor with a placeholder plus a small inline script that runs on page load:

```html
<a id="backLink" href="/">&larr; Back to labeling</a>
```

```js
(function() {
  const params = new URLSearchParams(location.search);
  if (params.get('from') === 'lightbox') {
    const link = document.getElementById('backLink');
    link.textContent = '← Back';
    link.href = '#';
    link.onclick = function(e) { e.preventDefault(); history.back(); };
  }
})();
```

No change when reached directly from the labeling home page (`from` absent) - the
link keeps today's text and `/` target.

## Testing

Given this is server-rendered HTML/JS embedded in Rust string constants with no
existing frontend test harness, testing follows the same pattern already used
elsewhere in `report.rs`'s test suite (`crates/videre/tests/report.rs` and similar):

- **Route/handler tests** (Rust, `tests/report.rs` or a new test module): for each new
  endpoint (`/api/delete-person`, `/api/rename-person`, `/api/people`), spin up the
  test server fixture already used by existing face-labeling route tests, seed a
  `faces` row or two, hit the endpoint, assert the resulting DB state (e.g. after
  delete-person, assert `cluster_id`/`person_label`/`confirmed` are reset; after
  rename, assert the label changed on all matching rows; `/api/people` returns
  distinct sorted labels).
- **Sanitization unit tests** (Rust, alongside `sanitize_person_label` in
  `report.rs`): existing tests (if any) plus new cases for control characters,
  zero-width/bidi characters, and a 61+-character multibyte string to confirm
  code-point-safe truncation.
- **Cache wiring tests** (Rust, `crates/videre-core/src/thumb_cache.rs` and/or
  `tests/report.rs`): unit test `face_thumb_path`/`original_path` produce the
  expected filename format; an integration test hitting `/api/face-image/{id}` twice
  and asserting the second response is served from a cache hit (e.g. by checking the
  cache file now exists on disk after the first call, or by asserting byte-identical
  output - whichever is simpler to assert without mocking the filesystem clock/mtime).
- **Manual smoke test** (since this is UI-heavy and there's no browser test
  automation in this project): run `videre report --faces` against a real db with a
  labeled person, a cluster, and a singleton; walk through all seven fixes by hand
  (Enter-to-submit, remove person, rename person, assign modal + autocomplete,
  emoji/control-char name entry, reload a face thumbnail twice and confirm the second
  load is instant/cached, and the lightbox-to-person-back-button flow) before
  reporting the slice done.

## Error handling

- All new `POST` handlers return `4xx`/`5xx` `StatusCode`s on failure (empty
  sanitized name, DB error) with no response body, matching the existing
  `handle_assign`/`handle_new_person` convention.
- Client-side, failures show a plain `alert()` and leave the user on the current
  page/state (no partial navigation), matching existing error handling elsewhere in
  these templates (e.g. `assignOne`'s prior error path).
- Cache writes (`thumb_cache`) are best-effort: if a write fails (e.g. disk full,
  permissions), the handler still returns the freshly generated bytes to the
  client - caching is a performance optimization, not a correctness requirement, so
  a write failure must not turn into a request failure. Log a `println!`/`eprintln!`
  (matching the codebase's existing lightweight error-visibility convention for
  non-fatal issues) rather than propagating an error to the HTTP response.
