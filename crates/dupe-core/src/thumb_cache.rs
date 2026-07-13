use std::path::PathBuf;

/// Directory holding pre-converted HEIC thumbnails, keyed by content hash
/// rather than file path - the same photo scanned into different databases
/// only needs converting once. Mirrors this project's existing
/// `~/.cache/ort/` convention for cached model weights.
pub fn cache_dir() -> PathBuf {
    dirs_cache_dir().join("dupe").join("thumbnails")
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

fn dirs_cache_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(|home| PathBuf::from(home).join(".cache"))
        .unwrap_or_else(|| PathBuf::from(".cache"))
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
}
