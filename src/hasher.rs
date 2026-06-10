use crate::types::FileRecord;
use chrono::{DateTime, Utc};
use exif::{In, Reader, Tag, Value};
use std::fs::{self, File};
use std::io::{self, BufReader, Read};
use std::path::Path;
use std::time::SystemTime;

struct ExifData {
    exif_date: Option<String>,
    gps_lat: Option<f64>,
    gps_lon: Option<f64>,
    width: Option<u32>,
    height: Option<u32>,
}

fn rational_to_f64(r: &exif::Rational) -> f64 {
    if r.denom == 0 { 0.0 } else { r.num as f64 / r.denom as f64 }
}

fn extract_gps(
    exif: &exif::Exif,
    coord_tag: Tag,
    ref_tag: Tag,
    negative_ref: u8,
) -> Option<f64> {
    let coord_field = exif.get_field(coord_tag, In::PRIMARY)?;
    let ref_field = exif.get_field(ref_tag, In::PRIMARY)?;
    if let (Value::Rational(rationals), Value::Ascii(refs)) =
        (&coord_field.value, &ref_field.value)
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

fn extract_exif(path: &Path) -> ExifData {
    let mut result = ExifData {
        exif_date: None,
        gps_lat: None,
        gps_lon: None,
        width: None,
        height: None,
    };

    let file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return result,
    };
    let exif = match Reader::new().read_from_container(&mut BufReader::new(file)) {
        Ok(e) => e,
        Err(_) => return result,
    };

    // DateTimeOriginal: "YYYY:MM:DD HH:MM:SS" → "YYYY-MM-DDTHH:MM:SS"
    if let Some(field) = exif.get_field(Tag::DateTimeOriginal, In::PRIMARY) {
        if let Value::Ascii(ref vec) = field.value {
            if let Some(bytes) = vec.first() {
                let s = String::from_utf8_lossy(bytes);
                if s.len() >= 19 {
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

pub fn hash_file(path: &Path, exif: bool) -> io::Result<FileRecord> {
    let metadata = fs::metadata(path)?;
    let size_bytes = metadata.len();
    let created_at = metadata.created().ok().map(system_time_to_iso);
    let modified_at = metadata.modified().ok().map(system_time_to_iso);

    let mut hasher = blake3::Hasher::new();
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut buffer = [0u8; 65536];
    loop {
        let n = reader.read(&mut buffer)?;
        if n == 0 { break; }
        hasher.update(&buffer[..n]);
    }
    let hash = hasher.finalize().to_hex().to_string();

    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    let (exif_date, gps_lat, gps_lon, width, height) =
        if exif && EXIF_EXTENSIONS.contains(&ext.as_str()) {
            let d = extract_exif(path);
            (d.exif_date, d.gps_lat, d.gps_lon, d.width, d.height)
        } else {
            (None, None, None, None, None)
        };

    Ok(FileRecord {
        path: path.to_string_lossy().to_string(),
        hash,
        size_bytes,
        created_at,
        modified_at,
        ext,
        phash: None,
        exif_date,
        gps_lat,
        gps_lon,
        width,
        height,
    })
}

use image::imageops::{resize, FilterType};

const PHASH_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "gif", "webp", "bmp", "tiff"];
const EXIF_EXTENSIONS: &[&str] = &["jpg", "jpeg", "tiff", "heic"];

pub fn compute_dhash(path: &Path) -> Option<u64> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())?;

    if !PHASH_EXTENSIONS.contains(&ext.as_str()) {
        return None;
    }

    let img = image::open(path).ok()?;
    // dHash: resize to 9x8, compare adjacent pixels in each row → 64 bits
    let small = resize(&img.to_luma8(), 9, 8, FilterType::Lanczos3);
    let mut hash: u64 = 0;
    for row in 0..8u32 {
        for col in 0..8u32 {
            let left = small.get_pixel(col, row)[0];
            let right = small.get_pixel(col + 1, row)[0];
            hash = (hash << 1) | if left > right { 1 } else { 0 };
        }
    }
    Some(hash)
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
    use tempfile::tempdir;
    use std::fs;

    #[test]
    fn hash_file_returns_correct_record() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.jpg");
        fs::write(&path, b"hello world").unwrap();

        let record = hash_file(&path, false).unwrap();

        assert_eq!(record.ext, "jpg");
        assert_eq!(record.size_bytes, 11);
        assert!(!record.hash.is_empty());
        assert_eq!(record.path, path.to_string_lossy());
    }

    #[test]
    fn same_content_same_hash() {
        let dir = tempdir().unwrap();
        let a = dir.path().join("a.jpg");
        let b = dir.path().join("b.jpg");
        fs::write(&a, b"duplicate content").unwrap();
        fs::write(&b, b"duplicate content").unwrap();

        let ra = hash_file(&a, false).unwrap();
        let rb = hash_file(&b, false).unwrap();
        assert_eq!(ra.hash, rb.hash);
    }

    #[test]
    fn different_content_different_hash() {
        let dir = tempdir().unwrap();
        let a = dir.path().join("a.jpg");
        let b = dir.path().join("b.jpg");
        fs::write(&a, b"content A").unwrap();
        fs::write(&b, b"content B").unwrap();

        let ra = hash_file(&a, false).unwrap();
        let rb = hash_file(&b, false).unwrap();
        assert_ne!(ra.hash, rb.hash);
    }

    #[test]
    fn dhash_same_image_returns_same_hash() {
        let dir = tempdir().unwrap();
        let img = image::RgbImage::from_pixel(64, 64, image::Rgb([128u8, 64, 32]));
        let path_a = dir.path().join("a.png");
        let path_b = dir.path().join("b.png");
        img.save(&path_a).unwrap();
        img.save(&path_b).unwrap();

        let ha = compute_dhash(&path_a);
        let hb = compute_dhash(&path_b);
        assert!(ha.is_some());
        assert_eq!(ha, hb);
    }

    #[test]
    fn dhash_unsupported_ext_returns_none() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("video.mov");
        std::fs::write(&path, b"not an image").unwrap();
        assert!(compute_dhash(&path).is_none());
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
    fn hash_file_with_exif_true_populates_exif_fields_for_jpeg() {
        let path = std::path::Path::new("tests/fixtures/sample_with_exif.jpg");
        let record = hash_file(path, true).unwrap();
        assert_eq!(record.exif_date.as_deref(), Some("2017-06-03T11:54:36"));
        assert!(record.gps_lat.is_some());
        assert!(record.gps_lon.is_some());
        assert_eq!(record.width, Some(4032));
        assert_eq!(record.height, Some(3024));
    }

    #[test]
    fn hash_file_with_exif_false_leaves_exif_fields_empty() {
        let path = std::path::Path::new("tests/fixtures/sample_with_exif.jpg");
        let record = hash_file(path, false).unwrap();
        assert!(record.exif_date.is_none());
        assert!(record.gps_lat.is_none());
        assert!(record.gps_lon.is_none());
        assert!(record.width.is_none());
        assert!(record.height.is_none());
    }

    #[test]
    fn extract_exif_reads_fields_from_fixture() {
        // Real iPhone 6s Plus photo at tests/fixtures/sample_with_exif.jpg
        // DateTimeOriginal: 2017:06:03 11:54:36
        // GPS: 44°16'3.93"N, 28°37'15.57"E → lat≈44.268, lon≈28.621
        // PixelXDimension: 4032, PixelYDimension: 3024
        let path = std::path::Path::new("tests/fixtures/sample_with_exif.jpg");
        let data = extract_exif(path);
        assert_eq!(data.exif_date.as_deref(), Some("2017-06-03T11:54:36"));
        assert!((data.gps_lat.unwrap() - 44.268).abs() < 0.01);
        assert!((data.gps_lon.unwrap() - 28.621).abs() < 0.01);
        assert_eq!(data.width, Some(4032));
        assert_eq!(data.height, Some(3024));
    }
}