use crate::io_timeout::{wait_with_timeout, WaitOutcome};
use crate::semaphore::Semaphore;
use image::DynamicImage;
use std::sync::OnceLock;
use std::time::Duration;

/// Ceiling for a single `qlmanage` conversion. A disconnected/stale external
/// volume can make `qlmanage`'s own file access block indefinitely on macOS
/// rather than fail fast, which otherwise freezes the calling command with
/// no output; this bounds that to a single conversion's worth of waiting.
const QLMANAGE_TIMEOUT: Duration = Duration::from_secs(20);

/// Max `qlmanage` processes running at once, process-wide. QuickLook's
/// thumbnail-generation agent is a single shared per-user service, not
/// something that scales with parallel callers - a UI rendering hundreds of
/// HEIC thumbnails at once (or the Tauri app and a `videre faces` run both
/// hitting the same library concurrently) can launch enough simultaneous
/// `qlmanage` processes to make the agent and the source drive's I/O queue
/// up, causing conversions that would normally take under a second to
/// occasionally exceed even `QLMANAGE_TIMEOUT`. Capping concurrency here
/// keeps every caller (Tauri app, axum server, faces/embed/watch) well
/// behaved without needing to coordinate with each other.
const QLMANAGE_MAX_CONCURRENT: usize = 3;

/// Shared across every `qlmanage` call site in the codebase (this module's
/// own `heic_via_quicklook` and `videre-ml`'s separate `decode_heic`), so the
/// concurrency cap is process-wide rather than per-call-site.
pub fn qlmanage_semaphore() -> &'static Semaphore {
    static SEM: OnceLock<Semaphore> = OnceLock::new();
    SEM.get_or_init(|| Semaphore::new(QLMANAGE_MAX_CONCURRENT))
}

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
    let _permit = qlmanage_semaphore().acquire();
    let mut child = std::process::Command::new("qlmanage")
        .args(["-t", "-s", "10000", "-o"])
        .arg(&out_dir)
        .arg(path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
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
