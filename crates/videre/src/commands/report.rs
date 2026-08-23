use axum::extract::{Json as AxumJson, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::Router;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use videre_api::{ClusterDetail, FacesData, PersonDetail};

pub(crate) struct FileRow {
    path: String,
    hash: String,
    size_bytes: i64,
    ext: String,
    created_at: Option<String>,
    modified_at: Option<String>,
    exif_date: Option<String>,
    gps_lat: Option<f64>,
    gps_lon: Option<f64>,
    width: Option<i32>,
    height: Option<i32>,
}

pub(crate) struct Stats {
    total_files: i64,
    duplicate_groups: i64,
    duplicate_files: i64,
    wasted_bytes: i64,
}

/// How many files in this library are embedded under `model_id`, for the header
/// stat. `None` when nothing is.
///
/// :warning: This replaced a function that loaded **every vector** to answer the
/// same question. That existed because the page did in-browser similarity and
/// needed the vectors anyway; the server ranks now, so a page needs the number
/// and nothing else. A large library used to lose this stat entirely, because
/// the loader gave up above a size cap and the header then showed nothing.
fn query_embedded_count(conn: &Connection, model_id: &str) -> Option<usize> {
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
fn query_files_page(
    conn: &Connection,
    view: &str,
    date: Option<&str>,
    offset: i64,
    limit: i64,
) -> rusqlite::Result<(Vec<(FileRow, i64)>, i64)> {
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
    let (where_sql, params): (String, Vec<String>) = match date.filter(|d| !d.is_empty()) {
        Some(d) => (
            format!(" WHERE substr({effective}, 1, {}) = ?", d.len()),
            vec![d.to_string()],
        ),
        None => (String::new(), Vec::new()),
    };

    let total: i64 = conn
        .query_row(
            &format!("SELECT COUNT(*) FROM {total_from} AS t{}", where_sql),
            rusqlite::params_from_iter(params.iter()),
            |r| r.get(0),
        )
        .unwrap_or_else(|e| {
            eprintln!("videre gallery: /api/files count failed: {e}");
            -1
        });

    // `copies` is how many files share this hash. The client derived it by
    // scanning the whole array, which is the only reason it needed the whole
    // array. The database knew it all along.
    let sql = format!(
        "SELECT path, hash, size_bytes, COALESCE(ext,''), created_at, modified_at, exif_date, \
                gps_lat, gps_lon, width, height, \
                (SELECT COUNT(*) FROM file_hashes c WHERE c.hash = f.hash) AS copies \
         FROM {from} AS f{where_sql} ORDER BY f.path LIMIT ? OFFSET ?"
    );
    // :warning: A failed query must not read as an empty page. Returning
    // `(vec![], total)` here once hid malformed SQL behind a gallery that looked
    // like a library with nothing in it, which is the same shape as every other
    // fault in this file's history: nothing errored, so nothing was reported.
    let mut stmt = match conn.prepare(&sql) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("videre gallery: /api/files query failed: {e}\n  {sql}");
            return Err(rusqlite::Error::InvalidQuery);
        }
    };
    let mut bound: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    for p in &params {
        bound.push(Box::new(p.clone()));
    }
    bound.push(Box::new(limit));
    bound.push(Box::new(offset));
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
fn keep_set_sql() -> String {
    format!(
        "(SELECT * FROM (SELECT *, ROW_NUMBER() OVER \
          (PARTITION BY hash ORDER BY {}, path) AS rn FROM file_hashes) WHERE rn = 1)",
        videre_core::query::EFFECTIVE_DATE_SQL
    )
}

fn query_files_by_hash(
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
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
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
/// the server-mode report where thumbnails must be embedded inline rather
/// than served as raw bytes (that's what handle_face_image does instead).
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

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// A hand-built JSON body as a response.
///
/// These endpoints assemble JSON as a string rather than serialising a struct,
/// because the row shape is shared with the inlined static export and is built
/// by one function either way. This is the third endpoint to need the same two
/// lines, so it is a function.
fn json_response(body: String) -> axum::response::Response {
    use axum::response::IntoResponse;
    (
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        body,
    )
        .into_response()
}

/// Page chrome, shared by the gallery templates and the labeling page so the
/// two cannot drift into looking like different products.
pub(crate) const CHROME_CSS: &str = include_str!("../../static/chrome.css");

fn json_str(s: &str) -> String {
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

#[cfg(test)]
fn file_to_json(f: &FileRow, heic: bool, heic_original: bool) -> String {
    file_to_json_with_faces(f, heic, heic_original, &[], false)
}

/// Like file_to_json(), but also embeds labeled-face thumbnails into
/// meta.faces. `faces` is the (face_id, person_label, bbox) list for this
/// file's hash, as returned by videre_core::face_db::labeled_faces_by_hash()
/// (note the tuple order: label is `.1`, bbox is `.2`).
fn file_to_json_with_faces(
    f: &FileRow,
    heic: bool,
    heic_original: bool,
    faces: &[(i64, String, String)],
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
    // from `/api/face-image/{id}` when a lightbox opens. Inlining crops instead
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
        duplicate_files: s.duplicate_file_count,
        wasted_bytes: s.wasted_bytes,
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

fn query_all_files(conn: &Connection) -> Vec<FileRow> {
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
fn query_keep_files(conn: &Connection) -> Vec<FileRow> {
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
    People,
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
}

#[derive(askama::Template)]
#[template(path = "gallery.html")]
struct GalleryPage<'a> {
    /// The chrome every videre page shares. See `static/chrome.css`.
    chrome: &'static str,
    css: &'static str,
    js: &'static str,
    /// The `var GROUPS=[...]` script block. Built in Rust because it is
    /// serialisation, not markup; the template only decides where it goes.
    data: &'a str,
    /// Pre-escaped by `esc`, so the template must not escape it again.
    db: String,
    generated_at: &'a str,
    total_files: i64,
    has_groups: bool,
    duplicate_groups: i64,
    duplicate_files: i64,
    wasted: String,
    embedded: Option<usize>,
    all_files_count: Option<usize>,
    has_keep_files: bool,
    /// The current section, or `None` on a page with nowhere to navigate to.
    /// Read by the included `nav.html`, which documents the rule.
    nav: Option<Section>,
    /// A duplicates page with no duplicates. Without this the page renders a
    /// header and nothing else, which reads as broken rather than as good news,
    /// and a library that has already been deduped is the common case.
    no_duplicates: bool,
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
    let html = generate_html(
        &db_path,
        &stats,
        groups,
        flat,
        None,
        None,
        false,
        false,
        &faces_by_hash,
        false,
        // A static export has no server behind it, so every section link would
        // be dead the moment the file is opened from `file://`.
        None,
        // `dedupe --html` passes groups; `search --html` passes rows.
        flat.is_none(),
    );
    std::fs::write(output, &html)
        .map_err(|e| anyhow::anyhow!("failed to write {}: {e}", output.display()))?;
    eprintln!("Wrote {} ({} KB)", output.display(), html.len() / 1024);
    Ok(())
}

/// `nav` names the current section, or is `None` on a page with nowhere to
/// navigate to. See `templates/nav.html`.
pub(crate) fn generate_html(
    db_path: &str,
    stats: &Stats,
    groups: &[Vec<FileRow>],
    all_files: Option<&[FileRow]>,
    keep_files: Option<&[FileRow]>,
    embedded: Option<usize>,
    heic: bool,
    heic_original: bool,
    faces_by_hash: &videre_core::face_db::LabeledFacesByHash,
    live: bool,
    nav: Option<Section>,
    // `groups_view`: this page is about duplicate groups, so it must say
    // something when there are none rather than render a header and nothing
    // else. A doc comment is not allowed on a parameter.
    groups_view: bool,
) -> String {
    use askama::Template;
    use chrono::Utc;
    let now = Utc::now().format("%Y-%m-%d %H:%M UTC").to_string();

    // In server mode HEIC thumbnails are converted lazily per request via
    // /api/raw (see handle_raw_file). Converting every HEIC eagerly here made
    // server startup take minutes on a collection with many of them; the static
    // path pays that cost once at generation time instead.
    let heic = heic && !live;
    let heic_original = heic_original && !live;

    let data = build_data_block(
        groups,
        all_files,
        keep_files,
        heic,
        heic_original,
        faces_by_hash,
        live,
    );

    let page = GalleryPage {
        chrome: CHROME_CSS,
        css: include_str!("../../static/gallery.css"),
        js: include_str!("../../static/gallery.js"),
        data: &data,
        db: esc(db_path),
        generated_at: &now,
        total_files: stats.total_files,
        has_groups: !groups.is_empty(),
        duplicate_groups: stats.duplicate_groups,
        duplicate_files: stats.duplicate_files,
        wasted: videre_core::disk::human_bytes(stats.wasted_bytes.max(0) as u64),
        embedded,
        all_files_count: all_files.map(|f| f.len()),
        has_keep_files: keep_files.is_some(),
        nav,
        no_duplicates: groups_view && groups.is_empty(),
    };
    page.render().expect("gallery template")
}

/// Everything the page needs as JavaScript values, up to but not including the
/// closing `</script>`, which the template supplies after the rendering script.
#[allow(clippy::too_many_arguments)]
fn build_data_block(
    groups: &[Vec<FileRow>],
    all_files: Option<&[FileRow]>,
    keep_files: Option<&[FileRow]>,
    heic: bool,
    heic_original: bool,
    faces_by_hash: &videre_core::face_db::LabeledFacesByHash,
    live: bool,
) -> String {
    let mut out = String::with_capacity(256 * 1024);
    // GVIEW tells the client which view to ask /api/files for when it fetches
    // rather than reading an inlined array. It is the route's own identity, so
    // the client never has to infer it from the URL.
    let view = if keep_files.is_some() { "date" } else { "all" };
    out.push_str(&format!(
        "<script>\nvar LIVE_SERVER={live};\nvar GVIEW={};\n</script>\n",
        json_str(view)
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
            out.push_str(&file_to_json_with_faces(
                f,
                heic,
                heic_original,
                faces_by_hash
                    .get(&f.hash)
                    .map(|v| v.as_slice())
                    .unwrap_or(&[]),
                live,
            ));
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
            out.push_str(&file_to_json_with_faces(
                f,
                heic,
                heic_original,
                faces_by_hash
                    .get(&f.hash)
                    .map(|v| v.as_slice())
                    .unwrap_or(&[]),
                live,
            ));
        }
        out.push_str("\n];\n");
    }

    out
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
    }

    #[derive(Template)]
    #[template(path = "cluster.html")]
    pub struct Cluster {
        pub css: &'static str,
        pub js: &'static str,
        pub cluster_id: i64,
    }

    #[derive(Template)]
    #[template(path = "person.html")]
    pub struct Person {
        pub css: &'static str,
        pub js: &'static str,
        pub faces_ui_enabled: bool,
    }

    pub const FACES_CSS: &str = include_str!("../../static/faces.css");
    pub const FACES_JS: &str = include_str!("../../static/faces.js");
    pub const CLUSTER_CSS: &str = include_str!("../../static/cluster.css");
    pub const CLUSTER_JS: &str = include_str!("../../static/cluster.js");
    pub const PERSON_CSS: &str = include_str!("../../static/person.css");
    pub const PERSON_JS: &str = include_str!("../../static/person.js");
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
struct AssignRequest {
    face_ids: Vec<i64>,
    person_label: String,
}

#[derive(Deserialize)]
struct NewPersonRequest {
    face_ids: Vec<i64>,
    label: String,
}

#[derive(Deserialize)]
struct RemoveFaceRequest {
    face_id: i64,
}

#[derive(Deserialize)]
struct DissolveClusterRequest {
    cluster_id: i64,
}

#[derive(Deserialize)]
struct DeletePersonRequest {
    label: String,
}

/// Changing what a person is shown as, without touching their identity.
///
/// Separate from rename because intent cannot be read from the new string:
/// `Erhan` to `Erhan Gündoğan` is a display correction whose normalized form
/// also changes, so one endpoint would have to guess which was meant.
#[derive(Deserialize)]
struct SetFullNameRequest {
    name: String,
    full_name: String,
}

#[derive(Deserialize)]
struct SetPrimaryRequest {
    face_id: i64,
    person_label: String,
}

#[derive(Deserialize)]
struct PersonSearchQuery {
    name: String,
}

struct AppState {
    conn: Mutex<Connection>,
    shutdown_tx: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    report_all: bool,
    model_id: String,
    report_by_date: bool,
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
    };
    axum::response::Html(page.render().expect("faces template"))
}

/// Live-server equivalent of the static `--all`/`--by-date` HTML report,
/// rendered on each request from the current database state (labeled faces
/// included, since this route only exists when `--show-faces` is set).
async fn handle_report(State(state): State<Arc<AppState>>) -> impl axum::response::IntoResponse {
    // Not `videre gallery`: this server has `/` and nothing else.
    render_live(&state, state.report_all, state.report_by_date, true, None)
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
            media: super::selection_args::MediaArgs::default(),
            paths: super::selection_args::PathArgs::default(),
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
    let all_files = all.then(|| query_all_files(&conn));
    let keep_files = by_date.then(|| query_keep_files(&conn));
    let faces_by_hash = videre_core::face_db::labeled_faces_by_hash(&conn).unwrap_or_default();
    let embedded = if all {
        query_embedded_count(&conn, &state.model_id)
    } else {
        None
    };
    let db_path = conn.path().map(|p| p.to_string()).unwrap_or_default();
    drop(conn);
    let html = generate_html(
        &db_path,
        &stats,
        &groups,
        all_files.as_deref(),
        keep_files.as_deref(),
        embedded,
        state.report_heic,
        state.report_heic_original,
        &faces_by_hash,
        true,
        nav,
        with_groups,
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
        // Splice `copies` in rather than widening FileRow, which the static
        // export also builds and has no use for it.
        if obj.ends_with('}') {
            obj.truncate(obj.len() - 1);
            obj.push_str(&format!(",\"copies\":{copies}}}"));
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
    AxumJson(req): AxumJson<AssignRequest>,
) -> Result<StatusCode, StatusCode> {
    let conn = state
        .conn
        .lock()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    videre_api::assign(&conn, &req.face_ids, &req.person_label)
        .map(|_| StatusCode::OK)
        .map_err(api_status)
}

async fn handle_new_person(
    State(state): State<Arc<AppState>>,
    AxumJson(req): AxumJson<NewPersonRequest>,
) -> Result<StatusCode, StatusCode> {
    let conn = state
        .conn
        .lock()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    videre_api::new_person(&conn, &req.face_ids, &req.label)
        .map(|_| StatusCode::OK)
        .map_err(api_status)
}

async fn handle_remove_face(
    State(state): State<Arc<AppState>>,
    AxumJson(req): AxumJson<RemoveFaceRequest>,
) -> Result<StatusCode, StatusCode> {
    let conn = state
        .conn
        .lock()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    videre_api::remove_face(&conn, req.face_id)
        .map(|_| StatusCode::OK)
        .map_err(api_status)
}

async fn handle_delete_person(
    State(state): State<Arc<AppState>>,
    AxumJson(req): AxumJson<DeletePersonRequest>,
) -> Result<StatusCode, StatusCode> {
    let conn = state
        .conn
        .lock()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    videre_api::delete_person(&conn, &req.label)
        .map(|_| StatusCode::OK)
        .map_err(api_status)
}

async fn handle_set_full_name(
    State(state): State<Arc<AppState>>,
    AxumJson(req): AxumJson<SetFullNameRequest>,
) -> Result<StatusCode, StatusCode> {
    let conn = state
        .conn
        .lock()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    videre_api::set_full_name(&conn, &req.name, &req.full_name)
        .map(|_| StatusCode::OK)
        .map_err(api_status)
}

async fn handle_dissolve_cluster(
    State(state): State<Arc<AppState>>,
    AxumJson(req): AxumJson<DissolveClusterRequest>,
) -> Result<StatusCode, StatusCode> {
    let conn = state
        .conn
        .lock()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    videre_api::dissolve_cluster(&conn, req.cluster_id)
        .map(|_| StatusCode::OK)
        .map_err(api_status)
}

async fn handle_set_primary(
    State(state): State<Arc<AppState>>,
    AxumJson(req): AxumJson<SetPrimaryRequest>,
) -> Result<StatusCode, StatusCode> {
    let conn = state
        .conn
        .lock()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    videre_api::set_primary(&conn, req.face_id, &req.person_label)
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
) -> impl axum::response::IntoResponse {
    use askama::Template;
    let page = pages::Cluster {
        css: pages::CLUSTER_CSS,
        js: pages::CLUSTER_JS,
        cluster_id,
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
    path: String,
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
    Query(q): Query<RawFileQuery>,
    State(state): State<Arc<AppState>>,
) -> Result<impl axum::response::IntoResponse, StatusCode> {
    let (path, hash) = {
        let conn = state
            .conn
            .lock()
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        conn.query_row(
            "SELECT path, hash FROM file_hashes WHERE path = ?1 LIMIT 1",
            [&q.path],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
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
    /// True when `--show-faces` was passed, i.e. `/` should serve the live
    /// report instead of the labeling UI. Tracked separately from
    /// `serve_faces_ui` (`--faces`) because the two flags are independent:
    /// either can be passed alone or together.
    show_report: bool,
    report_all: bool,
    report_by_date: bool,
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
        report_all: opts.report_all,
        model_id: opts.model_id.clone(),
        report_by_date: opts.report_by_date,
        report_heic: opts.report_heic,
        report_heic_original: opts.report_heic_original,
        serve_faces_ui: opts.serve_faces_ui,
        gallery: opts.gallery,
        db: db.to_path_buf(),
        embedder: Mutex::new(None),
    });

    let mut router = Router::new()
        .route("/api/face-image/{id}", get(handle_face_image))
        .route("/api/original-image/{id}", get(handle_original_image))
        .route("/cluster/{id}", get(handle_cluster_page))
        .route("/api/cluster/{id}", get(handle_cluster_api))
        .route("/person/{name}", get(handle_person_page))
        .route("/api/person/{name}", get(handle_person_api))
        .route("/api/search/person", get(handle_search_person))
        .route("/api/quit", post(handle_quit))
        .route("/api/location", get(handle_location))
        .route("/api/raw", get(handle_raw_file))
        .route("/api/files", get(handle_files))
        .route("/api/dates", get(handle_dates))
        .route("/api/search", get(handle_search));

    if state.serve_faces_ui {
        router = router
            .route("/api/faces", get(handle_get_faces))
            .route("/api/assign", post(handle_assign))
            .route("/api/new-person", post(handle_new_person))
            .route("/api/remove-face", post(handle_remove_face))
            .route("/api/delete-person", post(handle_delete_person))
            .route("/api/set-full-name", post(handle_set_full_name))
            .route("/api/dissolve-cluster", post(handle_dissolve_cluster))
            .route("/api/set-primary", post(handle_set_primary));
    }

    // `/` and (when both modes are active) `/faces` depend on which combination
    // of --faces / --show-faces started this server:
    //   --faces alone        -> `/` = labeling UI, no report route at all
    //   --show-faces alone   -> `/` = live report, no `/faces` route
    //   both                 -> `/` = live report, `/faces` = labeling UI
    router = if opts.gallery {
        router
            .route("/", get(handle_gallery_all))
            .route("/duplicates", get(handle_gallery_duplicates))
            .route("/people", get(handle_root))
            .route("/date", get(handle_gallery_date))
            .route("/map", get(handle_not_yet))
            .route("/events", get(handle_not_yet))
            .route("/smart", get(handle_not_yet))
    } else {
        match (state.serve_faces_ui, opts.show_report) {
            (true, true) => router
                .route("/", get(handle_report))
                .route("/faces", get(handle_root)),
            (true, false) => router.route("/", get(handle_root)),
            (false, true) => router.route("/", get(handle_report)),
            (false, false) => router, // unreachable: serve_faces_async only runs when at least one is set
        }
    };

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
/// :warning: This file is named `report.rs` for a command that no longer
/// exists. `videre report` was removed in 0.20.0; what remains here is the
/// shared renderer and server that `gallery`, `dedupe --html` and
/// `search --html` all use. Renaming it, and moving this function to
/// `gallery.rs`, is tracked separately: it is a large mechanical move and does
/// not belong in the same change as the removal.
pub(crate) fn serve_gallery(
    db: &Path,
    model_id: String,
    port: u16,
    browse: bool,
) -> anyhow::Result<()> {
    let opts = ServeOptions {
        serve_faces_ui: true,
        show_report: false,
        report_all: true,
        report_by_date: false,
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
