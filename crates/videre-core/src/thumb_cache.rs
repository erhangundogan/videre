use std::path::PathBuf;

/// Directory holding pre-converted HEIC thumbnails, keyed by content hash
/// rather than file path, the same photo scanned into different databases
/// only needs converting once. Mirrors this project's existing
/// `~/.cache/ort/` convention for cached model weights.
pub fn cache_dir() -> PathBuf {
    dirs_cache_dir().join("videre").join("thumbnails")
}

/// Path to a cached thumbnail for `hash` at `size` pixels (e.g. 240 or
/// 1200), whether or not it currently exists on disk.
pub fn thumb_path(hash: &str, size: u32) -> PathBuf {
    cache_dir().join(format!("{hash}_{size}.jpg"))
}

/// True if a cached thumbnail already exists for this hash/size.
pub fn thumb_exists(hash: &str, size: u32) -> bool {
    thumb_path(hash, size).exists()
}

/// Cache path for a single face crop. Distinct from `thumb_path` because
/// many faces can share one source `hash`, the face id disambiguates.
pub fn face_thumb_path(hash: &str, face_id: i64, size: u32) -> PathBuf {
    cache_dir().join(format!("{hash}_face{face_id}_{size}.jpg"))
}

/// True if a cached face crop already exists for this hash/face_id/size.
pub fn face_thumb_exists(hash: &str, face_id: i64, size: u32) -> bool {
    face_thumb_path(hash, face_id, size).exists()
}

/// Cache path for a full-resolution HEIC-converted original. One per hash
/// (not per face, the original photo is the same regardless of which face
/// on it was clicked).
pub fn original_path(hash: &str) -> PathBuf {
    cache_dir().join(format!("{hash}_original.jpg"))
}

/// True if a cached full-resolution original already exists for this hash.
pub fn original_exists(hash: &str) -> bool {
    original_path(hash).exists()
}

/// Length of a BLAKE3 hex digest (32 bytes -> 64 hex chars), every
/// content-hash-keyed cache filename starts with exactly this many hex
/// chars, followed by `_` and a purpose-specific suffix
/// (`_240.jpg`, `_face3_140.jpg`, `_original.jpg`, `_original.tmp1234`, ...).
const HASH_HEX_LEN: usize = 64;

/// Extracts the leading content hash from a cache filename (the `.jpg`
/// files this module writes, `thumb_path`, `face_thumb_path`,
/// `original_path`), or `None` if `filename` doesn't match that shape.
/// Used by `videre prune` to find cache entries whose hash no longer has a
/// surviving `file_hashes` row, without hardcoding every suffix pattern this
/// module can produce. Deliberately does NOT match `.tmp*` scratch files
/// (see `thumb_tmp_path`/`original_tmp_path`), those may be actively being
/// written by a concurrently running `videre watch`, and reusing this same
/// hash-existence check against them could delete an in-flight write for a
/// hash that is still perfectly valid.
pub fn hash_from_cache_filename(filename: &str) -> Option<&str> {
    if !filename.ends_with(".jpg") {
        return None;
    }
    let bytes = filename.as_bytes();
    if bytes.len() <= HASH_HEX_LEN || bytes[HASH_HEX_LEN] != b'_' {
        return None;
    }
    let hash = &filename[..HASH_HEX_LEN];
    if hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        Some(hash)
    } else {
        None
    }
}

/// Scratch path for writing a full-res original before it's atomically
/// renamed into place at `original_path`, mirrors `thumb_tmp_path`'s
/// same-filesystem-atomic-rename pattern and process-id disambiguation.
pub fn original_tmp_path(hash: &str) -> PathBuf {
    cache_dir().join(format!("{hash}_original.tmp{}", std::process::id()))
}

/// Path to a scratch file for writing a thumbnail before it's atomically
/// renamed into place at `thumb_path`. Lives in the same directory as the
/// final file so the rename is same-filesystem (and thus atomic on POSIX).
/// Includes the current process ID so concurrent writers (e.g. two
/// `videre watch` instances, or a leftover file from a crashed process) don't
/// collide on the same temp name.
pub fn thumb_tmp_path(hash: &str, size: u32) -> PathBuf {
    cache_dir().join(format!("{hash}_{size}.tmp{}", std::process::id()))
}

fn dirs_cache_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(|home| PathBuf::from(home).join(".cache"))
        .unwrap_or_else(|| PathBuf::from(".cache"))
}

/// One-time migration from the pre-rename cache location. Thumbnails are
/// content-hash keyed and expensive to regenerate for large HEIC libraries,
/// so a rename of the tool should not orphan them. Only fires when the old
/// dir exists and the new one does not; a plain rename, so it is atomic on
/// the same filesystem and a no-op on any error (cache regenerates lazily).
pub fn migrate_legacy_dupe_cache() {
    let old = dirs_cache_dir().join("dupe").join("thumbnails");
    let new = cache_dir();
    migrate_dir(&old, &new);
}

fn migrate_dir(old: &std::path::Path, new: &std::path::Path) {
    if old.is_dir() && !new.exists() {
        if let Some(parent) = new.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::rename(old, new);
        if let Some(old_parent) = old.parent() {
            let _ = std::fs::remove_dir(old_parent); // only removes if empty
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thumb_path_is_keyed_by_hash_and_size() {
        let p1 = thumb_path("abc123", 240);
        let p2 = thumb_path("abc123", 1200);
        let p3 = thumb_path("def456", 240);
        assert_ne!(p1, p2, "different sizes must produce different paths");
        assert_ne!(p1, p3, "different hashes must produce different paths");
        assert!(p1.to_string_lossy().contains("abc123_240.jpg"));
    }

    #[test]
    fn thumb_exists_false_for_missing_file() {
        assert!(!thumb_exists("nonexistent-hash-xyz", 240));
    }

    #[test]
    fn cache_dir_is_under_videre() {
        assert!(cache_dir().to_string_lossy().contains("videre"));
        assert!(!cache_dir().to_string_lossy().contains("/dupe/"));
    }

    #[test]
    fn face_thumb_path_is_keyed_by_hash_face_id_and_size() {
        let p1 = face_thumb_path("abc123", 1, 140);
        let p2 = face_thumb_path("abc123", 2, 140);
        let p3 = face_thumb_path("def456", 1, 140);
        assert_ne!(p1, p2, "different face ids must produce different paths");
        assert_ne!(p1, p3, "different hashes must produce different paths");
        assert!(p1.to_string_lossy().contains("abc123_face1_140.jpg"));
    }

    #[test]
    fn face_thumb_exists_false_for_missing_file() {
        assert!(!face_thumb_exists("nonexistent-hash-xyz", 99, 140));
    }

    #[test]
    fn original_path_is_keyed_by_hash() {
        let p1 = original_path("abc123");
        let p2 = original_path("def456");
        assert_ne!(p1, p2);
        assert!(p1.to_string_lossy().contains("abc123_original.jpg"));
    }

    #[test]
    fn original_exists_false_for_missing_file() {
        assert!(!original_exists("nonexistent-hash-xyz"));
    }

    #[test]
    fn original_tmp_path_differs_from_final_path_and_is_keyed_by_hash() {
        let tmp = original_tmp_path("abc123");
        let final_path = original_path("abc123");
        assert_ne!(tmp, final_path);
        assert!(tmp.to_string_lossy().contains("abc123_original.tmp"));
    }

    fn test_hash(seed: &str) -> String {
        seed.repeat((HASH_HEX_LEN / seed.len()) + 1)[..HASH_HEX_LEN].to_string()
    }

    #[test]
    fn hash_from_cache_filename_parses_thumb_path() {
        let h1 = test_hash("0123456789abcdef");
        assert_eq!(hash_from_cache_filename(&format!("{h1}_240.jpg")), Some(h1.as_str()));
        assert_eq!(hash_from_cache_filename(&format!("{h1}_1200.jpg")), Some(h1.as_str()));
    }

    #[test]
    fn hash_from_cache_filename_parses_face_thumb_path() {
        let h1 = test_hash("0123456789abcdef");
        assert_eq!(hash_from_cache_filename(&format!("{h1}_face3_140.jpg")), Some(h1.as_str()));
    }

    #[test]
    fn hash_from_cache_filename_parses_original_path() {
        let h1 = test_hash("0123456789abcdef");
        assert_eq!(hash_from_cache_filename(&format!("{h1}_original.jpg")), Some(h1.as_str()));
    }

    #[test]
    fn hash_from_cache_filename_distinguishes_different_hashes() {
        let h2 = test_hash("fedcba9876543210");
        assert_eq!(hash_from_cache_filename(&format!("{h2}_240.jpg")), Some(h2.as_str()));
    }

    #[test]
    fn hash_from_cache_filename_rejects_tmp_files() {
        let h1 = test_hash("0123456789abcdef");
        assert_eq!(hash_from_cache_filename(&format!("{h1}_original.tmp1234")), None);
        assert_eq!(hash_from_cache_filename(&format!("{h1}_240.tmp5678")), None);
    }

    #[test]
    fn hash_from_cache_filename_rejects_too_short_or_malformed_names() {
        assert_eq!(hash_from_cache_filename("short_240.jpg"), None);
        assert_eq!(hash_from_cache_filename(".DS_Store"), None);
        let non_hex_64 = "g".repeat(HASH_HEX_LEN);
        assert_eq!(hash_from_cache_filename(&format!("{non_hex_64}_240.jpg")), None);
    }

    #[test]
    fn migrate_dir_moves_old_into_place() {
        let tmp = std::env::temp_dir().join(format!("thumb_migrate_{}", std::process::id()));
        let old = tmp.join("old_cache");
        let new = tmp.join("new_cache");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("h_240.jpg"), b"x").unwrap();
        migrate_dir(&old, &new);
        assert!(new.join("h_240.jpg").exists(), "cached file must survive migration");
        assert!(!old.exists(), "old dir must be gone after migration");
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
