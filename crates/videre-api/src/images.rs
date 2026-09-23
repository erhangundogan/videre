//! Image-bytes operations shared by every videre-api caller (the axum
//! `--faces` server in this repo): aligned face thumbnails and full original
//! images.

use crate::error::{Error, Result};
use rusqlite::Connection;

const FACE_THUMB_SIZE: u32 = 140;

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
/// `oriented` mirrors `faces.oriented` and says which canvas the bbox is in:
///
/// - `true`: the row was detected on the display canvas (every row written
///   since the decode became orientation-correct). Decode upright, then crop.
/// - `false` (every row written before the fix): the bbox is in the raw
///   sensor canvas, so crop the raw decode first, then rotate the small
///   square crop. Equivalent to the pre-fix behavior.
///
/// bbox coordinates are stored in terms of the *full-size* decoded image
/// (videre faces rescales detections back to original width/height before
/// writing to the DB), so the thumbnail must be cropped from an image of
/// the same dimensions used at detection time.
///
/// For HEIC: videre faces converts via QuickLook (see
/// `videre_core::heic::heic_via_quicklook`), which already applies correct
/// rotation, so no separate orientation step is needed.
///
/// `pub`: the static-page base64 thumbnail path (`face_thumb_b64` in
/// `render`) also needs this exact crop+orientation logic, so it calls
/// through here instead of keeping its own duplicate copy.
pub fn make_face_thumb(
    path: &str,
    bbox: [f32; 4],
    oriented: bool,
    face_id: i64,
) -> Option<image::DynamicImage> {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    if ext == "heic" {
        // None: bbox is stored relative to a full-res decode. See the
        // safety note on heic_via_quicklook.
        let img = videre_core::heic::heic_via_quicklook(path, &format!("thumb{face_id}"), None)?;
        return Some(crop_face_square(&img, bbox));
    }
    let timeout_path = path.to_string();
    let decoded = match videre_core::io_timeout::run_with_timeout(
        videre_core::io_timeout::DEFAULT_IO_TIMEOUT,
        move || {
            if oriented {
                videre_core::image_decode::decode_oriented_file(std::path::Path::new(&timeout_path))
                    .map(|img| (img, None))
            } else {
                videre_core::image_decode::decode_raw_with_orientation(std::path::Path::new(
                    &timeout_path,
                ))
                .map(|(img, o)| (img, Some(o)))
            }
        },
    ) {
        Ok(Ok(img)) => img,
        Ok(Err(e)) => {
            tracing::warn!("face thumbnail unavailable for {path}: {e}; skipping");
            return None;
        }
        Err(_) => {
            tracing::warn!(
                "timed out reading {path} for face thumbnail \
                 (file may be unreachable - is its drive connected?); skipping"
            );
            return None;
        }
    };
    let (img, raw_canvas_orientation) = decoded;
    let cropped = crop_face_square(&img, bbox);
    match raw_canvas_orientation {
        // Legacy row: the crop is still on the raw canvas; rotate the small
        // square, exactly as the pre-fix code did.
        Some(orientation) => {
            let mut cropped = image::DynamicImage::ImageRgba8(cropped.to_rgba8());
            cropped.apply_orientation(orientation);
            Some(cropped)
        }
        None => Some(cropped),
    }
}

/// Bounds a plain (non-HEIC) file read against a stale/disconnected mount
/// point the same way `videre_core::heic` bounds `qlmanage`, so a single
/// unreachable file can't hang the caller (an axum request thread, or any
/// other synchronous embedder) forever.
fn read_with_timeout(path: &str) -> std::io::Result<Vec<u8>> {
    let owned = path.to_string();
    videre_core::io_timeout::run_with_timeout(
        videre_core::io_timeout::DEFAULT_IO_TIMEOUT,
        move || std::fs::read(&owned),
    )
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
/// axum server serializes every request on one `Mutex<Connection>`)
/// can release that lock immediately after this cheap lookup, instead of
/// holding it for the entire decode/crop/resize/encode/cache-write below,
/// which otherwise fully serializes every thumbnail request behind the lock,
/// turning a many-thousand-singleton library into one thumbnail at a time.
pub struct FaceLookup {
    pub bbox_json: String,
    pub file_path: String,
    pub hash: String,
    /// Mirrors `faces.oriented`: which canvas `bbox_json` is in. `false`
    /// (NULL in the DB) = legacy raw-canvas row written before the
    /// orientation fix.
    pub oriented: bool,
}

/// The cheap part of `face_image_bytes`: just the DB row. No image I/O.
pub fn face_lookup(conn: &Connection, face_id: i64) -> Result<FaceLookup> {
    let (bbox_json, file_path, hash, oriented): (String, String, String, i64) = conn
        .query_row(
            "SELECT f.bbox, fh.path, f.hash, COALESCE(f.oriented, 0) FROM faces f \
             JOIN file_hashes fh ON f.hash = fh.hash WHERE f.id = ?1 LIMIT 1",
            [face_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .map_err(|_| Error::NotFound)?;
    Ok(FaceLookup {
        bbox_json,
        file_path,
        hash,
        oriented: oriented != 0,
    })
}

/// The expensive part of `face_image_bytes`: cache check, decode/crop/encode,
/// write-through. Takes no `Connection`, so it can run without holding the
/// shared DB lock.
pub fn face_bytes_from_lookup(
    lookup: &FaceLookup,
    face_id: i64,
    cache: &videre_core::library::CachePaths,
) -> Result<Vec<u8>> {
    let parts: Vec<f32> = lookup
        .bbox_json
        .split(',')
        .filter_map(|s| s.trim().parse().ok())
        .collect();
    if parts.len() != 4 {
        return Err(Error::NotFound);
    }
    let bbox = [parts[0], parts[1], parts[0] + parts[2], parts[1] + parts[3]];

    // The crop's cache identity includes its full geometry, so the path is
    // known only once the bbox is parsed.
    let cache_path = videre_core::thumb_cache::face_thumb_path_in(
        cache,
        &lookup.hash,
        face_id,
        bbox,
        FACE_THUMB_SIZE,
    );
    if videre_core::thumb_cache::face_thumb_exists_in(
        cache,
        &lookup.hash,
        face_id,
        bbox,
        FACE_THUMB_SIZE,
    ) {
        if let Ok(bytes) = read_with_timeout(&cache_path.to_string_lossy()) {
            return Ok(bytes);
        }
    }

    let thumb = make_face_thumb(&lookup.file_path, bbox, lookup.oriented, face_id)
        .ok_or(Error::NotFound)?;
    let mut buf = Vec::new();
    thumb
        .write_to(
            &mut std::io::Cursor::new(&mut buf),
            image::ImageFormat::Jpeg,
        )
        .map_err(|_| Error::NotFound)?;

    // Best-effort write-through (a cache-write failure must not fail the read).
    if let Some(parent) = cache_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let tmp = cache_path.with_extension(format!("tmp{}", std::process::id()));
    if std::fs::write(&tmp, &buf).is_ok() {
        let _ = std::fs::rename(&tmp, &cache_path);
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
pub fn face_image_bytes(
    conn: &Connection,
    face_id: i64,
    cache: &videre_core::library::CachePaths,
) -> Result<Vec<u8>> {
    let lookup = face_lookup(conn, face_id)?;
    face_bytes_from_lookup(&lookup, face_id, cache)
}

/// The single-row query `original_image_bytes` needs before any image I/O.
/// See `FaceLookup` for why this split matters for concurrency.
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
    cache: &videre_core::library::CachePaths,
) -> Result<(&'static str, Vec<u8>)> {
    let file_path = &lookup.file_path;
    let hash = &lookup.hash;
    let ext = std::path::Path::new(file_path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    if ext == "heic" {
        if let Ok(bytes) = read_with_timeout(
            &videre_core::thumb_cache::original_path_in(cache, hash).to_string_lossy(),
        ) {
            return Ok(("image/jpeg", bytes));
        }
        // None: this serves the true original image, so it must stay at
        // full resolution.
        let img = videre_core::heic::heic_via_quicklook(file_path, &format!("orig{face_id}"), None)
            .ok_or(Error::NotFound)?;
        let mut buf = Vec::new();
        img.write_to(
            &mut std::io::Cursor::new(&mut buf),
            image::ImageFormat::Jpeg,
        )
        .map_err(|_| Error::NotFound)?;
        let final_path = videre_core::thumb_cache::original_path_in(cache, hash);
        if let Some(parent) = final_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let tmp = final_path.with_extension(format!("tmp{}", std::process::id()));
        if std::fs::write(&tmp, &buf).is_ok() {
            let _ = std::fs::rename(&tmp, &final_path);
        }
        Ok(("image/jpeg", buf))
    } else {
        let bytes = read_with_timeout(file_path).map_err(|e| {
            tracing::warn!("original image unavailable for {file_path}: {e}; skipping");
            Error::NotFound
        })?;
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
pub fn original_image_bytes(
    conn: &Connection,
    face_id: i64,
    cache: &videre_core::library::CachePaths,
) -> Result<(&'static str, Vec<u8>)> {
    let lookup = original_lookup(conn, face_id)?;
    original_bytes_from_lookup(&lookup, face_id, cache)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both canvas branches must render the same upright face. The o6
    /// fixture is the untagged original plus EXIF Orientation = 6, so:
    /// - the oriented branch decodes it upright and crops with a bbox in
    ///   display-canvas coordinates (center rotated: display center of a
    ///   raw-centered bbox), while
    /// - the legacy branch crops the raw canvas with the raw-canvas bbox and
    ///   rotates the small square afterwards.
    ///
    /// Picking square bboxes centered on even coordinates makes the two
    /// regions pixel-identical after the integer rotation, so the crops must
    /// match exactly.
    #[test]
    fn oriented_and_legacy_branches_render_the_same_upright_crop() {
        let base = concat!(env!("CARGO_MANIFEST_DIR"), "/../videre/tests/fixtures");
        let tagged = format!("{base}/ai-generated-couple_o6.jpg");

        // Raw canvas is 1200x1543 portrait; display canvas is 1543x1200.
        // A 90 CW rotation maps raw (x, y) to display (H-1-y, x), which maps
        // the half-open region [a, b) to [H-b, H-a): the bbox center moves
        // from cy to H-cy, with no minus one, or the crop shifts by a pixel.
        let raw_center = (600u32, 772u32);
        let display_center = (1543 - raw_center.1, raw_center.0);
        let raw_bbox = [
            (raw_center.0 - 200) as f32,
            (raw_center.1 - 200) as f32,
            (raw_center.0 + 200) as f32,
            (raw_center.1 + 200) as f32,
        ];
        let display_bbox = [
            (display_center.0 - 200) as f32,
            (display_center.1 - 200) as f32,
            (display_center.0 + 200) as f32,
            (display_center.1 + 200) as f32,
        ];

        let legacy = make_face_thumb(&tagged, raw_bbox, false, 1).unwrap();
        let oriented = make_face_thumb(&tagged, display_bbox, true, 1).unwrap();
        assert_eq!(
            (legacy.width(), legacy.height()),
            (140, 140),
            "both branches produce 140x140 thumbnails"
        );
        let a: Vec<u8> = legacy.to_rgb8().pixels().map(|p| p.0[0]).collect();
        let b: Vec<u8> = oriented.to_rgb8().pixels().map(|p| p.0[0]).collect();
        let diff: u64 = a
            .iter()
            .zip(&b)
            .map(|(x, y)| (*x as i32 - *y as i32).unsigned_abs() as u64)
            .sum();
        assert!(
            diff < 1000,
            "both branches must render the same upright face, sum |diff| = {diff}"
        );
    }

    /// A row flagged oriented must not silently fall back to raw-canvas
    /// cropping: the two bboxes above only produce the same crop because the
    /// branches differ. A wrong flag must be visible as a wrong crop.
    #[test]
    fn the_oriented_flag_actually_changes_the_crop() {
        let base = concat!(env!("CARGO_MANIFEST_DIR"), "/../videre/tests/fixtures");
        let tagged = format!("{base}/ai-generated-couple_o6.jpg");
        // The raw-canvas bbox fed to the oriented branch crops a rotated view
        // of the same region, which for this asymmetric fixture differs.
        let raw_bbox = [400.0, 572.0, 800.0, 972.0];
        let as_legacy = make_face_thumb(&tagged, raw_bbox, false, 1).unwrap();
        let as_oriented = make_face_thumb(&tagged, raw_bbox, true, 1).unwrap();
        let a: Vec<u8> = as_legacy.to_rgb8().pixels().map(|p| p.0[0]).collect();
        let b: Vec<u8> = as_oriented.to_rgb8().pixels().map(|p| p.0[0]).collect();
        assert_ne!(a, b, "the flag must select between two different canvases");
    }

    #[test]
    fn a_face_crop_is_square_and_thumbnail_sized() {
        let img = image::DynamicImage::ImageLuma8(image::GrayImage::new(200, 100));
        let out = crop_face_square(&img, [80.0, 40.0, 120.0, 80.0]);
        assert_eq!((out.width(), out.height()), (140, 140));
    }

    /// A bbox against the edge would give a negative origin, and one larger
    /// than the image would run past it. Both are clamped rather than
    /// panicking inside `crop_imm`.
    #[test]
    fn a_face_crop_clamps_to_the_image_bounds() {
        let img = image::DynamicImage::ImageLuma8(image::GrayImage::new(50, 50));
        for bbox in [
            [0.0, 0.0, 10.0, 10.0],   // flush against the top-left
            [45.0, 45.0, 60.0, 60.0], // runs past the bottom-right
            [-20.0, -20.0, 5.0, 5.0], // negative origin
            [0.0, 0.0, 500.0, 500.0], // larger than the whole image
        ] {
            let out = crop_face_square(&img, bbox);
            assert_eq!((out.width(), out.height()), (140, 140), "bbox {bbox:?}");
        }
    }

    /// A zero-area bbox still has to produce a thumbnail rather than a
    /// zero-side crop: `crop_face_square` floors the side at 1.
    #[test]
    fn a_degenerate_bbox_still_produces_a_thumbnail() {
        let img = image::DynamicImage::ImageLuma8(image::GrayImage::new(50, 50));
        let out = crop_face_square(&img, [25.0, 25.0, 25.0, 25.0]);
        assert_eq!((out.width(), out.height()), (140, 140));
    }

    #[test]
    fn mime_types_cover_gallery_image_and_video_extensions() {
        for (ext, expected) in [
            ("jpg", "image/jpeg"),
            ("jpeg", "image/jpeg"),
            ("png", "image/png"),
            ("gif", "image/gif"),
            ("webp", "image/webp"),
            ("bmp", "image/bmp"),
            ("tiff", "image/tiff"),
            ("mov", "video/quicktime"),
            ("mp4", "video/mp4"),
            ("unknown", "application/octet-stream"),
        ] {
            assert_eq!(mime_for_ext(ext), expected, "extension {ext}");
        }
    }

    #[test]
    fn face_thumbnail_cache_is_returned_without_reading_the_source() {
        let temp = tempfile::tempdir().unwrap();
        let ctx =
            videre_core::library::LibraryContext::new(temp.path(), &temp.path().join("cache"))
                .unwrap();
        let lookup = FaceLookup {
            bbox_json: "10,20,30,40".to_string(),
            file_path: temp.path().join("missing.jpg").to_string_lossy().into(),
            hash: "face-cache-hash".to_string(),
            oriented: true,
        };
        let bbox = [10.0, 20.0, 40.0, 60.0];
        let cache_path = videre_core::thumb_cache::face_thumb_path_in(
            &ctx.cache,
            &lookup.hash,
            42,
            bbox,
            FACE_THUMB_SIZE,
        );
        std::fs::create_dir_all(cache_path.parent().unwrap()).unwrap();
        std::fs::write(&cache_path, b"cached thumbnail").unwrap();

        assert_eq!(
            face_bytes_from_lookup(&lookup, 42, &ctx.cache).unwrap(),
            b"cached thumbnail"
        );
    }

    #[test]
    fn malformed_face_bbox_is_not_found_before_image_io() {
        let temp = tempfile::tempdir().unwrap();
        let ctx =
            videre_core::library::LibraryContext::new(temp.path(), &temp.path().join("cache"))
                .unwrap();
        for bbox_json in ["", "1,2,3", "1,2,three,4", "1,2,3,4,5"] {
            let lookup = FaceLookup {
                bbox_json: bbox_json.to_string(),
                file_path: temp.path().join("missing.jpg").to_string_lossy().into(),
                hash: "bad-bbox-hash".to_string(),
                oriented: false,
            };
            assert!(matches!(
                face_bytes_from_lookup(&lookup, 1, &ctx.cache),
                Err(Error::NotFound)
            ));
        }
    }

    #[test]
    fn original_bytes_preserve_plain_file_contents_and_choose_mime() {
        let temp = tempfile::tempdir().unwrap();
        let ctx =
            videre_core::library::LibraryContext::new(temp.path(), &temp.path().join("cache"))
                .unwrap();
        let source = temp.path().join("original.JpEg");
        std::fs::write(&source, b"original image bytes").unwrap();
        let lookup = OriginalLookup {
            file_path: source.to_string_lossy().into(),
            hash: "original-hash".to_string(),
        };

        let (mime, bytes) = original_bytes_from_lookup(&lookup, 1, &ctx.cache).unwrap();
        assert_eq!(mime, "image/jpeg");
        assert_eq!(bytes, b"original image bytes");
    }

    #[test]
    fn missing_original_file_is_not_found() {
        let temp = tempfile::tempdir().unwrap();
        let ctx =
            videre_core::library::LibraryContext::new(temp.path(), &temp.path().join("cache"))
                .unwrap();
        let lookup = OriginalLookup {
            file_path: temp.path().join("missing.jpg").to_string_lossy().into(),
            hash: "missing-original-hash".to_string(),
        };
        assert!(matches!(
            original_bytes_from_lookup(&lookup, 1, &ctx.cache),
            Err(Error::NotFound)
        ));
    }

    #[test]
    fn unknown_face_id_is_not_found() {
        let conn = Connection::open_in_memory().unwrap();
        videre_core::face_db::create_faces_table(&conn).unwrap();
        conn.execute_batch("CREATE TABLE file_hashes (hash TEXT PRIMARY KEY, path TEXT);")
            .unwrap();
        let temp = tempfile::tempdir().unwrap();
        let ctx =
            videre_core::library::LibraryContext::new(temp.path(), &temp.path().join("cache"))
                .unwrap();
        assert!(matches!(
            face_image_bytes(&conn, 999, &ctx.cache),
            Err(Error::NotFound)
        ));
        assert!(matches!(
            original_image_bytes(&conn, 999, &ctx.cache),
            Err(Error::NotFound)
        ));
    }

    #[test]
    fn face_lookup_unknown_id_is_not_found() {
        let conn = Connection::open_in_memory().unwrap();
        videre_core::face_db::create_faces_table(&conn).unwrap();
        conn.execute_batch("CREATE TABLE file_hashes (hash TEXT PRIMARY KEY, path TEXT);")
            .unwrap();
        assert!(matches!(face_lookup(&conn, 999), Err(Error::NotFound)));
    }

    #[test]
    fn original_lookup_unknown_id_is_not_found() {
        let conn = Connection::open_in_memory().unwrap();
        videre_core::face_db::create_faces_table(&conn).unwrap();
        conn.execute_batch("CREATE TABLE file_hashes (hash TEXT PRIMARY KEY, path TEXT);")
            .unwrap();
        assert!(matches!(original_lookup(&conn, 999), Err(Error::NotFound)));
    }

    #[test]
    fn face_lookup_does_not_touch_the_filesystem() {
        // Regression test for the thumbnail-rendering serialization bug: the
        // DB lookup must be a pure query with no image I/O, so callers can
        // release the connection lock before doing the expensive part.
        let conn = Connection::open_in_memory().unwrap();
        videre_core::face_db::create_faces_table(&conn).unwrap();
        conn.execute_batch("CREATE TABLE file_hashes (hash TEXT PRIMARY KEY, path TEXT);")
            .unwrap();
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
