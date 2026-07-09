use clap::Parser;
use rusqlite::Connection;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "dupe-report", about = "Generate an HTML duplicate report from a dupe SQLite database")]
struct Args {
    /// SQLite database produced by: dupe --output-sqlite <db>
    db: PathBuf,

    /// HTML output path [default: <db>_report.html]
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Embed HEIC thumbnails as base64 JPEG (requires sips, macOS only; increases HTML size)
    #[arg(long)]
    heic: bool,

    /// Embed HEIC thumbnails + full lightbox version (requires sips, macOS only; significantly increases HTML size)
    #[arg(long)]
    heic_original: bool,

    /// Include every file (singular and duplicate) in a searchable gallery
    #[arg(long)]
    all: bool,
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
        .query_map([dupe_core::embeddings::DEFAULT_MODEL_ID], |r| {
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

fn heic_to_b64(path: &str, max_px: u32) -> Option<String> {
    use std::hash::{Hash, Hasher};
    use std::collections::hash_map::DefaultHasher;
    let mut h = DefaultHasher::new();
    path.hash(&mut h);
    let tmp = std::env::temp_dir().join(format!("dupe_{:016x}_{max_px}.jpg", h.finish()));
    let ok = std::process::Command::new("sips")
        .args(["-s", "format", "jpeg", "-Z", &max_px.to_string(), path, "--out"])
        .arg(&tmp)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !ok { return None; }
    let bytes = std::fs::read(&tmp).ok()?;
    let _ = std::fs::remove_file(&tmp);
    Some(base64_encode(&bytes))
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
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c    => out.push(c),
        }
    }
    out.push('"');
    out
}

fn file_to_json(f: &FileRow, heic: bool, heic_original: bool) -> String {
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

    format!(
        "{{\"hash\":{hash},\"path\":{path},\"ext\":{ext},\"size\":{size},\
         \"cr\":{cr},\"mo\":{mo},\"ex\":{ex},\
         \"lat\":{lat},\"lon\":{lon},\"w\":{w},\"h\":{h},\
         \"tb\":{tb},\"fb\":{fb}}}",
        hash = json_str(&f.hash),
        path = json_str(&f.path),
        ext  = json_str(&f.ext),
        size = f.size_bytes,
        cr = cr, mo = mo, ex = ex,
        lat = lat, lon = lon, w = w, h = h,
        tb = tb, fb = fb,
    )
}

fn group_to_json(group: &[FileRow], heic: bool, heic_original: bool) -> String {
    let hash_prefix = &group[0].hash[..group[0].hash.len().min(8)];
    let waste = group[0].size_bytes * (group.len() as i64 - 1);
    let keep_date = best_date(&group[0]);
    let date_json = if keep_date.is_empty() { "null".to_string() } else { json_str(keep_date) };
    let files_json: Vec<String> = group.iter()
        .map(|f| file_to_json(f, heic, heic_original))
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
    .collect()
}

fn generate_html(
    db_path: &str,
    stats: &Stats,
    groups: &[Vec<FileRow>],
    all_files: Option<&[FileRow]>,
    vectors: Option<&VectorBlock>,
    heic: bool,
    heic_original: bool,
) -> String {
    use chrono::Utc;
    let now = Utc::now().format("%Y-%m-%d %H:%M UTC").to_string();

    let mut out = String::with_capacity(512 * 1024);

    out.push_str(concat!(
        "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n",
        "<meta charset=\"UTF-8\">\n",
        "<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\n",
        "<title>dupe report</title>\n<style>\n",
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
        "</style>\n</head>\n<body>\n",
        "<div id=\"sort-overlay\"><div class=\"sort-card\">",
        "<div class=\"spinner\"></div>Sorting&hellip;</div></div>\n",
        "<div class=\"lightbox\" id=\"lb\" onclick=\"closeLb()\">\n",
        "  <img id=\"lb-img\" src=\"\" alt=\"\" onclick=\"event.stopPropagation()\">\n",
        "  <video id=\"lb-vid\" src=\"\" controls autoplay onclick=\"event.stopPropagation()\" style=\"display:none\"></video>\n",
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
    out.push_str(&format!(
        "<div class=\"header\">\
          <h1>dupe report</h1>\
          <p class=\"subtitle\">{db} &mdash; {now}</p>\
          <div class=\"stats\">\
            <div class=\"stat\"><span class=\"num\">{total}</span><span class=\"label\">Files scanned</span></div>\
            <div class=\"stat warn\"><span class=\"num\">{groups}</span><span class=\"label\">Duplicate groups</span></div>\
            <div class=\"stat warn\"><span class=\"num\">{dups}</span><span class=\"label\">Duplicate files</span></div>\
            <div class=\"stat warn\"><span class=\"num\">{wasted}</span><span class=\"label\">Wasted space</span></div>\
            {embedded_stat}\
          </div>\
        </div>\n",
        db     = esc(db_path),
        now    = now,
        total  = stats.total_files,
        groups = stats.duplicate_groups,
        dups   = stats.duplicate_files,
        wasted = format_bytes(stats.wasted_bytes),
        embedded_stat = embedded_stat,
    ));

    // Toolbar
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

    if all_files.is_some() {
        out.push_str("<div class=\"results-panel\" id=\"results\" style=\"display:none\"></div>\n");
    }

    // Empty groups container — JS fills it
    out.push_str("<div class=\"groups\" id=\"groups-container\">");
    if groups.is_empty() {
        out.push_str("<div class=\"no-dupes\">No duplicate groups found.</div>");
    }
    out.push_str("</div>\n");
    out.push_str("<div class=\"more-wrap\"><button id=\"more-btn\" onclick=\"showMore()\"></button></div>\n");

    if let Some(files) = all_files {
        out.push_str(&format!(
            "<div class=\"gallery-head\"><h2>All files</h2><span class=\"info\" id=\"gallery-info\">{} files</span></div>\n\
             <div class=\"gallery\" id=\"gallery\"></div>\n\
             <div class=\"more-wrap\"><button id=\"gallery-more\" onclick=\"showMoreGallery()\"></button></div>\n",
            files.len()
        ));
    }

    // Embed all group data as JSON
    out.push_str("<script>\nvar GROUPS=[\n");
    for (i, group) in groups.iter().enumerate() {
        if i > 0 { out.push(','); }
        out.push('\n');
        out.push_str(&group_to_json(group, heic, heic_original));
    }
    out.push_str("\n];\n");

    // All-files gallery data and similarity vectors (--all only).
    // Without --all nothing is emitted so the page is unchanged.
    if let Some(files) = all_files {
        out.push_str("var ALLFILES=[\n");
        for (i, f) in files.iter().enumerate() {
            if i > 0 { out.push(','); }
            out.push_str(&file_to_json(f, heic, heic_original));
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
function buildPreview(f){
  var ext=f.ext,path=f.path;
  if(ext==='jpg'||ext==='jpeg'||ext==='png'||ext==='gif'||ext==='webp'||ext==='bmp'){
    var url='file://'+path;
    return '<a href="'+escA(url)+'" target="_blank" data-lb-url="'+escA(url)+'" data-lb-type="image">'+
      '<img src="'+escA(url)+'" class="thumb" loading="lazy" '+
      'onerror="this.parentElement.innerHTML=\'<span class=no-prev>no preview</span>\'"></a>';
  }
  if(ext==='heic'){
    if(f.tb){
      var src='data:image/jpeg;base64,'+f.tb;
      var lb=f.fb?'data:image/jpeg;base64,'+f.fb:src;
      return '<img src="'+src+'" class="thumb" data-lb-url="'+escA(lb)+'" data-lb-type="image">';
    }
    return '<span class="no-prev">HEIC</span>';
  }
  if(ext==='tiff')return '<span class="no-prev">TIFF</span>';
  if(ext==='dng') return '<span class="no-prev">DNG</span>';
  if(ext==='mov'||ext==='mp4'){
    var url='file://'+path;
    return '<video src="'+escA(url)+'" class="thumb" preload="metadata" muted playsinline '+
      'data-lb-url="'+escA(url)+'" data-lb-type="video" '+
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
function openLb(url,type){
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
// Event delegation: toggle, lightbox, copy — one listener for all dynamic content
document.addEventListener('click',function(e){
  var lb=e.target.closest('[data-lb-url]');
  if(lb){e.preventDefault();e.stopPropagation();openLb(lb.dataset.lbUrl,lb.dataset.lbType||'image');return;}
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
"#);

    out.push_str("</script>\n</body>\n</html>");
    out
}

fn main() {
    let args = Args::parse();

    if !args.db.exists() {
        eprintln!("Error: {:?} does not exist", args.db);
        std::process::exit(1);
    }

    if args.heic_original && !args.heic {
        eprintln!("Warning: --heic-original has no effect without --heic");
    }

    let output = args.output.unwrap_or_else(|| {
        let stem = args.db.file_stem().unwrap_or_default().to_string_lossy();
        args.db.with_file_name(format!("{}_report.html", stem))
    });

    let conn = Connection::open(&args.db).expect("failed to open database");
    let stats = query_stats(&conn);
    let groups = query_groups(&conn);
    let all_files = args.all.then(|| query_all_files(&conn));
    let vectors = if args.all {
        let v = query_vectors(&conn);
        if v.is_none() {
            eprintln!("no embeddings found; run dupe-embed for similarity search");
        }
        v
    } else {
        None
    };
    let html = generate_html(
        &args.db.to_string_lossy(),
        &stats,
        &groups,
        all_files.as_deref(),
        vectors.as_ref(),
        args.heic,
        args.heic_original,
    );

    fs::write(&output, &html).expect("failed to write HTML file");

    eprintln!("Report: {}", output.display());
    eprintln!(
        "{} groups · {} duplicate files · {} wasted",
        stats.duplicate_groups,
        stats.duplicate_files,
        format_bytes(stats.wasted_bytes)
    );
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
        let one = dupe_core::vectors::to_f16_bytes(&[1.0, 0.0]);
        let two = dupe_core::vectors::to_f16_bytes(&[0.0, 1.0]);
        // Insert out of order to prove ORDER BY hash
        conn.execute(
            "INSERT INTO embeddings VALUES ('bbb', ?1, ?2, 'now')",
            rusqlite::params![dupe_core::embeddings::DEFAULT_MODEL_ID, two],
        ).unwrap();
        conn.execute(
            "INSERT INTO embeddings VALUES ('aaa', ?1, ?2, 'now')",
            rusqlite::params![dupe_core::embeddings::DEFAULT_MODEL_ID, one.clone()],
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
        let good = dupe_core::vectors::to_f16_bytes(&[1.0, 0.0]);
        let bad = dupe_core::vectors::to_f16_bytes(&[1.0, 0.0, 0.0]); // 3 dims
        conn.execute(
            "INSERT INTO embeddings VALUES ('aaa', ?1, ?2, 'now')",
            rusqlite::params![dupe_core::embeddings::DEFAULT_MODEL_ID, good],
        ).unwrap();
        conn.execute(
            "INSERT INTO embeddings VALUES ('bbb', ?1, ?2, 'now')",
            rusqlite::params![dupe_core::embeddings::DEFAULT_MODEL_ID, bad],
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
        let v = dupe_core::vectors::to_f16_bytes(&[1.0, 0.0]);
        for hash in ["aaa", "orphan"] {
            conn.execute(
                "INSERT INTO embeddings VALUES (?1, ?2, ?3, 'now')",
                rusqlite::params![hash, dupe_core::embeddings::DEFAULT_MODEL_ID, v.clone()],
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
}
