//! Image-bytes operations shared by the axum `--faces` server and the Tauri
//! desktop app: aligned face thumbnails and full original images.

use crate::error::{Error, Result};
use rusqlite::Connection;
use std::io::BufReader;

const FACE_THUMB_SIZE: u32 = 140;

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
        let timeout_path = path.to_string();
        let img = videre_core::io_timeout::run_with_timeout(
            videre_core::io_timeout::DEFAULT_IO_TIMEOUT,
            move || image::open(&timeout_path),
        )
        .ok()?
        .ok()?;
        let cropped = crop_face_square(&img, bbox);
        Some(apply_exif_orientation(cropped, path))
    }
}

/// Bounds a plain (non-HEIC) file read against a stale/disconnected mount
/// point the same way `videre_core::heic` bounds `qlmanage`, so a single
/// unreachable file can't hang the caller (an axum request thread, or a
/// synchronous Tauri command) forever.
fn read_with_timeout(path: &str) -> std::io::Result<Vec<u8>> {
    let owned = path.to_string();
    videre_core::io_timeout::run_with_timeout(videre_core::io_timeout::DEFAULT_IO_TIMEOUT, move || {
        std::fs::read(&owned)
    })
    .unwrap_or_else(|_| {
        Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            format!("timed out reading {path} (file may be unreachable - is its drive connected?)"),
        ))
    })
}

pub fn mime_for_ext(ext: &str) -> &'static str {
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

/// The single-row query `face_image_bytes` needs before it can do any image
/// work, split out so a caller holding a shared/locked `Connection` (the
/// axum server and the Tauri app both serialize on one `Mutex<Connection>`)
/// can release that lock immediately after this cheap lookup, instead of
/// holding it for the entire decode/crop/resize/encode/cache-write below -
/// which otherwise fully serializes every thumbnail request behind the lock,
/// turning a many-thousand-singleton library into one thumbnail at a time.
pub struct FaceLookup {
    pub bbox_json: String,
    pub file_path: String,
    pub hash: String,
}

/// The cheap part of `face_image_bytes`: just the DB row. No image I/O.
pub fn face_lookup(conn: &Connection, face_id: i64) -> Result<FaceLookup> {
    let (bbox_json, file_path, hash): (String, String, String) = conn
        .query_row(
            "SELECT f.bbox, fh.path, f.hash FROM faces f \
             JOIN file_hashes fh ON f.hash = fh.hash WHERE f.id = ?1 LIMIT 1",
            [face_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .map_err(|_| Error::NotFound)?;
    Ok(FaceLookup { bbox_json, file_path, hash })
}

/// The expensive part of `face_image_bytes`: cache check, decode/crop/encode,
/// write-through. Takes no `Connection`, so it can run without holding the
/// shared DB lock.
pub fn face_bytes_from_lookup(lookup: &FaceLookup, face_id: i64) -> Result<Vec<u8>> {
    let cache = videre_core::thumb_cache::face_thumb_path(&lookup.hash, face_id, FACE_THUMB_SIZE);
    if videre_core::thumb_cache::face_thumb_exists(&lookup.hash, face_id, FACE_THUMB_SIZE) {
        if let Ok(bytes) = read_with_timeout(&cache.to_string_lossy()) {
            return Ok(bytes);
        }
    }

    let parts: Vec<f32> = lookup.bbox_json.split(',').filter_map(|s| s.trim().parse().ok()).collect();
    if parts.len() != 4 {
        return Err(Error::NotFound);
    }
    let bbox = [parts[0], parts[1], parts[0] + parts[2], parts[1] + parts[3]];
    let thumb = make_face_thumb(&lookup.file_path, bbox, face_id).ok_or(Error::NotFound)?;
    let mut buf = Vec::new();
    thumb
        .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Jpeg)
        .map_err(|_| Error::NotFound)?;

    // Best-effort write-through (a cache-write failure must not fail the read).
    if let Some(parent) = cache.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let tmp = cache.with_extension(format!("tmp{}", std::process::id()));
    if std::fs::write(&tmp, &buf).is_ok() {
        let _ = std::fs::rename(&tmp, &cache);
    }
    Ok(buf)
}

/// JPEG bytes for a single aligned face thumbnail (140px), reading the disk
/// cache first and converting from the source image (HEIC via QuickLook) on a
/// miss, writing through to the cache. Returns `Error::NotFound` if the face id
/// is unknown or the crop cannot be produced. Synchronous: callers that need
/// async should run this on a blocking thread.
///
/// Holds `conn` only for the initial lookup (see `face_lookup`); callers that
/// share `conn` behind a lock across many concurrent requests should call
/// `face_lookup`/`face_bytes_from_lookup` directly instead, releasing the
/// lock between the two.
pub fn face_image_bytes(conn: &Connection, face_id: i64) -> Result<Vec<u8>> {
    let lookup = face_lookup(conn, face_id)?;
    face_bytes_from_lookup(&lookup, face_id)
}

/// The single-row query `original_image_bytes` needs before any image I/O -
/// see `FaceLookup` for why this split matters for concurrency.
pub struct OriginalLookup {
    pub file_path: String,
    pub hash: String,
}

/// The cheap part of `original_image_bytes`: just the DB row. No image I/O.
pub fn original_lookup(conn: &Connection, face_id: i64) -> Result<OriginalLookup> {
    let (file_path, hash): (String, String) = conn
        .query_row(
            "SELECT fh.path, f.hash FROM faces f \
             JOIN file_hashes fh ON f.hash = fh.hash WHERE f.id = ?1 LIMIT 1",
            [face_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|_| Error::NotFound)?;
    Ok(OriginalLookup { file_path, hash })
}

/// The expensive part of `original_image_bytes`: read/convert/cache. Takes no
/// `Connection`, so it can run without holding the shared DB lock.
pub fn original_bytes_from_lookup(
    lookup: &OriginalLookup,
    face_id: i64,
) -> Result<(&'static str, Vec<u8>)> {
    let file_path = &lookup.file_path;
    let hash = &lookup.hash;
    let ext = std::path::Path::new(file_path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    if ext == "heic" {
        if let Ok(bytes) =
            read_with_timeout(&videre_core::thumb_cache::original_path(&hash).to_string_lossy())
        {
            return Ok(("image/jpeg", bytes));
        }
        let img = videre_core::heic::heic_via_quicklook(&file_path, &format!("orig{face_id}"))
            .ok_or(Error::NotFound)?;
        let mut buf = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Jpeg)
            .map_err(|_| Error::NotFound)?;
        let final_path = videre_core::thumb_cache::original_path(&hash);
        if let Some(parent) = final_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let tmp = final_path.with_extension(format!("tmp{}", std::process::id()));
        if std::fs::write(&tmp, &buf).is_ok() {
            let _ = std::fs::rename(&tmp, &final_path);
        }
        Ok(("image/jpeg", buf))
    } else {
        let bytes = read_with_timeout(file_path).map_err(|_| Error::NotFound)?;
        Ok((mime_for_ext(&ext), bytes))
    }
}

/// Bytes for the full original image behind a face (raw for common formats,
/// QuickLook-converted JPEG for HEIC, with the HEIC result cached). Returns the
/// MIME type alongside the bytes. `Error::NotFound` if the id is unknown or the
/// file cannot be read/converted. Synchronous.
///
/// Holds `conn` only for the initial lookup (see `original_lookup`); callers
/// that share `conn` behind a lock across many concurrent requests should
/// call `original_lookup`/`original_bytes_from_lookup` directly instead,
/// releasing the lock between the two.
pub fn original_image_bytes(conn: &Connection, face_id: i64) -> Result<(&'static str, Vec<u8>)> {
    let lookup = original_lookup(conn, face_id)?;
    original_bytes_from_lookup(&lookup, face_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_face_id_is_not_found() {
        let conn = Connection::open_in_memory().unwrap();
        videre_core::face_db::create_faces_table(&conn).unwrap();
        conn.execute_batch("CREATE TABLE file_hashes (hash TEXT PRIMARY KEY, path TEXT);").unwrap();
        assert!(matches!(face_image_bytes(&conn, 999), Err(Error::NotFound)));
        assert!(matches!(original_image_bytes(&conn, 999), Err(Error::NotFound)));
    }

    #[test]
    fn face_lookup_unknown_id_is_not_found() {
        let conn = Connection::open_in_memory().unwrap();
        videre_core::face_db::create_faces_table(&conn).unwrap();
        conn.execute_batch("CREATE TABLE file_hashes (hash TEXT PRIMARY KEY, path TEXT);").unwrap();
        assert!(matches!(face_lookup(&conn, 999), Err(Error::NotFound)));
    }

    #[test]
    fn original_lookup_unknown_id_is_not_found() {
        let conn = Connection::open_in_memory().unwrap();
        videre_core::face_db::create_faces_table(&conn).unwrap();
        conn.execute_batch("CREATE TABLE file_hashes (hash TEXT PRIMARY KEY, path TEXT);").unwrap();
        assert!(matches!(original_lookup(&conn, 999), Err(Error::NotFound)));
    }

    #[test]
    fn face_lookup_does_not_touch_the_filesystem() {
        // Regression test for the thumbnail-rendering serialization bug: the
        // DB lookup must be a pure query with no image I/O, so callers can
        // release the connection lock before doing the expensive part.
        let conn = Connection::open_in_memory().unwrap();
        videre_core::face_db::create_faces_table(&conn).unwrap();
        conn.execute_batch("CREATE TABLE file_hashes (hash TEXT PRIMARY KEY, path TEXT);").unwrap();
        conn.execute(
            "INSERT INTO file_hashes (hash, path) VALUES ('h1', '/no/such/file.jpg')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO faces (id, hash, bbox, embedding) VALUES (1, 'h1', '0,0,10,10', X'00')",
            [],
        )
        .unwrap();
        let lookup = face_lookup(&conn, 1).unwrap();
        assert_eq!(lookup.file_path, "/no/such/file.jpg");
        assert_eq!(lookup.hash, "h1");
        assert_eq!(lookup.bbox_json, "0,0,10,10");
    }
}
