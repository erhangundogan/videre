use image::DynamicImage;

/// Convert a HEIC file to a `DynamicImage` via QuickLook (`qlmanage -t`).
///
/// `sips -s format jpeg` copies the raw sensor-buffer pixels unrotated for
/// HEIC files where the camera encoded rotation via the HEIF `irot`
/// transform box rather than a classic EXIF Orientation tag - the same
/// rotation Finder/Preview/Photos apply via QuickLook. Using `sips` would
/// produce sideways images (or, for dupe-faces, detect faces and compute
/// bounding boxes against the wrongly oriented image).
///
/// `tag` disambiguates concurrent/repeated conversions of the same path for
/// different purposes (e.g. a 240px thumbnail vs a 1200px lightbox version)
/// so their temp-directory names don't collide.
pub fn heic_via_quicklook(path: &str, tag: &str) -> Option<DynamicImage> {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    path.hash(&mut hasher);
    tag.hash(&mut hasher);
    let out_dir = std::env::temp_dir().join(format!("dupe_ql_{:016x}", hasher.finish()));
    let _ = std::fs::remove_dir_all(&out_dir);
    std::fs::create_dir_all(&out_dir).ok()?;
    let ok = std::process::Command::new("qlmanage")
        .args(["-t", "-s", "10000", "-o"])
        .arg(&out_dir)
        .arg(path)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    let file_name = std::path::Path::new(path).file_name()?.to_str()?;
    let out_file = out_dir.join(format!("{file_name}.png"));
    let result = if ok { image::open(&out_file).ok() } else { None };
    let _ = std::fs::remove_dir_all(&out_dir);
    result
}
