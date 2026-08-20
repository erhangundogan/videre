//! Image-bytes operations shared by every videre-api caller (the axum
//! `--faces` server in this repo): aligned face thumbnails and full original
//! images.

use crate::error::{Error, Result};
use rusqlite::Connection;
use std::io::BufReader;

const FACE_THUMB_SIZE: u32 = 140;

fn read_exif_orientation(path: &str) -> u16 {
    let Ok(f) = std::fs::File::open(path) else {
        return 1;
    };
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

/// The eight EXIF orientation transforms, as pure image maths.
///
/// Split from `apply_exif_orientation` so it can be tested exhaustively: the
/// alternative is hand-crafting a JPEG with an EXIF APP1 segment per case,
/// which would test the `exif` crate's parser far more than this mapping. The
/// mapping is where the bugs actually live, since 5 and 7 combine a rotation
/// with a flip and are easy to transpose.
///
/// Anything outside 1..=8, including the 1 that `read_exif_orientation`
/// returns when a file has no EXIF at all, is the identity.
fn apply_orientation(img: image::DynamicImage, orientation: u16) -> image::DynamicImage {
    match orientation {
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
    apply_orientation(img, read_exif_orientation(path))
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
///
/// `pub` (unlike the three helpers above it): the static-page
/// base64 thumbnail path (`face_thumb_b64` in `report.rs`) also needs this
/// exact crop+orientation logic, so it calls through here instead of keeping
/// its own duplicate copy.
pub fn make_face_thumb(path: &str, bbox: [f32; 4], face_id: i64) -> Option<image::DynamicImage> {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    if ext == "heic" {
        // None: bbox is stored relative to a full-res decode. See the
        // safety note on heic_via_quicklook.
        let img = videre_core::heic::heic_via_quicklook(path, &format!("thumb{face_id}"), None)?;
        Some(crop_face_square(&img, bbox))
    } else {
        // Detection ran on raw pixels; crop first, then correct orientation
        let timeout_path = path.to_string();
        let img = match videre_core::io_timeout::run_with_timeout(
            videre_core::io_timeout::DEFAULT_IO_TIMEOUT,
            move || image::open(&timeout_path),
        ) {
            Ok(Ok(img)) => img,
            Ok(Err(e)) => {
                eprintln!("warning: face thumbnail unavailable for {path}: {e}; skipping");
                return None;
            }
            Err(_) => {
                eprintln!(
                    "warning: timed out reading {path} for face thumbnail \
                     (file may be unreachable - is its drive connected?); skipping"
                );
                return None;
            }
        };
        let cropped = crop_face_square(&img, bbox);
        Some(apply_exif_orientation(cropped, path))
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
    Ok(FaceLookup {
        bbox_json,
        file_path,
        hash,
    })
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

    let parts: Vec<f32> = lookup
        .bbox_json
        .split(',')
        .filter_map(|s| s.trim().parse().ok())
        .collect();
    if parts.len() != 4 {
        return Err(Error::NotFound);
    }
    let bbox = [parts[0], parts[1], parts[0] + parts[2], parts[1] + parts[3]];
    let thumb = make_face_thumb(&lookup.file_path, bbox, face_id).ok_or(Error::NotFound)?;
    let mut buf = Vec::new();
    thumb
        .write_to(
            &mut std::io::Cursor::new(&mut buf),
            image::ImageFormat::Jpeg,
        )
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
        // None: this serves the true original image, so it must stay at
        // full resolution.
        let img =
            videre_core::heic::heic_via_quicklook(&file_path, &format!("orig{face_id}"), None)
                .ok_or(Error::NotFound)?;
        let mut buf = Vec::new();
        img.write_to(
            &mut std::io::Cursor::new(&mut buf),
            image::ImageFormat::Jpeg,
        )
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
        let bytes = read_with_timeout(file_path).map_err(|e| {
            eprintln!("warning: original image unavailable for {file_path}: {e}; skipping");
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
pub fn original_image_bytes(conn: &Connection, face_id: i64) -> Result<(&'static str, Vec<u8>)> {
    let lookup = original_lookup(conn, face_id)?;
    original_bytes_from_lookup(&lookup, face_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 2x3 image whose every pixel is distinguishable, so a transposed
    /// rotation or a flip on the wrong axis actually fails. A square or
    /// symmetric fixture would pass for several wrong mappings.
    ///
    /// Pixel values encode position as `10 * x + y`:
    ///
    /// ```text
    ///   (0,0)=0   (1,0)=10
    ///   (0,1)=1   (1,1)=11
    ///   (0,2)=2   (1,2)=12
    /// ```
    fn asymmetric() -> image::DynamicImage {
        let mut img = image::GrayImage::new(2, 3);
        for y in 0..3u32 {
            for x in 0..2u32 {
                img.put_pixel(x, y, image::Luma([(x * 10 + y) as u8]));
            }
        }
        image::DynamicImage::ImageLuma8(img)
    }

    fn pixels(img: &image::DynamicImage) -> (u32, u32, Vec<u8>) {
        let g = img.to_luma8();
        (g.width(), g.height(), g.pixels().map(|p| p.0[0]).collect())
    }

    #[test]
    fn orientation_1_and_unknown_values_are_the_identity() {
        let expected = pixels(&asymmetric());
        // 1 is "normal", and is also what read_exif_orientation returns for a
        // file with no EXIF, so this is the common path, not an edge case.
        for o in [0u16, 1, 9, 42, u16::MAX] {
            assert_eq!(
                pixels(&apply_orientation(asymmetric(), o)),
                expected,
                "orientation {o} must not transform the image"
            );
        }
    }

    #[test]
    fn orientation_2_mirrors_horizontally() {
        let (w, h, px) = pixels(&apply_orientation(asymmetric(), 2));
        assert_eq!((w, h), (2, 3));
        // Rows reversed left-to-right: (0,y) and (1,y) swap.
        assert_eq!(px, vec![10, 0, 11, 1, 12, 2]);
    }

    #[test]
    fn orientation_3_rotates_180() {
        let (w, h, px) = pixels(&apply_orientation(asymmetric(), 3));
        assert_eq!((w, h), (2, 3));
        assert_eq!(px, vec![12, 2, 11, 1, 10, 0]);
    }

    #[test]
    fn orientation_4_mirrors_vertically() {
        let (w, h, px) = pixels(&apply_orientation(asymmetric(), 4));
        assert_eq!((w, h), (2, 3));
        assert_eq!(px, vec![2, 12, 1, 11, 0, 10]);
    }

    /// 5 and 7 are the two that combine a rotation with a flip, and are the
    /// pair most easily transposed. Their dimensions swap to 3x2.
    #[test]
    fn orientation_5_and_7_transpose_and_differ_from_each_other() {
        let five = pixels(&apply_orientation(asymmetric(), 5));
        let seven = pixels(&apply_orientation(asymmetric(), 7));
        assert_eq!((five.0, five.1), (3, 2));
        assert_eq!((seven.0, seven.1), (3, 2));
        assert_ne!(five.2, seven.2, "5 and 7 must not be the same transform");
        assert_eq!(five.2, vec![0, 1, 2, 10, 11, 12]);
        assert_eq!(seven.2, vec![12, 11, 10, 2, 1, 0]);
    }

    #[test]
    fn orientation_6_and_8_rotate_opposite_ways() {
        let six = pixels(&apply_orientation(asymmetric(), 6));
        let eight = pixels(&apply_orientation(asymmetric(), 8));
        assert_eq!((six.0, six.1), (3, 2));
        assert_eq!((eight.0, eight.1), (3, 2));
        assert_ne!(six.2, eight.2, "90 and 270 must not be the same transform");
        assert_eq!(six.2, vec![2, 1, 0, 12, 11, 10]);
        assert_eq!(eight.2, vec![10, 11, 12, 0, 1, 2]);
    }

    /// A minimal JPEG carrying nothing but an EXIF APP1 segment declaring
    /// `orientation`.
    ///
    /// Built by hand rather than shipping eight binary fixtures, and rather
    /// than borrowing `crates/videre/tests/fixtures`, which would couple this
    /// crate's unit tests to another crate's test data.
    ///
    /// Layout: SOI, then APP1 holding "Exif\0\0" and a little-endian TIFF
    /// header whose IFD0 has exactly one entry, Orientation (tag 0x0112,
    /// type SHORT), then EOI.
    fn jpeg_with_orientation(orientation: u16) -> Vec<u8> {
        jpeg_with_orientation_of_type(orientation, 3)
    }

    /// As above, but with the IFD entry's type field configurable, so a test
    /// can declare Orientation as something other than SHORT.
    fn jpeg_with_orientation_of_type(orientation: u16, tiff_type: u16) -> Vec<u8> {
        let mut tiff = Vec::new();
        tiff.extend_from_slice(b"II"); // little-endian
        tiff.extend_from_slice(&42u16.to_le_bytes()); // TIFF magic
        tiff.extend_from_slice(&8u32.to_le_bytes()); // offset of IFD0
        tiff.extend_from_slice(&1u16.to_le_bytes()); // one entry
        tiff.extend_from_slice(&0x0112u16.to_le_bytes()); // Orientation
        tiff.extend_from_slice(&tiff_type.to_le_bytes()); // 3 = SHORT
        tiff.extend_from_slice(&1u32.to_le_bytes()); // count
        tiff.extend_from_slice(&orientation.to_le_bytes()); // value, inline
        tiff.extend_from_slice(&[0, 0]); // pad to the 4-byte value field
        tiff.extend_from_slice(&0u32.to_le_bytes()); // no next IFD

        let mut app1 = Vec::from(*b"Exif\0\0");
        app1.extend_from_slice(&tiff);

        let mut jpeg = vec![0xFF, 0xD8]; // SOI
        jpeg.extend_from_slice(&[0xFF, 0xE1]); // APP1
        jpeg.extend_from_slice(&((app1.len() + 2) as u16).to_be_bytes());
        jpeg.extend_from_slice(&app1);
        jpeg.extend_from_slice(&[0xFF, 0xD9]); // EOI
        jpeg
    }

    #[test]
    fn every_exif_orientation_value_is_read_back() {
        let dir = std::env::temp_dir().join(format!("videre-api-orient-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for o in 1..=8u16 {
            let p = dir.join(format!("o{o}.jpg"));
            std::fs::write(&p, jpeg_with_orientation(o)).unwrap();
            assert_eq!(
                read_exif_orientation(p.to_str().unwrap()),
                o,
                "orientation {o} did not round-trip"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The whole path together: a file whose EXIF says "rotate 90" must come
    /// back rotated, with its dimensions swapped. Covers the join between
    /// reading the tag and applying the transform, which the two halves tested
    /// separately above cannot.
    #[test]
    fn a_jpeg_declaring_rotation_is_actually_rotated() {
        let dir = std::env::temp_dir().join(format!("videre-api-rot-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("rot90.jpg");
        std::fs::write(&p, jpeg_with_orientation(6)).unwrap();

        let out = apply_exif_orientation(asymmetric(), p.to_str().unwrap());
        let (w, h, px) = pixels(&out);
        assert_eq!((w, h), (3, 2), "orientation 6 must swap the dimensions");
        assert_eq!(px, vec![2, 1, 0, 12, 11, 10]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Orientation is a SHORT by spec. A file declaring it as some other type
    /// is malformed, and must fall back to 1 rather than being coerced into a
    /// rotation nobody asked for.
    #[test]
    fn an_orientation_of_the_wrong_exif_type_falls_back_to_1() {
        let dir = std::env::temp_dir().join(format!("videre-api-badtype-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("badtype.jpg");
        // Type 4 is LONG, not SHORT.
        std::fs::write(&p, jpeg_with_orientation_of_type(6, 4)).unwrap();
        assert_eq!(read_exif_orientation(p.to_str().unwrap()), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn exif_orientation_defaults_to_1_for_a_missing_or_non_exif_file() {
        assert_eq!(read_exif_orientation("/nonexistent/path/nope.jpg"), 1);

        let dir = std::env::temp_dir().join(format!("videre-api-exif-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let not_an_image = dir.join("plain.jpg");
        std::fs::write(&not_an_image, b"definitely not a jpeg").unwrap();
        assert_eq!(read_exif_orientation(not_an_image.to_str().unwrap()), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Orientation is only consulted for formats that carry EXIF. A PNG named
    /// with a non-EXIF extension must be returned untouched without the file
    /// even being opened, which is why this passes a path that does not exist.
    #[test]
    fn non_exif_extensions_skip_orientation_entirely() {
        let expected = pixels(&asymmetric());
        for path in [
            "/nonexistent/a.png",
            "/nonexistent/b.heic",
            "/nonexistent/c",
        ] {
            assert_eq!(
                pixels(&apply_exif_orientation(asymmetric(), path)),
                expected,
                "{path} must be returned unchanged"
            );
        }
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
    fn unknown_face_id_is_not_found() {
        let conn = Connection::open_in_memory().unwrap();
        videre_core::face_db::create_faces_table(&conn).unwrap();
        conn.execute_batch("CREATE TABLE file_hashes (hash TEXT PRIMARY KEY, path TEXT);")
            .unwrap();
        assert!(matches!(face_image_bytes(&conn, 999), Err(Error::NotFound)));
        assert!(matches!(
            original_image_bytes(&conn, 999),
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
