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

pub(crate) fn esc(s: &str) -> String {
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

/// Page chrome, shared by the gallery templates and the labeling page so the
/// two cannot drift into looking like different products.
pub(crate) const CHROME_CSS: &str = include_str!("../../static/chrome.css");

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

#[cfg(test)]
fn file_to_json(f: &FileRow, heic: bool, heic_original: bool) -> String {
    file_to_json_with_faces(f, heic, heic_original, &[], false)
}

/// Like file_to_json(), but also embeds labeled-face thumbnails into
/// meta.faces. `faces` is the (face_id, person_label, bbox) list for this
/// file's hash, as returned by videre_core::face_db::labeled_faces_by_hash()
/// (note the tuple order: label is `.1`, bbox is `.2`).
pub(crate) fn file_to_json_with_faces(
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
    // `dedupe --html` passes groups (a duplicates page); `search --html` passes
    // rows (a flat gallery). A static export has no server behind it, so `nav`
    // is None: every section link would be dead when opened from `file://`.
    let (items, groups, view) = match flat {
        None => (Vec::new(), groups.to_vec(), View::Duplicates),
        Some(rows) => (rows.to_vec(), Vec::new(), View::All),
    };
    let set = RenderSet {
        stats,
        items,
        groups,
        faces_by_hash,
        nav: None,
        view,
        options: RenderOptions {
            live: false,
            heic: false,
            heic_original: false,
            embedded: None,
            db_path,
        },
    };
    let html = render(&set);
    std::fs::write(output, &html)
        .map_err(|e| anyhow::anyhow!("failed to write {}: {e}", output.display()))?;
    eprintln!("Wrote {} ({} KB)", output.display(), html.len() / 1024);
    Ok(())
}

/// Which gallery a [`RenderSet`] describes. Replaces the old all_files/keep_files
/// slice signalling: `Date` was `keep_files.is_some()`, `Duplicates` was the
/// `groups_view` flag, `All` was the remainder.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum View {
    All,
    Date,
    Duplicates,
}

/// Rendering knobs that are not the data themselves.
pub(crate) struct RenderOptions {
    pub live: bool,
    pub heic: bool,
    pub heic_original: bool,
    pub embedded: Option<usize>,
    pub db_path: String,
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
        View::Duplicates => (None, None),
    };
    let embedded = set.options.embedded;
    let heic = set.options.heic;
    let heic_original = set.options.heic_original;
    let faces_by_hash = &set.faces_by_hash;
    let live = set.options.live;
    let nav = set.nav;
    let groups_view = set.view == View::Duplicates;

    use askama::Template;
    use chrono::Utc;
    let now = Utc::now().format("%Y-%m-%d %H:%M UTC").to_string();

    // In server mode HEIC thumbnails are converted lazily per request via
    // /api/files/{hash}/raw (see handle_raw_file). Converting every HEIC eagerly here made
    // server startup take minutes on a collection with many of them; the static
    // path pays that cost once at generation time instead.
    let heic = heic && !live;
    let heic_original = heic_original && !live;

    let data = build_data_block(
        nav,
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
    nav: Option<Section>,
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
        "<script>\nvar LIVE_SERVER={live};\nvar GVIEW={};\nvar PEOPLE_ROOT={};\n</script>\n",
        json_str(view),
        // `nav` is Some only under `videre gallery`, which is the one
        // configuration with a `/people`. See `people_root`.
        json_str(people_root(nav.is_some()))
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
