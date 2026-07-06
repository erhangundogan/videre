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
}

struct FileRow {
    path: String,
    hash: String,
    size_bytes: i64,
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

fn best_date(r: &FileRow) -> &str {
    if let Some(d) = r.exif_date.as_deref() {
        return d;
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

// Convert a HEIC file to a JPEG via sips (macOS built-in) at max_px longest edge, return base64.
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

fn query_stats(conn: &Connection) -> Stats {
    let total_files: i64 = conn
        .query_row("SELECT COUNT(*) FROM file_hashes", [], |r| r.get(0))
        .unwrap_or(0);

    let duplicate_groups: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM \
             (SELECT hash FROM file_hashes GROUP BY hash HAVING COUNT(*) > 1)",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);

    let duplicate_files: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM file_hashes \
             WHERE hash IN (SELECT hash FROM file_hashes GROUP BY hash HAVING COUNT(*) > 1)",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);

    let wasted_bytes: i64 = conn
        .query_row(
            "SELECT COALESCE(SUM(size_bytes * (cnt - 1)), 0) FROM \
             (SELECT hash, size_bytes, COUNT(*) as cnt \
              FROM file_hashes GROUP BY hash HAVING cnt > 1)",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);

    Stats { total_files, duplicate_groups, duplicate_files, wasted_bytes }
}

fn query_groups(conn: &Connection) -> Vec<Vec<FileRow>> {
    let mut stmt = conn
        .prepare(
            "SELECT path, hash, size_bytes, created_at, modified_at, exif_date, \
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
                created_at: r.get(3)?,
                modified_at: r.get(4)?,
                exif_date: r.get(5)?,
                gps_lat: r.get(6)?,
                gps_lon: r.get(7)?,
                width: r.get(8)?,
                height: r.get(9)?,
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

    // Within each group: oldest date first: exif_date wins; falls back to min(created_at, modified_at)
    for group in &mut groups {
        group.sort_by(|a, b| best_date(a).cmp(best_date(b)));
    }

    // Sort groups by wasted space descending: biggest waste first
    groups.sort_by(|a, b| {
        let wa = a[0].size_bytes * (a.len() as i64 - 1);
        let wb = b[0].size_bytes * (b.len() as i64 - 1);
        wb.cmp(&wa)
    });

    groups
}

fn image_cell(path: &str, ext: &str, heic: bool, heic_original: bool) -> String {
    match ext {
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "bmp" => {
            let url = format!("file://{}", path);
            let url_esc = esc(&url);
            let js_url = url.replace('\'', "\\'");
            format!(
                "<a href=\"{url_esc}\" target=\"_blank\" onclick=\"openLb(event,'{js_url}')\">",
            ) + &format!(
                "<img src=\"{url_esc}\" class=\"thumb\" loading=\"lazy\" \
                 onerror=\"this.parentElement.innerHTML='<span class=\\'no-prev\\'>no preview</span>'\"></a>"
            )
        }
        "heic" if heic && heic_original => {
            match (heic_to_b64(path, 240), heic_to_b64(path, 1200)) {
                (Some(thumb), Some(full)) => format!(
                    "<img src=\"data:image/jpeg;base64,{thumb}\" \
                     data-lb=\"data:image/jpeg;base64,{full}\" \
                     class=\"thumb\" onclick=\"openLb(event,this.dataset.lb)\">"
                ),
                (Some(thumb), None) => format!(
                    "<img src=\"data:image/jpeg;base64,{thumb}\" class=\"thumb\" \
                     onclick=\"openLb(event,this.src)\">"
                ),
                _ => "<span class=\"no-prev\">HEIC</span>".into(),
            }
        }
        "heic" if heic => {
            match heic_to_b64(path, 240) {
                Some(b64) => format!(
                    "<img src=\"data:image/jpeg;base64,{b64}\" class=\"thumb\" \
                     onclick=\"openLb(event,this.src)\">"
                ),
                None => "<span class=\"no-prev\">HEIC</span>".into(),
            }
        }
        "heic" => "<span class=\"no-prev\">HEIC</span>".into(),
        "tiff" => "<span class=\"no-prev\">TIFF</span>".into(),
        "dng" => "<span class=\"no-prev\">DNG</span>".into(),
        "mov" | "mp4" => {
            let url = format!("file://{}", path);
            let url_esc = esc(&url);
            let js_url = url.replace('\'', "\\'");
            format!(
                "<video src=\"{url_esc}\" class=\"thumb\" preload=\"metadata\" \
                 muted playsinline \
                 onclick=\"openLb(event,'{js_url}','video')\" \
                 onerror=\"this.outerHTML='<span class=\\'no-prev\\'>no preview</span>'\"></video>"
            )
        }
        _ => "<span class=\"no-prev\">&mdash;</span>".into(),
    }
}

fn generate_html(db_path: &str, stats: &Stats, groups: &[Vec<FileRow>], heic: bool, heic_original: bool) -> String {
    use chrono::Utc;
    let now = Utc::now().format("%Y-%m-%d %H:%M UTC").to_string();

    let mut out = String::with_capacity(256 * 1024);

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
        "gap:10px;user-select:none;}\n",
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
        ".path-cell{font-family:monospace;font-size:11px;max-width:380px}\n",
        ".path-text{word-break:break-all;color:#3f3f46}\n",
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
        "</style>\n</head>\n<body>\n",
        "<div class=\"lightbox\" id=\"lb\" onclick=\"closeLb()\">\n",
        "  <img id=\"lb-img\" src=\"\" alt=\"\" onclick=\"event.stopPropagation()\">\n",
        "  <video id=\"lb-vid\" src=\"\" controls autoplay onclick=\"event.stopPropagation()\" style=\"display:none\"></video>\n",
        "</div>\n",
    ));

    // Header
    out.push_str(&format!(
        "<div class=\"header\">\
          <h1>dupe report</h1>\
          <p class=\"subtitle\">{db} &mdash; {now}</p>\
          <div class=\"stats\">\
            <div class=\"stat\"><span class=\"num\">{total}</span><span class=\"label\">Files scanned</span></div>\
            <div class=\"stat warn\"><span class=\"num\">{groups}</span><span class=\"label\">Duplicate groups</span></div>\
            <div class=\"stat warn\"><span class=\"num\">{dups}</span><span class=\"label\">Duplicate files</span></div>\
            <div class=\"stat warn\"><span class=\"num\">{wasted}</span><span class=\"label\">Wasted space</span></div>\
          </div>\
        </div>\n",
        db = esc(db_path),
        now = now,
        total = stats.total_files,
        groups = stats.duplicate_groups,
        dups = stats.duplicate_files,
        wasted = format_bytes(stats.wasted_bytes),
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
          <span class=\"info\">{} groups</span>\
        </div>\n<div class=\"groups\" id=\"groups-container\">\n",
        stats.duplicate_groups,
    ));

    if groups.is_empty() {
        out.push_str("<div class=\"no-dupes\">No duplicate groups found.</div>\n");
    }

    for (i, group) in groups.iter().enumerate() {
        let hash = &group[0].hash;
        let hash_prefix = &hash[..hash.len().min(8)];
        let count = group.len();
        let size_each = group[0].size_bytes;
        let wasted = size_each * (count as i64 - 1);
        let keep_date = best_date(&group[0]).to_string();

        out.push_str(&format!(
            "<div class=\"group\" id=\"g{i}\" data-waste=\"{wasted}\" data-date=\"{keep_date}\">\
              <div class=\"group-header\" onclick=\"toggle('g{i}')\">\
                <span class=\"arrow\">&#9654;</span>\
                <code class=\"hash\">{hash}</code>\
                <span class=\"group-meta\">{count} copies &middot; {each} each</span>\
                <span class=\"waste\">&minus;{wasted_fmt} wasted</span>\
              </div>\
              <div class=\"group-body\">\
                <table>\
                  <thead><tr>\
                    <th class=\"preview-th\">Preview</th>\
                    <th>Status</th><th>Filename</th><th>Path</th>\
                    <th>Size</th><th>Created</th><th>Modified</th><th>EXIF date</th>\
                    <th>GPS</th><th>Dimensions</th>\
                  </tr></thead><tbody>\n",
            i = i,
            wasted = wasted,
            keep_date = esc(&keep_date),
            hash = esc(hash_prefix),
            count = count,
            each = format_bytes(size_each),
            wasted_fmt = format_bytes(wasted),
        ));

        for (j, file) in group.iter().enumerate() {
            let is_keep = j == 0;
            let row_class = if is_keep { "keep" } else { "remove" };
            let badge_class = if is_keep { "keep-badge" } else { "remove-badge" };
            let badge_text = if is_keep { "KEEP" } else { "REMOVE" };

            let filename = std::path::Path::new(&file.path)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();

            let created = file
                .created_at
                .as_deref()
                .map(|d| esc(&d[..d.len().min(19)]))
                .unwrap_or_else(|| "<span class=\"dim\">\u{2014}</span>".into());

            let modified = file
                .modified_at
                .as_deref()
                .map(|d| esc(&d[..d.len().min(19)]))
                .unwrap_or_else(|| "<span class=\"dim\">\u{2014}</span>".into());

            let exif = file
                .exif_date
                .as_deref()
                .map(|d| esc(d))
                .unwrap_or_else(|| "<span class=\"dim\">\u{2014}</span>".into());

            let gps = match (file.gps_lat, file.gps_lon) {
                (Some(lat), Some(lon)) => format!(
                    "<div class=\"gps\"><a href=\"https://maps.google.com/?q={lat:.6},{lon:.6}\" \
                     target=\"_blank\" rel=\"noopener\">{dlat:.4}&deg;{ns} {dlon:.4}&deg;{ew}</a></div>",
                    lat = lat, lon = lon,
                    dlat = lat.abs(), ns = if lat >= 0.0 { "N" } else { "S" },
                    dlon = lon.abs(), ew = if lon >= 0.0 { "E" } else { "W" },
                ),
                _ => "<span class=\"dim\">\u{2014}</span>".into(),
            };

            let dims = match (file.width, file.height) {
                (Some(w), Some(h)) => format!("{w}\u{d7}{h}"),
                _ => "<span class=\"dim\">\u{2014}</span>".into(),
            };

            let js_path = file.path.replace('\\', "\\\\").replace('\'', "\\'");

            let ext = std::path::Path::new(&file.path)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_lowercase();

            out.push_str(&format!(
                "<tr class=\"{row_class}\">\
                  <td class=\"preview\">{preview}</td>\
                  <td class=\"badge\"><span class=\"{badge_class}\">{badge_text}</span></td>\
                  <td class=\"filename\" title=\"{fname_esc}\">{fname_esc}</td>\
                  <td class=\"path-cell\"><span class=\"path-text\">{path_esc}</span>\
                    <button class=\"copy-btn\" onclick=\"copyPath('{js_path}')\" title=\"Copy path\">&#x2398;</button></td>\
                  <td>{size}</td>\
                  <td class=\"dim\">{created}</td>\
                  <td class=\"dim\">{modified}</td>\
                  <td class=\"dim\">{exif}</td>\
                  <td>{gps}</td>\
                  <td class=\"dim\">{dims}</td>\
                </tr>\n",
                row_class = row_class,
                preview = image_cell(&file.path, &ext, heic, heic_original),
                badge_class = badge_class,
                badge_text = badge_text,
                fname_esc = esc(&filename),
                path_esc = esc(&file.path),
                js_path = js_path,
                size = format_bytes(file.size_bytes),
                created = created,
                modified = modified,
                exif = exif,
                gps = gps,
                dims = dims,
            ));
        }

        out.push_str("      </tbody></table></div></div>\n");
    }

    out.push_str("</div>\n");

    // JS
    out.push_str(concat!(
        "<script>\n",
        "function toggle(id){\n",
        "  var g=document.getElementById(id);\n",
        "  g.classList.toggle('open');\n",
        "  if(g.classList.contains('open')){\n",
        "    g.querySelectorAll('img').forEach(function(img){\n",
        "      if(img.loading==='lazy'){img.loading='eager';}\n",
        "    });\n",
        "    g.querySelectorAll('video').forEach(function(v){\n",
        "      if(v.preload==='metadata'){v.preload='auto';}\n",
        "    });\n",
        "  }\n",
        "}\n",
        "function expandAll(){\n",
        "  document.querySelectorAll('.group').forEach(function(g){\n",
        "    g.classList.add('open');\n",
        "    g.querySelectorAll('img').forEach(function(img){\n",
        "      if(img.loading==='lazy'){img.loading='eager';}\n",
        "    });\n",
        "    g.querySelectorAll('video').forEach(function(v){\n",
        "      if(v.preload==='metadata'){v.preload='auto';}\n",
        "    });\n",
        "  });\n",
        "}\n",
        "function collapseAll(){document.querySelectorAll('.group').forEach(function(g){g.classList.remove('open');});}\n",
        "function copyPath(p){\n",
        "  navigator.clipboard.writeText(p).catch(function(){\n",
        "    var t=document.createElement('textarea');t.value=p;\n",
        "    document.body.appendChild(t);t.select();document.execCommand('copy');\n",
        "    document.body.removeChild(t);\n",
        "  });\n",
        "}\n",
        "function openLb(e,url,type){\n",
        "  e.preventDefault();e.stopPropagation();\n",
        "  var img=document.getElementById('lb-img');\n",
        "  var vid=document.getElementById('lb-vid');\n",
        "  if(type==='video'){\n",
        "    img.style.display='none';\n",
        "    vid.style.display='block';\n",
        "    vid.src=url;\n",
        "    vid.play();\n",
        "  } else {\n",
        "    vid.style.display='none';\n",
        "    img.style.display='block';\n",
        "    img.src=url;\n",
        "  }\n",
        "  document.getElementById('lb').classList.add('on');\n",
        "}\n",
        "function closeLb(){\n",
        "  var vid=document.getElementById('lb-vid');\n",
        "  vid.pause();vid.src='';\n",
        "  document.getElementById('lb-img').src='';\n",
        "  document.getElementById('lb').classList.remove('on');\n",
        "}\n",
        "function sortGroups(by){\n",
        "  var container=document.getElementById('groups-container');\n",
        "  var groups=Array.from(container.querySelectorAll(':scope > .group'));\n",
        "  groups.sort(function(a,b){\n",
        "    if(by==='waste'){\n",
        "      return Number(b.dataset.waste)-Number(a.dataset.waste);\n",
        "    } else {\n",
        "      var da=a.dataset.date||'';var db=b.dataset.date||'';\n",
        "      return by==='date-asc'?da.localeCompare(db):db.localeCompare(da);\n",
        "    }\n",
        "  });\n",
        "  groups.forEach(function(g){container.appendChild(g);});\n",
        "}\n",
        "document.addEventListener('keydown',function(e){if(e.key==='Escape')closeLb();});\n",
        "</script>\n</body>\n</html>",
    ));

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
    let html = generate_html(&args.db.to_string_lossy(), &stats, &groups, args.heic, args.heic_original);

    fs::write(&output, &html).expect("failed to write HTML file");

    eprintln!("Report: {}", output.display());
    eprintln!(
        "{} groups · {} duplicate files · {} wasted",
        stats.duplicate_groups,
        stats.duplicate_files,
        format_bytes(stats.wasted_bytes)
    );
}
