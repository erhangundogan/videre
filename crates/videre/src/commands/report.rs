use axum::extract::{Json as AxumJson, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::Router;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

#[derive(clap::Args)]
pub struct ReportArgs {
    /// SQLite database (default: resolved from ~/.videre; see 'videre config')
    #[arg(long)]
    db: Option<PathBuf>,

    /// HTML output path [default: <db>_report.html]
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Embed HEIC thumbnails as base64 JPEG (requires qlmanage, macOS only; increases HTML size)
    #[arg(long)]
    heic: bool,

    /// Embed HEIC thumbnails + full lightbox version (requires qlmanage, macOS only; significantly increases HTML size)
    #[arg(long)]
    heic_original: bool,

    /// Include every file (singular and duplicate) in a searchable gallery
    #[arg(long)]
    all: bool,

    /// Start a local face-labeling HTTP server on port 7878
    #[arg(long)]
    faces: bool,

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
}

struct FileRow {
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

struct Stats {
    total_files: i64,
    duplicate_groups: i64,
    duplicate_files: i64,
    wasted_bytes: i64,
}

struct VectorBlock {
    hashes: Vec<String>,
    b64: String,
    dim: usize,
}

/// Load all embeddings for the default model, ordered by hash, as one
/// base64-encoded f16 buffer. Returns None when the table is missing or empty.
/// Rows whose blob length disagrees with the first valid row's dimension are
/// skipped (mirrors search.rs semantics for corrupt rows).
fn query_vectors(conn: &Connection) -> Option<VectorBlock> {
    let table_exists = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='embeddings'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .map(|n| n > 0)
        .unwrap_or(false);
    if !table_exists {
        return None;
    }
    let mut stmt = conn
        .prepare(
            "SELECT hash, embedding FROM embeddings WHERE model_id = ?1 \
             AND hash IN (SELECT hash FROM file_hashes) ORDER BY hash",
        )
        .ok()?;
    let rows: Vec<(String, Vec<u8>)> = stmt
        .query_map([videre_core::embeddings::DEFAULT_MODEL_ID], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .ok()?
        .filter_map(|r| r.ok())
        .collect();
    let first_len = rows.iter().map(|(_, b)| b.len()).find(|l| *l > 0 && l % 2 == 0)?;
    let dim = first_len / 2;
    let mut blob = Vec::with_capacity(rows.len() * first_len);
    let mut hashes = Vec::with_capacity(rows.len());
    for (hash, bytes) in rows {
        if bytes.len() != first_len {
            continue;
        }
        blob.extend_from_slice(&bytes);
        hashes.push(hash);
    }
    if hashes.is_empty() {
        return None;
    }
    Some(VectorBlock { hashes, b64: base64_encode(&blob), dim })
}

fn best_date(r: &FileRow) -> &str {
    if let Some(d) = r.exif_date.as_deref() {
        if !d.starts_with("0000") { return d; }
    }
    match (r.created_at.as_deref(), r.modified_at.as_deref()) {
        (Some(c), Some(m)) => if c < m { c } else { m },
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
        out.push(if chunk.len() > 1 { CHARS[((n >> 6) & 63) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { CHARS[(n & 63) as usize] as char } else { '=' });
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
    let img = videre_core::heic::heic_via_quicklook(path, &format!("b64_{max_px}"))?;
    let img = if img.width() > max_px || img.height() > max_px {
        img.resize(max_px, max_px, image::imageops::FilterType::Triangle)
    } else {
        img
    };
    let mut buf = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Jpeg)
        .ok()?;
    Some(base64_encode(&buf))
}

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

fn format_bytes(bytes: i64) -> String {
    if bytes >= 1_073_741_824 {
        format!("{:.1} GB", bytes as f64 / 1_073_741_824.0)
    } else if bytes >= 1_048_576 {
        format!("{:.1} MB", bytes as f64 / 1_048_576.0)
    } else if bytes >= 1024 {
        format!("{:.0} KB", bytes as f64 / 1024.0)
    } else {
        format!("{} B", bytes)
    }
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"'  => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '<'  => out.push_str("\\u003c"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c    => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
fn file_to_json(f: &FileRow, heic: bool, heic_original: bool) -> String {
    file_to_json_with_faces(f, heic, heic_original, &[])
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

    let cr = f.created_at.as_deref()
        .map(|d| json_str(&d[..d.len().min(19)]))
        .unwrap_or_else(|| "null".to_string());
    let mo = f.modified_at.as_deref()
        .map(|d| json_str(&d[..d.len().min(19)]))
        .unwrap_or_else(|| "null".to_string());
    let ex = f.exif_date.as_deref()
        .map(json_str)
        .unwrap_or_else(|| "null".to_string());
    let lat = f.gps_lat.map(|v| format!("{:.6}", v)).unwrap_or_else(|| "null".to_string());
    let lon = f.gps_lon.map(|v| format!("{:.6}", v)).unwrap_or_else(|| "null".to_string());
    let w = f.width.map(|v| v.to_string()).unwrap_or_else(|| "null".to_string());
    let h = f.height.map(|v| v.to_string()).unwrap_or_else(|| "null".to_string());

    let faces_json: Vec<String> = faces
        .iter()
        .filter_map(|(id, name, bbox)| {
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
        ext  = json_str(&f.ext),
        size = f.size_bytes,
        cr = cr, mo = mo, ex = ex,
        lat = lat, lon = lon, w = w, h = h,
        tb = tb, fb = fb,
        faces = faces_json.join(","),
        loc = loc,
    )
}

fn group_to_json(
    group: &[FileRow],
    heic: bool,
    heic_original: bool,
    faces_by_hash: &HashMap<String, Vec<(i64, String, String)>>,
) -> String {
    let hash_prefix = &group[0].hash[..group[0].hash.len().min(8)];
    let waste = group[0].size_bytes * (group.len() as i64 - 1);
    let keep_date = best_date(&group[0]);
    let date_json = if keep_date.is_empty() { "null".to_string() } else { json_str(keep_date) };
    let files_json: Vec<String> = group.iter()
        .map(|f| {
            file_to_json_with_faces(
                f,
                heic,
                heic_original,
                faces_by_hash.get(&f.hash).map(|v| v.as_slice()).unwrap_or(&[]),
            )
        })
        .collect();
    format!(
        "{{\"hash\":{hash},\"waste\":{waste},\"date\":{date},\"files\":[{files}]}}",
        hash  = json_str(hash_prefix),
        waste = waste,
        date  = date_json,
        files = files_json.join(","),
    )
}

fn query_stats(conn: &Connection) -> Stats {
    let total_files: i64 = conn
        .query_row("SELECT COUNT(*) FROM file_hashes", [], |r| r.get(0))
        .unwrap_or(0);
    let duplicate_groups: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM \
             (SELECT hash FROM file_hashes GROUP BY hash HAVING COUNT(*) > 1)",
            [], |r| r.get(0),
        )
        .unwrap_or(0);
    let duplicate_files: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM file_hashes \
             WHERE hash IN (SELECT hash FROM file_hashes GROUP BY hash HAVING COUNT(*) > 1)",
            [], |r| r.get(0),
        )
        .unwrap_or(0);
    let wasted_bytes: i64 = conn
        .query_row(
            "SELECT COALESCE(SUM(size_bytes * (cnt - 1)), 0) FROM \
             (SELECT hash, size_bytes, COUNT(*) as cnt \
              FROM file_hashes GROUP BY hash HAVING cnt > 1)",
            [], |r| r.get(0),
        )
        .unwrap_or(0);
    Stats { total_files, duplicate_groups, duplicate_files, wasted_bytes }
}

fn query_groups(conn: &Connection) -> Vec<Vec<FileRow>> {
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
    .collect()
}

/// Per-hash KEEP-only file set: like query_all_files(), but for hashes with
/// more than one surviving path, only the earliest-by-best_date() row is
/// kept (mirrors query_groups()'s sort-then-take-first rule). Hashes with a
/// single surviving path are trivially KEEP. Used by --by-date so REMOVE-side
/// duplicates never appear in the date-grouped gallery.
fn query_keep_files(conn: &Connection) -> Vec<FileRow> {
    let rows = query_all_files(conn);

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

fn generate_html(
    db_path: &str,
    stats: &Stats,
    groups: &[Vec<FileRow>],
    all_files: Option<&[FileRow]>,
    keep_files: Option<&[FileRow]>,
    vectors: Option<&VectorBlock>,
    heic: bool,
    heic_original: bool,
    faces_by_hash: &HashMap<String, Vec<(i64, String, String)>>,
    live: bool,
) -> String {
    use chrono::Utc;
    let now = Utc::now().format("%Y-%m-%d %H:%M UTC").to_string();

    // In server mode, HEIC thumbnails are converted lazily per-request via
    // /api/raw (see handle_raw_file) instead of eagerly here - eagerly
    // converting every HEIC file with QuickLook before returning any
    // response made server mode take minutes on a collection with many
    // HEIC files. Static mode keeps the eager --heic/--heic-original
    // behavior, since it only pays that cost once at generation time.
    let heic = heic && !live;
    let heic_original = heic_original && !live;

    let mut out = String::with_capacity(512 * 1024);

    out.push_str(concat!(
        "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n",
        "<meta charset=\"UTF-8\">\n",
        "<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\n",
        "<title>videre report</title>\n<style>\n",
        "*,*::before,*::after{box-sizing:border-box;margin:0;padding:0}\n",
        "body{font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',sans-serif;",
        "background:#f4f4f5;color:#18181b;font-size:14px;line-height:1.5}\n",
        ".header{background:#18181b;color:#fff;padding:24px 32px}\n",
        ".header h1{font-size:20px;font-weight:700;margin-bottom:2px}\n",
        ".subtitle{color:#71717a;font-size:12px;font-family:monospace;margin-bottom:20px}\n",
        ".stats{display:flex;gap:16px;flex-wrap:wrap}\n",
        ".stat{background:#27272a;border-radius:8px;padding:12px 20px;min-width:130px}\n",
        ".num{font-size:22px;font-weight:700;display:block}\n",
        ".label{font-size:11px;color:#a1a1aa;text-transform:uppercase;letter-spacing:.06em}\n",
        ".stat.warn .num{color:#fbbf24}\n",
        ".toolbar{padding:10px 32px;background:#fff;border-bottom:1px solid #e4e4e7;",
        "display:flex;gap:8px;align-items:center;position:sticky;top:0;z-index:10;",
        "box-shadow:0 1px 3px rgba(0,0,0,.06)}\n",
        "button{padding:5px 12px;border:1px solid #d4d4d8;background:#fff;",
        "border-radius:6px;cursor:pointer;font-size:12px;color:#3f3f46}\n",
        "button:hover{background:#f4f4f5;border-color:#a1a1aa}\n",
        ".sort-label{font-size:12px;color:#3f3f46;display:flex;align-items:center;gap:6px}\n",
        ".sort-label select{padding:4px 8px;border:1px solid #d4d4d8;border-radius:6px;",
        "font-size:12px;background:#fff;color:#3f3f46;cursor:pointer}\n",
        ".info{margin-left:auto;color:#a1a1aa;font-size:12px}\n",
        ".groups{padding:16px 32px;display:flex;flex-direction:column;gap:10px}\n",
        ".group{background:#fff;border-radius:10px;border:1px solid #e4e4e7;overflow:hidden}\n",
        ".group-header{padding:12px 16px;cursor:pointer;display:flex;align-items:center;",
        "gap:10px;user-select:none}\n",
        ".group-header:hover{background:#fafafa}\n",
        ".arrow{font-size:9px;color:#a1a1aa;transition:transform .15s;display:inline-block;",
        "width:10px;flex-shrink:0}\n",
        ".group.open .arrow{transform:rotate(90deg)}\n",
        ".hash{font-family:monospace;font-size:12px;background:#f4f4f5;",
        "padding:2px 8px;border-radius:4px;color:#52525b;flex-shrink:0}\n",
        ".group-meta{font-size:13px;color:#71717a}\n",
        ".waste{margin-left:auto;font-size:12px;font-weight:600;color:#dc2626;flex-shrink:0}\n",
        ".group-body{display:none;border-top:1px solid #f4f4f5;overflow-x:auto}\n",
        ".group.open .group-body{display:block}\n",
        "table{width:100%;border-collapse:collapse;font-size:13px}\n",
        "th{background:#fafafa;padding:7px 12px;text-align:left;font-size:11px;",
        "font-weight:600;text-transform:uppercase;letter-spacing:.05em;color:#71717a;",
        "border-bottom:1px solid #e4e4e7;white-space:nowrap}\n",
        "td{padding:8px 12px;border-bottom:1px solid #f4f4f5;vertical-align:middle}\n",
        "tr:last-child td{border-bottom:none}\n",
        "tr.keep td{background:#f0fdf4}\n",
        "tr.remove:hover td{background:#fef2f2}\n",
        ".badge span{padding:2px 7px;border-radius:4px;font-size:11px;font-weight:700;",
        "letter-spacing:.04em;white-space:nowrap}\n",
        ".keep-badge{background:#dcfce7;color:#166534}\n",
        ".remove-badge{background:#fee2e2;color:#991b1b}\n",
        ".filename{font-weight:500;white-space:nowrap;max-width:220px;overflow:hidden;",
        "text-overflow:ellipsis}\n",
        ".path-cell{font-family:monospace;font-size:11px;max-width:380px;",
        "white-space:nowrap;overflow:hidden;text-overflow:ellipsis}\n",
        ".path-text{color:#3f3f46}\n",
        ".copy-btn{margin-left:4px;padding:1px 5px;font-size:11px;vertical-align:middle;",
        "opacity:.5;border-radius:4px}\n",
        ".copy-btn:hover{opacity:1}\n",
        ".dim{color:#a1a1aa;font-size:12px}\n",
        ".gps a{color:#3b82f6;text-decoration:none;font-size:12px}\n",
        ".gps a:hover{text-decoration:underline}\n",
        ".no-dupes{padding:48px;text-align:center;color:#71717a}\n",
        "td.preview{width:130px;text-align:center;vertical-align:middle;padding:6px 10px}\n",
        "th.preview-th{width:130px}\n",
        ".thumb{max-width:120px;max-height:120px;object-fit:contain;border-radius:6px;",
        "display:block;margin:0 auto;cursor:zoom-in;transition:transform .15s}\n",
        ".thumb:hover{transform:scale(1.05)}\n",
        ".no-prev{color:#a1a1aa;font-size:11px;display:block;text-align:center}\n",
        ".lightbox{display:none;position:fixed;inset:0;background:rgba(0,0,0,.85);",
        "z-index:1000;align-items:center;justify-content:center;cursor:zoom-out}\n",
        ".lightbox.on{display:flex}\n",
        ".lightbox img,.lightbox video{max-width:90vw;max-height:90vh;object-fit:contain;",
        "border-radius:8px;box-shadow:0 8px 40px rgba(0,0,0,.6)}\n",
        ".lb-meta{position:absolute;bottom:0;left:0;right:0;background:rgba(24,24,27,.85);",
        "padding:10px 16px;display:none;gap:12px;align-items:flex-start;flex-wrap:wrap}\n",
        ".lb-meta.on{display:flex}\n",
        ".lb-face{text-align:center;font-size:11px;color:#fff}\n",
        ".lb-face img{width:48px;height:48px;border-radius:50%;object-fit:cover;display:block;margin-bottom:4px}\n",
        ".lb-face a{color:#fff;text-decoration:underline}\n",
        ".lb-location{color:#e4e4e7;font-size:12px;align-self:center}\n",
        "#sort-overlay{display:none;position:fixed;inset:0;background:rgba(0,0,0,.45);",
        "z-index:2000;align-items:center;justify-content:center}\n",
        ".sort-card{background:#fff;border-radius:12px;padding:22px 36px;",
        "display:flex;align-items:center;gap:14px;",
        "box-shadow:0 8px 32px rgba(0,0,0,.28);font-size:15px;font-weight:600;color:#3f3f46}\n",
        ".spinner{width:22px;height:22px;border:3px solid #e4e4e7;",
        "border-top-color:#3b82f6;border-radius:50%;animation:spin .7s linear infinite;flex-shrink:0}\n",
        "@keyframes spin{to{transform:rotate(360deg)}}\n",
        ".more-wrap{text-align:center;padding:16px 0 32px}\n",
        "#more-btn{padding:8px 28px;font-size:13px;display:none}\n",
        ".results-panel{margin:16px 32px;padding:14px 16px;background:#fff;",
        "border:1px solid #e4e4e7;border-radius:10px;scroll-margin-top:56px}\n",
        ".results-head{display:flex;align-items:center;gap:10px;margin-bottom:10px}\n",
        ".results-head h2{font-size:14px}\n",
        ".results-strip{display:flex;gap:10px;overflow-x:auto;padding-bottom:6px}\n",
        ".rcard{flex:0 0 auto;width:132px;text-align:center;position:relative}\n",
        ".rcard .thumb{max-width:120px;max-height:120px}\n",
        ".rcard.query{border-right:2px solid #e4e4e7;padding-right:10px;margin-right:4px}\n",
        ".score{position:absolute;top:4px;left:8px;background:rgba(24,24,27,.75);color:#fff;",
        "font-size:10px;padding:1px 5px;border-radius:4px}\n",
        ".copies{position:absolute;top:4px;right:8px;background:#fbbf24;color:#18181b;",
        "font-size:10px;font-weight:700;padding:1px 5px;border-radius:4px}\n",
        ".rname{font-size:11px;white-space:nowrap;overflow:hidden;text-overflow:ellipsis;",
        "color:#52525b;margin-top:2px}\n",
        ".gallery-head{padding:20px 32px 4px;display:flex;align-items:baseline;gap:12px}\n",
        ".gallery-head h2{font-size:16px}\n",
        ".gallery{padding:12px 32px;display:grid;",
        "grid-template-columns:repeat(auto-fill,minmax(150px,1fr));gap:10px}\n",
        ".card{background:#fff;border:1px solid #e4e4e7;border-radius:10px;padding:8px;",
        "text-align:center;position:relative}\n",
        ".card .thumb{max-width:100%;max-height:130px}\n",
        ".card-meta{font-size:11px;color:#71717a;margin-top:4px;white-space:nowrap;",
        "overflow:hidden;text-overflow:ellipsis}\n",
        ".similar-btn{margin-top:6px;padding:2px 10px;font-size:11px}\n",
        ".date-view{padding:24px 32px}\n",
        ".date-breadcrumb{margin-bottom:16px;font-size:13px;color:#71717a}\n",
        ".date-breadcrumb a{color:#3f3f46;cursor:pointer;text-decoration:underline}\n",
        ".date-grid{display:grid;grid-template-columns:repeat(auto-fill,minmax(160px,1fr));gap:12px}\n",
        ".date-card{background:#fff;border-radius:8px;overflow:hidden;cursor:pointer;",
        "box-shadow:0 1px 3px rgba(0,0,0,.08)}\n",
        ".date-card img{width:100%;aspect-ratio:1;object-fit:cover;display:block}\n",
        ".date-card .date-card-label{padding:8px;font-size:13px;font-weight:600}\n",
        ".date-card .date-card-count{padding:0 8px 8px;font-size:11px;color:#71717a}\n",
        // Shimmer placeholder for HEIC thumbnails while /api/raw converts
        // them lazily (server mode) - cleared via onload once the image
        // paints, so the animation never runs behind a loaded image.
        "img.heic-loading{display:block;width:100%;aspect-ratio:1;object-fit:cover;",
        "background:linear-gradient(90deg,#e4e4e7 25%,#f4f4f5 37%,#e4e4e7 63%);",
        "background-size:400% 100%;animation:heicShimmer 1.4s ease infinite}\n",
        "@keyframes heicShimmer{0%{background-position:100% 0}100%{background-position:0 0}}\n",
        "</style>\n</head>\n<body>\n",
        "<div id=\"sort-overlay\"><div class=\"sort-card\">",
        "<div class=\"spinner\"></div>Sorting&hellip;</div></div>\n",
        "<div class=\"lightbox\" id=\"lb\" onclick=\"closeLb()\">\n",
        "  <img id=\"lb-img\" src=\"\" alt=\"\" onclick=\"event.stopPropagation()\">\n",
        "  <video id=\"lb-vid\" src=\"\" controls autoplay onclick=\"event.stopPropagation()\" style=\"display:none\"></video>\n",
        "  <div class=\"lb-meta\" id=\"lbMeta\" onclick=\"event.stopPropagation()\"></div>\n",
        "</div>\n",
    ));

    // Header
    let embedded_stat = match vectors {
        Some(vb) => format!(
            "<div class=\"stat\"><span class=\"num\">{}</span><span class=\"label\">Embedded</span></div>",
            vb.hashes.len()
        ),
        None => String::new(),
    };
    // The three duplicate-related tiles are only useful when there's
    // something to report - an all-zero "Duplicate groups / Duplicate files
    // / Wasted space" row is noise on a collection with no duplicates,
    // especially alongside --by-date/--all.
    let dupe_stats = if groups.is_empty() {
        String::new()
    } else {
        format!(
            "<div class=\"stat warn\"><span class=\"num\">{groups}</span><span class=\"label\">Duplicate groups</span></div>\
             <div class=\"stat warn\"><span class=\"num\">{dups}</span><span class=\"label\">Duplicate files</span></div>\
             <div class=\"stat warn\"><span class=\"num\">{wasted}</span><span class=\"label\">Wasted space</span></div>",
            groups = stats.duplicate_groups,
            dups   = stats.duplicate_files,
            wasted = format_bytes(stats.wasted_bytes),
        )
    };
    out.push_str(&format!(
        "<div class=\"header\">\
          <h1>videre report</h1>\
          <p class=\"subtitle\">{db} &mdash; {now}</p>\
          <div class=\"stats\">\
            <div class=\"stat\"><span class=\"num\">{total}</span><span class=\"label\">Files scanned</span></div>\
            {dupe_stats}\
            {embedded_stat}\
          </div>\
        </div>\n",
        db     = esc(db_path),
        now    = now,
        total  = stats.total_files,
        dupe_stats = dupe_stats,
        embedded_stat = embedded_stat,
    ));

    // Toolbar + groups list: skip entirely when there's nothing to review -
    // an empty "0 groups" toolbar with working Expand/Collapse/Sort controls
    // is just noise, especially alongside --by-date/--all which have their
    // own reason to exist regardless of duplicate count.
    if !groups.is_empty() {
        out.push_str(&format!(
            "<div class=\"toolbar\">\
              <button onclick=\"expandAll()\">Expand all</button>\
              <button onclick=\"collapseAll()\">Collapse all</button>\
              <label class=\"sort-label\">Sort by\
                <select id=\"sort-select\" onchange=\"sortGroups(this.value)\">\
                  <option value=\"waste\">Wasted space</option>\
                  <option value=\"date-asc\">Date kept (oldest first)</option>\
                  <option value=\"date-desc\">Date kept (newest first)</option>\
                </select>\
              </label>\
              <span class=\"info\" id=\"shown-info\">{} groups</span>\
            </div>\n",
            stats.duplicate_groups,
        ));

        // Empty groups container — JS fills it
        out.push_str("<div class=\"groups\" id=\"groups-container\"></div>\n");
        out.push_str("<div class=\"more-wrap\"><button id=\"more-btn\" onclick=\"showMore()\"></button></div>\n");
    }

    if all_files.is_some() {
        out.push_str("<div class=\"results-panel\" id=\"results\" style=\"display:none\"></div>\n");
    }

    if let Some(files) = all_files {
        out.push_str(&format!(
            "<div class=\"gallery-head\"><h2>All files</h2><span class=\"info\" id=\"gallery-info\">{} files</span></div>\n\
             <div class=\"gallery\" id=\"gallery\"></div>\n\
             <div class=\"more-wrap\"><button id=\"gallery-more\" onclick=\"showMoreGallery()\"></button></div>\n",
            files.len()
        ));
    }

    if keep_files.is_some() {
        out.push_str(concat!(
            "<div class=\"date-view\" id=\"dateView\">\n",
            "<h2>Browse by date</h2>\n",
            "<div class=\"date-breadcrumb\" id=\"dateBreadcrumb\"></div>\n",
            "<div class=\"date-grid\" id=\"dateGrid\"></div>\n",
            "</div>\n",
        ));
    }

    // In server mode (--show-faces), thumbnails/lightbox point at
    // /api/raw?path=... instead of file://, since browsers refuse to load a
    // file:// subresource from an http://-served page. Static mode keeps
    // file:// links, since the report itself is opened via file:// there.
    out.push_str(&format!("<script>\nvar LIVE_SERVER={};\n</script>\n", live));

    // Embed all group data as JSON
    out.push_str("<script>\nvar GROUPS=[\n");
    for (i, group) in groups.iter().enumerate() {
        if i > 0 { out.push(','); }
        out.push('\n');
        out.push_str(&group_to_json(group, heic, heic_original, faces_by_hash));
    }
    out.push_str("\n];\n");

    // All-files gallery data and similarity vectors (--all only).
    // Without --all nothing is emitted so the page is unchanged.
    if let Some(files) = all_files {
        out.push_str("var ALLFILES=[\n");
        for (i, f) in files.iter().enumerate() {
            if i > 0 { out.push(','); }
            out.push_str(&file_to_json_with_faces(
                f,
                heic,
                heic_original,
                faces_by_hash.get(&f.hash).map(|v| v.as_slice()).unwrap_or(&[]),
            ));
        }
        out.push_str("\n];\n");
        match vectors {
            Some(vb) => {
                out.push_str(&format!("var VEC_DIM={};\n", vb.dim));
                out.push_str("var VEC_HASHES=[");
                for (i, h) in vb.hashes.iter().enumerate() {
                    if i > 0 { out.push(','); }
                    out.push_str(&json_str(h));
                }
                out.push_str("];\n");
                out.push_str("var VEC_B64=\"");
                out.push_str(&vb.b64);
                out.push_str("\";\n");
            }
            None => {
                out.push_str("var VEC_DIM=0;\nvar VEC_HASHES=[];\nvar VEC_B64=\"\";\n");
            }
        }
    }

    // Date-grouped KEEP-only file set (--by-date only).
    if let Some(kf) = keep_files {
        out.push_str("var KEEPFILES=[\n");
        for (i, f) in kf.iter().enumerate() {
            if i > 0 { out.push(','); }
            out.push_str(&file_to_json_with_faces(
                f,
                heic,
                heic_original,
                faces_by_hash.get(&f.hash).map(|v| v.as_slice()).unwrap_or(&[]),
            ));
        }
        out.push_str("\n];\n");
    }

    // All rendering JS using raw string to avoid escaping hell
    out.push_str(r#"
var PAGE=100,sorted=GROUPS.slice(),shown=0;

function escA(s){
  return String(s).replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;').replace(/"/g,'&quot;');
}
function escH(s){
  return s?String(s).replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;'):'';
}
function fmtB(b){
  if(b>=1073741824)return(b/1073741824).toFixed(1)+' GB';
  if(b>=1048576)return(b/1048576).toFixed(1)+' MB';
  if(b>=1024)return Math.round(b/1024)+' KB';
  return b+' B';
}
function rawUrl(path){
  return LIVE_SERVER ? '/api/raw?path='+encodeURIComponent(path) : 'file://'+path;
}
function buildPreview(f){
  var ext=f.ext,path=f.path;
  var metaAttr=escA(JSON.stringify(f.meta));
  if(ext==='jpg'||ext==='jpeg'||ext==='png'||ext==='gif'||ext==='webp'||ext==='bmp'){
    var url=rawUrl(path);
    return '<a href="'+escA(url)+'" target="_blank" data-lb-url="'+escA(url)+'" data-lb-type="image" '+
      'data-lb-meta="'+metaAttr+'">'+
      '<img src="'+escA(url)+'" class="thumb" loading="lazy" '+
      'onerror="this.parentElement.innerHTML=\'<span class=no-prev>no preview</span>\'"></a>';
  }
  if(ext==='heic'){
    if(LIVE_SERVER){
      var thumbUrl=rawUrl(path)+'&size=240';
      var lbUrl=rawUrl(path)+'&size=1200';
      return '<img src="'+escA(thumbUrl)+'" class="thumb heic-loading" loading="lazy" data-lb-url="'+escA(lbUrl)+'" '+
        'data-lb-type="image" data-lb-meta="'+metaAttr+'" '+
        'onload="this.classList.remove(\'heic-loading\')" '+
        'onerror="this.parentElement.innerHTML=\'<span class=no-prev>no preview</span>\'">';
    }
    if(f.tb){
      var src='data:image/jpeg;base64,'+f.tb;
      var lb=f.fb?'data:image/jpeg;base64,'+f.fb:src;
      return '<img src="'+src+'" class="thumb" data-lb-url="'+escA(lb)+'" data-lb-type="image" '+
        'data-lb-meta="'+metaAttr+'">';
    }
    return '<span class="no-prev">HEIC</span>';
  }
  if(ext==='tiff')return '<span class="no-prev">TIFF</span>';
  if(ext==='dng') return '<span class="no-prev">DNG</span>';
  if(ext==='mov'||ext==='mp4'){
    var url=rawUrl(path);
    return '<video src="'+escA(url)+'" class="thumb" preload="metadata" muted playsinline '+
      'data-lb-url="'+escA(url)+'" data-lb-type="video" '+
      'data-lb-meta="'+metaAttr+'" '+
      'onerror="this.outerHTML=\'<span class=no-prev>no preview</span>\'"></video>';
  }
  return '<span class="no-prev">&mdash;</span>';
}
function buildRow(f,isKeep){
  var rc=isKeep?'keep':'remove';
  var bc=isKeep?'keep-badge':'remove-badge';
  var bt=isKeep?'KEEP':'REMOVE';
  var fname=f.path.split('/').pop()||f.path;
  var cr=f.cr||'<span class="dim">—</span>';
  var mo=f.mo||'<span class="dim">—</span>';
  var ex=f.ex||'<span class="dim">—</span>';
  var gps='<span class="dim">—</span>';
  if(f.lat!=null&&f.lon!=null){
    gps='<div class="gps"><a href="https://maps.google.com/?q='+f.lat.toFixed(6)+','+f.lon.toFixed(6)+
      '" target="_blank" rel="noopener">'+Math.abs(f.lat).toFixed(4)+'&deg;'+(f.lat>=0?'N':'S')+' '+
      Math.abs(f.lon).toFixed(4)+'&deg;'+(f.lon>=0?'E':'W')+'</a></div>';
  }
  var dims=(f.w&&f.h)?f.w+'×'+f.h:'<span class="dim">—</span>';
  return '<tr class="'+rc+'">'+
    '<td class="preview">'+buildPreview(f)+'</td>'+
    '<td class="badge"><span class="'+bc+'">'+bt+'</span>'+similarBtn(f.hash)+'</td>'+
    '<td class="filename" title="'+escA(fname)+'">'+escH(fname)+'</td>'+
    '<td class="path-cell"><span class="path-text">'+escH(f.path)+'</span>'+
    '<button class="copy-btn" data-path="'+escA(f.path)+'" title="Copy path">&#x2398;</button></td>'+
    '<td>'+fmtB(f.size)+'</td>'+
    '<td class="dim">'+cr+'</td>'+
    '<td class="dim">'+mo+'</td>'+
    '<td class="dim">'+ex+'</td>'+
    '<td>'+gps+'</td>'+
    '<td class="dim">'+dims+'</td>'+
    '</tr>';
}
function buildGroup(g,idx){
  var rows=g.files.map(function(f,j){return buildRow(f,j===0);}).join('');
  return '<div class="group" id="g'+idx+'">'+
    '<div class="group-header">'+
    '<span class="arrow">&#9654;</span>'+
    '<code class="hash">'+escH(g.hash)+'</code>'+
    '<span class="group-meta">'+g.files.length+' copies &middot; '+fmtB(g.files[0].size)+' each</span>'+
    '<span class="waste">&minus;'+fmtB(g.waste)+' wasted</span>'+
    '</div>'+
    '<div class="group-body">'+
    '<table><thead><tr>'+
    '<th class="preview-th">Preview</th>'+
    '<th>Status</th><th>Filename</th><th>Path</th>'+
    '<th>Size</th><th>Created</th><th>Modified</th><th>EXIF date</th>'+
    '<th>GPS</th><th>Dimensions</th>'+
    '</tr></thead><tbody>'+rows+'</tbody></table></div></div>';
}
function render(reset){
  var overlay=document.getElementById('sort-overlay');
  var container=document.getElementById('groups-container');
  if(!container){if(overlay)overlay.style.display='none';return;}
  if(reset){shown=0;container.innerHTML='';}
  var end=Math.min(shown+PAGE,sorted.length);
  var html='';
  for(var i=shown;i<end;i++)html+=buildGroup(sorted[i],i);
  var tmp=document.createElement('div');
  tmp.innerHTML=html;
  while(tmp.firstChild)container.appendChild(tmp.firstChild);
  shown=end;
  updateBtn();
  overlay.style.display='none';
}
function updateBtn(){
  var btn=document.getElementById('more-btn');
  if(!btn)return;
  var rem=sorted.length-shown;
  if(rem>0){btn.style.display='inline-block';btn.textContent='Show more ('+rem+' remaining)';}
  else btn.style.display='none';
}
function showMore(){render(false);}
function toggle(id){
  var g=document.getElementById(id);
  g.classList.toggle('open');
  if(g.classList.contains('open')){
    g.querySelectorAll('img').forEach(function(img){if(img.loading==='lazy')img.loading='eager';});
    g.querySelectorAll('video').forEach(function(v){if(v.preload==='metadata')v.preload='auto';});
  }
}
function expandAll(){
  document.querySelectorAll('.group').forEach(function(g){
    g.classList.add('open');
    g.querySelectorAll('img').forEach(function(img){if(img.loading==='lazy')img.loading='eager';});
    g.querySelectorAll('video').forEach(function(v){if(v.preload==='metadata')v.preload='auto';});
  });
}
function collapseAll(){document.querySelectorAll('.group').forEach(function(g){g.classList.remove('open');});}
function copyPath(p){
  navigator.clipboard.writeText(p).catch(function(){
    var t=document.createElement('textarea');t.value=p;
    document.body.appendChild(t);t.select();document.execCommand('copy');
    document.body.removeChild(t);
  });
}
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
function openLb(url,type,metaJson){
  var meta = null;
  try { meta = metaJson ? JSON.parse(metaJson) : null; } catch(e) {}
  renderMetaPanel(meta);
  var img=document.getElementById('lb-img');
  var vid=document.getElementById('lb-vid');
  if(type==='video'){
    img.style.display='none';vid.style.display='block';
    vid.src=url;vid.play();
  } else {
    vid.style.display='none';img.style.display='block';img.src=url;
  }
  document.getElementById('lb').classList.add('on');
}
function closeLb(){
  var vid=document.getElementById('lb-vid');
  vid.pause();vid.src='';
  document.getElementById('lb-img').src='';
  document.getElementById('lb').classList.remove('on');
}
function sortGroups(by){
  var overlay=document.getElementById('sort-overlay');
  overlay.style.display='flex';
  requestAnimationFrame(function(){
    requestAnimationFrame(function(){
      sorted.sort(function(a,b){
        if(by==='waste')return b.waste-a.waste;
        var da=a.date||'￿',db=b.date||'￿';
        return by==='date-asc'?da.localeCompare(db):db.localeCompare(da);
      });
      render(true);
    });
  });
}
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
// Event delegation: toggle, lightbox, copy — one listener for all dynamic content
document.addEventListener('click',function(e){
  var lb=e.target.closest('[data-lb-url]');
  if(lb){e.preventDefault();e.stopPropagation();openLb(lb.dataset.lbUrl,lb.dataset.lbType||'image',lb.dataset.lbMeta);return;}
  var cp=e.target.closest('[data-path]');
  if(cp){copyPath(cp.dataset.path);return;}
  var hdr=e.target.closest('.group-header');
  if(hdr){toggle(hdr.closest('.group').id);return;}
});
document.addEventListener('keydown',function(e){if(e.key==='Escape')closeLb();});
document.getElementById('lb').addEventListener('click',function(e){
  if(e.target===this)closeLb();
});
"#);

    out.push_str(r#"
// ---- All-files gallery and similarity search (active only with --all) ----
var GPAGE=200,gShown=0,HASH_FILES={},VECS=null,VEC_INDEX={};
function decodeVecs(b64,n,dim){
  var bin=atob(b64);
  var out=new Float32Array(n*dim);
  for(var i=0;i<n*dim;i++){
    var lo=bin.charCodeAt(i*2),hi=bin.charCodeAt(i*2+1);
    var h=(hi<<8)|lo;
    var s=(h&0x8000)?-1:1,e=(h>>10)&0x1f,f=h&0x3ff;
    if(e===0)out[i]=s*f*Math.pow(2,-24);
    else if(e===31)out[i]=f?NaN:s*Infinity;
    else out[i]=s*(1+f/1024)*Math.pow(2,e-15);
  }
  return out;
}
function bestDateJs(f){
  if(f.ex&&f.ex.indexOf('0000')!==0)return f.ex;
  if(f.cr&&f.mo)return f.cr<f.mo?f.cr:f.mo;
  return f.cr||f.mo||'';
}
function similarBtn(hash){
  if(!VECS||VEC_INDEX[hash]==null)return '';
  return '<button class="similar-btn" data-similar="'+escA(hash)+'">Similar</button>';
}
function buildCard(f){
  var fname=f.path.split('/').pop()||f.path;
  var copies=HASH_FILES[f.hash]&&HASH_FILES[f.hash].length>1?
    '<span class="copies">x'+HASH_FILES[f.hash].length+'</span>':'';
  return '<div class="card" data-hash="'+escA(f.hash)+'">'+copies+
    buildPreview(f)+
    '<div class="card-meta" title="'+escA(f.path)+'">'+escH(fname)+'</div>'+
    '<div class="card-meta">'+fmtB(f.size)+(bestDateJs(f)?' &middot; '+escH(bestDateJs(f)):'')+'</div>'+
    similarBtn(f.hash)+
    '</div>';
}
function renderGallery(){
  if(typeof ALLFILES==='undefined')return;
  var g=document.getElementById('gallery');
  var end=Math.min(gShown+GPAGE,ALLFILES.length);
  var html='';
  for(var i=gShown;i<end;i++)html+=buildCard(ALLFILES[i]);
  var tmp=document.createElement('div');
  tmp.innerHTML=html;
  while(tmp.firstChild)g.appendChild(tmp.firstChild);
  gShown=end;
  var btn=document.getElementById('gallery-more');
  var rem=ALLFILES.length-gShown;
  if(rem>0){btn.style.display='inline-block';btn.textContent='Show more ('+rem+' remaining)';}
  else btn.style.display='none';
}
function showMoreGallery(){renderGallery();}
function findSimilar(hash){
  var qi=VEC_INDEX[hash];
  if(qi==null||!VECS)return;
  var q=VECS.subarray(qi*VEC_DIM,(qi+1)*VEC_DIM);
  var scores=[];
  for(var i=0;i<VEC_HASHES.length;i++){
    if(i===qi)continue;
    var v=VECS.subarray(i*VEC_DIM,(i+1)*VEC_DIM);
    var dot=0;
    for(var d=0;d<VEC_DIM;d++)dot+=q[d]*v[d];
    if(isFinite(dot))scores.push([i,dot]);
  }
  scores.sort(function(a,b){return b[1]-a[1];});
  renderResults(hash,scores.slice(0,24));
}
function resultCard(hash,score,isQuery){
  var files=HASH_FILES[hash];
  if(!files||!files.length)return '';
  var f=files[0];
  var fname=f.path.split('/').pop()||f.path;
  var badge=isQuery?'':'<span class="score">'+score.toFixed(3)+'</span>';
  var copies=files.length>1?'<span class="copies">x'+files.length+'</span>':'';
  return '<div class="rcard'+(isQuery?' query':'')+'" data-hash="'+escA(hash)+'">'+
    badge+copies+buildPreview(f)+
    '<div class="rname" title="'+escA(f.path)+'">'+(isQuery?'query: ':'')+escH(fname)+'</div>'+
    '</div>';
}
function renderResults(qHash,scored){
  var panel=document.getElementById('results');
  var html='<div class="results-head"><h2>Similar images</h2>'+
    '<button onclick="clearResults()">Clear</button></div>'+
    '<div class="results-strip">'+resultCard(qHash,1,true);
  for(var i=0;i<scored.length;i++){
    html+=resultCard(VEC_HASHES[scored[i][0]],scored[i][1],false);
  }
  html+='</div>';
  panel.innerHTML=html;
  panel.style.display='block';
  panel.querySelectorAll('img').forEach(function(img){if(img.loading==='lazy')img.loading='eager';});
  panel.scrollIntoView({behavior:'smooth',block:'start'});
}
function clearResults(){
  var panel=document.getElementById('results');
  panel.style.display='none';
  panel.innerHTML='';
}
if(typeof ALLFILES!=='undefined'){
  ALLFILES.forEach(function(f){
    (HASH_FILES[f.hash]=HASH_FILES[f.hash]||[]).push(f);
  });
  if(VEC_HASHES.length>0){
    VECS=decodeVecs(VEC_B64,VEC_HASHES.length,VEC_DIM);
    for(var vi=0;vi<VEC_HASHES.length;vi++)VEC_INDEX[VEC_HASHES[vi]]=vi;
  }
  renderGallery();
}
document.addEventListener('click',function(e){
  var sb=e.target.closest('[data-similar]');
  if(sb){e.preventDefault();e.stopPropagation();findSimilar(sb.dataset.similar);}
});
render(true);
if(typeof KEEPFILES!=='undefined') buildYearView();
"#);

    out.push_str("</script>\n</body>\n</html>");
    out
}

// ---- Faces labeling server ----

const FACES_HTML: &str = r##"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <title>videre faces labeling</title>
  <style>
    :root {
      --blue-border: #6c8ebf;
      --blue-bg: #e8f0fe;
      --blue-text: #4a6da3;
      --blue-hover: #2a6db5;
      --green-border: #6cc36c;
      --green-bg: #eaf7ea;
      --green-text: #1a7a1a;
      --orange-border: #e2a03f;
      --orange-bg: #fdf1df;
      --orange-text: #8a5a00;
    }
    body { font-family: sans-serif; margin: 0; padding: 16px; background: #fff; }
    h2 { border-bottom: 1px solid #ccc; padding-bottom: 4px; }
    .toolbar { display: flex; gap: 8px; align-items: center; margin-bottom: 16px; }
    .grid { display: grid; grid-template-columns: repeat(auto-fill, 160px); gap: 12px; margin-bottom: 24px; }
    .card { background: white; border: 2px solid #ddd; border-radius: 8px; padding: 10px; width: 160px; box-sizing: border-box; cursor: grab; }
    .card.person-card { cursor: default; border-color: var(--blue-border); background: var(--blue-bg); transition: border-color 0.15s; }
    .card.person-card:hover { border-color: var(--blue-hover); }
    .card.drag-over { border-color: var(--blue-hover); background: #d0e4ff; }
    .card.cluster-card { border-color: var(--green-border); background: var(--green-bg); }
    .card.singleton-card { border-color: var(--orange-border); background: var(--orange-bg); }
    .badge { display: inline-flex; align-items: center; vertical-align: middle; border-radius: 12px; padding: 3px 9px; font-size: 12px; line-height: 1; margin-left: 4px; font-weight: 600; border: 1px solid transparent; }
    .badge-blue { background: var(--blue-bg); border-color: var(--blue-border); color: var(--blue-text); }
    .badge-green { background: var(--green-bg); border-color: var(--green-border); color: var(--green-text); }
    .badge-orange { background: var(--orange-bg); border-color: var(--orange-border); color: var(--orange-text); }
    h2.title-people { color: var(--blue-text); }
    h2.title-clusters { color: var(--green-text); }
    h2.title-singletons { color: var(--orange-text); }
    .new-person-area { margin-top: 8px; display: flex; flex-direction: column; gap: 6px; }
    .new-person-area button { flex: 1; }
    .new-person-area input[type=text] { width: 100%; box-sizing: border-box; }
    .np-btn-row { display: flex; gap: 4px; }
    .np-btn-row button { flex: 1; }
    .new-person-btn { background: #fff; font-weight: 600; transition: background 0.15s, color 0.15s; }
    .cluster-card .new-person-btn { border-color: var(--green-border); color: var(--green-text); }
    .cluster-card .new-person-btn:hover { background: var(--green-text); border-color: var(--green-text); color: #fff; }
    .singleton-card .new-person-btn { border-color: var(--orange-border); color: var(--orange-text); }
    .singleton-card .new-person-btn:hover { background: var(--orange-text); border-color: var(--orange-text); color: #fff; }
    .np-create-btn { font-weight: 600; color: #fff; transition: background 0.15s, border-color 0.15s; }
    .cluster-card .np-create-btn { background: var(--green-border); border-color: var(--green-border); }
    .cluster-card .np-create-btn:hover { background: var(--green-text); border-color: var(--green-text); }
    .singleton-card .np-create-btn { background: var(--orange-border); border-color: var(--orange-border); }
    .singleton-card .np-create-btn:hover { background: var(--orange-text); border-color: var(--orange-text); }
    .cluster-card .np-input { border-color: var(--green-border); color: var(--green-text); }
    .cluster-card .np-input:focus { outline: none; border-color: var(--green-text); box-shadow: 0 0 0 2px rgba(108,195,108,0.25); }
    .singleton-card .np-input { border-color: var(--orange-border); color: var(--orange-text); }
    .singleton-card .np-input:focus { outline: none; border-color: var(--orange-text); box-shadow: 0 0 0 2px rgba(226,160,63,0.25); }
    button { cursor: pointer; padding: 4px 10px; border-radius: 4px; border: 1px solid #999; background: white; }
    button.primary { background: var(--blue-hover); color: white; border-color: var(--blue-hover); }
    input[type=text] { padding: 4px 8px; border: 1px solid #999; border-radius: 4px; width: 120px; }
    #status { font-size: 13px; color: #555; }
    .face-img { object-fit: cover; border-radius: 3px; background: #ddd; display: block; }
    .people-section { position: sticky; top: 0; background: #fff; z-index: 100; padding-bottom: 8px; }
    .people-scroll { max-height: 45vh; overflow-y: auto; padding-bottom: 4px; }
    .drag-handle { display: flex; align-items: center; gap: 6px; cursor: grab; color: #aaa; padding: 2px 0 6px; user-select: none; }
    .drag-handle .drag-dots { font-size: 16px; letter-spacing: 2px; flex-shrink: 0; }
    .drag-handle .drag-hint { font-size: 10px; color: #bbb; line-height: 1.2; }
    .drag-handle:hover { color: #777; }
    .drag-handle:hover .drag-hint { color: #999; }
    .cluster-card .drag-handle .drag-dots, .cluster-card .drag-handle .drag-hint { color: var(--green-border); }
    .cluster-card .drag-handle:hover .drag-dots, .cluster-card .drag-handle:hover .drag-hint { color: var(--green-text); }
    .singleton-card .drag-handle .drag-dots, .singleton-card .drag-handle .drag-hint { color: var(--orange-border); }
    .singleton-card .drag-handle:hover .drag-dots, .singleton-card .drag-handle:hover .drag-hint { color: var(--orange-text); }
    .cluster-link { color: var(--blue-hover); text-decoration: none; font-weight: bold; display: block; max-width: 100%; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
    .cluster-link:hover { text-decoration: underline; }
    .extra-count { font-size: 11px; margin-top: 2px; }
    .person-card .extra-count { color: var(--blue-text); }
    .cluster-card .extra-count { color: var(--green-text); }
    .singleton-card .extra-count { color: var(--orange-text); }
  </style>
</head>
<body>
  <div class="toolbar">
    <strong>videre faces labeling</strong>
    <span id="status">Loading...</span>
    <button class="primary" onclick="saveAndClose()">Save &amp; Close</button>
  </div>

  <div class="people-section">
    <h2 class="title-people">People <span id="people-count" class="badge badge-blue">0</span></h2>
    <div class="people-scroll">
      <div id="people-grid" class="grid"></div>
    </div>
  </div>

  <h2 class="title-clusters">Unassigned Clusters <span id="cluster-count" class="badge badge-green">0</span></h2>
  <div id="cluster-grid" class="grid"></div>

  <h2 class="title-singletons">Singletons <span id="singleton-count" class="badge badge-orange">0</span></h2>
  <div id="singleton-grid" class="grid"></div>

  <script>
    let facesData = { people: [], clusters: [], singletons: [] };

    async function loadFaces() {
      try {
        const r = await fetch('/api/faces');
        facesData = await r.json();
        render();
        document.getElementById('status').textContent =
          `${facesData.people.length} people, ${facesData.clusters.length} clusters, ${facesData.singletons.length} singletons`;
      } catch(e) {
        document.getElementById('status').textContent = 'Error loading: ' + e;
      }
    }

    function faceImg(faceId, w, h) {
      return `<img class="face-img" src="/api/face-image/${faceId}" width="${w}" height="${h}" title="#${faceId}" onerror="this.removeAttribute('src');this.style.background='#ddd'">`;
    }

    function thumbGrid(faceIds) {
      if (faceIds.length === 1) {
        return `<div style="margin-bottom:6px">${faceImg(faceIds[0], 140, 140)}</div>`;
      }
      const visible = faceIds.slice(0, 4);
      const extra = faceIds.length > 4
        ? `<div class="extra-count">+${faceIds.length - 4} more</div>` : '';
      return `
        <div style="display:grid;grid-template-columns:repeat(2,66px);gap:4px;margin-bottom:6px">
          ${visible.map(id => faceImg(id, 66, 66)).join('')}
        </div>${extra}`;
    }

    function renderPeople(people) {
      const grid = document.getElementById('people-grid');
      document.getElementById('people-count').textContent = people.length;
      const sorted = [...people].sort((a, b) => b.face_ids.length - a.face_ids.length);
      grid.innerHTML = sorted.map(p => {
        const url = `/person/${encodeURIComponent(p.label)}`;
        const extra = p.face_ids.length > 1
          ? `<div class="extra-count">+${p.face_ids.length - 1} more</div>` : '';
        return `
        <div class="card person-card"
             data-label="${escHtml(p.label)}"
             ondragover="event.preventDefault(); this.classList.add('drag-over')"
             ondragleave="this.classList.remove('drag-over')"
             ondrop="onDropToPerson(event, this.dataset.label); this.classList.remove('drag-over')">
          <a href="${url}">
            <div style="margin-bottom:6px">${faceImg(p.representative_id, 140, 140)}</div>
          </a>
          <a class="cluster-link" href="${url}" title="${escHtml(p.label)}">${escHtml(p.label)}</a>
          ${extra}
        </div>
      `;
      }).join('');
    }

    const MAX_NAME_LEN = 60;

    // Trim, collapse internal whitespace, strip control/bidi-spoofing
    // characters, and cap length by code point (not UTF-16 code unit) so a
    // pasted wall of text or a spoofed name can't stretch card layout,
    // corrupt display order, or bloat the DB.
    function sanitizeName(raw) {
      const filtered = Array.from(raw).filter(function(ch) {
        const cp = ch.codePointAt(0);
        if (cp < 0x20 || (cp >= 0x7f && cp <= 0x9f)) return false;
        if (cp === 0x200B) return false;
        if (cp === 0x200E || cp === 0x200F) return false;
        // 0x200C (ZWNJ) and 0x200D (ZWJ) are intentionally allowed -
        // required for Persian/Indic text and emoji ZWJ sequences.
        if (cp >= 0x202A && cp <= 0x202E) return false;
        if (cp >= 0x2060 && cp <= 0x2069) return false;
        if (cp === 0xFEFF) return false;
        return true;
      }).join('');
      const collapsed = filtered.trim().replace(/\s+/g, ' ');
      return Array.from(collapsed).slice(0, MAX_NAME_LEN).join('');
    }

    function renderAssignableCard(faceIds, linkUrl, cardClass) {
      const faceIdsJson = JSON.stringify(faceIds);
      const thumb = linkUrl
        ? `<a href="${escHtml(linkUrl)}">${thumbGrid(faceIds)}</a>`
        : thumbGrid(faceIds);
      return `
        <div class="card ${cardClass}">
          <div class="drag-handle" draggable="true" ondragstart="onDragStart(event, ${faceIdsJson})" title="Drag to assign to a person">
            <span class="drag-dots">&#8942;&#8942;&#8942;</span>
            <span class="drag-hint">Drag on person above</span>
          </div>
          ${thumb}
          <div class="new-person-area">
            <button class="new-person-btn" onclick="showNewPersonInput(this, ${faceIdsJson})">New Person</button>
          </div>
        </div>
      `;
    }

    function renderClusters(clusters) {
      const grid = document.getElementById('cluster-grid');
      document.getElementById('cluster-count').textContent = clusters.length;
      const sorted = [...clusters].sort((a, b) => b.face_ids.length - a.face_ids.length);
      grid.innerHTML = sorted.map(c =>
        renderAssignableCard(c.face_ids, `/cluster/${c.cluster_id}`, 'cluster-card')
      ).join('');
    }

    function renderSingletons(singletons) {
      const grid = document.getElementById('singleton-grid');
      document.getElementById('singleton-count').textContent = singletons.length;
      grid.innerHTML = singletons.map(s =>
        renderAssignableCard([s.face_id], null, 'singleton-card')
      ).join('');
    }

    function render() {
      renderPeople(facesData.people);
      renderClusters(facesData.clusters);
      renderSingletons(facesData.singletons);
    }

    function escHtml(s) {
      return s.replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;').replace(/"/g,'&quot;');
    }

    function onDragStart(event, faceIds) {
      if (!event.target.closest('.drag-handle')) {
        event.preventDefault();
        return;
      }
      event.dataTransfer.setData('application/json', JSON.stringify({ face_ids: faceIds }));
    }

    async function onDropToPerson(event, personLabel) {
      event.preventDefault();
      const data = JSON.parse(event.dataTransfer.getData('application/json'));
      const r = await fetch('/api/assign', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ face_ids: data.face_ids, person_label: personLabel })
      });
      if (!r.ok) {
        document.getElementById('status').textContent = 'Error: assign failed';
        return;
      }
      await loadFaces();
    }

    function showNewPersonInput(btn, faceIds) {
      const area = btn.parentElement;
      const faceIdsJson = JSON.stringify(faceIds);
      area.innerHTML = `
        <input type="text" class="np-input" id="np-input-${faceIds[0]}" placeholder="Person name" maxlength="${MAX_NAME_LEN}" autofocus>
        <div class="np-btn-row">
          <button class="np-create-btn" onclick="submitNewPerson('np-input-${faceIds[0]}', ${faceIdsJson})">Create</button>
          <button class="new-person-btn" onclick="loadFaces()">Cancel</button>
        </div>
      `;
    }

    async function submitNewPerson(inputId, faceIds) {
      const input = document.getElementById(inputId);
      const label = sanitizeName(input.value);
      if (!label) return;
      const r = await fetch('/api/new-person', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ face_ids: faceIds, label: label })
      });
      if (!r.ok) {
        document.getElementById('status').textContent = 'Error: create person failed';
        return;
      }
      await loadFaces();
    }

    async function saveAndClose() {
      await fetch('/api/quit', { method: 'POST' });
      document.body.innerHTML = '<div style="padding:32px;font-size:18px">Server stopped. You can close this tab.</div>';
    }

    loadFaces();
  </script>
</body>
</html>
"##;

const CLUSTER_HTML: &str = r##"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <title>Cluster __CLUSTER_ID__</title>
  <style>
    body { font-family: sans-serif; margin: 0; padding: 16px; background: #f5f5f5; }
    .toolbar { display: flex; gap: 10px; align-items: center; margin-bottom: 12px; flex-wrap: wrap; }
    .toolbar a { color: #2a6db5; text-decoration: none; font-size: 14px; }
    .assign-bar { background: white; border: 1px solid #ddd; border-radius: 8px; padding: 12px; margin-bottom: 16px; display: flex; gap: 8px; align-items: center; flex-wrap: wrap; }
    .grid { display: grid; grid-template-columns: repeat(auto-fill, 200px); gap: 14px; }
    .card { background: white; border: 1px solid #ddd; border-radius: 8px; padding: 10px; }
    .face-img { object-fit: cover; border-radius: 4px; display: block; background: #ddd; }
    .path { font-size: 11px; color: #666; word-break: break-all; margin-top: 5px; }
    .face-id { font-size: 11px; color: #999; margin-top: 2px; }
    .btns { display: flex; gap: 6px; margin-top: 8px; flex-wrap: wrap; }
    button { cursor: pointer; padding: 4px 10px; border-radius: 4px; border: 1px solid #999; background: white; font-size: 13px; }
    button.danger { color: #c00; border-color: #fbb; }
    button.primary { background: #2a6db5; color: white; border-color: #2a6db5; }
    input[type=text] { padding: 4px 8px; border: 1px solid #999; border-radius: 4px; width: 160px; font-size: 13px; }
    #status { font-size: 13px; color: #555; }
  </style>
</head>
<body>
  <div class="toolbar">
    <a href="/">&larr; Back to labeling</a>
    <strong>Cluster __CLUSTER_ID__</strong>
    <span id="face-count" style="color:#555;font-size:13px"></span>
    <span id="status"></span>
  </div>

  <div class="assign-bar">
    <strong>Assign all to:</strong>
    <input type="text" id="person-input" placeholder="Person name" maxlength="60" list="people-list">
    <datalist id="people-list"></datalist>
    <button class="primary" onclick="assignAll()">Assign cluster</button>
    <button class="danger" onclick="dissolveCluster()" style="margin-left:auto">Dissolve cluster (wrong grouping)</button>
  </div>

  <div id="faces-grid" class="grid"></div>

  <script>
    const clusterId = __CLUSTER_ID__;
    let facesData = [];
    const MAX_NAME_LEN = 60;

    // Trim, collapse internal whitespace, strip control/bidi-spoofing
    // characters, and cap length by code point (not UTF-16 code unit) so a
    // pasted wall of text or a spoofed name can't stretch card layout,
    // corrupt display order, or bloat the DB.
    function sanitizeName(raw) {
      const filtered = Array.from(raw).filter(function(ch) {
        const cp = ch.codePointAt(0);
        if (cp < 0x20 || (cp >= 0x7f && cp <= 0x9f)) return false;
        if (cp === 0x200B) return false;
        if (cp === 0x200E || cp === 0x200F) return false;
        // 0x200C (ZWNJ) and 0x200D (ZWJ) are intentionally allowed -
        // required for Persian/Indic text and emoji ZWJ sequences.
        if (cp >= 0x202A && cp <= 0x202E) return false;
        if (cp >= 0x2060 && cp <= 0x2069) return false;
        if (cp === 0xFEFF) return false;
        return true;
      }).join('');
      const collapsed = filtered.trim().replace(/\s+/g, ' ');
      return Array.from(collapsed).slice(0, MAX_NAME_LEN).join('');
    }

    async function load() {
      try {
        const [clusterRes, mainRes] = await Promise.all([
          fetch(`/api/cluster/${clusterId}`),
          fetch('/api/faces')
        ]);
        if (!clusterRes.ok) throw new Error('cluster fetch failed');
        const clusterData = await clusterRes.json();
        const mainData = mainRes.ok ? await mainRes.json() : { people: [] };
        facesData = clusterData.faces;
        const dl = document.getElementById('people-list');
        dl.innerHTML = mainData.people.map(p => `<option value="${escHtml(p.label)}">`).join('');
        document.getElementById('face-count').textContent = `${facesData.length} face(s)`;
        render();
      } catch(e) {
        document.getElementById('status').textContent = 'Error: ' + e;
      }
    }

    function render() {
      const grid = document.getElementById('faces-grid');
      grid.innerHTML = facesData.map(f => `
        <div class="card" id="card-${f.face_id}">
          <img class="face-img" src="/api/face-image/${f.face_id}" width="180" height="180"
               onerror="this.removeAttribute('src');this.style.background='#ddd'">
          <div class="path" title="${escHtml(f.path)}">${escHtml(basename(f.path))}</div>
          <div class="face-id">#${f.face_id}</div>
          <div class="btns">
            <button class="danger" onclick="removeFace(${f.face_id})">Remove</button>
            <button onclick="assignOne(${f.face_id})">Assign</button>
          </div>
        </div>
      `).join('');
    }

    function basename(p) { return p.split('/').pop() || p; }

    function escHtml(s) {
      return String(s).replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;').replace(/"/g,'&quot;');
    }

    async function removeFace(faceId) {
      const r = await fetch('/api/remove-face', {
        method: 'POST', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ face_id: faceId })
      });
      if (!r.ok) { document.getElementById('status').textContent = 'Error: remove failed'; return; }
      document.getElementById(`card-${faceId}`)?.remove();
      facesData = facesData.filter(f => f.face_id !== faceId);
      document.getElementById('face-count').textContent = `${facesData.length} face(s)`;
    }

    async function assignAll() {
      const label = sanitizeName(document.getElementById('person-input').value);
      if (!label) return;
      const faceIds = facesData.map(f => f.face_id);
      const r = await fetch('/api/new-person', {
        method: 'POST', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ face_ids: faceIds, label })
      });
      if (!r.ok) { document.getElementById('status').textContent = 'Error: assign failed'; return; }
      document.getElementById('status').textContent = `Assigned ${faceIds.length} face(s) to "${label}"`;
      setTimeout(() => { window.location.href = '/'; }, 800);
    }

    async function dissolveCluster() {
      if (!confirm(`Dissolve cluster ${clusterId}? Its ${facesData.length} face(s) will become unassigned singletons (not deleted).`)) return;
      const r = await fetch('/api/dissolve-cluster', {
        method: 'POST', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ cluster_id: clusterId })
      });
      if (!r.ok) { document.getElementById('status').textContent = 'Error: dissolve failed'; return; }
      document.getElementById('status').textContent = 'Cluster dissolved';
      setTimeout(() => { window.location.href = '/'; }, 500);
    }

    async function assignOne(faceId) {
      const raw = prompt('Assign face #' + faceId + ' to person:');
      if (!raw) return;
      const label = sanitizeName(raw);
      if (!label) return;
      const r = await fetch('/api/new-person', {
        method: 'POST', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ face_ids: [faceId], label })
      });
      if (!r.ok) { document.getElementById('status').textContent = 'Error: assign failed'; return; }
      document.getElementById(`card-${faceId}`)?.remove();
      facesData = facesData.filter(f => f.face_id !== faceId);
      document.getElementById('face-count').textContent = `${facesData.length} face(s)`;
    }

    load();
  </script>
</body>
</html>
"##;

const PERSON_HTML: &str = r##"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <title>Person</title>
  <style>
    body { font-family: sans-serif; margin: 0; padding: 16px; background: #f5f5f5; }
    .toolbar { display: flex; gap: 10px; align-items: center; margin-bottom: 12px; flex-wrap: wrap; }
    .toolbar a { color: #2a6db5; text-decoration: none; font-size: 14px; }
    .grid { display: grid; grid-template-columns: repeat(auto-fill, 200px); gap: 14px; }
    .card { background: white; border: 1px solid #ddd; border-radius: 8px; padding: 10px; }
    .face-img { object-fit: cover; border-radius: 4px; display: block; background: #ddd; }
    .path { font-size: 11px; color: #666; word-break: break-all; margin-top: 5px; }
    .face-id { font-size: 11px; color: #999; margin-top: 2px; }
    .btns { display: flex; gap: 6px; margin-top: 8px; flex-wrap: wrap; }
    button { cursor: pointer; padding: 4px 10px; border-radius: 4px; border: 1px solid #999; background: white; font-size: 13px; }
    button.danger { color: #c00; border-color: #fbb; }
    #status { font-size: 13px; color: #555; }
  </style>
</head>
<body>
  <div class="toolbar">
    <a href="/">&larr; Back to labeling</a>
    <strong id="person-title">Person</strong>
    <span id="face-count" style="color:#555;font-size:13px"></span>
    <span id="status"></span>
  </div>

  <div id="faces-grid" class="grid"></div>

  <script>
    const personName = decodeURIComponent(window.location.pathname.split('/').pop());
    let facesData = [];

    async function load() {
      try {
        document.getElementById('person-title').textContent = personName;
        document.title = personName;
        const r = await fetch(`/api/person/${encodeURIComponent(personName)}`);
        if (!r.ok) throw new Error('person fetch failed');
        const data = await r.json();
        facesData = data.faces;
        document.getElementById('face-count').textContent = `${facesData.length} face(s)`;
        render();
      } catch(e) {
        document.getElementById('status').textContent = 'Error: ' + e;
      }
    }

    function render() {
      const grid = document.getElementById('faces-grid');
      grid.innerHTML = facesData.map(f => `
        <div class="card" id="card-${f.face_id}">
          <a href="/api/original-image/${f.face_id}" target="_blank" title="Open original image">
            <img class="face-img" src="/api/face-image/${f.face_id}" width="180" height="180"
                 onerror="this.removeAttribute('src');this.style.background='#ddd'">
          </a>
          <div class="path" title="${escHtml(f.path)}">${escHtml(basename(f.path))}</div>
          <div class="face-id">#${f.face_id}</div>
          <div class="btns">
            <button class="danger" onclick="removeFace(${f.face_id})">Remove</button>
          </div>
        </div>
      `).join('');
    }

    function basename(p) { return p.split('/').pop() || p; }

    function escHtml(s) {
      return String(s).replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;').replace(/"/g,'&quot;');
    }

    async function removeFace(faceId) {
      const r = await fetch('/api/remove-face', {
        method: 'POST', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ face_id: faceId })
      });
      if (!r.ok) { document.getElementById('status').textContent = 'Error: remove failed'; return; }
      document.getElementById(`card-${faceId}`)?.remove();
      facesData = facesData.filter(f => f.face_id !== faceId);
      document.getElementById('face-count').textContent = `${facesData.length} face(s)`;
    }

    load();
  </script>
</body>
</html>
"##;

#[derive(Serialize)]
struct ClusterFaceData {
    face_id: i64,
    hash: String,
    path: String,
}

#[derive(Serialize)]
struct ClusterDetailResponse {
    cluster_id: i64,
    faces: Vec<ClusterFaceData>,
}

#[derive(Serialize)]
struct PersonDetailResponse {
    label: String,
    faces: Vec<ClusterFaceData>,
}

#[derive(Serialize)]
struct PersonData {
    label: String,
    face_ids: Vec<i64>,
    representative_id: i64,
    hashes: Vec<String>,
}

#[derive(Serialize)]
struct ClusterData {
    cluster_id: i64,
    face_ids: Vec<i64>,
    hashes: Vec<String>,
}

#[derive(Serialize)]
struct SingletonData {
    face_id: i64,
    hash: String,
}

#[derive(Serialize)]
struct FacesResponse {
    people: Vec<PersonData>,
    clusters: Vec<ClusterData>,
    singletons: Vec<SingletonData>,
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

#[derive(Deserialize)]
struct RenamePersonRequest {
    old_label: String,
    new_label: String,
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
    report_by_date: bool,
    report_heic: bool,
    report_heic_original: bool,
    serve_faces_ui: bool,
    tmp_counter: AtomicU64,
}

fn query_faces_data(conn: &Connection) -> rusqlite::Result<FacesResponse> {
    let mut people: HashMap<String, PersonData> = HashMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT id, hash, person_label FROM faces \
             WHERE confirmed = 1 AND person_label IS NOT NULL \
             ORDER BY person_label, is_primary DESC, id ASC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
        })?;
        for row in rows {
            let (id, hash, label) = row?;
            let person = people.entry(label.clone()).or_insert(PersonData {
                label: label.clone(),
                face_ids: vec![],
                representative_id: id,
                hashes: vec![],
            });
            person.face_ids.push(id);
            if !person.hashes.contains(&hash) {
                person.hashes.push(hash);
            }
        }
    }

    let mut cluster_map: HashMap<i64, ClusterData> = HashMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT id, hash, cluster_id FROM faces \
             WHERE cluster_id IS NOT NULL AND (confirmed = 0 OR person_label IS NULL) \
             ORDER BY cluster_id, id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?))
        })?;
        for row in rows {
            let (id, hash, cid) = row?;
            let cluster = cluster_map.entry(cid).or_insert(ClusterData {
                cluster_id: cid,
                face_ids: vec![],
                hashes: vec![],
            });
            cluster.face_ids.push(id);
            if !cluster.hashes.contains(&hash) {
                cluster.hashes.push(hash);
            }
        }
    }

    let mut singletons: Vec<SingletonData> = vec![];
    {
        let mut stmt = conn.prepare(
            "SELECT id, hash FROM faces \
             WHERE cluster_id IS NULL AND (confirmed = 0 OR person_label IS NULL) \
             ORDER BY id",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
        for row in rows {
            let (id, hash) = row?;
            singletons.push(SingletonData { face_id: id, hash });
        }
    }

    Ok(FacesResponse {
        people: people.into_values().collect(),
        clusters: cluster_map.into_values().collect(),
        singletons,
    })
}

async fn handle_root() -> impl axum::response::IntoResponse {
    axum::response::Html(FACES_HTML)
}

/// Live-server equivalent of the static `--all`/`--by-date` HTML report,
/// rendered on each request from the current database state (labeled faces
/// included, since this route only exists when `--show-faces` is set).
async fn handle_report(State(state): State<Arc<AppState>>) -> impl axum::response::IntoResponse {
    let conn = state.conn.lock().unwrap();
    let stats = query_stats(&conn);
    let groups = query_groups(&conn);
    let all_files = state.report_all.then(|| query_all_files(&conn));
    let keep_files = state.report_by_date.then(|| query_keep_files(&conn));
    let faces_by_hash = videre_core::face_db::labeled_faces_by_hash(&conn).unwrap_or_default();
    let vectors = if state.report_all { query_vectors(&conn) } else { None };
    let db_path = conn.path().map(|p| p.to_string()).unwrap_or_default();
    drop(conn);
    let html = generate_html(
        &db_path,
        &stats,
        &groups,
        all_files.as_deref(),
        keep_files.as_deref(),
        vectors.as_ref(),
        state.report_heic,
        state.report_heic_original,
        &faces_by_hash,
        true,
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
    // `q.lat`/`q.lon` arrive rounded to 6 decimal places (the precision
    // `file_to_json_with_faces` bakes into `meta.location` client-side), but
    // `file_hashes.gps_lat`/`gps_lon` are stored at full EXIF precision - an
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

async fn handle_get_faces(
    State(state): State<Arc<AppState>>,
) -> Result<AxumJson<FacesResponse>, StatusCode> {
    let conn = state.conn.lock().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let resp = query_faces_data(&conn).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(AxumJson(resp))
}

/// Trim, collapse internal whitespace, and cap length so a client that
/// bypasses the UI's own sanitization can't stretch card layout or bloat
/// the DB with an unbounded label. Mirrors the client-side sanitizeName().
/// Known, accepted gaps: this filters control and bidi/zero-width format
/// characters, not visual homoglyph confusables (e.g. Cyrillic A vs Latin
/// A) - full spoof-proofing would need a confusables-detection dependency,
/// which is out of scope here. The 60-char cap also truncates by raw code
/// point rather than grapheme cluster, so it can in principle bisect a
/// multi-codepoint sequence (e.g. a ZWJ emoji sequence) sitting right at
/// the boundary.
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
/// Deliberately excludes U+200C (ZWNJ) and U+200D (ZWJ): both are required
/// for legitimate text - ZWJ joins emoji sequences (a family emoji is three
/// emoji joined by ZWJ; stripping it splits them apart) and ZWNJ is
/// orthographically required in Persian and several Indic scripts.
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

async fn handle_assign(
    State(state): State<Arc<AppState>>,
    AxumJson(req): AxumJson<AssignRequest>,
) -> Result<StatusCode, StatusCode> {
    let label = sanitize_person_label(&req.person_label).ok_or(StatusCode::BAD_REQUEST)?;
    let conn = state.conn.lock().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    for id in &req.face_ids {
        conn.execute(
            "UPDATE faces SET person_label = ?1, confirmed = 1 WHERE id = ?2",
            rusqlite::params![label, id],
        )
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    }
    Ok(StatusCode::OK)
}

async fn handle_new_person(
    State(state): State<Arc<AppState>>,
    AxumJson(req): AxumJson<NewPersonRequest>,
) -> Result<StatusCode, StatusCode> {
    let label = sanitize_person_label(&req.label).ok_or(StatusCode::BAD_REQUEST)?;
    let conn = state.conn.lock().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    for id in &req.face_ids {
        conn.execute(
            "UPDATE faces SET person_label = ?1, confirmed = 1 WHERE id = ?2",
            rusqlite::params![label, id],
        )
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    }
    Ok(StatusCode::OK)
}

async fn handle_remove_face(
    State(state): State<Arc<AppState>>,
    AxumJson(req): AxumJson<RemoveFaceRequest>,
) -> Result<StatusCode, StatusCode> {
    let conn = state.conn.lock().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    conn.execute(
        "UPDATE faces SET cluster_id = NULL, person_label = NULL, confirmed = 0, is_primary = 0 WHERE id = ?1",
        [req.face_id],
    )
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(StatusCode::OK)
}

/// Resets every face carrying `label` back to unassigned. Deliberately does
/// NOT touch `cluster_id`: a face that came from a DBSCAN cluster rejoins
/// that cluster's unassigned group (picked up by query_faces_data's cluster
/// query) instead of scattering to Singletons; a face that was already a
/// singleton (cluster_id already NULL) stays a singleton. Does not trigger
/// re-clustering - there is no live DBSCAN re-run in this server.
async fn handle_delete_person(
    State(state): State<Arc<AppState>>,
    AxumJson(req): AxumJson<DeletePersonRequest>,
) -> Result<StatusCode, StatusCode> {
    let conn = state.conn.lock().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    conn.execute(
        "UPDATE faces SET person_label = NULL, confirmed = 0, is_primary = 0 WHERE person_label = ?1",
        rusqlite::params![req.label],
    )
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(StatusCode::OK)
}

/// Renames every face carrying `old_label` to `new_label`. Rejects a
/// collision with an existing person (409) rather than silently merging -
/// a silent merge would be irreversible and could leave the merged person
/// with two faces marked is_primary = 1, violating the invariant
/// handle_set_primary maintains via its clear-then-set transaction.
async fn handle_rename_person(
    State(state): State<Arc<AppState>>,
    AxumJson(req): AxumJson<RenamePersonRequest>,
) -> Result<StatusCode, StatusCode> {
    let Some(sanitized) = sanitize_person_label(&req.new_label) else {
        return Err(StatusCode::BAD_REQUEST);
    };
    let conn = state.conn.lock().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

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

/// Ungroup a bad cluster: every face in it becomes an unassigned singleton
/// instead of being deleted, so it can still be labeled individually later.
async fn handle_dissolve_cluster(
    State(state): State<Arc<AppState>>,
    AxumJson(req): AxumJson<DissolveClusterRequest>,
) -> Result<StatusCode, StatusCode> {
    let conn = state.conn.lock().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    conn.execute(
        "UPDATE faces SET cluster_id = NULL WHERE cluster_id = ?1",
        [req.cluster_id],
    )
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(StatusCode::OK)
}

async fn handle_set_primary(
    State(state): State<Arc<AppState>>,
    AxumJson(req): AxumJson<SetPrimaryRequest>,
) -> Result<StatusCode, StatusCode> {
    let conn = state.conn.lock().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    conn.execute_batch("BEGIN").map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let result = (|| -> rusqlite::Result<()> {
        // Clear primary flag for all faces of this person
        conn.execute(
            "UPDATE faces SET is_primary = 0 WHERE person_label = ?1",
            rusqlite::params![req.person_label],
        )?;
        // Set primary flag for the target face; guard against stealing from another person
        conn.execute(
            "UPDATE faces SET is_primary = 1, confirmed = 1, person_label = ?1 WHERE id = ?2 AND person_label = ?1",
            rusqlite::params![req.person_label, req.face_id],
        )?;
        Ok(())
    })();
    match result {
        Ok(()) => {
            conn.execute_batch("COMMIT").map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        }
        Err(_) => {
            let _ = conn.execute_batch("ROLLBACK");
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
    }
    Ok(StatusCode::OK)
}

async fn handle_search_person(
    State(state): State<Arc<AppState>>,
    Query(q): Query<PersonSearchQuery>,
) -> Result<AxumJson<Vec<String>>, StatusCode> {
    let conn = state.conn.lock().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let paths = videre_core::person_search::search_by_person(&conn, &q.name, None)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(AxumJson(paths))
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
    axum::response::Html(CLUSTER_HTML.replace("__CLUSTER_ID__", &cluster_id.to_string()))
}

async fn handle_cluster_api(
    axum::extract::Path(cluster_id): axum::extract::Path<i64>,
    State(state): State<Arc<AppState>>,
) -> Result<AxumJson<ClusterDetailResponse>, StatusCode> {
    let conn = state.conn.lock().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut stmt = conn
        .prepare(
            "SELECT f.id, f.hash, fh.path FROM faces f \
             JOIN file_hashes fh ON f.hash = fh.hash \
             WHERE f.cluster_id = ?1 \
             ORDER BY f.id",
        )
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let faces: Vec<ClusterFaceData> = stmt
        .query_map([cluster_id], |r| {
            Ok(ClusterFaceData { face_id: r.get(0)?, hash: r.get(1)?, path: r.get(2)? })
        })
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(AxumJson(ClusterDetailResponse { cluster_id, faces }))
}

async fn handle_person_page() -> impl axum::response::IntoResponse {
    axum::response::Html(PERSON_HTML)
}

async fn handle_person_api(
    axum::extract::Path(name): axum::extract::Path<String>,
    State(state): State<Arc<AppState>>,
) -> Result<AxumJson<PersonDetailResponse>, StatusCode> {
    let conn = state.conn.lock().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut stmt = conn
        .prepare(
            "SELECT f.id, f.hash, fh.path FROM faces f \
             JOIN file_hashes fh ON f.hash = fh.hash \
             WHERE f.person_label = ?1 AND f.confirmed = 1 \
             ORDER BY f.is_primary DESC, f.id",
        )
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let faces: Vec<ClusterFaceData> = stmt
        .query_map([&name], |r| {
            Ok(ClusterFaceData { face_id: r.get(0)?, hash: r.get(1)?, path: r.get(2)? })
        })
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(AxumJson(PersonDetailResponse { label: name, faces }))
}

fn read_exif_orientation(path: &str) -> u16 {
    let Ok(f) = std::fs::File::open(path) else { return 1 };
    let Ok(exif_data) = exif::Reader::new().read_from_container(&mut BufReader::new(f)) else {
        return 1;
    };
    exif_data
        .get_field(exif::Tag::Orientation, exif::In::PRIMARY)
        .and_then(|field| {
            if let exif::Value::Short(ref v) = field.value {
                v.first().copied()
            } else {
                None
            }
        })
        .unwrap_or(1)
}

/// Rotate/flip `img` to match its EXIF orientation (read from `path`).
fn apply_exif_orientation(img: image::DynamicImage, path: &str) -> image::DynamicImage {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    if !matches!(ext.as_str(), "jpg" | "jpeg" | "tiff" | "dng") {
        return img;
    }
    match read_exif_orientation(path) {
        2 => img.fliph(),
        3 => img.rotate180(),
        4 => img.flipv(),
        5 => img.rotate90().fliph(),
        6 => img.rotate90(),
        7 => img.rotate270().fliph(),
        8 => img.rotate270(),
        _ => img,
    }
}

/// Square crop centered on bbox [x1,y1,x2,y2] with 25% padding, then resize to 140x140.
fn crop_face_square(img: &image::DynamicImage, bbox: [f32; 4]) -> image::DynamicImage {
    let w = img.width() as f32;
    let h = img.height() as f32;
    let bw = bbox[2] - bbox[0];
    let bh = bbox[3] - bbox[1];
    let pad = (bw.max(bh) * 0.25).max(4.0);
    let half = bw.max(bh) * 0.5 + pad;
    let cx = (bbox[0] + bbox[2]) * 0.5;
    let cy = (bbox[1] + bbox[3]) * 0.5;
    let x1 = (cx - half).max(0.0) as u32;
    let y1 = (cy - half).max(0.0) as u32;
    let x2 = (cx + half).min(w) as u32;
    let y2 = (cy + half).min(h) as u32;
    let side = (x2 - x1).min(y2 - y1).max(1);
    img.crop_imm(x1, y1, side, side)
        .resize_exact(140, 140, image::imageops::FilterType::Triangle)
}

/// Load, crop, and orientation-correct a face thumbnail.
///
/// bbox coordinates are stored in terms of the *full-size* decoded image
/// (videre faces rescales detections back to original width/height before
/// writing to the DB), so the thumbnail must be cropped from an image of
/// the same dimensions used at detection time.
///
/// For HEIC: videre faces converts via QuickLook (see
/// `videre_core::heic::heic_via_quicklook`), which already applies correct
/// rotation, so no separate orientation step is needed. For JPEG/PNG/etc:
/// detection ran on raw pixels; apply EXIF orientation after crop.
fn make_face_thumb(path: &str, bbox: [f32; 4], face_id: i64) -> Option<image::DynamicImage> {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    if ext == "heic" {
        let img = videre_core::heic::heic_via_quicklook(path, &format!("thumb{face_id}"))?;
        Some(crop_face_square(&img, bbox))
    } else {
        // Detection ran on raw pixels; crop first, then correct orientation
        let img = image::open(path).ok()?;
        let cropped = crop_face_square(&img, bbox);
        Some(apply_exif_orientation(cropped, path))
    }
}

async fn handle_face_image(
    axum::extract::Path(face_id): axum::extract::Path<i64>,
    State(state): State<Arc<AppState>>,
) -> Result<impl axum::response::IntoResponse, StatusCode> {
    let (bbox_json, file_path, hash) = {
        let conn = state.conn.lock().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        conn.query_row(
            "SELECT f.bbox, fh.path, f.hash FROM faces f \
             JOIN file_hashes fh ON f.hash = fh.hash \
             WHERE f.id = ?1 LIMIT 1",
            [face_id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)),
        )
        .optional()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?
    };

    const FACE_THUMB_SIZE: u32 = 140;
    if videre_core::thumb_cache::face_thumb_exists(&hash, face_id, FACE_THUMB_SIZE) {
        if let Ok(bytes) =
            tokio::fs::read(videre_core::thumb_cache::face_thumb_path(&hash, face_id, FACE_THUMB_SIZE)).await
        {
            return Ok(([(axum::http::header::CONTENT_TYPE, "image/jpeg")], bytes));
        }
    }

    // bbox stored as "x,y,w,h" → convert to x1,y1,x2,y2
    let parts: Vec<f32> = bbox_json
        .split(',')
        .filter_map(|s| s.trim().parse().ok())
        .collect();
    if parts.len() != 4 {
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }
    let bbox: [f32; 4] = [parts[0], parts[1], parts[0] + parts[2], parts[1] + parts[3]];

    let tmp_id = state.tmp_counter.fetch_add(1, Ordering::Relaxed);
    let jpeg_bytes = tokio::task::spawn_blocking(move || -> Option<Vec<u8>> {
        let thumb = make_face_thumb(&file_path, bbox, face_id)?;
        let mut buf = Vec::new();
        thumb
            .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Jpeg)
            .ok()?;
        Some(buf)
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .ok_or(StatusCode::NOT_FOUND)?;

    // Best-effort write-through: a cache write failure (disk full,
    // permissions) must not fail the request - caching is a performance
    // optimization, not a correctness requirement. The per-request `tmp_id`
    // suffix (distinct from the pid-only scheme `thumb_tmp_path` uses)
    // prevents two concurrent requests for the same uncached face from
    // colliding on the same tmp path and corrupting the cache.
    let final_path = videre_core::thumb_cache::face_thumb_path(&hash, face_id, FACE_THUMB_SIZE);
    let tmp_path = final_path.with_extension(format!("tmp{}-{}", std::process::id(), tmp_id));
    if let Some(parent) = final_path.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    if tokio::fs::write(&tmp_path, &jpeg_bytes).await.is_ok() {
        let _ = tokio::fs::rename(&tmp_path, &final_path).await;
    }

    Ok(([(axum::http::header::CONTENT_TYPE, "image/jpeg")], jpeg_bytes))
}

fn mime_for_ext(ext: &str) -> &'static str {
    match ext {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "tiff" => "image/tiff",
        "mov" => "video/quicktime",
        "mp4" => "video/mp4",
        _ => "application/octet-stream",
    }
}

#[derive(Deserialize)]
struct RawFileQuery {
    path: String,
    /// Optional max width/height in pixels - only meaningful for HEIC
    /// (which always needs QuickLook conversion) so the caller can request a
    /// small thumbnail (240px in the grid) or a larger version (1200px in
    /// the lightbox) without paying to decode/re-encode a huge image for a
    /// tiny `<img>`. Ignored for formats served as raw bytes.
    size: Option<u32>,
}

/// Streams a file already recorded in `file_hashes.path` over HTTP - added
/// so the live report (server mode, --show-faces) can point thumbnail
/// `<img>`/`<video>` tags and the lightbox at an http:// URL instead of
/// `file://`, which browsers refuse to load as a subresource of an
/// http://-served page. Only paths that exist as a `file_hashes.path` value
/// are served - this is a deliberate allowlist, not a general file server,
/// so a client can't request arbitrary paths off the filesystem.
///
/// HEIC is converted to JPEG on demand via QuickLook (same
/// `videre_core::heic::heic_via_quicklook` helper used elsewhere), one file per request,
/// lazily as the browser requests each thumbnail/lightbox image - NOT
/// eagerly for the whole report up front, which is what made server mode
/// unusably slow on a collection with many HEIC files before this endpoint
/// existed (`generate_html` used to call `heic_to_b64` synchronously for
/// every HEIC file before returning any response).
async fn handle_raw_file(
    Query(q): Query<RawFileQuery>,
    State(state): State<Arc<AppState>>,
) -> Result<impl axum::response::IntoResponse, StatusCode> {
    let (path, hash) = {
        let conn = state.conn.lock().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
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
    // a thumbnail for this file's content hash at this exact size - serve
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
    let (content_type, bytes) = tokio::task::spawn_blocking(move || -> Option<(&'static str, Vec<u8>)> {
        if ext == "heic" {
            let img = videre_core::heic::heic_via_quicklook(&path, &format!("raw{}", size.unwrap_or(0)))?;
            let img = match size {
                Some(max_px) if img.width() > max_px || img.height() > max_px => {
                    img.resize(max_px, max_px, image::imageops::FilterType::Triangle)
                }
                _ => img,
            };
            let mut buf = Vec::new();
            img.write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Jpeg)
                .ok()?;
            Some(("image/jpeg", buf))
        } else {
            let bytes = std::fs::read(&path).ok()?;
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
/// original can't be linked to directly - it has to be read and served
/// over HTTP like everything else.
async fn handle_original_image(
    axum::extract::Path(face_id): axum::extract::Path<i64>,
    State(state): State<Arc<AppState>>,
) -> Result<impl axum::response::IntoResponse, StatusCode> {
    let (file_path, hash) = {
        let conn = state.conn.lock().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        conn.query_row(
            "SELECT fh.path, f.hash FROM faces f \
             JOIN file_hashes fh ON f.hash = fh.hash \
             WHERE f.id = ?1 LIMIT 1",
            [face_id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?
    };

    let ext = std::path::Path::new(&file_path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    if ext == "heic" {
        if let Ok(bytes) = tokio::fs::read(videre_core::thumb_cache::original_path(&hash)).await {
            return Ok(([(axum::http::header::CONTENT_TYPE, "image/jpeg")], bytes));
        }
    }

    let tmp_id = state.tmp_counter.fetch_add(1, Ordering::Relaxed);
    let hash_for_cache = hash.clone();
    let ext_for_cache = ext.clone();
    let (content_type, bytes) = tokio::task::spawn_blocking(move || -> Option<(&'static str, Vec<u8>)> {
        if ext == "heic" {
            let img = videre_core::heic::heic_via_quicklook(&file_path, &format!("orig{face_id}"))?;
            let mut buf = Vec::new();
            img.write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Jpeg)
                .ok()?;
            Some(("image/jpeg", buf))
        } else {
            let bytes = std::fs::read(&file_path).ok()?;
            Some((mime_for_ext(&ext), bytes))
        }
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .ok_or(StatusCode::NOT_FOUND)?;

    // Only cache the HEIC-conversion case - non-HEIC files are served as raw
    // bytes with no conversion cost to save, and the read path above only
    // ever checks the cache when ext == "heic".
    if ext_for_cache == "heic" {
        let final_path = videre_core::thumb_cache::original_path(&hash_for_cache);
        let tmp_path = final_path.with_extension(format!("tmp{}-{}", std::process::id(), tmp_id));
        if let Some(parent) = final_path.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        if tokio::fs::write(&tmp_path, &bytes).await.is_ok() {
            let _ = tokio::fs::rename(&tmp_path, &final_path).await;
        }
    }

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
}

async fn serve_faces_async(db: &Path, opts: ServeOptions) -> Result<(), Box<dyn std::error::Error>> {
    let conn = videre_core::db::open_wal(db)?;
    videre_core::location::ensure_location_column(&conn);
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let state = Arc::new(AppState {
        conn: Mutex::new(conn),
        shutdown_tx: Mutex::new(Some(shutdown_tx)),
        report_all: opts.report_all,
        report_by_date: opts.report_by_date,
        report_heic: opts.report_heic,
        report_heic_original: opts.report_heic_original,
        serve_faces_ui: opts.serve_faces_ui,
        tmp_counter: AtomicU64::new(0),
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
        .route("/api/raw", get(handle_raw_file));

    if state.serve_faces_ui {
        router = router
            .route("/api/faces", get(handle_get_faces))
            .route("/api/assign", post(handle_assign))
            .route("/api/new-person", post(handle_new_person))
            .route("/api/remove-face", post(handle_remove_face))
            .route("/api/delete-person", post(handle_delete_person))
            .route("/api/rename-person", post(handle_rename_person))
            .route("/api/dissolve-cluster", post(handle_dissolve_cluster))
            .route("/api/set-primary", post(handle_set_primary));
    }

    // `/` and (when both modes are active) `/faces` depend on which combination
    // of --faces / --show-faces started this server:
    //   --faces alone        -> `/` = labeling UI, no report route at all
    //   --show-faces alone   -> `/` = live report, no `/faces` route
    //   both                 -> `/` = live report, `/faces` = labeling UI
    router = match (state.serve_faces_ui, opts.show_report) {
        (true, true) => router.route("/", get(handle_report)).route("/faces", get(handle_root)),
        (true, false) => router.route("/", get(handle_root)),
        (false, true) => router.route("/", get(handle_report)),
        (false, false) => router, // unreachable: serve_faces_async only runs when at least one is set
    };

    let app = router.with_state(state);

    let addr = "127.0.0.1:7878";
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| format!("Cannot bind to {addr}: {e}"))?;
    eprintln!("Faces labeling server: http://{addr}");
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = shutdown_rx.await;
        })
        .await?;
    Ok(())
}

fn serve_faces(db: &Path, opts: ServeOptions) -> Result<(), Box<dyn std::error::Error>> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(serve_faces_async(db, opts))
}

pub fn run(args: ReportArgs) -> anyhow::Result<()> {
    let db = super::resolve_reader_db(args.db.clone())?;

    if !db.exists() {
        eprintln!("Error: {:?} does not exist", db);
        std::process::exit(1);
    }

    if args.faces || args.show_faces {
        let opts = ServeOptions {
            serve_faces_ui: args.faces,
            show_report: args.show_faces,
            report_all: args.all,
            report_by_date: args.by_date,
            report_heic: args.heic,
            report_heic_original: args.heic_original,
        };
        if let Err(e) = serve_faces(&db, opts) {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
        return Ok(());
    }

    if args.heic_original && !args.heic {
        eprintln!("Warning: --heic-original has no effect without --heic");
    }

    let output = args.output.unwrap_or_else(|| {
        let stem = db.file_stem().unwrap_or_default().to_string_lossy();
        db.with_file_name(format!("{}_report.html", stem))
    });

    let conn = videre_core::db::open_wal(&db).expect("failed to open database");
    let stats = query_stats(&conn);
    let groups = query_groups(&conn);
    let all_files = args.all.then(|| query_all_files(&conn));
    let keep_files = args.by_date.then(|| query_keep_files(&conn));
    let vectors = if args.all {
        let v = query_vectors(&conn);
        if v.is_none() {
            eprintln!("no embeddings found; run videre embed for similarity search");
        }
        v
    } else {
        None
    };
    let html = generate_html(
        &db.to_string_lossy(),
        &stats,
        &groups,
        all_files.as_deref(),
        keep_files.as_deref(),
        vectors.as_ref(),
        args.heic,
        args.heic_original,
        &HashMap::new(),
        false,
    );

    fs::write(&output, &html).expect("failed to write HTML file");

    eprintln!("Report: {}", output.display());
    eprintln!(
        "{} groups · {} duplicate files · {} wasted",
        stats.duplicate_groups,
        stats.duplicate_files,
        format_bytes(stats.wasted_bytes)
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(path: &str, hash: &str, ext: &str) -> FileRow {
        FileRow {
            path: path.to_string(),
            hash: hash.to_string(),
            size_bytes: 100,
            ext: ext.to_string(),
            created_at: None,
            modified_at: None,
            exif_date: None,
            gps_lat: None,
            gps_lon: None,
            width: None,
            height: None,
        }
    }

    fn mem_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE file_hashes (
                path TEXT PRIMARY KEY, hash TEXT NOT NULL, size_bytes INTEGER,
                created_at TEXT, modified_at TEXT, ext TEXT, phash INTEGER,
                exif_date TEXT, gps_lat REAL, gps_lon REAL, width INTEGER, height INTEGER
            );",
        )
        .unwrap();
        conn
    }

    fn mem_db_with_faces() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE file_hashes (path TEXT PRIMARY KEY, hash TEXT NOT NULL,
             size_bytes INTEGER, created_at TEXT, modified_at TEXT, ext TEXT,
             phash INTEGER, exif_date TEXT, gps_lat REAL, gps_lon REAL,
             width INTEGER, height INTEGER);
             CREATE TABLE faces (id INTEGER PRIMARY KEY, hash TEXT NOT NULL,
             bbox TEXT NOT NULL, landmark TEXT, embedding BLOB NOT NULL,
             cluster_id INTEGER, person_label TEXT, confirmed INTEGER DEFAULT 0,
             is_primary INTEGER DEFAULT 0);",
        )
        .unwrap();
        conn
    }

    fn test_state(conn: Connection, serve_faces_ui: bool) -> Arc<AppState> {
        Arc::new(AppState {
            conn: Mutex::new(conn),
            shutdown_tx: Mutex::new(None),
            report_all: false,
            report_by_date: false,
            report_heic: false,
            report_heic_original: false,
            serve_faces_ui,
            tmp_counter: AtomicU64::new(0),
        })
    }

    fn add_embeddings_table(conn: &Connection) {
        conn.execute_batch(
            "CREATE TABLE embeddings (
                hash TEXT PRIMARY KEY, model_id TEXT NOT NULL,
                embedding BLOB NOT NULL, embedded_at TEXT NOT NULL
            );",
        )
        .unwrap();
    }

    fn add_file(conn: &Connection, path: &str, hash: &str) {
        conn.execute(
            "INSERT INTO file_hashes (path, hash, ext) VALUES (?1, ?2, 'jpg')",
            rusqlite::params![path, hash],
        )
        .unwrap();
    }

    #[test]
    fn query_vectors_returns_none_without_table() {
        let conn = mem_db();
        assert!(query_vectors(&conn).is_none());
    }

    #[test]
    fn query_vectors_returns_none_when_empty() {
        let conn = mem_db();
        add_embeddings_table(&conn);
        assert!(query_vectors(&conn).is_none());
    }

    #[test]
    fn query_vectors_orders_by_hash_and_encodes_f16() {
        let conn = mem_db();
        add_embeddings_table(&conn);
        add_file(&conn, "/a/a.jpg", "aaa");
        add_file(&conn, "/a/b.jpg", "bbb");
        add_file(&conn, "/a/c.jpg", "ccc");
        // f16 1.0 = 0x3C00 little-endian = [0x00, 0x3C]
        let one = videre_core::vectors::to_f16_bytes(&[1.0, 0.0]);
        let two = videre_core::vectors::to_f16_bytes(&[0.0, 1.0]);
        // Insert out of order to prove ORDER BY hash
        conn.execute(
            "INSERT INTO embeddings VALUES ('bbb', ?1, ?2, 'now')",
            rusqlite::params![videre_core::embeddings::DEFAULT_MODEL_ID, two],
        ).unwrap();
        conn.execute(
            "INSERT INTO embeddings VALUES ('aaa', ?1, ?2, 'now')",
            rusqlite::params![videre_core::embeddings::DEFAULT_MODEL_ID, one.clone()],
        ).unwrap();
        // Wrong model id must be excluded
        conn.execute(
            "INSERT INTO embeddings VALUES ('ccc', 'other-model', ?1, 'now')",
            rusqlite::params![one],
        ).unwrap();

        let vb = query_vectors(&conn).unwrap();
        assert_eq!(vb.hashes, vec!["aaa".to_string(), "bbb".to_string()]);
        assert_eq!(vb.dim, 2);
        // blob = [00 3C 00 00] ++ [00 00 00 3C]
        let expected = base64_encode(&[0x00, 0x3C, 0x00, 0x00, 0x00, 0x00, 0x00, 0x3C]);
        assert_eq!(vb.b64, expected);
    }

    #[test]
    fn query_vectors_skips_rows_with_wrong_dimension() {
        let conn = mem_db();
        add_embeddings_table(&conn);
        add_file(&conn, "/a/a.jpg", "aaa");
        add_file(&conn, "/a/b.jpg", "bbb");
        let good = videre_core::vectors::to_f16_bytes(&[1.0, 0.0]);
        let bad = videre_core::vectors::to_f16_bytes(&[1.0, 0.0, 0.0]); // 3 dims
        conn.execute(
            "INSERT INTO embeddings VALUES ('aaa', ?1, ?2, 'now')",
            rusqlite::params![videre_core::embeddings::DEFAULT_MODEL_ID, good],
        ).unwrap();
        conn.execute(
            "INSERT INTO embeddings VALUES ('bbb', ?1, ?2, 'now')",
            rusqlite::params![videre_core::embeddings::DEFAULT_MODEL_ID, bad],
        ).unwrap();
        let vb = query_vectors(&conn).unwrap();
        assert_eq!(vb.hashes, vec!["aaa".to_string()]);
    }

    #[test]
    fn query_vectors_excludes_hashes_without_files() {
        let conn = mem_db();
        add_embeddings_table(&conn);
        conn.execute(
            "INSERT INTO file_hashes (path, hash, ext) VALUES ('/a/x.jpg', 'aaa', 'jpg')",
            [],
        ).unwrap();
        let v = videre_core::vectors::to_f16_bytes(&[1.0, 0.0]);
        for hash in ["aaa", "orphan"] {
            conn.execute(
                "INSERT INTO embeddings VALUES (?1, ?2, ?3, 'now')",
                rusqlite::params![hash, videre_core::embeddings::DEFAULT_MODEL_ID, v.clone()],
            ).unwrap();
        }
        let vb = query_vectors(&conn).unwrap();
        assert_eq!(vb.hashes, vec!["aaa".to_string()]);
    }

    #[test]
    fn file_json_includes_full_hash() {
        let f = row("/a/x.jpg", "deadbeefcafe", "jpg");
        let json = file_to_json(&f, false, false);
        assert!(json.contains("\"hash\":\"deadbeefcafe\""), "{json}");
    }

    #[test]
    fn json_str_escapes_less_than_for_script_safety() {
        let s = json_str("</script>");
        assert!(s.contains("\\u003c/script"), "{s}");
        assert!(!s.contains("</script>"), "{s}");
    }

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
        let faces = vec![(1i64, "Alice".to_string(), "0,0,10,10".to_string())];
        // make_face_thumb will return None (file doesn't exist), so faces_json
        // ends up empty - this test instead verifies the no-crash path and
        // that meta.faces is present in the output shape.
        let json = file_to_json_with_faces(&f, false, false, &faces);
        assert!(json.contains("\"meta\":"), "{json}");
    }

    #[test]
    fn parse_bbox_converts_xywh_to_corners() {
        assert_eq!(parse_bbox("10,20,5,5"), Some([10.0, 20.0, 15.0, 25.0]));
        assert_eq!(parse_bbox("not,valid"), None);
    }

    #[test]
    fn sanitize_person_label_strips_control_chars() {
        assert_eq!(sanitize_person_label("Al\u{0007}ice"), Some("Alice".to_string()));
    }

    #[test]
    fn sanitize_person_label_strips_bidi_override() {
        assert_eq!(sanitize_person_label("A\u{202E}lice"), Some("Alice".to_string()));
    }

    #[test]
    fn sanitize_person_label_keeps_zwj_emoji_sequences() {
        // family emoji: man + ZWJ + woman + ZWJ + girl - must survive intact,
        // since stripping the ZWJ (U+200D) would split it into three separate emoji.
        let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
        assert_eq!(sanitize_person_label(family), Some(family.to_string()));
    }

    #[test]
    fn sanitize_person_label_truncates_by_char_not_byte() {
        let long = "é".repeat(65); // each char is 2 bytes in UTF-8 - this is what actually
                                    // distinguishes .chars().take(60) from a byte-based slice
        let result = sanitize_person_label(&long).unwrap();
        assert_eq!(result.chars().count(), 60);
    }

    #[tokio::test]
    async fn remove_face_resets_is_primary() {
        let conn = mem_db_with_faces();
        conn.execute(
            "INSERT INTO faces (id, hash, bbox, embedding, cluster_id, person_label, confirmed, is_primary) \
             VALUES (1, 'h1', '0,0,10,10', X'0000', 5, 'Alice', 1, 1)",
            [],
        )
        .unwrap();
        let state = test_state(conn, true);
        let result = handle_remove_face(State(state.clone()), AxumJson(RemoveFaceRequest { face_id: 1 })).await;
        assert_eq!(result, Ok(StatusCode::OK));
        let conn = state.conn.lock().unwrap();
        let is_primary: i64 = conn.query_row("SELECT is_primary FROM faces WHERE id = 1", [], |r| r.get(0)).unwrap();
        assert_eq!(is_primary, 0, "is_primary must be reset when a face is removed");
    }

    #[tokio::test]
    async fn face_image_request_populates_and_then_hits_cache() {
        let dir = tempfile::tempdir().unwrap();
        let img_path = dir.path().join("test.jpg");
        let img = image::DynamicImage::new_rgb8(20, 20);
        img.save(&img_path).unwrap();

        let conn = mem_db_with_faces();
        conn.execute(
            "INSERT INTO file_hashes (path, hash, ext) VALUES (?1, 'facecachehash', 'jpg')",
            rusqlite::params![img_path.to_str().unwrap()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO faces (id, hash, bbox, embedding) VALUES (9001, 'facecachehash', '0,0,10,10', X'0000')",
            [],
        )
        .unwrap();
        let state = test_state(conn, true);

        let cache_path = videre_core::thumb_cache::face_thumb_path("facecachehash", 9001, 140);
        let _ = std::fs::remove_file(&cache_path);
        assert!(!cache_path.exists(), "precondition: no stale cache file");

        let first = handle_face_image(axum::extract::Path(9001), State(state.clone())).await;
        assert!(first.is_ok());
        assert!(cache_path.exists(), "handler must write through to the cache on a miss");

        let second = handle_face_image(axum::extract::Path(9001), State(state.clone())).await;
        assert!(second.is_ok(), "second request must be served from cache");

        let _ = std::fs::remove_file(&cache_path);
    }

    #[tokio::test]
    async fn delete_person_resets_faces_but_keeps_cluster_id() {
        let conn = mem_db_with_faces();
        conn.execute(
            "INSERT INTO faces (id, hash, bbox, embedding, cluster_id, person_label, confirmed, is_primary) \
             VALUES (1, 'h1', '0,0,10,10', X'0000', 5, 'Alice', 1, 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO faces (id, hash, bbox, embedding, cluster_id, person_label, confirmed, is_primary) \
             VALUES (2, 'h2', '0,0,10,10', X'0000', NULL, 'Alice', 1, 0)",
            [],
        )
        .unwrap();
        let state = test_state(conn, true);
        let result = handle_delete_person(
            State(state.clone()),
            AxumJson(DeletePersonRequest { label: "Alice".to_string() }),
        )
        .await;
        assert_eq!(result, Ok(StatusCode::OK));

        let conn = state.conn.lock().unwrap();
        let (cluster_id, person_label, confirmed, is_primary): (Option<i64>, Option<String>, i64, i64) = conn
            .query_row(
                "SELECT cluster_id, person_label, confirmed, is_primary FROM faces WHERE id = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(cluster_id, Some(5), "cluster_id must survive removal - the face rejoins its unassigned cluster");
        assert_eq!(person_label, None);
        assert_eq!(confirmed, 0);
        assert_eq!(is_primary, 0);

        let cluster_id2: Option<i64> =
            conn.query_row("SELECT cluster_id FROM faces WHERE id = 2", [], |r| r.get(0)).unwrap();
        assert_eq!(cluster_id2, None, "a face that was already a singleton stays a singleton");
    }

    #[tokio::test]
    async fn rename_person_updates_label() {
        let conn = mem_db_with_faces();
        conn.execute(
            "INSERT INTO faces (id, hash, bbox, embedding, person_label, confirmed) \
             VALUES (1, 'h1', '0,0,10,10', X'0000', 'Alice', 1)",
            [],
        )
        .unwrap();
        let state = test_state(conn, true);
        let result = handle_rename_person(
            State(state.clone()),
            AxumJson(RenamePersonRequest { old_label: "Alice".to_string(), new_label: "Alicia".to_string() }),
        )
        .await;
        assert_eq!(result, Ok(StatusCode::OK));
        let conn = state.conn.lock().unwrap();
        let label: String = conn.query_row("SELECT person_label FROM faces WHERE id = 1", [], |r| r.get(0)).unwrap();
        assert_eq!(label, "Alicia");
    }

    #[tokio::test]
    async fn rename_person_rejects_collision_with_existing_label() {
        let conn = mem_db_with_faces();
        conn.execute(
            "INSERT INTO faces (id, hash, bbox, embedding, person_label, confirmed) \
             VALUES (1, 'h1', '0,0,10,10', X'0000', 'Alice', 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO faces (id, hash, bbox, embedding, person_label, confirmed) \
             VALUES (2, 'h2', '0,0,10,10', X'0000', 'Bob', 1)",
            [],
        )
        .unwrap();
        let state = test_state(conn, true);
        let result = handle_rename_person(
            State(state.clone()),
            AxumJson(RenamePersonRequest { old_label: "Alice".to_string(), new_label: "Bob".to_string() }),
        )
        .await;
        assert_eq!(result, Err(StatusCode::CONFLICT));
        let conn = state.conn.lock().unwrap();
        let label: String = conn.query_row("SELECT person_label FROM faces WHERE id = 1", [], |r| r.get(0)).unwrap();
        assert_eq!(label, "Alice", "rename must not have applied on collision");
    }

    #[tokio::test]
    async fn rename_person_rejects_nonexistent_old_label() {
        let conn = mem_db_with_faces();
        let state = test_state(conn, true);
        let result = handle_rename_person(
            State(state.clone()),
            AxumJson(RenamePersonRequest { old_label: "Ghost".to_string(), new_label: "Someone".to_string() }),
        )
        .await;
        assert_eq!(result, Err(StatusCode::NOT_FOUND));
    }

    #[tokio::test]
    async fn original_image_request_serves_cached_heic_without_reconversion() {
        let conn = mem_db_with_faces();
        conn.execute(
            "INSERT INTO file_hashes (path, hash, ext) \
             VALUES ('/nonexistent/path/that/would/fail/to/convert.heic', 'origcachehash', 'heic')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO faces (id, hash, bbox, embedding) VALUES (9002, 'origcachehash', '0,0,10,10', X'0000')",
            [],
        )
        .unwrap();
        let state = test_state(conn, true);

        let cache_path = videre_core::thumb_cache::original_path("origcachehash");
        std::fs::create_dir_all(videre_core::thumb_cache::cache_dir()).unwrap();
        std::fs::write(&cache_path, b"fake-cached-jpeg-bytes").unwrap();

        // The source file path doesn't exist, so a live-conversion attempt
        // would fail (NOT_FOUND) - success here proves the cache was used
        // instead of trying to convert the (nonexistent) source file.
        let result = handle_original_image(axum::extract::Path(9002), State(state.clone())).await;
        assert!(result.is_ok(), "must serve from cache instead of failing to convert a nonexistent source file");

        let _ = std::fs::remove_file(&cache_path);
    }
}
