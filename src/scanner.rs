use std::path::{Path, PathBuf};
use walkdir::WalkDir;

const SUPPORTED_EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "png", "gif", "webp", "bmp", "tiff", "mov", "heic",
];

pub fn scan(dir: &Path) -> Vec<PathBuf> {
    WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter(|e| {
            e.path()
                .extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| SUPPORTED_EXTENSIONS.contains(&ext.to_lowercase().as_str()))
                .unwrap_or(false)
        })
        .map(|e| e.path().to_path_buf())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn scan_finds_image_files_recursively() {
        let dir = tempdir().unwrap();
        let sub = dir.path().join("sub");
        fs::create_dir(&sub).unwrap();
        fs::write(dir.path().join("a.jpg"), b"").unwrap();
        fs::write(dir.path().join("b.txt"), b"").unwrap(); // excluded
        fs::write(sub.join("c.PNG"), b"").unwrap();        // case-insensitive
        fs::write(sub.join("d.heic"), b"").unwrap();

        let results = scan(dir.path());
        assert_eq!(results.len(), 3);
        let names: Vec<_> = results
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_lowercase())
            .collect();
        assert!(names.contains(&"a.jpg".to_string()));
        assert!(names.contains(&"c.png".to_string()));
        assert!(names.contains(&"d.heic".to_string()));
        assert!(!names.iter().any(|n| n.ends_with(".txt")));
    }

    #[test]
    fn scan_empty_dir_returns_empty() {
        let dir = tempdir().unwrap();
        assert!(scan(dir.path()).is_empty());
    }
}
