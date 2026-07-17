use std::path::PathBuf;

/// Directory holding pre-converted HEIC thumbnails, keyed by content hash
/// rather than file path - the same photo scanned into different databases
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
/// many faces can share one source `hash` - the face id disambiguates.
pub fn face_thumb_path(hash: &str, face_id: i64, size: u32) -> PathBuf {
    cache_dir().join(format!("{hash}_face{face_id}_{size}.jpg"))
}

/// True if a cached face crop already exists for this hash/face_id/size.
pub fn face_thumb_exists(hash: &str, face_id: i64, size: u32) -> bool {
    face_thumb_path(hash, face_id, size).exists()
}

/// Cache path for a full-resolution HEIC-converted original. One per hash
/// (not per face - the original photo is the same regardless of which face
/// on it was clicked).
pub fn original_path(hash: &str) -> PathBuf {
    cache_dir().join(format!("{hash}_original.jpg"))
}

/// True if a cached full-resolution original already exists for this hash.
pub fn original_exists(hash: &str) -> bool {
    original_path(hash).exists()
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
