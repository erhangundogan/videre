//! The gallery renderer and its shared data layer.
//!
//! One rendering pipeline (`render` over a [`RenderSet`]) serves both the
//! static `--html` exports and the live `gallery` page routes; the `live`
//! flag is the only difference. This is the lower layer: `commands::dedupe`,
//! `commands::search` and `commands::gallery` depend on it, never the reverse.

use rusqlite::Connection;
use std::collections::HashMap;
use std::path::Path;

#[derive(Clone)]
pub(crate) struct FileRow {
    pub(crate) path: String,
    pub(crate) hash: String,
    pub(crate) size_bytes: i64,
    pub(crate) ext: String,
    pub(crate) created_at: Option<String>,
    pub(crate) modified_at: Option<String>,
    pub(crate) exif_date: Option<String>,
    pub(crate) gps_lat: Option<f64>,
    pub(crate) gps_lon: Option<f64>,
    pub(crate) width: Option<i32>,
    pub(crate) height: Option<i32>,
}

pub(crate) struct Stats {
    total_files: i64,
    duplicate_groups: i64,
}

pub(crate) enum FileDateFilter {
    Prefix(String),
    Range {
        from: Option<String>,
        to: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct LocationFilter {
    pub(crate) lat: f64,
    pub(crate) lon: f64,
    pub(crate) radius_km: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum LongitudeBounds {
    Between { west: f64, east: f64 },
    Wrapped { west: f64, east: f64 },
    All,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct LocationBounds {
    south: f64,
    north: f64,
    longitude: LongitudeBounds,
}

fn normalize_longitude(lon: f64) -> f64 {
    (lon + 180.0).rem_euclid(360.0) - 180.0
}

fn location_bounds(location: LocationFilter) -> LocationBounds {
    const EARTH_RADIUS_KM: f64 = 6371.0;
    let angular = location.radius_km / EARTH_RADIUS_KM;
    let latitude_delta = angular.to_degrees();
    let south = (location.lat - latitude_delta).max(-90.0);
    let north = (location.lat + latitude_delta).min(90.0);

    let longitude = if south <= -90.0 || north >= 90.0 || angular >= std::f64::consts::PI {
        LongitudeBounds::All
    } else {
        let ratio = angular.sin() / location.lat.to_radians().cos();
        if ratio.abs() >= 1.0 {
            LongitudeBounds::All
        } else {
            let delta = ratio.asin().abs().to_degrees();
            let west = normalize_longitude(location.lon - delta);
            let east = normalize_longitude(location.lon + delta);
            if west <= east {
                LongitudeBounds::Between { west, east }
            } else {
                LongitudeBounds::Wrapped { west, east }
            }
        }
    };

    LocationBounds {
        south,
        north,
        longitude,
    }
}

/// How many files in this library are embedded under `model_id`, for the header
/// stat. `None` when nothing is.
///
/// :warning: This replaced a function that loaded **every vector** to answer the
/// same question. That existed because the page did in-browser similarity and
/// needed the vectors anyway; the server ranks now, so a page needs the number
/// and nothing else. A large library used to lose this stat entirely, because
/// the loader gave up above a size cap and the header then showed nothing.
pub(crate) fn query_embedded_count(conn: &Connection, model_id: &str) -> Option<usize> {
    let table_exists = conn
        .query_row(
            "SELECT COUNT(*) FROM emb.sqlite_master WHERE type='table' AND name='embeddings'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .map(|n| n > 0)
        .unwrap_or(false);
    if !table_exists {
        return None;
    }
    conn.query_row(
        "SELECT COUNT(*) FROM emb.embeddings WHERE model_id = ?1 \
         AND hash IN (SELECT hash FROM file_hashes)",
        [model_id],
        |r| r.get::<_, i64>(0),
    )
    .ok()
    .filter(|n| *n > 0)
    .map(|n| n as usize)
}

/// Which field `/api/files` sorts by. Unknown query values fall back to the
/// default rather than erroring: an unknown sort is a client bug, and a 400
/// here would render as an empty gallery with no explanation, the same rule
/// `view` follows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FileSortField {
    Date,
    Name,
    Size,
    Rating,
    Liked,
    Type,
}

impl FileSortField {
    fn from_query(value: Option<&str>) -> Self {
        match value {
            Some("name") => Self::Name,
            Some("size") => Self::Size,
            Some("rating") => Self::Rating,
            Some("liked") => Self::Liked,
            Some("type") => Self::Type,
            _ => Self::Date,
        }
    }

    /// Rating and Liked read the marks table; the other four keep today's
    /// single-table query plan.
    fn needs_marks(self) -> bool {
        matches!(self, Self::Rating | Self::Liked)
    }
}

/// The field and direction `/api/files` orders by. The default is the
/// gallery's own default: effective date, newest first.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct FileSort {
    pub field: FileSortField,
    pub desc: bool,
}

impl Default for FileSort {
    fn default() -> Self {
        Self {
            field: FileSortField::Date,
            desc: true,
        }
    }
}

impl FileSort {
    /// `dir` anything but `asc` means `desc`, so a missing or unknown value
    /// lands on the default direction.
    pub(crate) fn from_query(sort: Option<&str>, dir: Option<&str>) -> Self {
        Self {
            field: FileSortField::from_query(sort),
            desc: dir != Some("asc"),
        }
    }

    /// The full ORDER BY body, ending in the `path` tie-break. `path` is the
    /// table's primary key, so the order is total, which is what keeps
    /// LIMIT/OFFSET pages disjoint. Nulls are forced last in both directions
    /// with the `(expr IS NULL)` prefix: SQLite's default puts them first
    /// ascending, which would read as undated files on top of "newest first".
    fn order_by(self) -> String {
        let dir = if self.desc { "DESC" } else { "ASC" };
        let body = match self.field {
            FileSortField::Date => {
                format!("({FILE_EFFECTIVE_DATE} IS NULL), {FILE_EFFECTIVE_DATE} {dir}")
            }
            FileSortField::Name => format!("{FILE_BASENAME} COLLATE NOCASE {dir}"),
            FileSortField::Size => format!("(f.size_bytes IS NULL), f.size_bytes {dir}"),
            FileSortField::Rating => format!("(m.rating IS NULL), m.rating {dir}"),
            // Liked keeps the effective date, newest first, as its second key
            // in both directions, so liked files surface in shooting order.
            FileSortField::Liked => {
                format!("(COALESCE(m.liked, 0)) {dir}, {FILE_EFFECTIVE_DATE} DESC")
            }
            // NULL mime means unknown, which reads as a photo.
            FileSortField::Type => format!("(COALESCE(f.mime LIKE 'video/%', 0)) {dir}"),
        };
        format!("{body}, f.path")
    }
}

/// `EFFECTIVE_DATE_SQL` spelled against the `f` alias of the `/api/files`
/// outer query, whose FROM is a flat `file_hashes` or the keep-set subquery;
/// both expose the bare columns. The shared constant is unqualified and is
/// still used inside the keep set and by the unqualified WHERE clauses.
const FILE_EFFECTIVE_DATE: &str = "CASE WHEN f.exif_date IS NOT NULL \
     AND f.exif_date NOT LIKE '0000%' THEN f.exif_date ELSE f.modified_at END";

/// The basename of `f.path`. `rtrim(path, replace(path, '/', ''))` strips
/// every trailing non-slash character (they are all in the set), leaving the
/// directory part, so the `substr` starts after it. The `CASE` covers
/// root-level paths with no slash. Always used with `COLLATE NOCASE`
/// (ASCII folding only; ICU is not bundled).
const FILE_BASENAME: &str = "CASE WHEN instr(f.path, '/') = 0 THEN f.path ELSE \
     substr(f.path, length(rtrim(f.path, replace(f.path, '/', ''))) + 1) END";

/// One page of the gallery's file list, for `/api/files`.
///
/// Returns each row with its copy count, plus the total across the whole view
/// so the client can say "Show more (N remaining)" without holding the rest.
///
/// :warning: **Paged in SQL, so it does not filter by `Path::exists()`.**
/// `query_all_files` filters in Rust after the query, which cannot be paged: a
/// page of 200 would yield fewer than 200 rows, raggedly, and `total` would
/// disagree with what the pages actually contain.
///
/// Not filtering also matches `query_groups`, which never filtered, and matches
/// what the docs now say: the database is the source of truth until
/// `videre prune` removes a row. The consequence to handle when this is wired
/// up is that an unplugged drive shows files rather than an empty grid, which
/// needs saying in the page rather than being left to look broken.
pub(crate) fn query_files_page(
    conn: &Connection,
    view: &str,
    date_filter: Option<&FileDateFilter>,
    offset: i64,
    limit: i64,
    location: Option<&LocationFilter>,
    sort: FileSort,
) -> anyhow::Result<(Vec<(FileRow, i64)>, i64)> {
    // `view=date` shows one row per hash, the same KEEP set `/date` renders.
    // Choosing it in SQL rather than in Rust is what makes it pageable.
    let (from, total_from) = if view == "date" {
        // :warning: Counting `SELECT DISTINCT hash` was wrong once a date filter
        // arrived: that subquery exposes only `hash`, so the date expression had
        // no columns to read and the count silently came back 0 while the page
        // returned rows. Counting the keep set itself keeps the columns in scope
        // and keeps the count and the rows describing the same thing.
        (keep_set_sql(), keep_set_sql())
    } else {
        ("file_hashes".to_string(), "file_hashes".to_string())
    };

    // A date prefix narrows both the count and the page, so "Show more" counts
    // what is actually in the day rather than in the library.
    let effective = videre_core::query::EFFECTIVE_DATE_SQL;
    let mut clauses = Vec::new();
    let mut params = Vec::<rusqlite::types::Value>::new();
    match date_filter {
        Some(FileDateFilter::Prefix(d)) if !d.is_empty() => {
            clauses.push(format!("substr({effective}, 1, {}) = ?", d.len()));
            params.push(d.clone().into());
        }
        Some(FileDateFilter::Range { from, to }) => {
            if let Some(from) = from {
                clauses.push(format!("{effective} >= ?"));
                params.push(from.clone().into());
            }
            if let Some(to) = to {
                clauses.push(format!("{effective} < ?"));
                params.push(to.clone().into());
            }
        }
        _ => {}
    }
    if view != "date" {
        if let Some(location) = location {
            let bounds = location_bounds(*location);
            clauses.push("gps_lat BETWEEN ? AND ?".to_string());
            params.push(bounds.south.into());
            params.push(bounds.north.into());
            match bounds.longitude {
                LongitudeBounds::Between { west, east } => {
                    clauses.push("gps_lon BETWEEN ? AND ?".to_string());
                    params.push(west.into());
                    params.push(east.into());
                }
                LongitudeBounds::Wrapped { west, east } => {
                    clauses.push("(gps_lon >= ? OR gps_lon <= ?)".to_string());
                    params.push(west.into());
                    params.push(east.into());
                }
                LongitudeBounds::All => {}
            }
            clauses.push("haversine_km(gps_lat, gps_lon, ?, ?) <= ?".to_string());
            params.push(location.lat.into());
            params.push(location.lon.into());
            params.push(location.radius_km.into());
        }
    }
    let where_sql = if clauses.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", clauses.join(" AND "))
    };

    let count_sql = format!("SELECT COUNT(*) FROM {total_from} AS t{}", where_sql);
    let total: i64 = conn
        .query_row(&count_sql, rusqlite::params_from_iter(params.iter()), |r| {
            r.get(0)
        })
        .map_err(|e| {
            anyhow::Error::new(e).context(format!("/api/files count failed\n  {count_sql}"))
        })?;

    // `copies` is how many files share this hash. The client derived it by
    // scanning the whole array, which is the only reason it needed the whole
    // array. The database knew it all along.
    //
    // The join exists only for the sorts that read marks, so the other five
    // keep today's query plan. Every selected column and the WHERE clauses
    // above stay resolvable with the join present: marks shares `hash`, so
    // the selected columns are qualified with `f`.
    let join_marks = if sort.field.needs_marks() {
        " LEFT JOIN marks AS m ON m.hash = f.hash"
    } else {
        ""
    };
    let sql = format!(
        "SELECT f.path, f.hash, f.size_bytes, COALESCE(f.ext,''), f.created_at, f.modified_at, \
                f.exif_date, f.gps_lat, f.gps_lon, f.width, f.height, \
                (SELECT COUNT(*) FROM file_hashes c WHERE c.hash = f.hash) AS copies \
         FROM {from} AS f{join_marks}{where_sql} ORDER BY {} LIMIT ? OFFSET ?",
        sort.order_by()
    );
    // :warning: A failed query must not read as an empty page. Returning
    // `(vec![], total)` here once hid malformed SQL behind a gallery that looked
    // like a library with nothing in it, which is the same shape as every other
    // fault in this file's history: nothing errored, so nothing was reported.
    // The error carries the SQL; the handler's boundary logs it, once.
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| anyhow::Error::new(e).context(format!("/api/files query failed\n  {sql}")))?;
    let mut bound = params.clone();
    bound.push(limit.into());
    bound.push(offset.into());
    let rows = stmt
        .query_map(rusqlite::params_from_iter(bound.iter()), |r| {
            Ok((
                FileRow {
                    path: r.get(0)?,
                    hash: r.get(1)?,
                    size_bytes: r.get(2)?,
                    ext: r.get(3)?,
                    created_at: r.get(4)?,
                    modified_at: r.get(5)?,
                    exif_date: r.get(6)?,
                    gps_lat: r.get(7)?,
                    gps_lon: r.get(8)?,
                    width: r.get(9)?,
                    height: r.get(10)?,
                },
                r.get::<_, i64>(11)?,
            ))
        })
        .map(|it| it.filter_map(|r| r.ok()).collect::<Vec<_>>())
        .unwrap_or_default();

    Ok((rows, total))
}

/// Rows for a specific set of hashes, for similarity results.
///
/// Capped for the same reason `limit` is: this must not become a way to ask for
/// the library one comma at a time.
/// The KEEP set: one row per hash, the earliest by effective date.
///
/// Extracted because `/api/files?view=date` and `/api/dates` must agree about
/// which row represents a group. Two definitions would drift, and the symptom
/// would be a bucket count that disagrees with the files inside it.
pub(crate) fn keep_set_sql() -> String {
    format!(
        "(SELECT * FROM (SELECT *, ROW_NUMBER() OVER \
          (PARTITION BY hash ORDER BY {}, path) AS rn FROM file_hashes) WHERE rn = 1)",
        videre_core::query::EFFECTIVE_DATE_SQL
    )
}

pub(crate) fn query_files_by_hash(
    conn: &Connection,
    csv: &str,
) -> rusqlite::Result<(Vec<(FileRow, i64)>, i64)> {
    const MAX_HASHES: usize = 100;
    let wanted: Vec<String> = csv
        .split(',')
        .map(|h| h.trim())
        .filter(|h| !h.is_empty() && h.chars().all(|c| c.is_ascii_hexdigit()))
        .take(MAX_HASHES)
        .map(|h| h.to_string())
        .collect();
    if wanted.is_empty() {
        return Ok((Vec::new(), 0));
    }
    let placeholders = std::iter::repeat_n("?", wanted.len())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT path, hash, size_bytes, COALESCE(ext,''), created_at, modified_at, exif_date, \
                gps_lat, gps_lon, width, height, \
                (SELECT COUNT(*) FROM file_hashes c WHERE c.hash = f.hash) AS copies \
         FROM file_hashes AS f WHERE f.hash IN ({placeholders}) ORDER BY f.path"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(rusqlite::params_from_iter(wanted.iter()), |r| {
            Ok((
                FileRow {
                    path: r.get(0)?,
                    hash: r.get(1)?,
                    size_bytes: r.get(2)?,
                    ext: r.get(3)?,
                    created_at: r.get(4)?,
                    modified_at: r.get(5)?,
                    exif_date: r.get(6)?,
                    gps_lat: r.get(7)?,
                    gps_lon: r.get(8)?,
                    width: r.get(9)?,
                    height: r.get(10)?,
                },
                r.get::<_, i64>(11)?,
            ))
        })?
        .filter_map(|r| r.ok())
        .collect::<Vec<_>>();
    let n = rows.len() as i64;
    Ok((rows, n))
}

/// Fetch every row named in `hashes`, in the given order, with no cap. Unlike
/// `query_files_by_hash` (bounded at 100 for the in-page similarity feature),
/// this backs the `/events` leaf, which must render a whole trip however
/// large. Rows are returned in `hashes` order so an event stays chronological.
pub(crate) fn query_event_files(
    conn: &Connection,
    hashes: &[String],
) -> rusqlite::Result<(Vec<(FileRow, i64)>, i64)> {
    let wanted: Vec<&str> = hashes
        .iter()
        .map(|h| h.trim())
        .filter(|h| !h.is_empty() && h.chars().all(|c| c.is_ascii_hexdigit()))
        .collect();
    if wanted.is_empty() {
        return Ok((Vec::new(), 0));
    }
    // Modern SQLite allows 32,766 variables by default, but older builds
    // allow only 999. A trip can easily exceed either limit, so keep every
    // lookup below the older bound without truncating exact membership.
    const BATCH: usize = 900;
    let mut by_hash = std::collections::HashMap::<String, (FileRow, i64)>::new();
    for batch in wanted.chunks(BATCH) {
        let placeholders = std::iter::repeat_n("?", batch.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT path, hash, size_bytes, COALESCE(ext,''), created_at, modified_at, exif_date, \
                    gps_lat, gps_lon, width, height, \
                    (SELECT COUNT(*) FROM file_hashes c WHERE c.hash = f.hash) AS copies \
             FROM file_hashes AS f WHERE f.hash IN ({placeholders})"
        );
        let mut stmt = conn.prepare(&sql)?;
        let entries = stmt
            .query_map(rusqlite::params_from_iter(batch.iter()), |r| {
                Ok((
                    FileRow {
                        path: r.get(0)?,
                        hash: r.get(1)?,
                        size_bytes: r.get(2)?,
                        ext: r.get(3)?,
                        created_at: r.get(4)?,
                        modified_at: r.get(5)?,
                        exif_date: r.get(6)?,
                        gps_lat: r.get(7)?,
                        gps_lon: r.get(8)?,
                        width: r.get(9)?,
                        height: r.get(10)?,
                    },
                    r.get::<_, i64>(11)?,
                ))
            })?
            .map(|row| row.map(|entry| (entry.0.hash.clone(), entry)))
            .collect::<rusqlite::Result<Vec<_>>>()?;
        by_hash.extend(entries);
    }
    // Preserve the caller's (chronological) order; a hash present more than
    // once in the library still resolves to a single row here.
    let mut rows = Vec::with_capacity(wanted.len());
    for hash in &wanted {
        if let Some(entry) = by_hash.remove(*hash) {
            rows.push(entry);
        }
    }
    let n = rows.len() as i64;
    Ok((rows, n))
}

fn best_date(r: &FileRow) -> &str {
    if let Some(d) = r.exif_date.as_deref() {
        if !d.starts_with("0000") {
            return d;
        }
    }
    match (r.created_at.as_deref(), r.modified_at.as_deref()) {
        (Some(c), Some(m)) => {
            if c < m {
                c
            } else {
                m
            }
        }
        (Some(c), None) => c,
        (None, Some(m)) => m,
        (None, None) => "",
    }
}

fn base64_encode(data: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(CHARS[((n >> 18) & 63) as usize] as char);
        out.push(CHARS[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            CHARS[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            CHARS[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// Convert a HEIC file to a base64 JPEG data-URI payload, downscaled so
/// neither dimension exceeds `max_px`.
///
/// Uses QuickLook (see `videre_core::heic::heic_via_quicklook`) rather than
/// `sips -s format jpeg` because `sips` copies the raw sensor-buffer pixels
/// unrotated for HEIC files where the iPhone camera encoded rotation via the
/// HEIF `irot` transform box rather than a classic EXIF Orientation tag.
fn heic_to_b64(path: &str, max_px: u32) -> Option<String> {
    // Some(max_px): the caller already downscales to max_px below, so
    // requesting a decode already capped at that size avoids wasted
    // decode/resize/PNG-encode work. See the safety note on
    // heic_via_quicklook for why this is only safe when the result is
    // downscaled by the caller anyway.
    let img = videre_core::heic::heic_via_quicklook(path, &format!("b64_{max_px}"), Some(max_px))?;
    let img = if img.width() > max_px || img.height() > max_px {
        img.resize(max_px, max_px, image::imageops::FilterType::Triangle)
    } else {
        img
    };
    let mut buf = Vec::new();
    img.write_to(
        &mut std::io::Cursor::new(&mut buf),
        image::ImageFormat::Jpeg,
    )
    .ok()?;
    Some(base64_encode(&buf))
}

/// Crops a face thumbnail (via videre_api::make_face_thumb) and encodes it
/// as a base64 JPEG data URI, mirroring heic_to_b64()'s pattern, for use in
/// the static export where thumbnails must be embedded inline rather than
/// served as raw bytes (that's what handle_face_image does instead).
fn face_thumb_b64(path: &str, bbox: [f32; 4], face_id: i64) -> Option<String> {
    let thumb = videre_api::make_face_thumb(path, bbox, face_id)?;
    let mut buf = Vec::new();
    thumb
        .write_to(
            &mut std::io::Cursor::new(&mut buf),
            image::ImageFormat::Jpeg,
        )
        .ok()?;
    Some(format!("data:image/jpeg;base64,{}", base64_encode(&buf)))
}

/// Parses the "x,y,w,h" bbox format stored in faces.bbox into the
/// [x1,y1,x2,y2] shape make_face_thumb expects (same conversion
/// handle_face_image already does inline).
fn parse_bbox(bbox: &str) -> Option<[f32; 4]> {
    let parts: Vec<f32> = bbox
        .split(',')
        .filter_map(|p| p.trim().parse().ok())
        .collect();
    if parts.len() != 4 {
        return None;
    }
    Some([parts[0], parts[1], parts[0] + parts[2], parts[1] + parts[3]])
}

pub(crate) fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Page chrome, shared by the gallery templates and the labeling page so the
/// two cannot drift into looking like different products.
pub(crate) const CHROME_CSS: &str = include_str!("../../static/chrome.css");

/// The file-grid script, with the shared multi-select component ahead of it.
pub(crate) const GALLERY_JS: &str = concat!(
    include_str!("../../static/selection.js"),
    "\n",
    include_str!("../../static/gallery.js")
);

/// A hand-built JSON body as a response.
///
/// These endpoints assemble JSON as a string rather than serialising a struct,
/// because the row shape is shared with the inlined static export and is built
/// by one function either way. This is the third endpoint to need the same two
/// lines, so it is a function.
pub(crate) fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '<' => out.push_str("\\u003c"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Renders one file's row to a JSON object, embedding labeled-face thumbnails
/// into meta.faces. `faces` is the (face_id, person_label, bbox) list for this
/// file's hash, as returned by videre_core::face_db::labeled_faces_by_hash()
/// (note the tuple order: label is `.1`, bbox is `.2`).
/// The mark fields for a file object, as a leading-comma JSON fragment
/// (`,"rating":..,"pick":..,"label":..,"liked":..`). An unmarked photo reads as
/// nulls and `liked:false`, so every file has the same stable shape. Shared by
/// the live `/api/files` response and the static export's inlined rows, whose
/// offline sorts read the same fields.
pub(crate) fn mark_fields_json(m: Option<&videre_core::marks::Marks>) -> String {
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

pub(crate) fn file_to_json_with_faces(
    f: &FileRow,
    heic: bool,
    heic_original: bool,
    faces: &[videre_core::face_db::LabeledFace],
    live: bool,
) -> String {
    let (tb, fb) = if f.ext == "heic" && heic {
        let thumb = heic_to_b64(&f.path, 240)
            .map(|b| json_str(&b))
            .unwrap_or_else(|| "null".to_string());
        let full = if heic_original {
            heic_to_b64(&f.path, 1200)
                .map(|b| json_str(&b))
                .unwrap_or_else(|| "null".to_string())
        } else {
            "null".to_string()
        };
        (thumb, full)
    } else {
        ("null".to_string(), "null".to_string())
    };

    let cr = f
        .created_at
        .as_deref()
        .map(|d| json_str(&d[..d.len().min(19)]))
        .unwrap_or_else(|| "null".to_string());
    let mo = f
        .modified_at
        .as_deref()
        .map(|d| json_str(&d[..d.len().min(19)]))
        .unwrap_or_else(|| "null".to_string());
    let ex = f
        .exif_date
        .as_deref()
        .map(json_str)
        .unwrap_or_else(|| "null".to_string());
    let lat = f
        .gps_lat
        .map(|v| format!("{:.6}", v))
        .unwrap_or_else(|| "null".to_string());
    let lon = f
        .gps_lon
        .map(|v| format!("{:.6}", v))
        .unwrap_or_else(|| "null".to_string());
    let w = f
        .width
        .map(|v| v.to_string())
        .unwrap_or_else(|| "null".to_string());
    let h = f
        .height
        .map(|v| v.to_string())
        .unwrap_or_else(|| "null".to_string());

    // :warning: A live page emits a face id and lets the browser fetch the crop
    // from `/api/faces/{id}/image` when a lightbox opens. Inlining crops instead
    // means **decoding every original in full** to cut out one face, for every
    // labelled face in the library, on every single request.
    //
    // Measured on a real library: 23,217 labelled faces across 16,572 files, so
    // one request meant 16,572 full-resolution JPEG decodes off an external
    // drive. It never returned, and nothing reported an error, because nothing
    // failed. It was still working.
    //
    // A static export has no server to ask, so it keeps the inline crops. That
    // is the whole reason `live` is threaded down here, and the cost is
    // acceptable there because an exported page is built once, not per view.
    let faces_json: Vec<String> = faces
        .iter()
        .filter_map(|(id, name, bbox)| {
            if live {
                return Some(format!(
                    "{{\"id\":{id},\"name\":{name}}}",
                    name = json_str(name),
                ));
            }
            let bbox = parse_bbox(bbox)?;
            let thumb = face_thumb_b64(&f.path, bbox, *id)?;
            Some(format!(
                "{{\"thumb\":{thumb},\"name\":{name}}}",
                thumb = json_str(&thumb),
                name = json_str(name),
            ))
        })
        .collect();

    let loc = if f.gps_lat.is_some() && f.gps_lon.is_some() {
        format!("{{\"lat\":{},\"lon\":{}}}", lat, lon)
    } else {
        "null".to_string()
    };

    format!(
        "{{\"hash\":{hash},\"path\":{path},\"ext\":{ext},\"size\":{size},\
         \"cr\":{cr},\"mo\":{mo},\"ex\":{ex},\
         \"lat\":{lat},\"lon\":{lon},\"w\":{w},\"h\":{h},\
         \"tb\":{tb},\"fb\":{fb},\"meta\":{{\"faces\":[{faces}],\"location\":{loc}}}}}",
        hash = json_str(&f.hash),
        path = json_str(&f.path),
        ext = json_str(&f.ext),
        size = f.size_bytes,
        cr = cr,
        mo = mo,
        ex = ex,
        lat = lat,
        lon = lon,
        w = w,
        h = h,
        tb = tb,
        fb = fb,
        faces = faces_json.join(","),
        loc = loc,
    )
}

fn group_to_json(
    group: &[FileRow],
    heic: bool,
    heic_original: bool,
    faces_by_hash: &videre_core::face_db::LabeledFacesByHash,
    live: bool,
) -> String {
    let hash_prefix = &group[0].hash[..group[0].hash.len().min(8)];
    let waste = group[0].size_bytes * (group.len() as i64 - 1);
    let keep_date = best_date(&group[0]);
    let date_json = if keep_date.is_empty() {
        "null".to_string()
    } else {
        json_str(keep_date)
    };
    let files_json: Vec<String> = group
        .iter()
        .map(|f| {
            file_to_json_with_faces(
                f,
                heic,
                heic_original,
                faces_by_hash
                    .get(&f.hash)
                    .map(|v| v.as_slice())
                    .unwrap_or(&[]),
                live,
            )
        })
        .collect();
    format!(
        "{{\"hash\":{hash},\"waste\":{waste},\"date\":{date},\"files\":[{files}]}}",
        hash = json_str(hash_prefix),
        waste = waste,
        date = date_json,
        files = files_json.join(","),
    )
}

pub(crate) fn query_stats(conn: &Connection) -> Stats {
    let s = videre_core::library_stats::compute(conn).unwrap_or_default();
    Stats {
        total_files: s.total_files,
        duplicate_groups: s.duplicate_group_count,
    }
}

pub(crate) fn query_groups(conn: &Connection) -> Vec<Vec<FileRow>> {
    let mut stmt = conn
        .prepare(
            "SELECT path, hash, size_bytes, COALESCE(ext,''), created_at, modified_at, exif_date, \
                    gps_lat, gps_lon, width, height \
             FROM file_hashes \
             WHERE hash IN \
               (SELECT hash FROM file_hashes GROUP BY hash HAVING COUNT(*) > 1) \
             ORDER BY hash",
        )
        .expect("failed to prepare query");

    let rows: Vec<FileRow> = stmt
        .query_map([], |r| {
            Ok(FileRow {
                path: r.get(0)?,
                hash: r.get(1)?,
                size_bytes: r.get(2)?,
                ext: r.get(3)?,
                created_at: r.get(4)?,
                modified_at: r.get(5)?,
                exif_date: r.get(6)?,
                gps_lat: r.get(7)?,
                gps_lon: r.get(8)?,
                width: r.get(9)?,
                height: r.get(10)?,
            })
        })
        .expect("failed to execute query")
        .filter_map(|r| r.ok())
        .collect();

    let mut map: HashMap<String, Vec<FileRow>> = HashMap::new();
    for row in rows {
        map.entry(row.hash.clone()).or_default().push(row);
    }

    let mut groups: Vec<Vec<FileRow>> = map.into_values().collect();

    for group in &mut groups {
        group.sort_by(|a, b| best_date(a).cmp(best_date(b)));
    }
    groups.sort_by(|a, b| {
        let wa = a[0].size_bytes * (a.len() as i64 - 1);
        let wb = b[0].size_bytes * (b.len() as i64 - 1);
        wb.cmp(&wa)
    });
    groups
}

pub(crate) fn query_all_files(conn: &Connection) -> Vec<FileRow> {
    let mut stmt = conn
        .prepare(
            "SELECT path, hash, size_bytes, COALESCE(ext,''), created_at, modified_at, exif_date, \
                    gps_lat, gps_lon, width, height \
             FROM file_hashes ORDER BY path",
        )
        .expect("failed to prepare query");
    stmt.query_map([], |r| {
        Ok(FileRow {
            path: r.get(0)?,
            hash: r.get(1)?,
            size_bytes: r.get(2)?,
            ext: r.get(3)?,
            created_at: r.get(4)?,
            modified_at: r.get(5)?,
            exif_date: r.get(6)?,
            gps_lat: r.get(7)?,
            gps_lon: r.get(8)?,
            width: r.get(9)?,
            height: r.get(10)?,
        })
    })
    .expect("failed to execute query")
    .filter_map(|r| r.ok())
    .filter(|f| std::path::Path::new(&f.path).exists())
    .collect()
}

/// Per-hash KEEP-only file set: like query_all_files(), but for hashes with
/// more than one surviving path, only the earliest-by-best_date() row is
/// kept (mirrors query_groups()'s sort-then-take-first rule). Hashes with a
/// single surviving path are trivially KEEP. Used by --by-date so REMOVE-side
/// duplicates never appear in the date-grouped gallery.
pub(crate) fn query_keep_files(conn: &Connection) -> Vec<FileRow> {
    let rows = query_all_files(conn);

    // :warning: Grouping must not lose the query's `ORDER BY path`. An earlier
    // version collected into a `HashMap` and returned `into_values()`, and
    // Rust seeds that hasher randomly per process, so two runs of
    // `report --by-date` over one unchanged database produced the same files in
    // a different order. Keeping first-seen order restores the SQL ordering.
    let mut order: Vec<String> = Vec::new();
    let mut map: HashMap<String, Vec<FileRow>> = HashMap::new();
    for row in rows {
        let hash = row.hash.clone();
        if !map.contains_key(&hash) {
            order.push(hash.clone());
        }
        map.entry(hash).or_default().push(row);
    }

    order
        .into_iter()
        .filter_map(|hash| {
            let mut group = map.remove(&hash)?;
            group.sort_by(|a, b| best_date(a).cmp(best_date(b)));
            group.into_iter().next()
        })
        .collect()
}

/// Where the labeling UI lives on this server, which decides both the URL
/// prefix of its sub-pages and where their back link points.
///
/// :warning: **It is not always `/people`.** `videre gallery` routes the
/// labeling UI at `/people` under a nav strip; a labeling-only server serves it
/// at `/` with no `/people` route at all. Hardcoding either one 404s in the
/// other configuration, which is why this is derived from state rather than
/// written into the templates.
///
/// :warning: **The second configuration is currently unreachable**, and this is
/// deliberately written for it anyway. `serve_gallery` is the only constructor
/// of `ServeOptions` and always sets `gallery: true`; the labeling-only entry
/// point went with `videre report` in 0.20.0. `ServeOptions` still models it and
/// the router still branches on it, so a value that silently assumed `/people`
/// would be a trap for whoever restores that path. It is three lines and it
/// cannot be covered by a test until something can produce the configuration.
pub(crate) fn people_root(gallery: bool) -> &'static str {
    if gallery {
        "/people"
    } else {
        "/"
    }
}

/// The wording for a link back to it. The nav calls that section "People";
/// a labeling-only server has no nav and calls it what it is.
pub(crate) fn people_back_label(gallery: bool) -> &'static str {
    if gallery {
        "Back to people"
    } else {
        "Back to labeling"
    }
}

/// Which section a page is, for the strip in `templates/nav.html`.
///
/// An enum rather than a `&str` because the template asks which one this is on
/// every entry, and a mistyped string would compile and quietly highlight
/// nothing.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Section {
    All,
    Duplicates,
    Date,
    Events,
    People,
    Map,
    /// `/settings`, reached from the nav's `...` menu rather than a section
    /// link, so no link is highlighted on it.
    Settings,
}

impl Section {
    pub(crate) fn is_all(&self) -> bool {
        *self == Section::All
    }
    pub(crate) fn is_duplicates(&self) -> bool {
        *self == Section::Duplicates
    }
    pub(crate) fn is_date(&self) -> bool {
        *self == Section::Date
    }
    pub(crate) fn is_people(&self) -> bool {
        *self == Section::People
    }
    pub(crate) fn is_map(&self) -> bool {
        *self == Section::Map
    }
    pub(crate) fn is_events(&self) -> bool {
        *self == Section::Events
    }
}

#[derive(askama::Template)]
#[template(path = "gallery.html")]
struct GalleryPage<'a> {
    /// The chrome every videre page shares. See `static/chrome.css`.
    chrome: &'static str,
    css: &'static str,
    js: &'static str,
    /// The vendored Flickr justified-layout library, loaded as its own script
    /// before `js` so the Tile view can call the `justifiedLayout` global. See
    /// `static/justified-layout.js` and `static/THIRD_PARTY_LICENSES.md`.
    justified_js: &'static str,
    /// The `var GROUPS=[...]` script block. Built in Rust because it is
    /// serialisation, not markup; the template only decides where it goes.
    data: &'a str,
    /// The library root (the db path with its `/.videre/hashes.db` tail
    /// removed), shown in the header. Pre-escaped by `esc`, so the template must
    /// not escape it again.
    library: String,
    generated_at: &'a str,
    /// Total scanned files and how many carry embeddings, shown as the header's
    /// "Files/Embedding Count" line on the home page.
    total_files: i64,
    embedded: Option<usize>,
    has_groups: bool,
    duplicate_groups: i64,
    all_files_count: Option<usize>,
    has_keep_files: bool,
    /// The current section, or `None` on a page with nowhere to navigate to.
    /// Read by the included `nav.html`, which documents the rule.
    /// Gallery settings, emitted by the included `nav.html`.
    settings_script: &'a str,
    nav: Option<Section>,
    /// Whether to render the tall library header. It belongs on the home page
    /// (`/`, the All-files section) and on a standalone static export, and is
    /// dropped on the secondary sections (`/duplicates`, `/date`) where the nav
    /// strip already names where you are and the header only pushed content down.
    show_header: bool,
    /// A duplicates page with no duplicates. Without this the page renders a
    /// header and nothing else, which reads as broken rather than as good news,
    /// and a library that has already been deduped is the common case.
    no_duplicates: bool,
    /// Whether the page's heads carry the Sort control. True for the Files
    /// tab and the Date galleries; false on the Events view, whose rows are
    /// chronological by definition. The duplicates page renders neither head.
    show_sort: bool,
}

/// The rows behind a list of paths, in the order given.
///
/// `search --html` arrives holding paths and hashes rather than rows: it ranked
/// them, so it knows *which* files, not everything about them. One query fills
/// in the rest, and the ranking order is preserved because the caller's order
/// is the answer.
pub(crate) fn rows_for_paths(conn: &Connection, paths: &[String]) -> Vec<FileRow> {
    let mut by_path: HashMap<String, FileRow> = HashMap::new();
    for row in query_all_files(conn) {
        by_path.insert(row.path.clone(), row);
    }
    paths.iter().filter_map(|p| by_path.remove(p)).collect()
}

/// Render a set to a self-contained page and write it.
///
/// Shared by `dedupe --html` and `search --html`. `groups` renders a
/// duplicate-review page; `flat` renders a gallery of a result set. Both go
/// through the same renderer the live gallery uses, with `live: false`, so a
/// file references originals on disk and embeds only what a browser cannot
/// display.
pub(crate) fn write_static_page(
    conn: &Connection,
    output: &Path,
    groups: &[Vec<FileRow>],
    flat: Option<&[FileRow]>,
) -> anyhow::Result<()> {
    let stats = query_stats(conn);
    let faces_by_hash = videre_core::face_db::labeled_faces_by_hash(conn).unwrap_or_default();
    let db_path = conn.path().map(|p| p.to_string()).unwrap_or_default();
    // The settings of the library it came from, so the export looks like its
    // gallery. Nothing is saved: there is no server.
    let settings_script = crate::commands::gallery::settings::page_script_for(
        Path::new(&db_path).parent().unwrap_or(Path::new(".")),
        false,
    );
    // `dedupe --html` passes groups (a duplicates page); `search --html` passes
    // rows (a flat gallery). A static export has no server behind it, so `nav`
    // is None: every section link would be dead when opened from `file://`.
    let (items, groups, view) = match flat {
        None => (Vec::new(), groups.to_vec(), View::Duplicates),
        Some(rows) => (rows.to_vec(), Vec::new(), View::All),
    };
    // The flat gallery's rows are what a static page inlines as ALLFILES, so
    // they carry the marks fields the offline sorts read.
    let hashes: Vec<String> = flat
        .map(|rows| rows.iter().map(|r| r.hash.clone()).collect())
        .unwrap_or_default();
    let marks_by_hash = videre_core::marks::get_many(conn, &hashes).unwrap_or_default();
    let set = RenderSet {
        stats,
        items,
        groups,
        faces_by_hash,
        marks_by_hash,
        nav: None,
        view,
        options: RenderOptions {
            live: false,
            heic: false,
            heic_original: false,
            embedded: None,
            db_path,
            date_filter_json: "null".to_string(),
            event_json: "null".to_string(),
            settings_script,
        },
    };
    let html = render(&set);
    std::fs::write(output, &html)
        .map_err(|e| anyhow::anyhow!("failed to write {}: {e}", output.display()))?;
    tracing::info!("Wrote {} ({} KB)", output.display(), html.len() / 1024);
    Ok(())
}

/// Which gallery a [`RenderSet`] describes. Replaces the old all_files/keep_files
/// slice signalling: `Date` was `keep_files.is_some()`, `Duplicates` was the
/// `groups_view` flag, `All` was the remainder.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum View {
    All,
    Date,
    Events,
    Duplicates,
}

/// Rendering knobs that are not the data themselves.
pub(crate) struct RenderOptions {
    pub live: bool,
    pub heic: bool,
    pub heic_original: bool,
    pub embedded: Option<usize>,
    pub db_path: String,
    pub date_filter_json: String,
    pub event_json: String,
    /// Gallery settings for `templates/nav.html`. See
    /// `commands::gallery::settings::page_script`.
    pub settings_script: String,
}

/// One set of files plus everything known about them, ready to render as a
/// static file (`live = false`, data embedded) or a served page (`live = true`,
/// data fetched from `/api/...`). Collapses the twelve former parameters of
/// `generate_html`.
pub(crate) struct RenderSet {
    pub stats: Stats,
    pub items: Vec<FileRow>,
    pub groups: Vec<Vec<FileRow>>,
    pub faces_by_hash: videre_core::face_db::LabeledFacesByHash,
    /// Marks for the rows a static page inlines, so offline sorting by rating
    /// and liked reads the same fields the live API splices in. Live pages
    /// inline nothing and pass an empty map.
    pub marks_by_hash: HashMap<String, videre_core::marks::Marks>,
    pub nav: Option<Section>,
    pub view: View,
    pub options: RenderOptions,
}

/// `nav` names the current section, or is `None` on a page with nowhere to
/// navigate to. See `templates/nav.html`.
pub(crate) fn render(set: &RenderSet) -> String {
    // Reconstruct the former positional parameters as locals so the body below
    // is unchanged. `all_files` and `keep_files` came from different queries and
    // are per-view exclusive; preserve that exactly.
    let db_path: &str = &set.options.db_path;
    let stats = &set.stats;
    let groups: &[Vec<FileRow>] = &set.groups;
    let (all_files, keep_files): (Option<&[FileRow]>, Option<&[FileRow]>) = match set.view {
        View::All => (Some(&set.items), None),
        View::Date => (None, Some(&set.items)),
        View::Events => (None, None),
        View::Duplicates => (None, None),
    };
    let embedded = set.options.embedded;
    let heic = set.options.heic;
    let heic_original = set.options.heic_original;
    let faces_by_hash = &set.faces_by_hash;
    let live = set.options.live;
    let nav = set.nav;
    let groups_view = set.view == View::Duplicates;
    // The Sort control rides both heads (Files and Date); the Events view is
    // chronological by definition, so its leaf page goes without.
    let show_sort = set.view != View::Events;

    use askama::Template;
    use chrono::Utc;
    let now = Utc::now().format("%Y-%m-%d %H:%M UTC").to_string();

    // In server mode HEIC thumbnails are converted lazily per request via
    // /api/files/{hash}/raw (see handle_raw_file). Converting every HEIC eagerly here made
    // server startup take minutes on a collection with many of them; the static
    // path pays that cost once at generation time instead.
    let heic = heic && !live;
    let heic_original = heic_original && !live;

    // The in-page Similar search ranks against embedding vectors, so it only
    // works when the active model has embeddings for this library. Tell the
    // client, so it can hide a Similar button that would otherwise fail.
    let has_embeddings = embedded.is_some_and(|n| n > 0);
    let data = build_data_block(
        nav,
        groups,
        all_files,
        keep_files,
        heic,
        heic_original,
        faces_by_hash,
        &set.marks_by_hash,
        live,
        has_embeddings,
        &set.options.date_filter_json,
        set.view,
        &set.options.event_json,
    );

    let page = GalleryPage {
        chrome: CHROME_CSS,
        css: include_str!("../../static/gallery.css"),
        js: GALLERY_JS,
        justified_js: include_str!("../../static/justified-layout.js"),
        data: &data,
        // The header shows the library root, not the database file inside it.
        library: esc(db_path
            .strip_suffix("/.videre/hashes.db")
            .unwrap_or(db_path)),
        generated_at: &now,
        total_files: stats.total_files,
        embedded,
        has_groups: !groups.is_empty(),
        duplicate_groups: stats.duplicate_groups,
        all_files_count: all_files.map(|f| f.len()),
        has_keep_files: keep_files.is_some() || set.view == View::Events,
        nav,
        // Home (`/` = All) and standalone exports (no nav) keep the header; the
        // secondary sections drop it. See `GalleryPage::show_header`.
        show_header: nav.is_none() || nav == Some(Section::All),
        settings_script: &set.options.settings_script,
        no_duplicates: groups_view && groups.is_empty(),
        show_sort,
    };
    page.render().expect("gallery template")
}

/// Everything the page needs as JavaScript values, up to but not including the
/// closing `</script>`, which the template supplies after the rendering script.
#[allow(clippy::too_many_arguments)]
fn build_data_block(
    nav: Option<Section>,
    groups: &[Vec<FileRow>],
    all_files: Option<&[FileRow]>,
    keep_files: Option<&[FileRow]>,
    heic: bool,
    heic_original: bool,
    faces_by_hash: &videre_core::face_db::LabeledFacesByHash,
    marks_by_hash: &HashMap<String, videre_core::marks::Marks>,
    live: bool,
    has_embeddings: bool,
    date_filter_json: &str,
    view_kind: View,
    event_json: &str,
) -> String {
    let mut out = String::with_capacity(256 * 1024);
    // GVIEW tells the client which view to ask /api/files for when it fetches
    // rather than reading an inlined array. It is the route's own identity, so
    // the client never has to infer it from the URL.
    let view = match view_kind {
        View::Date => "date",
        View::Events => "events",
        View::All | View::Duplicates => "all",
    };
    // Video grid tiles are served as oriented poster frames only on a live macOS
    // server: poster extraction is QuickLook (macOS-only), and a static export
    // has no server to fetch them from. Elsewhere the client keeps the plain
    // `<video>` tile. See buildPreview in gallery.js.
    let video_posters = live && cfg!(target_os = "macos");
    out.push_str(&format!(
        "<script>\nvar LIVE_SERVER={live};\nvar HAS_EMBEDDINGS={has_embeddings};\nvar VIDEO_POSTERS={video_posters};\nvar GVIEW={};\nvar PEOPLE_ROOT={};\nvar GDATE={};\nvar GEVENT={};\nvar GLOC=null;\n</script>\n",
        json_str(view),
        // `nav` is Some only under `videre gallery`, which is the one
        // configuration with a `/people`. See `people_root`.
        json_str(people_root(nav.is_some())),
        date_filter_json,
        event_json,
    ));
    out.push_str("<script>\nvar GROUPS=[\n");
    for (i, group) in groups.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push('\n');
        out.push_str(&group_to_json(
            group,
            heic,
            heic_original,
            faces_by_hash,
            live,
        ));
    }
    out.push_str("\n];\n");
    // All-files gallery data and similarity vectors.
    //
    // :warning: **A live page never inlines the file list.** It always fetches
    // from `/api/files`, whatever the library size.
    //
    // Tying this to the vector gate was tried and is wrong: a page that carries
    // rows whenever it carries vectors can never meet a size ceiling, because
    // the rows are the larger half for any library below the gate. One rule for
    // live pages is also simply less to get wrong.
    //
    // In-page similarity still works below the gate: it computes neighbours
    // from the inlined vectors, then asks `/api/files?hashes=` for the rows it
    // needs to display. It resolves a handful, not a library.
    //
    // A static export always inlines, having no server to fetch from.
    let inline_files = !live;
    if let Some(files) = all_files.filter(|_| inline_files) {
        out.push_str("var ALLFILES=[\n");
        for (i, f) in files.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            // Same splice the live `/api/files` response does, so a static
            // page's rows carry rating and liked for the offline sorts.
            let mut obj = file_to_json_with_faces(
                f,
                heic,
                heic_original,
                faces_by_hash
                    .get(&f.hash)
                    .map(|v| v.as_slice())
                    .unwrap_or(&[]),
                live,
            );
            if let Some(m) = marks_by_hash.get(&f.hash) {
                if obj.ends_with('}') {
                    obj.truncate(obj.len() - 1);
                    obj.push_str(&mark_fields_json(Some(m)));
                    obj.push('}');
                }
            }
            out.push_str(&obj);
        }
        out.push_str("\n];\n");
    }

    // Date-grouped KEEP-only file set (--by-date only).
    // A live date view fetches its tree from /api/dates and its rows from
    // /api/files, so it inlines nothing. A static export still carries them.
    if let Some(kf) = keep_files.filter(|_| inline_files) {
        out.push_str("var KEEPFILES=[\n");
        for (i, f) in kf.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            let mut obj = file_to_json_with_faces(
                f,
                heic,
                heic_original,
                faces_by_hash
                    .get(&f.hash)
                    .map(|v| v.as_slice())
                    .unwrap_or(&[]),
                live,
            );
            if let Some(m) = marks_by_hash.get(&f.hash) {
                if obj.ends_with('}') {
                    obj.truncate(obj.len() - 1);
                    obj.push_str(&mark_fields_json(Some(m)));
                    obj.push('}');
                }
            }
            out.push_str(&obj);
        }
        out.push_str("\n];\n");
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn geo_connection(rows: &str) -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE file_hashes (
                path TEXT PRIMARY KEY,
                hash TEXT NOT NULL,
                size_bytes INTEGER,
                ext TEXT,
                created_at TEXT,
                modified_at TEXT,
                exif_date TEXT,
                gps_lat REAL,
                gps_lon REAL,
                width INTEGER,
                height INTEGER
            );",
        )
        .unwrap();
        conn.execute_batch(rows).unwrap();
        videre_core::location_cluster::register_haversine_sql_function(&conn).unwrap();
        conn
    }

    fn hashes(rows: &[(FileRow, i64)]) -> Vec<&str> {
        rows.iter().map(|(row, _)| row.hash.as_str()).collect()
    }

    #[test]
    fn event_file_lookup_handles_more_hashes_than_one_sqlite_parameter_batch() {
        let mut conn = geo_connection("");
        videre_core::library_db::ensure_hash_index(&conn).unwrap();
        let wanted: Vec<String> = (0..32_768).map(|i| format!("{i:064x}")).collect();
        let transaction = conn.transaction().unwrap();
        {
            let mut insert = transaction
                .prepare(
                    "INSERT INTO file_hashes (path,hash,size_bytes,ext) VALUES (?1,?2,100,'jpg')",
                )
                .unwrap();
            for (i, hash) in wanted.iter().enumerate() {
                insert
                    .execute(rusqlite::params![format!("/p/{i}.jpg"), hash])
                    .unwrap();
            }
            for i in [0, 32_767] {
                insert
                    .execute(rusqlite::params![format!("/p/copy-{i}.jpg"), wanted[i]])
                    .unwrap();
            }
        }
        transaction.commit().unwrap();
        let (rows, total) = query_event_files(&conn, &wanted).unwrap();
        assert_eq!(total, 32_768);
        assert_eq!(
            hashes(&rows),
            wanted.iter().map(String::as_str).collect::<Vec<_>>()
        );
        assert_eq!(rows[0].1, 2);
        assert_eq!(rows[32_767].1, 2);
    }

    #[test]
    fn a_failed_page_query_returns_its_sql_and_logs_nothing_itself() {
        #[derive(Clone, Default)]
        struct Buf(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
        impl std::io::Write for Buf {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(b);
                Ok(b.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let buf = Buf::default();
        let w = buf.clone();
        let sub = tracing_subscriber::fmt()
            .with_writer(move || w.clone())
            .finish();
        // No tables at all: the page query cannot prepare.
        let conn = Connection::open_in_memory().unwrap();
        let err = tracing::subscriber::with_default(sub, || {
            match query_files_page(&conn, "all", None, 0, 10, None, FileSort::default()) {
                Ok(_) => panic!("a page query with no tables must fail"),
                Err(e) => e,
            }
        });
        let text = format!("{err:#}");
        assert!(
            text.contains("SELECT") && text.contains("no such table"),
            "the error keeps the SQL and its cause: {text}"
        );
        assert!(
            !String::from_utf8_lossy(&buf.0.lock().unwrap()).contains("query failed"),
            "the handler's boundary logs it, once"
        );
    }

    #[test]
    fn location_page_excludes_bbox_corners_and_keeps_total_in_step() {
        let conn = geo_connection(
            "INSERT INTO file_hashes (path, hash, size_bytes, ext, gps_lat, gps_lon) VALUES
                ('/a-center.jpg', 'center', 1, 'jpg', 52.52, 13.405),
                ('/b-axis.jpg', 'axis', 1, 'jpg', 52.60, 13.405),
                ('/c-corner.jpg', 'corner', 1, 'jpg', 52.70, 13.69),
                ('/d-outside.jpg', 'outside', 1, 'jpg', 53.00, 13.405),
                ('/e-no-gps.jpg', 'no_gps', 1, 'jpg', NULL, NULL);",
        );
        let filter = LocationFilter {
            lat: 52.52,
            lon: 13.405,
            radius_km: 25.0,
        };

        let (first, first_total) =
            query_files_page(&conn, "all", None, 0, 1, Some(&filter), FileSort::default()).unwrap();
        let (second, second_total) =
            query_files_page(&conn, "all", None, 1, 1, Some(&filter), FileSort::default()).unwrap();

        assert_eq!(first_total, 2);
        assert_eq!(second_total, 2);
        assert_eq!(hashes(&first), vec!["center"]);
        assert_eq!(hashes(&second), vec!["axis"]);
    }

    #[test]
    fn location_page_wraps_antimeridian_and_unbounds_longitude_at_a_pole() {
        let antimeridian = geo_connection(
            "INSERT INTO file_hashes (path, hash, size_bytes, ext, gps_lat, gps_lon) VALUES
                ('/a-east.jpg', 'east', 1, 'jpg', 0.0, 179.9),
                ('/b-west.jpg', 'west', 1, 'jpg', 0.0, -179.9);",
        );
        let antimeridian_filter = LocationFilter {
            lat: 0.0,
            lon: 179.9,
            radius_km: 30.0,
        };
        let (rows, total) = query_files_page(
            &antimeridian,
            "all",
            None,
            0,
            10,
            Some(&antimeridian_filter),
            FileSort::default(),
        )
        .unwrap();
        assert_eq!(total, 2);
        assert_eq!(hashes(&rows), vec!["east", "west"]);

        let pole = geo_connection(
            "INSERT INTO file_hashes (path, hash, size_bytes, ext, gps_lat, gps_lon) VALUES
                ('/a-prime.jpg', 'prime', 1, 'jpg', 89.9, 0.0),
                ('/b-opposite.jpg', 'opposite', 1, 'jpg', 89.9, 180.0);",
        );
        let pole_filter = LocationFilter {
            lat: 89.9,
            lon: 0.0,
            radius_km: 30.0,
        };
        let (rows, total) = query_files_page(
            &pole,
            "all",
            None,
            0,
            10,
            Some(&pole_filter),
            FileSort::default(),
        )
        .unwrap();
        assert_eq!(total, 2);
        assert_eq!(hashes(&rows), vec!["prime", "opposite"]);
    }

    /// file_hashes plus marks, the two tables the sort reads. `mime` is
    /// included because the Type sort reads it.
    fn sort_connection(rows: &str) -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE file_hashes (
                path TEXT PRIMARY KEY,
                hash TEXT NOT NULL,
                size_bytes INTEGER,
                ext TEXT,
                created_at TEXT,
                modified_at TEXT,
                exif_date TEXT,
                gps_lat REAL,
                gps_lon REAL,
                width INTEGER,
                height INTEGER,
                mime TEXT
            );
            CREATE TABLE marks (
                hash TEXT PRIMARY KEY,
                rating INTEGER,
                pick INTEGER,
                label TEXT,
                liked INTEGER NOT NULL DEFAULT 0,
                updated_at TEXT NOT NULL
            );",
        )
        .unwrap();
        conn.execute_batch(rows).unwrap();
        conn
    }

    const SORT_ROWS: &str = "
INSERT INTO file_hashes (path, hash, size_bytes, ext, mime, modified_at, exif_date) VALUES
    ('/lib/a_alpha.jpg', 'ha', 30, 'jpg', 'image/jpeg', '2021-01-01T00:00:00', '2021-08-10T19:34:03'),
    ('/lib/b_beta.jpg',  'hb', 30, 'jpg', 'image/jpeg', '2021-06-01T00:00:00', '2021-08-10T19:34:03'),
    ('/lib/c_clip.mp4',  'hc', 22, 'mp4', 'video/mp4',  '2022-05-01T00:00:00', NULL),
    ('/lib/d_undated.jpg', 'hd', 10, 'jpg', 'image/jpeg', NULL, NULL),
    ('/lib/e_copy.jpg',  'ha', 30, 'jpg', 'image/jpeg', '2021-02-01T00:00:00', '2021-08-10T19:34:03');
INSERT INTO marks (hash, rating, liked, updated_at) VALUES
    ('ha', 5, 0, '2026-01-01T00:00:00'),
    ('hb', 3, 0, '2026-01-01T00:00:00'),
    ('hc', NULL, 1, '2026-01-01T00:00:00');
";

    fn paths(rows: &[(FileRow, i64)]) -> Vec<String> {
        rows.iter().map(|(r, _)| r.path.clone()).collect()
    }

    fn page_sorted(conn: &Connection, view: &str, sort: FileSort) -> Vec<String> {
        let (rows, _) = query_files_page(conn, view, None, 0, 100, None, sort).unwrap();
        paths(&rows)
    }

    #[test]
    fn default_sort_is_newest_effective_date_with_path_tie_break() {
        let conn = sort_connection(SORT_ROWS);
        assert_eq!(
            page_sorted(&conn, "all", FileSort::default()),
            vec![
                "/lib/c_clip.mp4".to_string(),    // modified 2022-05, no exif
                "/lib/a_alpha.jpg".to_string(),   // exif 2021-08-10, path before b
                "/lib/b_beta.jpg".to_string(),    // same exif, path tie-break
                "/lib/e_copy.jpg".to_string(),    // same hash as a, later path
                "/lib/d_undated.jpg".to_string(), // no dates at all: last
            ]
        );
    }

    #[test]
    fn date_ascending_puts_undated_last() {
        let conn = sort_connection(SORT_ROWS);
        assert_eq!(
            page_sorted(
                &conn,
                "all",
                FileSort {
                    field: FileSortField::Date,
                    desc: false
                }
            ),
            vec![
                "/lib/a_alpha.jpg".to_string(),
                "/lib/b_beta.jpg".to_string(),
                "/lib/e_copy.jpg".to_string(),
                "/lib/c_clip.mp4".to_string(),
                "/lib/d_undated.jpg".to_string(),
            ]
        );
    }

    #[test]
    fn name_sort_reads_the_basename_case_insensitively() {
        let conn = sort_connection(&format!(
            "{SORT_ROWS}\nINSERT INTO file_hashes (path, hash, size_bytes, ext, mime) VALUES \
             ('/lib/Z_root.jpg', 'hz', 1, 'jpg', 'image/jpeg');"
        ));
        let asc = page_sorted(
            &conn,
            "all",
            FileSort {
                field: FileSortField::Name,
                desc: false,
            },
        );
        assert_eq!(asc[0], "/lib/a_alpha.jpg");
        assert_eq!(*asc.last().unwrap(), "/lib/Z_root.jpg"); // NOCASE: z after the rest
        assert_eq!(asc[3], "/lib/d_undated.jpg");
    }

    #[test]
    fn basename_expression_handles_a_path_without_a_slash() {
        let conn = sort_connection(&format!(
            "{SORT_ROWS}\nINSERT INTO file_hashes (path, hash, size_bytes, ext, mime) VALUES \
             ('plain.jpg', 'hp', 1, 'jpg', 'image/jpeg');"
        ));
        let asc = page_sorted(
            &conn,
            "all",
            FileSort {
                field: FileSortField::Name,
                desc: false,
            },
        );
        assert_eq!(*asc.last().unwrap(), "plain.jpg"); // basename 'p' after the /lib names
    }

    #[test]
    fn size_sort_orders_both_ways() {
        let conn = sort_connection(SORT_ROWS);
        assert_eq!(
            page_sorted(
                &conn,
                "all",
                FileSort {
                    field: FileSortField::Size,
                    desc: true
                }
            ),
            vec![
                "/lib/a_alpha.jpg".to_string(),
                "/lib/b_beta.jpg".to_string(),
                "/lib/e_copy.jpg".to_string(),
                "/lib/c_clip.mp4".to_string(),
                "/lib/d_undated.jpg".to_string(),
            ]
        );
        let asc = page_sorted(
            &conn,
            "all",
            FileSort {
                field: FileSortField::Size,
                desc: false,
            },
        );
        assert_eq!(asc[0], "/lib/d_undated.jpg");
        assert_eq!(asc[1], "/lib/c_clip.mp4");
    }

    #[test]
    fn rating_sort_puts_unrated_last_in_both_directions() {
        let conn = sort_connection(SORT_ROWS);
        // Marks are hash-keyed: e_copy shares hash 'ha' with a_alpha, so the
        // rating 5 rides both copies. The unrated block sorts by path.
        let desc = page_sorted(
            &conn,
            "all",
            FileSort {
                field: FileSortField::Rating,
                desc: true,
            },
        );
        assert_eq!(
            desc,
            vec![
                "/lib/a_alpha.jpg".to_string(),   // 5
                "/lib/e_copy.jpg".to_string(),    // 5, same hash, path after a
                "/lib/b_beta.jpg".to_string(),    // 3
                "/lib/c_clip.mp4".to_string(),    // unrated: last block
                "/lib/d_undated.jpg".to_string(), // unrated: last block
            ]
        );
        let asc = page_sorted(
            &conn,
            "all",
            FileSort {
                field: FileSortField::Rating,
                desc: false,
            },
        );
        assert_eq!(
            asc,
            vec![
                "/lib/b_beta.jpg".to_string(),    // 3 before 5
                "/lib/a_alpha.jpg".to_string(),   // 5, path tie-break within the hash
                "/lib/e_copy.jpg".to_string(),    // 5, same hash
                "/lib/c_clip.mp4".to_string(),    // unrated: last block
                "/lib/d_undated.jpg".to_string(), // unrated: last block
            ]
        );
    }

    #[test]
    fn liked_sort_keeps_newest_first_as_its_second_key() {
        let conn = sort_connection(SORT_ROWS);
        let first = page_sorted(
            &conn,
            "all",
            FileSort {
                field: FileSortField::Liked,
                desc: true,
            },
        );
        assert_eq!(first[0], "/lib/c_clip.mp4"); // the liked one
        assert_eq!(first[1], "/lib/a_alpha.jpg"); // then newest first
        let last = page_sorted(
            &conn,
            "all",
            FileSort {
                field: FileSortField::Liked,
                desc: false,
            },
        );
        assert_eq!(last[0], "/lib/a_alpha.jpg"); // unliked block, path order
        assert_eq!(*last.last().unwrap(), "/lib/c_clip.mp4"); // liked one last
    }

    #[test]
    fn type_sort_reads_the_mime_and_treats_null_as_photo() {
        let conn = sort_connection(&format!(
            "{SORT_ROWS}\nINSERT INTO file_hashes (path, hash, size_bytes, ext, mime) VALUES \
             ('/lib/f_nomime.jpg', 'hf', 1, 'jpg', NULL);"
        ));
        let photos_first = page_sorted(
            &conn,
            "all",
            FileSort {
                field: FileSortField::Type,
                desc: false,
            },
        );
        assert_eq!(photos_first[0], "/lib/a_alpha.jpg");
        assert_eq!(
            photos_first.iter().position(|p| p == "/lib/c_clip.mp4"),
            Some(5)
        );
        assert_eq!(photos_first[4], "/lib/f_nomime.jpg"); // NULL mime groups with the photos
    }

    #[test]
    fn pages_of_a_sorted_view_are_disjoint_and_concatenate_to_the_order() {
        let conn = sort_connection(SORT_ROWS);
        let mut seen = Vec::new();
        for offset in [0, 2, 4] {
            let (rows, total) =
                query_files_page(&conn, "all", None, offset, 2, None, FileSort::default()).unwrap();
            assert_eq!(total, 5);
            seen.extend(rows.into_iter().map(|(r, _)| r.path));
        }
        assert_eq!(seen.len(), 5, "no row appears twice across pages");
        // Concatenating the pages reproduces the single-query order.
        let (all, _) =
            query_files_page(&conn, "all", None, 0, 100, None, FileSort::default()).unwrap();
        let all: Vec<String> = all.into_iter().map(|(r, _)| r.path).collect();
        assert_eq!(seen, all);
    }

    #[test]
    fn the_keep_set_view_sorts_by_the_same_whitelist() {
        let conn = sort_connection(SORT_ROWS);
        let date_view = page_sorted(&conn, "date", FileSort::default());
        // 'ha' is one hash with two paths; the keep set keeps a_alpha (earlier
        // path at equal effective date) and the outer order is the whitelist.
        assert_eq!(date_view[0], "/lib/c_clip.mp4");
        assert!(date_view.contains(&"/lib/a_alpha.jpg".to_string()));
        assert!(!date_view.contains(&"/lib/e_copy.jpg".to_string()));
    }

    #[test]
    fn unknown_query_values_fall_back_to_the_default() {
        assert_eq!(
            FileSort::from_query(Some("bogus"), Some("sideways")),
            FileSort::default()
        );
        assert_eq!(FileSort::from_query(None, None), FileSort::default());
        assert_eq!(
            FileSort::from_query(Some("name"), Some("asc")),
            FileSort {
                field: FileSortField::Name,
                desc: false
            }
        );
        assert_eq!(
            FileSort::from_query(Some("liked"), Some("DESC")),
            FileSort {
                field: FileSortField::Liked,
                desc: true
            }
        );
    }
}
