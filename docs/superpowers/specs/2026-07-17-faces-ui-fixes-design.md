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
8. (Found during review, folded into item 2) Fix `handle_remove_face` to also reset
   `is_primary` on removal, matching the reset the new delete-person handler performs.

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
  (~2074-2082) delegates to `videre_core::person_search::search_by_person`, a
  case-insensitive **prefix** match on `person_label`, not a full label list.
- `assignOne` (~1615-1628, cluster page only) uses `prompt('Assign face #' + faceId +
  ' to person:')` and POSTs to `/api/new-person` with `{face_ids: [faceId], label}`.
- `handle_face_image` (~2235-2276) and `handle_original_image` (~2382-2423) both
  reconvert/re-crop from the source file on every request - no caching.
  `make_face_thumb`/`crop_face_square` always produce a fixed 140x140 crop
  (report.rs:2204) regardless of the display size used by any given card (66/140/180px
  via CSS), so there is exactly one cache size to key on.
- `videre_core::thumb_cache` (crates/videre-core/src/thumb_cache.rs) is keyed
  strictly by `(hash, size)`: `thumb_path(hash, size) -> <hash>_<size>.jpg`. It has
  no concept of a face id or bbox, so it cannot serve face crops as-is.
  `thumb_tmp_path` disambiguates only by process id (`.tmp<pid>`), which was
  sufficient for `videre watch`'s single-threaded writer but is not safe for
  concurrent axum request handlers (see Design item 6 below).
- `handle_raw_file` (~2318-2374) checks the cache on read but never writes to it on a
  miss - it is a check-only reader, not the check-then-write pattern this spec needs.
  The actual check-then-write-with-tmp-rename pattern to model this on lives in
  `crates/videre/src/commands/watch.rs` (~173-210), which runs single-threaded so its
  pid-only tmp naming has been safe there; the server context needs a stronger
  per-request-unique tmp name (see Design item 6).
- Router (~2455-2487): page routes are bare nouns (`/cluster/{id}`, `/person/{name}`);
  API routes are `/api/<verb-or-noun>` in kebab-case (`new-person`, `remove-face`,
  `dissolve-cluster`, `set-primary`), all `POST` except reads. New routes in this spec
  follow the same convention and are gated behind `serve_faces_ui` like the existing
  mutating routes. `/person/{name}` itself, however, is registered **unconditionally**
  (available under `--show-faces` alone, without `--faces`) - see Design item 7 for
  why this matters for the new Remove/Rename buttons.
- `renderMetaPanel` (~943-971, lightbox metadata panel) builds a person link as a raw
  string with no escaping call on `fc.name`:
  `'<a href="/person/'+encodeURIComponent(fc.name)+'">'+fc.name+'</a>'` - the second
  `fc.name` (link text) is unescaped HTML. Note this function is part of the main
  report/lightbox JS, not the faces-labeling templates - the escape helpers in scope
  here are `escA`/`escH` (report.rs:798-803), not the `escHtml` defined separately
  inside `FACES_HTML`/`CLUSTER_HTML`/`PERSON_HTML`.
- `handle_remove_face` (~2020-2022) resets a single face via
  `SET cluster_id = NULL, person_label = NULL, confirmed = 0` - it does **not** reset
  `is_primary`, which is a latent bug (a removed face can keep `is_primary = 1` while
  unassigned). Fixed alongside item 2 below for consistency.
- "Back to labeling" anchors on the cluster page (~1512) and person page (~1658) are
  static `<a href="/">&larr; Back to labeling</a>` with no JS/query-param handling.

## Design

### 1. Enter-to-submit on New Person inputs

Corrected from the initial pass: the card-flow input (`FACES_HTML`, injected via
`innerHTML` inside `showNewPersonInput` at click time, ~1448-1458) has `autofocus`,
but the cluster page's `#person-input` (`CLUSTER_HTML` ~1520) does **not** - it only
has `maxlength` and `list`. Also, because the card input is injected dynamically, a
listener attached at page load would never reach it; it must be attached inside
`showNewPersonInput` itself, right after the `innerHTML` assignment.

Real function names to wire up: `submitNewPerson(inputId, faceIds)` (report.rs:1460)
for the card flow, `assignAll()` (report.rs:1591) for the cluster page's bulk-assign
field.

**Card flow** (inside `showNewPersonInput`, after the input is inserted into the DOM):

```js
const input = document.getElementById(inputId);
input.addEventListener('keydown', function(e) {
  if (e.key === 'Enter') { e.preventDefault(); submitNewPerson(inputId, faceIds); }
});
```

**Cluster page** (`#person-input`, attached once at page-load time since this input
is static markup, not injected):

```js
document.getElementById('person-input').addEventListener('keydown', function(e) {
  if (e.key === 'Enter') { e.preventDefault(); assignAll(); }
});
```

No new state; purely additive.

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
        "UPDATE faces SET person_label = NULL, confirmed = 0, is_primary = 0 WHERE person_label = ?1",
        rusqlite::params![req.label],
    );
    match result {
        Ok(_) => StatusCode::OK,
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}
```

Deliberately **does not** touch `cluster_id`. Your original ask was "move those
photos back to unassigned cluster" - leaving `cluster_id` alone is what makes that
true: faces that came from a DBSCAN cluster rejoin that cluster's unassigned group
(picked up by the existing cluster query, ~1844-1846) instead of scattering to
Singletons; faces that were singletons to begin with (`cluster_id` already `NULL`)
stay singletons. It does **not** trigger re-clustering - there is no live DBSCAN
re-run in this server, and adding one is out of scope.

Same-slice fix for consistency: `handle_remove_face` (~2020-2022) currently resets a
single face via `SET cluster_id = NULL, person_label = NULL, confirmed = 0` - both
the unconditional `cluster_id = NULL` (single-face removal has always scattered to
Singletons, unlike the person-level removal above, which is a legitimate
inconsistency but out of scope to change here since it'd alter existing single-face
behavior) and the missing `is_primary = 0` reset are worth aligning. This spec fixes
only the `is_primary` omission, since that's a straightforward bug fix with no
behavior-change judgment call attached:

```rust
"UPDATE faces SET cluster_id = NULL, person_label = NULL, confirmed = 0, is_primary = 0 WHERE id = ?1"
```

UI: on `PERSON_HTML`, add a "Remove person" button to the right side of the toolbar
(`~1658` area), **only rendered when the mutating faces-UI routes are actually
available** - see the `--show-faces`-only gap called out in item 7 below; the same
guard applies here. On click: `confirm('Remove ' + name + '? Their ' + count + '
photo(s) will become unassigned.')`, then POST, then `location.href = '/'` on
success. On failure, `alert('Failed to remove person.')` and stay on the page.

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

    let old_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM faces WHERE person_label = ?1",
            rusqlite::params![req.old_label],
            |row| row.get(0),
        )
        .unwrap_or(0);
    if old_count == 0 {
        return Err(StatusCode::NOT_FOUND);
    }

    let collision_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM faces WHERE person_label = ?1",
            rusqlite::params![sanitized],
            |row| row.get(0),
        )
        .unwrap_or(0);
    if collision_count > 0 && sanitized != req.old_label {
        return Err(StatusCode::CONFLICT);
    }

    conn.execute(
        "UPDATE faces SET person_label = ?1 WHERE person_label = ?2",
        rusqlite::params![sanitized, req.old_label],
    )
    .map(|_| StatusCode::OK)
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}
```

Rename is rejected outright (`409 Conflict`) if `new_label` already names an existing
person, rather than silently merging the two. A silent merge would be irreversible
and could leave the merged person with two faces marked `is_primary = 1`, which would
violate the invariant `handle_set_primary` (~2043-2071) maintains elsewhere via its
clear-then-set transaction. Merging is a real feature someone might want later, but
it needs its own explicit UI affordance and primary-face resolution, not a side
effect of rename - out of scope here. `404 Not Found` covers the edge case of
`old_label` no longer existing (e.g. a stale page open in two tabs, one of which
already renamed or removed the person).

UI: on `PERSON_HTML`, add a text input (pre-filled with the current name, via the
existing pattern of picking the name up client-side) plus a "Save" button on the
left side of the toolbar, gated the same way as the Remove button in item 2. Sanitize
client-side with the same `sanitizeName()` used elsewhere before sending. On success,
`location.href = '/person/' + encodeURIComponent(sanitized)`. On failure: `409` shows
`alert('A person named "' + sanitized + '" already exists.')`; `400`/`404`/`500` show
`alert('Rename failed.')`; all failures stay on the current page.

### 4. Real assign modal

`/api/assign` and `/api/new-person` are already functionally identical (see Current
State) - confirmed byte-for-byte identical apart from the request struct's field name
(`person_label` vs `label`). No new mutating endpoint needed.

No new read endpoint either: the cluster page already fetches the full people list on
load via `/api/faces` and uses it to populate the existing bulk-assign datalist
(`mainData.people`, report.rs:1549-1550). The modal reuses that same in-memory array
rather than adding a redundant `/api/people` endpoint.

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
  // mainData.people is already populated from the /api/faces load that
  // built this page - same source the existing bulk-assign datalist uses.
  document.getElementById('assign-people-list').innerHTML =
    mainData.people.map(l => '<option value="' + escHtml(l) + '">').join('');
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
/// Deliberately excludes U+200C (ZWNJ) and U+200D (ZWJ): both are required for
/// legitimate text - ZWJ joins emoji sequences (a family emoji is three emoji
/// joined by ZWJ; stripping it splits them into separate characters) and ZWNJ
/// is orthographically required in Persian and several Indic scripts.
fn is_disallowed_format_char(c: char) -> bool {
    matches!(
        c,
        '\u{200B}' // zero-width space
        | '\u{200E}'..='\u{200F}' // LRM/RLM
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
    if (cp === 0x200B) return false; // zero-width space
    if (cp === 0x200E || cp === 0x200F) return false; // LRM/RLM
    // 0x200C (ZWNJ) and 0x200D (ZWJ) are intentionally allowed - required for
    // Persian/Indic text and emoji ZWJ sequences (family emoji etc).
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

Wire `handle_face_image` (~2235-2276) to check-then-write, following the pattern in
`watch.rs` (~173-210) rather than `handle_raw_file` (which is check-only, see Current
State above):

1. Look up `hash`, `bbox`, `path` for `face_id` (unchanged).
2. Check `thumb_cache::face_thumb_exists(&hash, face_id, FACE_THUMB_SIZE)`. If
   present, read and return those bytes directly (no reconversion). `FACE_THUMB_SIZE`
   is `140` - the fixed crop size `make_face_thumb`/`crop_face_square` already
   produce today (report.rs:2204); this spec doesn't change the rendered size, only
   adds caching around it.
3. Otherwise, run the existing `make_face_thumb` path, then write the result to
   `thumb_cache::face_thumb_path(&hash, face_id, FACE_THUMB_SIZE)` before returning
   it.

**Tmp-file naming must not reuse the existing `.tmp<pid>` scheme as-is.**
`thumb_tmp_path`'s pid-only disambiguation was written for `videre watch`, a
single-threaded writer where one process only ever writes one tmp file for a given
`(hash, size)` at a time. In the axum server, two concurrent requests for the same
uncached face run inside the *same* process and would collide on the same tmp path,
risking a torn file getting renamed into a cache that (per the no-invalidation policy
below) would then be served indefinitely. Add a per-request-unique suffix, e.g. an
`AtomicU64` counter on `AppState` combined with the pid
(`.tmp<pid>-<counter.fetch_add(1, Ordering::Relaxed)>`), so concurrent writers never
target the same tmp path even under a request storm for the same face.

Wire `handle_original_image` (~2382-2423) the same way using `original_path`/
`original_exists`, caching the full HEIC-converted JPEG once per hash, with the same
unique-tmp-suffix requirement.

Cache invalidation: none needed for the common case - both caches are keyed by
content hash plus, for face crops, a DB-local `face_id`. One edge case worth noting
rather than dismissing: `faces.id` is `INTEGER PRIMARY KEY` without `AUTOINCREMENT`
(face_db.rs), and `--reprocess` deletes-then-reinserts by hash, so SQLite can in
principle reuse a previously-deleted id for a *different* face/bbox after a
reprocess. This would serve a stale cached crop under the reused id. Low probability
and not worth engineering around in this slice (the existing whole-image thumb cache
accepts the same class of risk with no invalidation), but call it out rather than
assert it can't happen.

### 7. Back-link referrer + escaping fix

**Lightbox side** (`renderMetaPanel`, ~943-971): append `?from=lightbox` to the
person link's `href`, and fix the unescaped name in the link text. This function is
part of the main report/lightbox JS, so it must use the escape helper already in
scope there, `escH` (report.rs:798-803) - **not** `escHtml`, which is a
different, unrelated function defined only inside the separate `FACES_HTML`/
`CLUSTER_HTML`/`PERSON_HTML` templates and is not in scope here:

```js
parts.push(meta.faces.map(function(fc){
  return '<div class="lb-face"><img src="'+escA(fc.thumb)+'">'+
    '<a href="/person/'+encodeURIComponent(fc.name)+'?from=lightbox">'+escH(fc.name)+'</a></div>';
}).join(''));
```

(`fc.thumb` is wrapped in `escA` too - it's a server-generated base64 data URI today
so this isn't fixing a live bug, just costing nothing to be consistent.)

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
link keeps today's text and `/` target. Note only `PERSON_HTML` actually needs this:
nothing currently links to `/cluster/{id}?from=lightbox` (the lightbox only links to
person pages, never cluster pages), so the `CLUSTER_HTML` copy of this script would
be dead code - add it only to `PERSON_HTML`.

**`--show-faces`-only gap:** `/person/{name}` is registered unconditionally
(report.rs:2460), so it's reachable from the lightbox even when the server was
started with `--show-faces` alone (no `--faces`). The new Remove-person and
Rename-person buttons from items 2 and 3, however, call routes gated behind
`serve_faces_ui`, which would 404 in that mode. `PERSON_HTML` needs a server-injected
flag so it knows whether to render those controls at all:

```rust
const PERSON_HTML: &str = r#"..."#; // existing template

// at render time, alongside any other __PLACEHOLDER__ substitutions already done for this template:
let html = PERSON_HTML.replace("__FACES_UI_ENABLED__", if serve_faces_ui { "true" } else { "false" });
```

```js
const FACES_UI_ENABLED = __FACES_UI_ENABLED__;
// ...
if (FACES_UI_ENABLED) {
  // render Remove-person / Rename-person controls
}
```

This is the same class of fix the per-face Remove button on this page already
needed and didn't have - this spec closes it for the new controls being added here,
without attempting to retrofit the pre-existing per-face Remove button (out of scope,
but worth a follow-up note since it has the identical latent gap).

## Testing

Given this is server-rendered HTML/JS embedded in Rust string constants with no
existing frontend test harness, testing follows the same pattern already used
elsewhere in `report.rs`'s test suite (`crates/videre/tests/report.rs` and similar):

- **Route/handler tests** (Rust, `crates/videre/tests/faces_server.rs`, the actual
  existing fixture file for these routes): for each new endpoint
  (`/api/delete-person`, `/api/rename-person`), spin up the test server fixture
  already used by existing face-labeling route tests, seed a `faces` row or two, hit
  the endpoint, assert the resulting DB state (e.g. after delete-person, assert
  `person_label`/`confirmed`/`is_primary` are reset and `cluster_id` is
  **unchanged**; after rename, assert the label changed on all matching rows and that
  renaming onto an existing label returns `409` without modifying either person's
  rows; renaming a nonexistent `old_label` returns `404`). Also add a regression test
  for `handle_remove_face` confirming `is_primary` is now reset.
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
