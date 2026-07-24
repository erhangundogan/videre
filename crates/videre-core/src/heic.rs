use crate::io_timeout::{wait_with_timeout, WaitOutcome};
use image::DynamicImage;
use std::time::Duration;

/// Ceiling for a single `qlmanage` conversion. A disconnected/stale external
/// volume can make `qlmanage`'s own file access block indefinitely on macOS
/// rather than fail fast, which otherwise freezes the calling command with
/// no output; this bounds that to a single conversion's worth of waiting.
const QLMANAGE_TIMEOUT: Duration = Duration::from_secs(20);

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
    let mut child = std::process::Command::new("qlmanage")
        .args(["-t", "-s", "10000", "-o"])
        .arg(&out_dir)
        .arg(path)
        .spawn()
        .ok()?;
    let outcome = wait_with_timeout(&mut child, QLMANAGE_TIMEOUT);
    if outcome == WaitOutcome::TimedOut {
        eprintln!(
            "warning: qlmanage timed out after {}s converting {path} (file may be unreachable - is its drive disconnected?); skipping",
            QLMANAGE_TIMEOUT.as_secs()
        );
    }
    let file_name = std::path::Path::new(path).file_name()?.to_str()?;
    let out_file = out_dir.join(format!("{file_name}.png"));
    let result = if outcome == WaitOutcome::Success { image::open(&out_file).ok() } else { None };
    let _ = std::fs::remove_dir_all(&out_dir);
    result
}
