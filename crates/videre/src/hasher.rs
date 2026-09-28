use crate::types::FileRecord;
use chrono::{DateTime, Utc};
use exif::{In, Reader, Tag, Value};
use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::path::Path;
use std::time::SystemTime;

#[derive(Default)]
struct ExifData {
    exif_date: Option<String>,
    gps_lat: Option<f64>,
    gps_lon: Option<f64>,
    width: Option<u32>,
    height: Option<u32>,
}

/// What `hash_file` records about a file's contents, from whichever extractor
/// applies.
///
/// A struct rather than the tuple this replaced: at seven fields, positional
/// returns are where a later edit silently swaps `width` and `height`, or
/// `gps_lat` and `gps_lon`, with nothing to catch it. Named fields cost one
/// `From` impl each.
#[derive(Default)]
struct ExtractedMeta {
    date: Option<String>,
    gps_lat: Option<f64>,
    gps_lon: Option<f64>,
    width: Option<u32>,
    height: Option<u32>,
    duration_secs: Option<f64>,
    codec: Option<String>,
}

impl From<ExifData> for ExtractedMeta {
    fn from(d: ExifData) -> Self {
        Self {
            date: d.exif_date,
            gps_lat: d.gps_lat,
            gps_lon: d.gps_lon,
            width: d.width,
            height: d.height,
            // Images have neither; the columns stay NULL.
            duration_secs: None,
            codec: None,
        }
    }
}

impl From<videre_core::video_meta::VideoMeta> for ExtractedMeta {
    fn from(v: videre_core::video_meta::VideoMeta) -> Self {
        Self {
            date: v.date,
            gps_lat: v.gps_lat,
            gps_lon: v.gps_lon,
            width: v.width,
            height: v.height,
            duration_secs: v.duration_secs,
            codec: v.codec,
        }
    }
}

fn rational_to_f64(r: &exif::Rational) -> f64 {
    if r.denom == 0 {
        0.0
    } else {
        r.num as f64 / r.denom as f64
    }
}

fn extract_gps(exif: &exif::Exif, coord_tag: Tag, ref_tag: Tag, negative_ref: u8) -> Option<f64> {
    let coord_field = exif.get_field(coord_tag, In::PRIMARY)?;
    let ref_field = exif.get_field(ref_tag, In::PRIMARY)?;
    if let (Value::Rational(rationals), Value::Ascii(refs)) = (&coord_field.value, &ref_field.value)
    {
        if rationals.len() < 3 {
            return None;
        }
        let d = rational_to_f64(&rationals[0]);
        let m = rational_to_f64(&rationals[1]);
        let s = rational_to_f64(&rationals[2]);
        let mut decimal = d + m / 60.0 + s / 3600.0;
        if refs.first().and_then(|r| r.first()).copied() == Some(negative_ref) {
            decimal = -decimal;
        }
        Some(decimal)
    } else {
        None
    }
}

fn extract_exif_file<R: Read + Seek>(mut file: R) -> ExifData {
    let mut result = ExifData::default();
    if file.seek(SeekFrom::Start(0)).is_err() {
        return result;
    }
    let exif = match Reader::new().read_from_container(&mut BufReader::new(file)) {
        Ok(e) => e,
        Err(_) => return result,
    };

    // DateTimeOriginal: "YYYY:MM:DD HH:MM:SS" → "YYYY-MM-DDTHH:MM:SS"
    if let Some(field) = exif.get_field(Tag::DateTimeOriginal, In::PRIMARY) {
        if let Value::Ascii(ref vec) = field.value {
            if let Some(bytes) = vec.first() {
                let s = String::from_utf8_lossy(bytes);
                if s.len() >= 19 && !s.starts_with("0000") {
                    result.exif_date = Some(format!(
                        "{}-{}-{}T{}",
                        &s[0..4],
                        &s[5..7],
                        &s[8..10],
                        &s[11..19]
                    ));
                }
            }
        }
    }

    // PixelXDimension / PixelYDimension
    if let Some(field) = exif.get_field(Tag::PixelXDimension, In::PRIMARY) {
        result.width = match &field.value {
            Value::Long(v) => v.first().copied(),
            Value::Short(v) => v.first().map(|&x| x as u32),
            _ => None,
        };
    }
    if let Some(field) = exif.get_field(Tag::PixelYDimension, In::PRIMARY) {
        result.height = match &field.value {
            Value::Long(v) => v.first().copied(),
            Value::Short(v) => v.first().map(|&x| x as u32),
            _ => None,
        };
    }

    // GPS
    result.gps_lat = extract_gps(&exif, Tag::GPSLatitude, Tag::GPSLatitudeRef, b'S');
    result.gps_lon = extract_gps(&exif, Tag::GPSLongitude, Tag::GPSLongitudeRef, b'W');

    result
}

#[cfg(test)]
fn extract_exif(path: &Path) -> ExifData {
    File::open(path).map(extract_exif_file).unwrap_or_default()
}

/// Bounds the whole read+hash+EXIF operation so a disconnected/stale mount
/// point (which can block `fs::metadata`/`File::open`/`read` indefinitely on
/// macOS rather than fail fast) turns into a returned error instead of
/// hanging the scan forever on one file.
pub fn hash_file(path: &Path) -> io::Result<FileRecord> {
    // Preserve the bounded, stat-first guard before opening a possibly stale
    // mount. The file size no longer sets a total hashing deadline.
    let owned = path.to_path_buf();
    videre_core::io_timeout::run_with_timeout(videre_core::io_timeout::STAT_TIMEOUT, move || {
        std::fs::metadata(owned).map(|_| ())
    })
    .map_err(videre_core::io_timeout::IoRunError::into_io_error)??;
    let owned = path.to_path_buf();
    videre_core::io_timeout::run_with_progress_timeout(
        videre_core::io_timeout::DEFAULT_IO_TIMEOUT,
        move |progress| {
            let file = File::open(&owned)?;
            hash_open_file(&owned, file, &progress)
        },
    )
    .map_err(videre_core::io_timeout::IoRunError::into_io_error)?
}

/// Hash one original through the selected library's pinned I/O boundary.
pub fn hash_file_in(
    ctx: &videre_core::library::LibraryContext,
    path: &Path,
) -> io::Result<FileRecord> {
    let file = videre_core::library_io::open_media(ctx, path).map_err(io::Error::other)?;
    let stat_file = file.try_clone()?;
    videre_core::io_timeout::run_with_timeout(videre_core::io_timeout::STAT_TIMEOUT, move || {
        stat_file.metadata().map(|_| ())
    })
    .map_err(videre_core::io_timeout::IoRunError::into_io_error)??;
    let owned = path.to_path_buf();
    videre_core::io_timeout::run_with_progress_timeout(
        videre_core::io_timeout::DEFAULT_IO_TIMEOUT,
        move |progress| hash_open_file(&owned, file, &progress),
    )
    .map_err(videre_core::io_timeout::IoRunError::into_io_error)?
}

fn hash_open_file(
    path: &Path,
    file: File,
    progress: &videre_core::io_timeout::ProgressHandle,
) -> io::Result<FileRecord> {
    let metadata = file.metadata()?;
    let size_bytes = metadata.len();
    let created_at = metadata.created().ok().map(system_time_to_iso);
    let modified_at = metadata.modified().ok().map(system_time_to_iso);

    // The first bytes identify the type; the content key is then computed
    // with that type's metadata left out (see `content_key`).
    let mut reader = progress.wrap(file.try_clone()?);
    let mut head = vec![0u8; 65536];
    let mut read = 0;
    while read < head.len() {
        let n = reader.read(&mut head[read..])?;
        if n == 0 {
            break;
        }
        read += n;
    }
    head.truncate(read);
    // An unrecognised file records the sentinel rather than NULL, so NULL
    // keeps its single meaning of "never scanned". A zero-byte file stays
    // NULL: nothing was read.
    let mime: Option<String> = (!head.is_empty()).then(|| {
        videre_core::mime_probe::sniff(&head)
            .unwrap_or(videre_core::mime_probe::UNKNOWN_MIME)
            .to_string()
    });
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    let format = videre_core::mime_probe::effective_mime(mime.as_deref(), &ext)
        .and_then(crate::content_key::Format::for_mime);
    let keys = crate::content_key::keys(&mut reader, size_bytes, format)?;

    let mut meta = match videre_core::mime_probe::effective_mime(mime.as_deref(), &ext) {
        Some(m) if videre_core::mime_probe::EXIF_MIMES.contains(&m) => file
            .try_clone()
            .map(|f| extract_exif_file(progress.wrap(f)))
            .unwrap_or_default()
            .into(),
        // QuickTime atoms are not EXIF, so `EXIF_MIMES` is deliberately not
        // widened to cover video; it would be a lie the next reader has to
        // unpick. See `videre_core::video_meta`.
        Some(m) if videre_core::mime_probe::is_video_mime(m) => file
            .try_clone()
            .map(|f| videre_core::video_meta::read_file_in_progress(&mut progress.wrap(f)))
            .unwrap_or_default()
            .into(),
        _ => ExtractedMeta::default(),
    };

    // EXIF often omits pixel dimensions (AI-generated images, screenshots, many
    // PNGs), yet width/height are load-bearing for XMP face-region export and
    // import, which normalize pixel bboxes against them. Fall back to the image
    // header, which `image::image_dimensions` reads without decoding pixels.
    // Images only: video dimensions come from the container above, and a HEIC or
    // non-image file simply fails the read and stays NULL.
    let is_video = videre_core::mime_probe::effective_mime(mime.as_deref(), &ext)
        .is_some_and(videre_core::mime_probe::is_video_mime);
    if !is_video && (meta.width.is_none() || meta.height.is_none()) {
        if let Ok(image_file) = file.try_clone() {
            let mut image_file = progress.wrap(image_file);
            let dimensions = image_file
                .seek(SeekFrom::Start(0))
                .ok()
                .and_then(|_| {
                    image::ImageReader::new(BufReader::new(image_file))
                        .with_guessed_format()
                        .ok()
                })
                .and_then(|reader| reader.into_dimensions().ok());
            if let Some((w, h)) = dimensions {
                meta.width = Some(w);
                meta.height = Some(h);
            }
        }
    }

    progress.check_cancelled()?;

    Ok(FileRecord {
        path: path.to_string_lossy().to_string(),
        hash: keys.content,
        meta_hash: keys.meta,
        size_bytes,
        created_at,
        modified_at,
        ext,
        mime,
        phash: None,
        exif_date: meta.date,
        gps_lat: meta.gps_lat,
        gps_lon: meta.gps_lon,
        width: meta.width,
        height: meta.height,
        duration_secs: meta.duration_secs,
        codec: meta.codec,
    })
}

pub fn hamming(a: u64, b: u64) -> u32 {
    (a ^ b).count_ones()
}

fn system_time_to_iso(t: SystemTime) -> String {
    let dt: DateTime<Utc> = t.into();
    dt.to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    /// The near-duplicate fingerprint as embed computes it for an image file.
    fn file_dhash(path: &std::path::Path) -> u64 {
        videre_core::image_decode::dhash(
            &videre_core::image_decode::decode_oriented_file(path).unwrap(),
        )
    }

    #[test]
    fn dhash_is_orientation_invariant() {
        // The o6 fixture is the untagged original plus EXIF Orientation = 6.
        // Orientation 6 displays as a 90 CW rotation, so the tagged file must
        // hash the same as a lossless PNG of the original rotated 90 CW.
        // Before the orientation fix this failed: the tagged file hashed its
        // raw sensor canvas instead of its display canvas.
        let base = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");
        let raw = image::open(format!("{base}/ai-generated-couple.jpg")).unwrap();
        let rotated = image::imageops::rotate90(&raw);
        let dir = tempdir().unwrap();
        let rotated_path = dir.path().join("rotated.png");
        let mut png = Vec::new();
        rotated
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        fs::write(&rotated_path, &png).unwrap();

        let tagged = file_dhash(std::path::Path::new(&format!(
            "{base}/ai-generated-couple_o6.jpg"
        )));
        let rotated_hash = file_dhash(&rotated_path);
        assert_eq!(
            tagged, rotated_hash,
            "the tagged file must hash its display canvas, not its raw canvas"
        );
    }

    #[test]
    fn dhash_value_is_pinned_for_a_jpeg_fixture() {
        let base = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");
        let got = file_dhash(std::path::Path::new(&format!(
            "{base}/ai-generated-couple.jpg"
        )));
        assert_eq!(
            got, 0x68c9_d51c_3a32_5948,
            "pinned dHash; any change breaks stored fingerprints"
        );
    }

    #[test]
    fn hash_file_returns_correct_record() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.jpg");
        fs::write(&path, b"hello world").unwrap();

        let record = hash_file(&path).unwrap();

        assert_eq!(record.ext, "jpg");
        assert_eq!(record.size_bytes, 11);
        assert!(!record.hash.is_empty());
        assert_eq!(record.path, path.to_string_lossy());
    }

    #[test]
    fn hash_entry_points_preserve_content_and_metadata_keys() {
        let base = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures"));
        let library_dir = tempdir().unwrap();
        let root = library_dir.path().join("photos");
        fs::create_dir(&root).unwrap();
        let library =
            videre_core::library::LibraryContext::new(&root, &library_dir.path().join("cache"))
                .unwrap();
        for relative in [
            "sample_with_exif.jpg",
            "content_key/tiny.heic",
            "content_key/testsrc_dated.mp4",
            "corrupt.jpg",
        ] {
            let path = base.join(relative);
            let mut raw = File::open(&path).unwrap();
            let len = raw.metadata().unwrap().len();
            let head = {
                let mut head = [0u8; 65536];
                let n = raw.read(&mut head).unwrap();
                head[..n].to_vec()
            };
            let ext = path.extension().and_then(|s| s.to_str()).unwrap();
            let mime = videre_core::mime_probe::sniff(&head);
            let format = videre_core::mime_probe::effective_mime(mime, ext)
                .and_then(crate::content_key::Format::for_mime);
            let expected = crate::content_key::keys(&mut raw, len, format).unwrap();
            let actual = hash_file(&path).unwrap();
            assert_eq!(actual.hash, expected.content, "{relative}");
            assert_eq!(actual.meta_hash, expected.meta, "{relative}");
            let confined_path = root.join(path.file_name().unwrap());
            fs::copy(&path, &confined_path).unwrap();
            let confined = hash_file_in(&library, &confined_path).unwrap();
            assert_eq!(confined.hash, expected.content, "{relative}");
            assert_eq!(confined.meta_hash, expected.meta, "{relative}");
        }
    }

    #[test]
    fn same_content_same_hash() {
        let dir = tempdir().unwrap();
        let a = dir.path().join("a.jpg");
        let b = dir.path().join("b.jpg");
        fs::write(&a, b"duplicate content").unwrap();
        fs::write(&b, b"duplicate content").unwrap();

        let ra = hash_file(&a).unwrap();
        let rb = hash_file(&b).unwrap();
        assert_eq!(ra.hash, rb.hash);
    }

    #[test]
    fn different_content_different_hash() {
        let dir = tempdir().unwrap();
        let a = dir.path().join("a.jpg");
        let b = dir.path().join("b.jpg");
        fs::write(&a, b"content A").unwrap();
        fs::write(&b, b"content B").unwrap();

        let ra = hash_file(&a).unwrap();
        let rb = hash_file(&b).unwrap();
        assert_ne!(ra.hash, rb.hash);
    }

    #[test]
    fn hash_file_mp4_has_no_exif_fields() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("clip.mp4");
        fs::write(&path, b"fake mp4 data").unwrap();
        let record = hash_file(&path).unwrap();
        assert_eq!(record.ext, "mp4");
        assert!(record.exif_date.is_none());
        assert!(record.gps_lat.is_none());
        assert!(record.phash.is_none());
    }

    #[test]
    fn hash_file_dng_attempts_exif_extraction() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("raw.dng");
        fs::write(&path, b"fake dng data").unwrap();
        // No valid EXIF in fake data, but the path confirms it is attempted (no panic)
        let record = hash_file(&path).unwrap();
        assert_eq!(record.ext, "dng");
        assert!(record.exif_date.is_none()); // graceful fallback on invalid data
    }

    #[test]
    fn hamming_distance_identical_hashes() {
        assert_eq!(hamming(0b1010u64, 0b1010u64), 0);
    }

    #[test]
    fn hamming_distance_one_bit_diff() {
        assert_eq!(hamming(0b1010u64, 0b1011u64), 1);
    }

    #[test]
    fn rational_to_f64_converts_correctly() {
        let r = exif::Rational { num: 51, denom: 1 };
        assert!((rational_to_f64(&r) - 51.0).abs() < f64::EPSILON);
    }

    #[test]
    fn rational_to_f64_zero_denom_returns_zero() {
        let r = exif::Rational { num: 5, denom: 0 };
        assert_eq!(rational_to_f64(&r), 0.0);
    }

    #[test]
    fn extract_exif_returns_none_for_non_jpeg() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("file.txt");
        fs::write(&path, b"not an image").unwrap();
        let data = extract_exif(&path);
        assert!(data.exif_date.is_none());
        assert!(data.gps_lat.is_none());
        assert!(data.gps_lon.is_none());
        assert!(data.width.is_none());
        assert!(data.height.is_none());
    }

    #[test]
    fn hash_file_populates_exif_fields_for_jpeg() {
        let path = std::path::Path::new("tests/fixtures/sample_with_exif.jpg");
        let record = hash_file(path).unwrap();
        assert_eq!(record.exif_date.as_deref(), Some("2021-08-10T19:34:03"));
        assert!(record.gps_lat.is_some());
        assert!(record.gps_lon.is_some());
        assert_eq!(record.width, Some(4032));
        assert_eq!(record.height, Some(3024));
    }

    #[test]
    fn hash_file_falls_back_to_image_header_when_exif_lacks_dimensions() {
        // A PNG carries no EXIF pixel dimensions, so width/height must come from
        // the image header. Without the fallback these stay NULL and XMP face
        // regions cannot be exported or imported for the file.
        let dir = tempdir().unwrap();
        let path = dir.path().join("no-exif.png");
        image::RgbImage::new(7, 5).save(&path).unwrap();
        let record = hash_file(&path).unwrap();
        assert_eq!(record.width, Some(7));
        assert_eq!(record.height, Some(5));
    }

    #[test]
    fn extract_exif_reads_fields_from_fixture() {
        // tests/fixtures/sample_with_exif.jpg
        // DateTimeOriginal: 2021:08:10 19:34:03
        // GPS: 40°59'50.11"N, 29°0'44.47"E → lat≈40.997, lon≈29.012
        // PixelXDimension: 4032, PixelYDimension: 3024
        let path = std::path::Path::new("tests/fixtures/sample_with_exif.jpg");
        let data = extract_exif(path);
        assert_eq!(data.exif_date.as_deref(), Some("2021-08-10T19:34:03"));
        assert!((data.gps_lat.unwrap() - 40.997).abs() < 0.01);
        assert!((data.gps_lon.unwrap() - 29.012).abs() < 0.01);
        assert_eq!(data.width, Some(4032));
        assert_eq!(data.height, Some(3024));
    }

    #[test]
    fn hashing_detects_the_real_type_not_the_extension() {
        // A JPEG named .png, the shape of the real file that fails every
        // embed run with "Invalid PNG signature".
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("actually_a_jpeg.png");
        let mut bytes = b"\xff\xd8\xff\xe0\x00\x10JFIF\x00\x01".to_vec();
        bytes.extend_from_slice(&[0u8; 64]);
        std::fs::write(&path, &bytes).unwrap();

        let rec = hash_file(&path).unwrap();
        assert_eq!(rec.ext, "png", "the extension is recorded as-is");
        assert_eq!(rec.mime.as_deref(), Some("image/jpeg"), "the content wins");
    }

    #[test]
    fn an_unrecognised_file_gets_the_sentinel_not_null() {
        // NULL must mean "never scanned" so an incremental scan terminates.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mystery.png");
        std::fs::write(&path, b"nothing resembling a known signature here").unwrap();
        assert_eq!(
            hash_file(&path).unwrap().mime.as_deref(),
            Some(videre_core::mime_probe::UNKNOWN_MIME)
        );
    }
}
