use crate::types::FileRecord;
use chrono::{DateTime, Utc};
use std::fs::{self, File};
use std::io::{self, BufReader, Read};
use std::path::Path;
use std::time::SystemTime;

pub fn hash_file(path: &Path) -> io::Result<FileRecord> {
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

    Ok(FileRecord {
        path: path.to_string_lossy().to_string(),
        hash,
        size_bytes,
        created_at,
        modified_at,
        ext,
        phash: None,
    })
}

use image::imageops::{resize, FilterType};

const PHASH_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "gif", "webp", "bmp", "tiff"];

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

        let record = hash_file(&path).unwrap();

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
}
