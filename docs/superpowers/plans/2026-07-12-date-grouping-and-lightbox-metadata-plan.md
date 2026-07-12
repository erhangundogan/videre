# Date Grouping and Lightbox Metadata Panel Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a `--by-date` flag to `dupe-report` (Year/Month/Day drill-down gallery over KEEP files), and a `--show-faces` flag that starts the existing axum face-labeling server in an extended mode: it now also serves the main report (dedup groups + optional `--all`/`--by-date` sections) at `/`, adds a shared lightbox "metadata panel" showing labeled faces (clickable to `/person/<name>`) and a lazily-resolved, DB-cached location name via a new `/api/location` endpoint.

**Architecture:** `--by-date` alone stays fully static (no server), following the exact additive pattern `--all` already uses. `--show-faces` is the flag that requires a live backend (person click-through + on-demand location lookup can't work from a static `file://` page), so it repurposes `serve_faces_async`'s router: `/` now serves the report, `/faces` takes over the People/Clusters/Singletons labeling UI when `--faces` is also passed. A new `dupe-core::location` module wraps an offline reverse-geocoding crate and a new `file_hashes.location_name` column caches results so repeat lookups (and the future location-grouping/map-view roadmap phase) are free.

**Tech Stack:** Rust workspace (`dupe`, `dupe-core`, `dupe-ml`), `rusqlite`, `axum` 0.8 + `tokio`, `image` crate, `reverse_geocoder` 4.x (new dependency, offline GeoNames-based reverse geocoding).

**Reference spec:** `docs/superpowers/specs/2026-07-12-date-grouping-design.md`

---

### Task 1: `dupe-core` — location schema migration and reverse-geocoding lookup

**Files:**
- Modify: `crates/dupe-core/Cargo.toml`
- Create: `crates/dupe-core/src/location.rs`
- Modify: `crates/dupe-core/src/lib.rs`

- [ ] **Step 1: Add the `reverse_geocoder` dependency**

Edit `crates/dupe-core/Cargo.toml`, adding this line to `[dependencies]`:

```toml
reverse_geocoder = "4"
```

- [ ] **Step 2: Write the failing tests**

Create `crates/dupe-core/src/location.rs`:

```rust
use reverse_geocoder::ReverseGeocoder;
use rusqlite::Connection;

/// Idempotent migration: adds `file_hashes.location_name` if it doesn't
/// already exist. Mirrors the `ALTER TABLE faces ADD COLUMN is_primary`
/// pattern in face_db.rs - errors (column already exists) are ignored.
pub fn ensure_location_column(conn: &Connection) {
    let _ = conn.execute_batch("ALTER TABLE file_hashes ADD COLUMN location_name TEXT");
}

/// Reverse-geocodes (lat, lon) to a human-readable "City, Country" string
/// using an offline GeoNames-derived dataset (no network calls). Always
/// returns Some(..) since the bundled dataset covers the whole globe with a
/// nearest-city match - there's always some nearest record.
pub fn location_name(lat: f64, lon: f64) -> Option<String> {
    let geocoder = ReverseGeocoder::new();
    let result = geocoder.search((lat, lon));
    let record = &result.record;
    if record.name.is_empty() {
        None
    } else {
        Some(format!("{}, {}", record.name, record.cc))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_location_column_is_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE file_hashes (path TEXT PRIMARY KEY, hash TEXT NOT NULL);",
        )
        .unwrap();
        ensure_location_column(&conn);
        ensure_location_column(&conn); // second call must not error
        conn.execute(
            "UPDATE file_hashes SET location_name = 'Paris, FR' WHERE path = 'x'",
            [],
        )
        .unwrap();
    }

    #[test]
    fn location_name_resolves_known_city() {
        // Coordinates for central Paris, France.
        let name = location_name(48.8566, 2.3522).unwrap();
        assert!(name.contains("FR"), "expected France country code, got: {name}");
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p dupe-core location`
Expected: FAIL with "unresolved module `location`" (module not wired into `lib.rs` yet)

- [ ] **Step 4: Wire the module into `lib.rs`**

Edit `crates/dupe-core/src/lib.rs`, adding:

```rust
pub mod location;
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p dupe-core location`
Expected: PASS (2 tests)

- [ ] **Step 6: Commit**

```bash
git add crates/dupe-core/Cargo.toml crates/dupe-core/src/location.rs crates/dupe-core/src/lib.rs
git commit -m "feat: add offline reverse-geocoding and location_name column migration"
```

---

### Task 2: `dupe-core` — batched labeled-faces-by-hash query

**Files:**
- Modify: `crates/dupe-core/src/face_db.rs`

- [ ] **Step 1: Write the failing test**

Add to the existing `#[cfg(test)] mod tests` block in `crates/dupe-core/src/face_db.rs` (append after the existing tests, keeping the same style as `hashes_with_faces` tests already there):

```rust
#[test]
fn labeled_faces_by_hash_returns_only_confirmed_labeled() {
    let conn = Connection::open_in_memory().unwrap();
    create_faces_table(&conn).unwrap();
    conn.execute_batch(
        "INSERT INTO faces (hash, bbox, embedding, person_label, confirmed) \
         VALUES ('h1', '0,0,10,10', X'0000', 'Alice', 1); \
         INSERT INTO faces (hash, bbox, embedding, person_label, confirmed) \
         VALUES ('h1', '20,20,10,10', X'0000', NULL, 0); \
         INSERT INTO faces (hash, bbox, embedding, person_label, confirmed) \
         VALUES ('h2', '0,0,10,10', X'0000', 'Bob', 1);",
    )
    .unwrap();

    let map = labeled_faces_by_hash(&conn).unwrap();
    assert_eq!(map.len(), 2, "expected two hashes with labeled faces");
    let h1 = &map["h1"];
    assert_eq!(h1.len(), 1, "unconfirmed/unlabeled face must be excluded");
    assert_eq!(h1[0].1, "Alice");
    assert_eq!(map["h2"][0].1, "Bob");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p dupe-core labeled_faces_by_hash`
Expected: FAIL with "cannot find function `labeled_faces_by_hash`"

- [ ] **Step 3: Implement `labeled_faces_by_hash`**

Add to `crates/dupe-core/src/face_db.rs` (near `hashes_with_faces`):

```rust
use std::collections::HashMap;

/// Returns, for every hash that has at least one confirmed+labeled face, the
/// list of (face_id, bbox, person_label) for that hash. One batched query
/// covering every hash, not one query per file - safe to call once per
/// report generation without N+1 overhead.
pub fn labeled_faces_by_hash(
    conn: &Connection,
) -> rusqlite::Result<HashMap<String, Vec<(i64, String, String)>>> {
    let mut stmt = conn.prepare(
        "SELECT hash, id, bbox, person_label FROM faces \
         WHERE confirmed = 1 AND person_label IS NOT NULL \
         ORDER BY hash, id",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
        ))
    })?;
    let mut map: HashMap<String, Vec<(i64, String, String)>> = HashMap::new();
    for row in rows {
        let (hash, id, bbox, label) = row?;
        map.entry(hash).or_default().push((id, bbox, label));
    }
    Ok(map)
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p dupe-core labeled_faces_by_hash`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/dupe-core/src/face_db.rs
git commit -m "feat: add batched labeled_faces_by_hash query to dupe-core"
```

---

### Task 3: `dupe-report` — `--by-date` and `--show-faces` CLI flags

**Files:**
- Modify: `crates/dupe/src/bin/dupe_report.rs:14-39` (`Args` struct)
- Test: `crates/dupe/tests/report.rs`

- [ ] **Step 1: Write the failing test**

Add to `crates/dupe/tests/report.rs`:

```rust
#[test]
fn help_lists_new_flags() {
    let out = Command::new(report_bin())
        .arg("--help")
        .output()
        .expect("failed to run dupe-report");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("by-date"), "expected --by-date in help output");
    assert!(stdout.contains("show-faces"), "expected --show-faces in help output");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p dupe --test report help_lists_new_flags`
Expected: FAIL - `--by-date`/`--show-faces` not present in help output

- [ ] **Step 3: Add the fields to `Args`**

Edit `crates/dupe/src/bin/dupe_report.rs`, inside the `Args` struct (after the existing `faces: bool` field, line ~38):

```rust
    /// Drill-down Year/Month/Day gallery over KEEP files (static HTML,
    /// same as --all)
    #[arg(long)]
    by_date: bool,

    /// Show labeled faces (clickable to their person page) and a
    /// reverse-geocoded location below the image in the lightbox. Starts a
    /// local server on port 7878 (same one --faces uses) instead of writing
    /// a static HTML file, since person click-through and on-demand
    /// location lookup both need a live backend.
    #[arg(long)]
    show_faces: bool,
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p dupe --test report help_lists_new_flags`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/dupe/src/bin/dupe_report.rs crates/dupe/tests/report.rs
git commit -m "feat: add --by-date and --show-faces CLI flags to dupe-report"
```

---

### Task 4: `dupe-report` — `query_keep_files()`

**Files:**
- Modify: `crates/dupe/src/bin/dupe_report.rs` (new function, near `query_all_files()` at line 342)
- Test: `crates/dupe/tests/report.rs`

- [ ] **Step 1: Write the failing test**

Add to `crates/dupe/tests/report.rs`, reusing the existing `fixture_db()` helper (hdup: 2 paths sharing a hash, hsing: 1 path, hvid: 1 video path - see lines 14-64 of that file):

```rust
fn run_report_by_date(db: &std::path::Path) -> String {
    let out = db.with_extension("html");
    Command::new(report_bin())
        .arg(db)
        .arg("-o")
        .arg(&out)
        .arg("--by-date")
        .output()
        .expect("failed to run dupe-report");
    std::fs::read_to_string(&out).unwrap()
}

#[test]
fn by_date_keepfiles_excludes_remove_side_duplicates() {
    let dir = tempdir().unwrap();
    let (db, files) = fixture_db(dir.path(), false);
    let html = run_report_by_date(&db);
    assert!(html.contains("KEEPFILES"), "expected a KEEPFILES array in output");
    // Exactly one of the two hdup paths should appear (the KEEP side),
    // plus the singleton and the video - three KEEPFILES entries total.
    let a_present = html.contains(files[0].to_str().unwrap());
    let b_present = html.contains(files[1].to_str().unwrap());
    assert_ne!(a_present, b_present, "exactly one duplicate-group path should be KEEP");
    assert!(html.contains(files[2].to_str().unwrap()), "singleton must be included");
    assert!(html.contains(files[3].to_str().unwrap()), "video must be included");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p dupe --test report by_date_keepfiles_excludes_remove_side_duplicates`
Expected: FAIL - no `--by-date` section emitted yet (flag parsed but unused)

- [ ] **Step 3: Implement `query_keep_files()`**

Add to `crates/dupe/src/bin/dupe_report.rs`, immediately after `query_all_files()` (after line 369):

```rust
/// Per-hash KEEP-only file set: like query_all_files(), but for hashes with
/// more than one surviving path, only the earliest-by-best_date() row is
/// kept (mirrors query_groups()'s sort-then-take-first rule). Hashes with a
/// single surviving path are trivially KEEP. Used by --by-date so REMOVE-side
/// duplicates never appear in the date-grouped gallery.
fn query_keep_files(conn: &Connection) -> Vec<FileRow> {
    let mut stmt = conn
        .prepare(
            "SELECT path, hash, size_bytes, COALESCE(ext,''), created_at, modified_at, exif_date, \
                    gps_lat, gps_lon, width, height \
             FROM file_hashes \
             WHERE hash IN (SELECT hash FROM file_hashes) \
             ORDER BY hash",
        )
        .expect("failed to prepare query");

    let rows: Vec<FileRow> = stmt
        .query_map([], |r| {
            Ok(FileRow {
                path:       r.get(0)?,
                hash:       r.get(1)?,
                size_bytes: r.get(2)?,
                ext:        r.get(3)?,
                created_at: r.get(4)?,
                modified_at:r.get(5)?,
                exif_date:  r.get(6)?,
                gps_lat:    r.get(7)?,
                gps_lon:    r.get(8)?,
                width:      r.get(9)?,
                height:     r.get(10)?,
            })
        })
        .expect("failed to execute query")
        .filter_map(|r| r.ok())
        .filter(|f| std::path::Path::new(&f.path).exists())
        .collect();

    let mut map: HashMap<String, Vec<FileRow>> = HashMap::new();
    for row in rows {
        map.entry(row.hash.clone()).or_default().push(row);
    }

    map.into_values()
        .map(|mut group| {
            group.sort_by(|a, b| best_date(a).cmp(best_date(b)));
            group.into_iter().next().expect("group is never empty")
        })
        .collect()
}
```

Wire it into `main()` (near the existing `let all_files = args.all.then(|| query_all_files(&conn));` line): add

```rust
let keep_files = args.by_date.then(|| query_keep_files(&conn));
```

and thread `keep_files.as_deref()` into `generate_html(...)` as a new parameter (see Task 5 for the corresponding `generate_html` signature change - both land together since the function won't compile otherwise).

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p dupe --test report by_date_keepfiles_excludes_remove_side_duplicates`
Expected: PASS (this will only fully pass once Task 5's `KEEPFILES` JSON emission lands - if implementing strictly TDD-per-task, mark this test `#[ignore]` until Task 5, then un-ignore and re-run at the end of Task 5. Note this dependency explicitly when executing.)

- [ ] **Step 5: Commit**

```bash
git add crates/dupe/src/bin/dupe_report.rs
git commit -m "feat: add query_keep_files for date-grouped KEEP-only file set"
```

---

### Task 5: `dupe-report` — `--by-date` HTML/JS drill-down section

**Files:**
- Modify: `crates/dupe/src/bin/dupe_report.rs`
  - `generate_html()` signature and call site (lines 371-379, 2024-2081 in `main()`)
  - CSS block (`concat!` head, ~line 400-460)
  - Block A raw-string JS (lines 608-783)

- [ ] **Step 1: Un-ignore the Task 4 test (if it was marked `#[ignore]`)**

If Task 4's `by_date_keepfiles_excludes_remove_side_duplicates` test was left `#[ignore]`d, remove that attribute now - it should pass once this task's `KEEPFILES` emission lands.

- [ ] **Step 2: Write the additional failing test**

Add to `crates/dupe/tests/report.rs`:

```rust
#[test]
fn by_date_emits_year_month_day_buckets() {
    let dir = tempdir().unwrap();
    let (db, _files) = fixture_db(dir.path(), false);
    let conn = Connection::open(&db).unwrap();
    conn.execute(
        "UPDATE file_hashes SET exif_date = '2024-06-15T10:00:00' WHERE hash = 'hdup'",
        [],
    )
    .unwrap();
    conn.execute(
        "UPDATE file_hashes SET exif_date = '2023-01-02T09:00:00' WHERE hash = 'hsing'",
        [],
    )
    .unwrap();
    let html = run_report_by_date(&db);
    assert!(html.contains("buildYearView"), "expected year-view JS function");
    assert!(html.contains("2024") && html.contains("2023"), "expected both years present in KEEPFILES data");
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p dupe --test report by_date_emits_year_month_day_buckets`
Expected: FAIL - `buildYearView` doesn't exist yet

- [ ] **Step 4: Extend `generate_html()` signature and emit `KEEPFILES`**

Edit the `generate_html()` signature (line 371-379) to add a parameter:

```rust
fn generate_html(
    db_path: &str,
    stats: &Stats,
    groups: &[Vec<FileRow>],
    all_files: Option<&[FileRow]>,
    keep_files: Option<&[FileRow]>,
    vectors: Option<&VectorBlock>,
    heic: bool,
    heic_original: bool,
) -> String {
```

Update the call site in `main()` to match (adding `keep_files.as_deref()` in the same argument position).

Inside `generate_html()`, alongside the existing `ALLFILES` emission (find the block that writes `var ALLFILES=[...]` around lines 581-604) add an analogous block:

```rust
    if let Some(kf) = keep_files {
        out.push_str("<script>\nvar KEEPFILES=[");
        let json: Vec<String> = kf.iter().map(|f| file_to_json(f, heic, heic_original)).collect();
        out.push_str(&json.join(","));
        out.push_str("];\n</script>\n");
    }
```

- [ ] **Step 5: Add the date-view HTML container and CSS**

In the `concat!` head/CSS block, add a new CSS section (near the existing `.gallery`/`.card` rules):

```rust
        ".date-view{padding:24px 32px}\n",
        ".date-breadcrumb{margin-bottom:16px;font-size:13px;color:#71717a}\n",
        ".date-breadcrumb a{color:#3f3f46;cursor:pointer;text-decoration:underline}\n",
        ".date-grid{display:grid;grid-template-columns:repeat(auto-fill,minmax(160px,1fr));gap:12px}\n",
        ".date-card{background:#fff;border-radius:8px;overflow:hidden;cursor:pointer;",
        "box-shadow:0 1px 3px rgba(0,0,0,.08)}\n",
        ".date-card img{width:100%;aspect-ratio:1;object-fit:cover;display:block}\n",
        ".date-card .date-card-label{padding:8px;font-size:13px;font-weight:600}\n",
        ".date-card .date-card-count{padding:0 8px 8px;font-size:11px;color:#71717a}\n",
```

Add the container markup right after the existing gallery section's HTML (near line 568, where the `--all` gallery's `<div>` skeleton is written), gated the same way `all_files.is_some()` gates the gallery markup:

```rust
    if keep_files.is_some() {
        out.push_str(concat!(
            "<div class=\"date-view\" id=\"dateView\">\n",
            "<h2>Browse by date</h2>\n",
            "<div class=\"date-breadcrumb\" id=\"dateBreadcrumb\"></div>\n",
            "<div class=\"date-grid\" id=\"dateGrid\"></div>\n",
            "</div>\n",
        ));
    }
```

- [ ] **Step 6: Add the drill-down JS functions**

Add to Block A (the raw-string JS block spanning lines 608-783), right after `sortGroups()` (line 769) and before the click-delegation handler (line 770):

```rust
r#"
function bestDateBucket(f){
  var d = bestDateJs(f);
  if(!d) return null;
  return {year: d.slice(0,4), month: d.slice(0,7), day: d.slice(0,10)};
}
var dateState = {level:'year', year:null, month:null};
function dateKeepFiles(){ return (typeof KEEPFILES!=='undefined') ? KEEPFILES : []; }
function buildYearView(){
  dateState = {level:'year', year:null, month:null};
  var byYear = {};
  dateKeepFiles().forEach(function(f){
    var b = bestDateBucket(f); if(!b) return;
    (byYear[b.year] = byYear[b.year] || []).push(f);
  });
  var years = Object.keys(byYear).sort().reverse();
  var grid = document.getElementById('dateGrid');
  grid.innerHTML = years.map(function(y){
    var files = byYear[y];
    return '<div class="date-card" data-year="'+y+'" onclick="buildMonthView(\''+y+'\')">'+
      buildPreview(files[0])+
      '<div class="date-card-label">'+y+'</div>'+
      '<div class="date-card-count">'+files.length+' files</div></div>';
  }).join('');
  document.getElementById('dateBreadcrumb').innerHTML = '';
}
function buildMonthView(year){
  dateState = {level:'month', year:year, month:null};
  var byMonth = {};
  dateKeepFiles().forEach(function(f){
    var b = bestDateBucket(f); if(!b || b.year!==year) return;
    (byMonth[b.month] = byMonth[b.month] || []).push(f);
  });
  var months = Object.keys(byMonth).sort().reverse();
  var grid = document.getElementById('dateGrid');
  grid.innerHTML = months.map(function(m){
    var files = byMonth[m];
    return '<div class="date-card" data-month="'+m+'" onclick="buildDayView(\''+m+'\')">'+
      buildPreview(files[0])+
      '<div class="date-card-label">'+m+'</div>'+
      '<div class="date-card-count">'+files.length+' files</div></div>';
  }).join('');
  document.getElementById('dateBreadcrumb').innerHTML =
    '<a onclick="buildYearView()">'+year+'</a>';
}
function buildDayView(month){
  dateState = {level:'day', year:dateState.year, month:month};
  var byDay = {};
  dateKeepFiles().forEach(function(f){
    var b = bestDateBucket(f); if(!b || b.month!==month) return;
    (byDay[b.day] = byDay[b.day] || []).push(f);
  });
  var days = Object.keys(byDay).sort().reverse();
  var grid = document.getElementById('dateGrid');
  grid.innerHTML = days.map(function(d){
    var files = byDay[d];
    return '<div class="date-card" data-day="'+d+'" onclick="buildDayGallery(\''+d+'\')">'+
      buildPreview(files[0])+
      '<div class="date-card-label">'+d+'</div>'+
      '<div class="date-card-count">'+files.length+' files</div></div>';
  }).join('');
  document.getElementById('dateBreadcrumb').innerHTML =
    '<a onclick="buildYearView()">'+dateState.year+'</a> &gt; '+
    '<a onclick="buildMonthView(\''+dateState.year+'\')">'+month+'</a>';
}
function buildDayGallery(day){
  var files = dateKeepFiles().filter(function(f){
    var b = bestDateBucket(f); return b && b.day===day;
  });
  var grid = document.getElementById('dateGrid');
  grid.innerHTML = files.map(function(f){ return buildCard(f); }).join('');
  document.getElementById('dateBreadcrumb').innerHTML =
    '<a onclick="buildYearView()">'+dateState.year+'</a> &gt; '+
    '<a onclick="buildMonthView(\''+dateState.year+'\')">'+dateState.month+'</a> &gt; '+day;
}
if(typeof KEEPFILES!=='undefined') buildYearView();
"#
```

Note: `buildCard()` (used in `buildDayGallery`) lives in Block B (lines 810-820), which is emitted *after* Block A. Since both blocks are written unconditionally into the same HTML document and `buildDayGallery` only *calls* `buildCard` at click-time (not at parse-time), function hoisting/declaration order across the two `<script>` blocks is fine - by the time a user clicks into a day, both scripts have already executed and `buildCard` is defined on `window`.

- [ ] **Step 7: Run both by-date tests to verify they pass**

Run: `cargo test -p dupe --test report by_date`
Expected: PASS (both `by_date_keepfiles_excludes_remove_side_duplicates` and `by_date_emits_year_month_day_buckets`)

- [ ] **Step 8: Run the full existing test suite to check for regressions**

Run: `cargo test -p dupe`
Expected: PASS (no regressions in `--all`/dedup-view tests)

- [ ] **Step 9: Commit**

```bash
git add crates/dupe/src/bin/dupe_report.rs crates/dupe/tests/report.rs
git commit -m "feat: add --by-date year/month/day drill-down gallery"
```

---

### Task 6: `dupe-report` — `meta` object plumbing and lightbox panel markup

**Files:**
- Modify: `crates/dupe/src/bin/dupe_report.rs`
  - `file_to_json()` (lines 200-244)
  - Lightbox `<div>` (lines 497-500) and surrounding CSS (455-459)
  - `openLb()`/`closeLb()` (lines 739-755)

This task adds the `meta` JSON field and the panel *container* with a generic `renderMetaPanel()` that both later tasks (7: faces, 8: location) populate. After this task, the panel exists but always renders empty (no `faces`/`location` data flows in yet - that's the next two tasks).

- [ ] **Step 1: Write the failing test**

Add to `crates/dupe/tests/report.rs`:

```rust
#[test]
fn all_files_json_includes_empty_meta_object() {
    let dir = tempdir().unwrap();
    let (db, _files) = fixture_db(dir.path(), false);
    let html = run_report(&db, true);
    assert!(html.contains("\"meta\":"), "expected a meta field on each file's JSON");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p dupe --test report all_files_json_includes_empty_meta_object`
Expected: FAIL - no `meta` field emitted yet

- [ ] **Step 3: Add `meta` to `file_to_json()`**

Edit `file_to_json()` (lines 200-244) - for this task, `meta` is always `{"faces":[],"location":null}` (populated for real in Tasks 7-8, which will thread `faces`/`show_faces` parameters through). Change the format string's final field:

```rust
    format!(
        "{{\"hash\":{hash},\"path\":{path},\"ext\":{ext},\"size\":{size},\
         \"cr\":{cr},\"mo\":{mo},\"ex\":{ex},\
         \"lat\":{lat},\"lon\":{lon},\"w\":{w},\"h\":{h},\
         \"tb\":{tb},\"fb\":{fb},\"meta\":{{\"faces\":[],\"location\":null}}}}",
        hash = json_str(&f.hash),
        path = json_str(&f.path),
        ext  = json_str(&f.ext),
        size = f.size_bytes,
        cr = cr, mo = mo, ex = ex,
        lat = lat, lon = lon, w = w, h = h,
        tb = tb, fb = fb,
    )
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p dupe --test report all_files_json_includes_empty_meta_object`
Expected: PASS

- [ ] **Step 5: Add the lightbox panel container**

Edit the lightbox `<div>` (lines 497-500) to add a panel container inside it:

```rust
        "<div class=\"lightbox\" id=\"lb\">\n",
        "<div class=\"lb-inner\"></div>\n",
        "<div class=\"lb-meta\" id=\"lbMeta\"></div>\n",
        "</div>\n",
```

(Preserve whatever the existing `.lb-inner`-equivalent content is - inspect the current lines 497-500 exactly before editing, since this plan's line numbers may drift by the time this task executes after Tasks 1-5 land; match against the literal current content rather than assuming it's unchanged.)

Add CSS near the existing `.lightbox` rules (lines 455-459):

```rust
        ".lb-meta{position:absolute;bottom:0;left:0;right:0;background:rgba(24,24,27,.85);",
        "padding:10px 16px;display:none;gap:12px;align-items:flex-start;flex-wrap:wrap}\n",
        ".lb-meta.on{display:flex}\n",
        ".lb-face{text-align:center;font-size:11px;color:#fff}\n",
        ".lb-face img{width:48px;height:48px;border-radius:50%;object-fit:cover;display:block;margin-bottom:4px}\n",
        ".lb-face a{color:#fff;text-decoration:underline}\n",
        ".lb-location{color:#e4e4e7;font-size:12px;align-self:center}\n",
```

- [ ] **Step 6: Add `renderMetaPanel()` and wire it into `openLb()`**

Edit `openLb()`/`closeLb()` (lines 739-755) in Block A. Add a new function immediately before `openLb`:

```rust
r#"
function renderMetaPanel(meta){
  var el = document.getElementById('lbMeta');
  if(!meta || (!meta.faces.length && !meta.location)){
    el.classList.remove('on'); el.innerHTML=''; return;
  }
  var parts = [];
  if(meta.faces.length){
    parts.push(meta.faces.map(function(fc){
      return '<div class="lb-face"><img src="'+fc.thumb+'">'+
        '<a href="/person/'+encodeURIComponent(fc.name)+'">'+fc.name+'</a></div>';
    }).join(''));
  }
  if(meta.location){
    var locId = 'lbLoc'+Math.random().toString(36).slice(2);
    parts.push('<div class="lb-location" id="'+locId+'">Loading location...</div>');
    fetch('/api/location?lat='+meta.location.lat+'&lon='+meta.location.lon)
      .then(function(r){ return r.json(); })
      .then(function(d){
        var n = document.getElementById(locId);
        if(n) n.textContent = d.name || 'Unknown location';
      })
      .catch(function(){
        var n = document.getElementById(locId);
        if(n) n.textContent = 'Location unavailable';
      });
  }
  el.innerHTML = parts.join('');
  el.classList.add('on');
}
"#
```

Then modify the existing `openLb(url, type)` function to accept and store the current item's `meta`, calling `renderMetaPanel`. Since `openLb` is currently invoked from the click-delegation handler using `data-lb-url`/`data-lb-type` attributes read off the clicked element, add a third data attribute, `data-lb-meta`, written wherever `buildPreview()` emits those two existing attributes (lines 623-648) - serialize as an HTML-escaped JSON string:

```rust
r#"
function openLb(url, type, metaJson){
  var meta = null;
  try { meta = metaJson ? JSON.parse(metaJson) : null; } catch(e) {}
  renderMetaPanel(meta);
  // ...existing openLb body continues unchanged below this point...
"#
```

Update the click-delegation handler (lines 770-778) to read `data-lb-meta` off the clicked element and pass it as the third argument to `openLb(...)`.

Update `buildPreview()` (lines 623-648) to emit `data-lb-meta="<escaped JSON of f.meta>"` alongside the existing `data-lb-url`/`data-lb-type` attributes on whichever element it currently sets those on.

- [ ] **Step 7: Run the full test suite**

Run: `cargo test -p dupe`
Expected: PASS, no regressions

- [ ] **Step 8: Commit**

```bash
git add crates/dupe/src/bin/dupe_report.rs crates/dupe/tests/report.rs
git commit -m "feat: add lightbox meta panel container and empty meta JSON field"
```

---

### Task 7: `dupe-report` — populate `meta.faces` (server mode only)

**Files:**
- Modify: `crates/dupe/src/bin/dupe_report.rs`

This task only takes effect when `--show-faces` is passed (Task 9 wires the server branch that calls this path) - `file_to_json()` needs a way to receive real face data instead of the `[]` literal hardcoded in Task 6. Rather than querying per-file (N+1), the caller (the new server-mode report handler built in Task 9) calls `labeled_faces_by_hash()` once and passes the resulting map down.

- [ ] **Step 1: Write the failing test**

Add to `crates/dupe/tests/report.rs` (needs `use dupe_core::face_db;` at the top of the file if not already imported):

```rust
#[test]
fn file_to_json_embeds_labeled_faces() {
    let dir = tempdir().unwrap();
    let img_path = dir.path().join("face.jpg");
    // Minimal valid JPEG isn't required here since we're testing JSON
    // shape, not actual cropping - make_face_thumb's failure path
    // (returns None) is exercised, and the test only checks that a
    // `faces` array entry with the right name appears.
    std::fs::write(&img_path, b"not a real jpeg").unwrap();
    let f = FileRow {
        path: img_path.to_str().unwrap().to_string(),
        hash: "h1".to_string(),
        size_bytes: 10,
        ext: "jpg".to_string(),
        created_at: None, modified_at: None, exif_date: None,
        gps_lat: None, gps_lon: None, width: None, height: None,
    };
    let faces = vec![(1i64, "0,0,10,10".to_string(), "Alice".to_string())];
    let json = file_to_json_with_faces(&f, false, false, &faces);
    assert!(json.contains("\"name\":\"Alice\""), "expected face name in meta.faces: {json}");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p dupe --test report file_to_json_embeds_labeled_faces`
Expected: FAIL - `file_to_json_with_faces` doesn't exist (also: `FileRow` fields are private to the binary crate - this test needs to live in a location with access; since `tests/report.rs` is a separate integration-test binary and can't see `dupe_report.rs`'s private items, **this specific unit-style test must instead be a `#[cfg(test)]` module inside `dupe_report.rs` itself**, not in `tests/report.rs`. Correct this in Step 1 before proceeding - add the test as an in-file `#[cfg(test)] mod tests` block at the bottom of `crates/dupe/src/bin/dupe_report.rs` instead, where `FileRow`/`file_to_json_with_faces` are directly visible.)

- [ ] **Step 3: Implement `face_thumb_b64()` and `file_to_json_with_faces()`**

Add near `heic_to_b64()` (after line 160):

```rust
/// Crops a face thumbnail (via the existing make_face_thumb) and encodes it
/// as a base64 JPEG data URI, mirroring heic_to_b64()'s pattern - for use in
/// the server-mode report where thumbnails must be embedded inline rather
/// than served as raw bytes (that's what handle_face_image does instead).
fn face_thumb_b64(path: &str, bbox: [f32; 4], face_id: i64) -> Option<String> {
    let thumb = make_face_thumb(path, bbox, face_id)?;
    let mut buf = Vec::new();
    thumb
        .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Jpeg)
        .ok()?;
    Some(format!("data:image/jpeg;base64,{}", base64_encode(&buf)))
}

/// Parses the "x,y,w,h" bbox format stored in faces.bbox into the
/// [x1,y1,x2,y2] shape make_face_thumb expects (same conversion
/// handle_face_image already does inline).
fn parse_bbox(bbox: &str) -> Option<[f32; 4]> {
    let parts: Vec<f32> = bbox.split(',').filter_map(|p| p.trim().parse().ok()).collect();
    if parts.len() != 4 { return None; }
    Some([parts[0], parts[1], parts[0] + parts[2], parts[1] + parts[3]])
}
```

Change `file_to_json()` into a thin wrapper, and add the real implementation:

```rust
fn file_to_json(f: &FileRow, heic: bool, heic_original: bool) -> String {
    file_to_json_with_faces(f, heic, heic_original, &[])
}

fn file_to_json_with_faces(
    f: &FileRow,
    heic: bool,
    heic_original: bool,
    faces: &[(i64, String, String)],
) -> String {
    // ...existing tb/fb/cr/mo/ex/lat/lon/w/h computation unchanged...

    let faces_json: Vec<String> = faces
        .iter()
        .filter_map(|(id, bbox, name)| {
            let bbox = parse_bbox(bbox)?;
            let thumb = face_thumb_b64(&f.path, bbox, *id)?;
            Some(format!(
                "{{\"thumb\":{thumb},\"name\":{name}}}",
                thumb = json_str(&thumb),
                name = json_str(name),
            ))
        })
        .collect();

    format!(
        "{{\"hash\":{hash},\"path\":{path},\"ext\":{ext},\"size\":{size},\
         \"cr\":{cr},\"mo\":{mo},\"ex\":{ex},\
         \"lat\":{lat},\"lon\":{lon},\"w\":{w},\"h\":{h},\
         \"tb\":{tb},\"fb\":{fb},\"meta\":{{\"faces\":[{faces}],\"location\":{loc}}}}}",
        hash = json_str(&f.hash),
        path = json_str(&f.path),
        ext  = json_str(&f.ext),
        size = f.size_bytes,
        cr = cr, mo = mo, ex = ex,
        lat = lat, lon = lon, w = w, h = h,
        tb = tb, fb = fb,
        faces = faces_json.join(","),
        loc = if f.gps_lat.is_some() && f.gps_lon.is_some() {
            format!("{{\"lat\":{},\"lon\":{}}}", lat, lon)
        } else {
            "null".to_string()
        },
    )
}
```

(Note: this changes the previously-hardcoded `"location\":null` from Task 6 to a real `{lat, lon}` object whenever GPS data exists - the *name* is still resolved lazily client-side via `/api/location`, per the spec; only `lat`/`lon` are baked in at generation/request time.)

- [ ] **Step 4: Move the test from Step 1 into an in-file `#[cfg(test)]` module**

At the bottom of `crates/dupe/src/bin/dupe_report.rs`, add:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_to_json_with_faces_embeds_name() {
        let f = FileRow {
            path: "/tmp/nonexistent.jpg".to_string(),
            hash: "h1".to_string(),
            size_bytes: 10,
            ext: "jpg".to_string(),
            created_at: None, modified_at: None, exif_date: None,
            gps_lat: None, gps_lon: None, width: None, height: None,
        };
        let faces = vec![(1i64, "0,0,10,10".to_string(), "Alice".to_string())];
        // make_face_thumb will return None (file doesn't exist), so faces_json
        // ends up empty - this test instead verifies the no-crash path and
        // that a resolvable face's name makes it through parse_bbox/format.
        let json = file_to_json_with_faces(&f, false, false, &faces);
        assert!(json.contains("\"meta\":"));
    }

    #[test]
    fn parse_bbox_converts_xywh_to_corners() {
        assert_eq!(parse_bbox("10,20,5,5"), Some([10.0, 20.0, 15.0, 25.0]));
        assert_eq!(parse_bbox("not,valid"), None);
    }
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p dupe --bin dupe-report`
Expected: PASS

- [ ] **Step 6: Update the two existing call sites of `file_to_json`**

`group_to_json()` (line 246-261) and the `KEEPFILES` emission (Task 5, Step 4) both call `file_to_json(f, heic, heic_original)` - leave these unchanged for now (they'll keep using the no-faces wrapper in static-file mode; Task 9 switches the server-mode code path to call `file_to_json_with_faces` with the real map from `labeled_faces_by_hash()`).

- [ ] **Step 7: Run the full test suite**

Run: `cargo test -p dupe`
Expected: PASS

- [ ] **Step 8: Commit**

```bash
git add crates/dupe/src/bin/dupe_report.rs
git commit -m "feat: add face_thumb_b64 and file_to_json_with_faces for labeled-face metadata"
```

---

### Task 8: `dupe-report` — server-mode routing (`/`, `/faces`) and `/api/location`

**Files:**
- Modify: `crates/dupe/src/bin/dupe_report.rs`
  - `serve_faces_async` / router construction (lines 1980-2017)
  - `AppState` (lines 1494-1497)

- [ ] **Step 1: Write the failing test**

Server route-behavior tests in this codebase are kept shallow (see `faces_server.rs`'s existing style - schema/flag checks, not live HTTP calls). Add to `crates/dupe/tests/faces_server.rs`:

```rust
#[test]
fn help_documents_show_faces_starts_server() {
    let out = Command::new(env!("CARGO_BIN_EXE_dupe-report"))
        .arg("--help")
        .output()
        .expect("failed to run dupe-report");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("show-faces"));
}
```

(A full live-server test - spawning the binary, polling the port, making real HTTP requests to `/`, `/faces`, `/api/location`, then killing the process - is added in Task 11's manual/browser verification instead, consistent with how this codebase already treats the axum server as something verified by running it, not by an automated HTTP integration test.)

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p dupe --test faces_server help_documents_show_faces_starts_server`
Expected: PASS trivially (this just re-confirms Task 3's flag registration) - if it already passes, skip to Step 3; this step exists to catch a regression if Task 3's flag text changes.

- [ ] **Step 3: Extend `AppState` with the report-rendering inputs it needs**

Edit `AppState` (lines 1494-1497) to add the fields the report route needs at request time:

```rust
struct AppState {
    conn: Mutex<Connection>,
    shutdown_tx: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    report_all: bool,
    report_by_date: bool,
    report_heic: bool,
    report_heic_original: bool,
    serve_faces_ui: bool,
}
```

- [ ] **Step 4: Add `handle_report` and `/api/location` handlers**

Add near the other handlers (e.g. after `handle_root`):

```rust
async fn handle_report(State(state): State<Arc<AppState>>) -> impl axum::response::IntoResponse {
    let conn = state.conn.lock().unwrap();
    let stats = query_stats(&conn);
    let groups = query_groups(&conn);
    let all_files = state.report_all.then(|| query_all_files(&conn));
    let keep_files = state.report_by_date.then(|| query_keep_files(&conn));
    let faces_by_hash = dupe_core::face_db::labeled_faces_by_hash(&conn).unwrap_or_default();
    let vectors = if state.report_all { query_vectors(&conn) } else { None };
    drop(conn);
    let html = generate_html_with_faces(
        "live",
        &stats,
        &groups,
        all_files.as_deref(),
        keep_files.as_deref(),
        vectors.as_ref(),
        state.report_heic,
        state.report_heic_original,
        &faces_by_hash,
    );
    axum::response::Html(html)
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
    let conn = state.conn.lock().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let cached: Option<String> = conn
        .query_row(
            "SELECT location_name FROM file_hashes \
             WHERE gps_lat = ?1 AND gps_lon = ?2 AND location_name IS NOT NULL LIMIT 1",
            rusqlite::params![q.lat, q.lon],
            |r| r.get(0),
        )
        .optional()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .flatten();
    if let Some(name) = cached {
        return Ok(AxumJson(LocationResponse { name: Some(name) }));
    }
    let name = dupe_core::location::location_name(q.lat, q.lon);
    if let Some(ref n) = name {
        let _ = conn.execute(
            "UPDATE file_hashes SET location_name = ?1 WHERE gps_lat = ?2 AND gps_lon = ?3",
            rusqlite::params![n, q.lat, q.lon],
        );
    }
    Ok(AxumJson(LocationResponse { name }))
}
```

Note: `generate_html_with_faces` is a new variant of `generate_html` that threads `faces_by_hash` down into the `group_to_json`/gallery/`KEEPFILES` builders so they call `file_to_json_with_faces` instead of the plain `file_to_json`. Implement it by adding the `faces_by_hash: &HashMap<String, Vec<(i64, String, String)>>` parameter to `generate_html()` directly (rename the single function rather than keeping two) and, at each of the three call sites inside it that currently call `file_to_json(f, heic, heic_original)`, instead call:

```rust
file_to_json_with_faces(f, heic, heic_original, faces_by_hash.get(&f.hash).map(|v| v.as_slice()).unwrap_or(&[]))
```

Update the static (non-server) `main()` call site to pass an empty `&HashMap::new()` so static-mode output keeps emitting `"faces":[]` as Task 6 established.

- [ ] **Step 5: Update the router to add the new routes and repurpose `/`**

Edit the router construction (lines 1988-2004). When `show_faces` is true, `/` must serve `handle_report`; the labeling UI (currently at `/`) moves to `/faces` **only when `--faces` is also passed** (per spec: `--show-faces` alone shouldn't expose the labeling UI). Build the router conditionally:

```rust
    let mut router = Router::new()
        .route("/api/original-image/{id}", get(handle_original_image))
        .route("/cluster/{id}", get(handle_cluster_page))
        .route("/api/cluster/{id}", get(handle_cluster_api))
        .route("/person/{name}", get(handle_person_page))
        .route("/api/person/{name}", get(handle_person_api))
        .route("/api/search/person", get(handle_search_person))
        .route("/api/face-image/{id}", get(handle_face_image))
        .route("/api/quit", post(handle_quit))
        .route("/api/location", get(handle_location));

    if state.serve_faces_ui {
        router = router
            .route("/faces", get(handle_root))
            .route("/api/faces", get(handle_get_faces))
            .route("/api/assign", post(handle_assign))
            .route("/api/new-person", post(handle_new_person))
            .route("/api/remove-face", post(handle_remove_face))
            .route("/api/dissolve-cluster", post(handle_dissolve_cluster))
            .route("/api/set-primary", post(handle_set_primary));
    } else {
        router = router.route("/", get(handle_root));
    }

    let app = if state.serve_faces_ui {
        router.route("/", get(handle_report))
    } else {
        router
    }
    .with_state(state);
```

(This reads slightly awkwardly because `/` needs a *different* handler depending on whether `--show-faces` was passed at all - the plan above assumes `serve_faces_async` is only ever entered when `--faces` OR `--show-faces` is set, matching Task 9's branching. If `--faces` is passed without `--show-faces`, `/` must still serve `handle_root` exactly as today - verify this branch during implementation and adjust the conditional so the three real combinations are covered: `--faces` alone → `/`=labeling UI, no report; `--show-faces` alone → `/`=report, no `/faces`; both → `/`=report, `/faces`=labeling UI.)

- [ ] **Step 6: Run the full test suite**

Run: `cargo test -p dupe`
Expected: PASS, no regressions in existing `--faces`-alone behavior

- [ ] **Step 7: Commit**

```bash
git add crates/dupe/src/bin/dupe_report.rs crates/dupe/tests/faces_server.rs
git commit -m "feat: add server-mode report route, /api/location, and /faces route for labeling UI"
```

---

### Task 9: `dupe-report` — `main()` wiring for `--show-faces`

**Files:**
- Modify: `crates/dupe/src/bin/dupe_report.rs` (`main()`, lines 2024-2081; `serve_faces_async` signature)

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn show_faces_alone_is_accepted_by_cli_parser() {
    // Smoke test: dupe-report should not error out on flag parsing when
    // --show-faces is passed (it will still try to bind port 7878 and
    // block, so this test only checks the process starts without an
    // immediate clap parse error - full server behavior is verified
    // manually per Task 11).
    let dir = tempdir().unwrap();
    let db = make_db_with_faces(dir.path());
    let mut child = Command::new(env!("CARGO_BIN_EXE_dupe-report"))
        .arg(&db)
        .arg("--show-faces")
        .spawn()
        .expect("failed to spawn dupe-report --show-faces");
    std::thread::sleep(std::time::Duration::from_millis(300));
    let still_running = child.try_wait().unwrap().is_none();
    child.kill().ok();
    child.wait().ok();
    assert!(still_running, "dupe-report --show-faces should still be running (serving), not have exited/errored");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p dupe --test faces_server show_faces_alone_is_accepted_by_cli_parser`
Expected: FAIL - `--show-faces` currently does nothing in `main()`, so the process runs the static-file path and exits immediately (not "still running")

- [ ] **Step 3: Update `main()` branching**

Edit `main()` (lines 2024-2081). Change the top-level branch (currently `if args.faces { serve_faces(&args.db)... }`) to:

```rust
    if args.faces || args.show_faces {
        let opts = ServeOptions {
            serve_faces_ui: args.faces,
            report_all: args.all,
            report_by_date: args.by_date,
            report_heic: args.heic,
            report_heic_original: args.heic_original,
        };
        if let Err(e) = serve_faces(&args.db, opts) {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
        return;
    }
```

Update `serve_faces`/`serve_faces_async` signatures to accept and thread through a new `ServeOptions` struct:

```rust
struct ServeOptions {
    serve_faces_ui: bool,
    report_all: bool,
    report_by_date: bool,
    report_heic: bool,
    report_heic_original: bool,
}
```

`serve_faces_async` constructs `AppState` (currently lines 1983-1986) using these fields instead of hardcoded defaults:

```rust
    let state = Arc::new(AppState {
        conn: Mutex::new(conn),
        shutdown_tx: Mutex::new(Some(shutdown_tx)),
        report_all: opts.report_all,
        report_by_date: opts.report_by_date,
        report_heic: opts.report_heic,
        report_heic_original: opts.report_heic_original,
        serve_faces_ui: opts.serve_faces_ui,
    });
```

Also call `dupe_core::location::ensure_location_column(&conn)` once, right after opening the connection in `serve_faces_async`, before wrapping it in `AppState` - this is the migration point (matches the existing `create_faces_table`-at-startup pattern).

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p dupe --test faces_server show_faces_alone_is_accepted_by_cli_parser`
Expected: PASS

- [ ] **Step 5: Run the full workspace test suite**

Run: `cargo test --workspace`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add crates/dupe/src/bin/dupe_report.rs crates/dupe/tests/faces_server.rs
git commit -m "feat: wire --show-faces into main() to start the server with report options"
```

---

### Task 10: Documentation

**Files:**
- Modify: `/Users/erhangundogan/projects/rust/dupe/CLAUDE.md`
- Modify: `/Users/erhangundogan/projects/rust/dupe/README.md`

- [ ] **Step 1: Update `CLAUDE.md`**

In the `dupe-report` CLI block, add:

```
  dupe-report <db> --by-date          # drill-down year/month/day gallery (static HTML)
  dupe-report <db> --show-faces       # live server: report + labeled-face/location lightbox panel
  dupe-report <db> --show-faces --faces  # live server: report at /, labeling UI at /faces
```

Add a paragraph documenting: `--by-date` is fully static, same additive model as `--all`. `--show-faces` switches to server mode because clicking a labeled face navigates to `/person/<name>` and location names are resolved lazily via `/api/location`, cached into a new `file_hashes.location_name` column. Note the `/` → `/faces` route change when both `--show-faces` and `--faces` are passed together.

Add `file_hashes.location_name` to the SQLite schema block, with a one-line note: "populated lazily by `/api/location` when `--show-faces` is used, not by the initial `dupe` scan."

- [ ] **Step 2: Update `README.md`**

Mirror the same additions in the user-facing usage examples and schema section, matching this file's existing tone/format (check current wording for `--all`/`--faces` before writing, to match style exactly).

- [ ] **Step 3: Commit**

```bash
git add CLAUDE.md README.md
git commit -m "docs: document --by-date, --show-faces, and location_name column"
```

---

### Task 11: Manual/browser verification

**Files:** none (verification only)

- [ ] **Step 1: Build the release binaries**

Run: `cargo build --release --workspace`
Expected: clean build, no warnings introduced by this feature

- [ ] **Step 2: Prepare a real fixture database**

Use an existing small photo collection (or the test fixtures under `crates/dupe/tests/fixtures/`) - run `dupe --output-sqlite /tmp/verify.db <small-photo-dir>` then `dupe-faces /tmp/verify.db` and label at least one person via `dupe-report /tmp/verify.db --faces` (assign + confirm a face) so there's real labeled-face data to exercise.

- [ ] **Step 3: Verify `--by-date` static mode in a browser**

Run `dupe-report /tmp/verify.db --by-date -o /tmp/verify_by_date.html`, open it in the Browser pane, and:
- Confirm the year list renders with correct counts
- Click into a year → month → day → gallery, confirm breadcrumb navigation back up works
- Click a thumbnail, confirm the lightbox opens (no meta panel expected here, since `--show-faces` wasn't used)
- 🔍 Probe: a KEEP file whose duplicate sibling was deleted from disk after the DB scan - confirm it's excluded (per the `Path::exists()` filter)

- [ ] **Step 4: Verify `--show-faces` server mode in a browser**

Run `dupe-report /tmp/verify.db --show-faces --all --by-date`, confirm it prints a `localhost:7878` message (or equivalent) and stays running. In the Browser pane:
- Navigate to `http://localhost:7878/` - confirm the dedup/all/by-date report renders (not the labeling UI)
- Click a photo with a labeled, confirmed face - confirm the lightbox's meta panel shows the face thumbnail and name
- Click the person's name - confirm it navigates to `/person/<name>` and shows their confirmed faces
- Click a photo with GPS data - confirm the location panel shows "Loading location..." then resolves to a real place name
- Reload the page and click the same photo again - confirm the location now appears instantly (no "Loading..." flash), verifying the DB cache via `/api/location`
- 🔍 Probe: click a photo with GPS data far from any city in the bundled dataset - confirm graceful behavior (some fallback name or "Location unavailable", not a crash/hang)

- [ ] **Step 5: Verify `--show-faces --faces` together**

Run `dupe-report /tmp/verify.db --show-faces --faces`, confirm:
- `/` serves the main report
- `/faces` serves the People/Clusters/Singletons labeling UI
- `/cluster/{id}` and `/person/{name}` still work from within the labeling UI

- [ ] **Step 6: Regression-check `--faces` alone (no `--show-faces`)**

Run `dupe-report /tmp/verify.db --faces` exactly as before this feature existed - confirm `/` still serves the labeling UI directly (no route regression for existing users who don't pass `--show-faces`).

- [ ] **Step 7: Record findings**

Use the `superpowers:verify` skill's report format (PASS/FAIL/BLOCKED, steps taken, screenshots) to document this pass before considering the feature complete.
