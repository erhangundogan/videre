use std::path::PathBuf;

const FACE_CACHE_FORMAT_VERSION: u8 = 1;

/// Path to a content thumbnail in one selected library's cache namespace.
pub fn thumb_path_in(cache: &crate::library::CachePaths, hash: &str, size: u32) -> PathBuf {
    cache.thumbnails.join(format!("{hash}_{size}.jpg"))
}

/// Path to a cached full-resolution conversion in one selected library.
pub fn original_path_in(cache: &crate::library::CachePaths, hash: &str) -> PathBuf {
    cache.thumbnails.join(format!("{hash}_original.jpg"))
}

/// Path to a face crop whose identity includes its complete source geometry.
pub fn face_thumb_path_in(
    cache: &crate::library::CachePaths,
    hash: &str,
    face_id: i64,
    bbox: [f32; 4],
    size: u32,
) -> PathBuf {
    let mut key = blake3::Hasher::new();
    key.update(&[FACE_CACHE_FORMAT_VERSION]);
    key.update(hash.as_bytes());
    key.update(&face_id.to_le_bytes());
    for value in bbox {
        key.update(&value.to_bits().to_le_bytes());
    }
    key.update(&size.to_le_bytes());
    cache.thumbnails.join(format!(
        "{hash}_face-{}_{size}.jpg",
        key.finalize().to_hex()
    ))
}

pub fn thumb_exists_in(cache: &crate::library::CachePaths, hash: &str, size: u32) -> bool {
    thumb_path_in(cache, hash, size).is_file()
}

/// Scratch path for an original conversion in the selected library's cache,
/// renamed into place at [`original_path_in`]. Process id disambiguates
/// concurrent writers; the `.tmp` infix keeps prune from treating it as a
/// published entry.
pub fn original_tmp_path_in(cache: &crate::library::CachePaths, hash: &str) -> PathBuf {
    cache
        .thumbnails
        .join(format!("{hash}_original.tmp{}", std::process::id()))
}

/// Scratch path for a thumbnail in the selected library's cache, renamed into
/// place at [`thumb_path_in`].
pub fn thumb_tmp_path_in(cache: &crate::library::CachePaths, hash: &str, size: u32) -> PathBuf {
    cache
        .thumbnails
        .join(format!("{hash}_{size}.tmp{}", std::process::id()))
}

pub fn original_exists_in(cache: &crate::library::CachePaths, hash: &str) -> bool {
    original_path_in(cache, hash).is_file()
}

pub fn face_thumb_exists_in(
    cache: &crate::library::CachePaths,
    hash: &str,
    face_id: i64,
    bbox: [f32; 4],
    size: u32,
) -> bool {
    face_thumb_path_in(cache, hash, face_id, bbox, size).is_file()
}

/// Length of a BLAKE3 hex digest (32 bytes -> 64 hex chars), every
/// content-hash-keyed cache filename starts with exactly this many hex
/// chars, followed by `_` and a purpose-specific suffix
/// (`_240.jpg`, `_face3_140.jpg`, `_original.jpg`, `_original.tmp1234`, ...).
const HASH_HEX_LEN: usize = 64;

/// Extracts the leading content hash from a cache filename (the `.jpg`
/// files this module writes, `thumb_path_in`, `face_thumb_path_in`,
/// `original_path_in`), or `None` if `filename` doesn't match that shape.
/// Used by `videre prune` to find cache entries whose hash no longer has a
/// surviving `file_hashes` row, without hardcoding every suffix pattern this
/// module can produce. Deliberately does NOT match `.tmp*` scratch files
/// (see `thumb_tmp_path_in`/`original_tmp_path_in`), those may be actively being
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

#[cfg(test)]
mod tests {
    use super::*;

    fn contexts() -> (
        tempfile::TempDir,
        crate::library::LibraryContext,
        crate::library::LibraryContext,
    ) {
        let temp = tempfile::tempdir().unwrap();
        let a = temp.path().join("a");
        let b = temp.path().join("b");
        let cache = temp.path().join("cache");
        std::fs::create_dir(&a).unwrap();
        std::fs::create_dir(&b).unwrap();
        let a = crate::library::LibraryContext::new(&a, &cache).unwrap();
        let b = crate::library::LibraryContext::new(&b, &cache).unwrap();
        (temp, a, b)
    }

    #[test]
    fn explicit_cache_paths_are_isolated_by_library() {
        let (_temp, a, b) = contexts();
        assert_ne!(
            thumb_path_in(&a.cache, "hash", 240),
            thumb_path_in(&b.cache, "hash", 240)
        );
        assert_ne!(
            original_path_in(&a.cache, "hash"),
            original_path_in(&b.cache, "hash")
        );
    }

    #[cfg(unix)]
    #[test]
    fn aliases_of_one_library_share_the_same_cache_identity() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        let alias = temp.path().join("alias");
        let cache = temp.path().join("cache");
        std::fs::create_dir(&root).unwrap();
        std::os::unix::fs::symlink(&root, &alias).unwrap();
        let direct = crate::library::LibraryContext::new(&root, &cache).unwrap();
        let aliased = crate::library::LibraryContext::new(&alias, &cache).unwrap();
        assert_eq!(direct.cache, aliased.cache);
    }

    #[test]
    fn face_keys_include_id_geometry_size_and_format_version() {
        let (_temp, a, _b) = contexts();
        let base = face_thumb_path_in(&a.cache, "hash", 1, [1.0, 2.0, 3.0, 4.0], 140);
        assert_ne!(
            base,
            face_thumb_path_in(&a.cache, "hash", 2, [1.0, 2.0, 3.0, 4.0], 140)
        );
        assert_ne!(
            base,
            face_thumb_path_in(&a.cache, "hash", 1, [1.0, 2.0, 3.0, 5.0], 140)
        );
        assert_ne!(
            base,
            face_thumb_path_in(&a.cache, "hash", 1, [1.0, 2.0, 3.0, 4.0], 280)
        );
        assert!(base
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("hash_face-"));
    }

    #[test]
    fn tmp_paths_differ_from_final_paths_and_are_keyed_by_hash() {
        let (_temp, a, _b) = contexts();
        let orig_tmp = original_tmp_path_in(&a.cache, "abc123");
        assert_ne!(orig_tmp, original_path_in(&a.cache, "abc123"));
        assert!(orig_tmp.to_string_lossy().contains("abc123_original.tmp"));
        let thumb_tmp = thumb_tmp_path_in(&a.cache, "abc123", 240);
        assert_ne!(thumb_tmp, thumb_path_in(&a.cache, "abc123", 240));
        assert!(thumb_tmp.to_string_lossy().contains("abc123_240.tmp"));
    }

    fn test_hash(seed: &str) -> String {
        seed.repeat((HASH_HEX_LEN / seed.len()) + 1)[..HASH_HEX_LEN].to_string()
    }

    #[test]
    fn hash_from_cache_filename_parses_thumb_path() {
        let h1 = test_hash("0123456789abcdef");
        assert_eq!(
            hash_from_cache_filename(&format!("{h1}_240.jpg")),
            Some(h1.as_str())
        );
        assert_eq!(
            hash_from_cache_filename(&format!("{h1}_1200.jpg")),
            Some(h1.as_str())
        );
    }

    #[test]
    fn hash_from_cache_filename_parses_face_thumb_path() {
        let h1 = test_hash("0123456789abcdef");
        assert_eq!(
            hash_from_cache_filename(&format!("{h1}_face3_140.jpg")),
            Some(h1.as_str())
        );
    }

    #[test]
    fn hash_from_cache_filename_parses_original_path() {
        let h1 = test_hash("0123456789abcdef");
        assert_eq!(
            hash_from_cache_filename(&format!("{h1}_original.jpg")),
            Some(h1.as_str())
        );
    }

    #[test]
    fn hash_from_cache_filename_distinguishes_different_hashes() {
        let h2 = test_hash("fedcba9876543210");
        assert_eq!(
            hash_from_cache_filename(&format!("{h2}_240.jpg")),
            Some(h2.as_str())
        );
    }

    #[test]
    fn hash_from_cache_filename_rejects_tmp_files() {
        let h1 = test_hash("0123456789abcdef");
        assert_eq!(
            hash_from_cache_filename(&format!("{h1}_original.tmp1234")),
            None
        );
        assert_eq!(hash_from_cache_filename(&format!("{h1}_240.tmp5678")), None);
    }

    #[test]
    fn hash_from_cache_filename_rejects_too_short_or_malformed_names() {
        assert_eq!(hash_from_cache_filename("short_240.jpg"), None);
        assert_eq!(hash_from_cache_filename(".DS_Store"), None);
        let non_hex_64 = "g".repeat(HASH_HEX_LEN);
        assert_eq!(
            hash_from_cache_filename(&format!("{non_hex_64}_240.jpg")),
            None
        );
    }
}
