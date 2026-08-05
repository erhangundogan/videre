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

/// Default max `qlmanage` processes running at once, process-wide, unless
/// overridden via `set_qlmanage_concurrency` (e.g. `videre faces
/// --qlmanage-concurrency <n>`). QuickLook's thumbnail-generation agent is a
/// single shared per-user service, not something that scales with parallel
/// callers, a UI rendering hundreds of HEIC thumbnails at once (or two
/// videre processes both hitting the same library concurrently) can launch
/// enough simultaneous `qlmanage` processes to make
/// the agent and the source drive's I/O queue up, causing conversions that
/// would normally take under a second to occasionally exceed even
/// `QLMANAGE_TIMEOUT`. Capping concurrency here keeps every caller (axum
/// server, faces/embed/watch, and any other embedder) well behaved without
/// needing to coordinate with each other. Raised from 3 to 6 on 2026-07-29 after real
/// A/B measurement of `videre faces`'s parallel pipeline showed HEIC-heavy
/// runs leaving CPU idle (477% of 1000% on a 10-core machine), a real
/// bottleneck candidate given `--workers` now defaults to 2x cores (up to
/// 20 concurrent workers, all previously queuing on a 3-permit cap).
const QLMANAGE_MAX_CONCURRENT_DEFAULT: usize = 6;

/// Process-wide override for `QLMANAGE_MAX_CONCURRENT_DEFAULT`, set at most
/// once per process (first call wins, matching the underlying semaphore's
/// own `OnceLock` semantics). See `set_qlmanage_concurrency`.
static QLMANAGE_CONCURRENCY_OVERRIDE: OnceLock<usize> = OnceLock::new();

/// Overrides the `qlmanage` concurrency cap for the remainder of this
/// process. Must be called before the first HEIC conversion (i.e. before
/// anything calls `qlmanage_semaphore()`) to take effect, like the
/// semaphore itself, this is a `OnceLock`: the first call sets the value,
/// every later call (including one after the semaphore has already been
/// created with the default) is a no-op. Intended for CLI flags like
/// `videre faces --qlmanage-concurrency <n>` to call once at startup.
pub fn set_qlmanage_concurrency(n: usize) {
    let _ = QLMANAGE_CONCURRENCY_OVERRIDE.set(n);
}

/// Pure resolution of the effective concurrency cap, split out from
/// `qlmanage_semaphore` so the "override if present, else default" logic is
/// unit-testable without touching the process-wide `OnceLock` singletons.
fn resolve_qlmanage_concurrency(override_val: Option<usize>) -> usize {
    override_val.unwrap_or(QLMANAGE_MAX_CONCURRENT_DEFAULT)
}

/// Shared across every `qlmanage` call site in the codebase (this module's
/// own `heic_via_quicklook` and `videre-ml`'s separate `decode_heic`), so the
/// concurrency cap is process-wide rather than per-call-site.
pub fn qlmanage_semaphore() -> &'static Semaphore {
    static SEM: OnceLock<Semaphore> = OnceLock::new();
    SEM.get_or_init(|| {
        let max = resolve_qlmanage_concurrency(QLMANAGE_CONCURRENCY_OVERRIDE.get().copied());
        Semaphore::new(max)
    })
}

/// Convert a HEIC file to a `DynamicImage` via QuickLook (`qlmanage -t`).
///
/// `sips -s format jpeg` copies the raw sensor-buffer pixels unrotated for
/// HEIC files where the camera encoded rotation via the HEIF `irot`
/// transform box rather than a classic EXIF Orientation tag, the same
/// rotation Finder/Preview/Photos apply via QuickLook. Using `sips` would
/// produce sideways images (or, for dupe-faces, detect faces and compute
/// bounding boxes against the wrongly oriented image).
///
/// `tag` disambiguates concurrent/repeated conversions of the same path for
/// different purposes (e.g. a 240px thumbnail vs a 1200px lightbox version)
/// so their temp-directory names don't collide.
///
/// Message shared by every QuickLook entry point, so a non-macOS user gets one
/// clear explanation instead of a stream of opaque per-file decode failures.
pub const QUICKLOOK_UNAVAILABLE: &str =
    "HEIC images and video frames are decoded via macOS QuickLook (`qlmanage`), \
     which has no equivalent on this platform - those files are skipped. \
     Scanning, dedupe, and search still work for jpg/jpeg/png/gif/webp/bmp/tiff.";

/// Prints `QUICKLOOK_UNAVAILABLE` at most once per process. Called on the
/// non-macOS path of every QuickLook helper: without it a Linux user just sees
/// each HEIC/video silently fail to decode, with nothing saying why.
pub fn warn_quicklook_unavailable_once() {
    static WARNED: std::sync::Once = std::sync::Once::new();
    WARNED.call_once(|| eprintln!("warning: {QUICKLOOK_UNAVAILABLE}"));
}

/// `max_size` caps `qlmanage -s`'s longest-side render size. `qlmanage -s`
/// only ever caps, never upscales, so `None` (rendered as `10000`, comfortably
/// above any real photo's native resolution) means "give me the native/full
/// resolution", the only safe choice for callers whose output pixels must
/// stay in the same coordinate space as something already stored (face
/// detection bboxes, face-thumbnail crops) or that need true full quality
/// (serving the original image). `Some(n)` is only safe for callers that
/// immediately downscale the result themselves anyway (report.rs's
/// b64/`/api/raw` thumbnails, `watch --heic`'s cache), for them, requesting
/// a smaller render up front avoids qlmanage decoding/resizing/PNG-encoding
/// pixels that get thrown away moments later. Do NOT pass `Some` for the
/// face-detection or face-thumbnail-crop call sites: detection's bbox
/// coordinates are stored in terms of whatever image size detection ran on
/// (see `face_detect.rs`), so shrinking that decode would silently corrupt
/// every later full-res thumbnail crop and the `--min-face-size` quality
/// gate, which measures bbox size in that same (assumed-full-res) space.
pub fn heic_via_quicklook(path: &str, tag: &str, max_size: Option<u32>) -> Option<DynamicImage> {
    if !cfg!(target_os = "macos") {
        warn_quicklook_unavailable_once();
        return None;
    }
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    path.hash(&mut hasher);
    tag.hash(&mut hasher);
    let out_dir = std::env::temp_dir().join(format!("dupe_ql_{:016x}", hasher.finish()));
    let _ = std::fs::remove_dir_all(&out_dir);
    std::fs::create_dir_all(&out_dir).ok()?;
    let _permit = qlmanage_semaphore().acquire();
    let size_arg = max_size.unwrap_or(10000).to_string();
    let mut child = std::process::Command::new("qlmanage")
        .args(["-t", "-s", &size_arg, "-o"])
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_qlmanage_concurrency_uses_override_when_present() {
        assert_eq!(resolve_qlmanage_concurrency(Some(10)), 10);
    }

    #[test]
    fn resolve_qlmanage_concurrency_falls_back_to_default_when_absent() {
        assert_eq!(resolve_qlmanage_concurrency(None), QLMANAGE_MAX_CONCURRENT_DEFAULT);
    }

    #[test]
    fn resolve_qlmanage_concurrency_override_of_zero_is_honored_literally() {
        // Not clamped here, resolve_qlmanage_concurrency is pure plumbing;
        // Semaphore::new(0) blocking forever is a caller-input-validation
        // concern (see the --qlmanage-concurrency CLI flag), not this
        // function's job.
        assert_eq!(resolve_qlmanage_concurrency(Some(0)), 0);
    }

    // heic_via_quicklook itself is not unit tested: qlmanage does not fail
    // fast on a nonexistent path, it hangs until QLMANAGE_TIMEOUT (20s) -
    // exactly the slow-path this module's own timeout mechanism exists to
    // bound. A test exercising it would tax every future test run by 20
    // seconds for one marginal coverage line; not a worthwhile trade. This
    // function is exercised in practice by videre faces/report/embed/watch
    // against real HEIC files instead.
}
