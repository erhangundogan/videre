use crate::io_timeout::{wait_with_timeout, WaitOutcome};
use crate::semaphore::Semaphore;
use anyhow::Context as _;
use image::DynamicImage;
use std::path::Path;
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

/// Shared by every `qlmanage` conversion in the codebase, so the
/// concurrency cap is process-wide rather than per-call-site.
pub fn qlmanage_semaphore() -> &'static Semaphore {
    static SEM: OnceLock<Semaphore> = OnceLock::new();
    SEM.get_or_init(|| {
        let max = resolve_qlmanage_concurrency(QLMANAGE_CONCURRENCY_OVERRIDE.get().copied());
        Semaphore::new(max)
    })
}

/// Convert an image or video file to a `DynamicImage` via QuickLook
/// (`qlmanage -t`). The one QuickLook decode path in the workspace: HEIC
/// photos (the `image` crate cannot decode HEIC) and video poster frames
/// (QuickLook generates one the same way it does for HEIC) decode here,
/// from every crate.
///
/// `sips -s format jpeg` copies the raw sensor-buffer pixels unrotated for
/// HEIC files where the camera encoded rotation via the HEIF `irot`
/// transform box rather than a classic EXIF Orientation tag, the same
/// rotation Finder/Preview/Photos apply via QuickLook. Using `sips` would
/// produce sideways images (or, for dupe-faces, detect faces and compute
/// bounding boxes against the wrongly oriented image).
///
/// `tag` disambiguates concurrent/repeated conversions of the same path for
/// different purposes (e.g. a 240px thumbnail vs a 1200px lightbox version,
/// or a HEIC conversion next to a video conversion of a same-named file) so
/// their temp-directory names don't collide.
///
/// `max_size` caps `qlmanage -s`'s longest-side render size. `qlmanage -s`
/// only ever caps, never upscales, so `None` (rendered as `10000`,
/// comfortably above any real photo's native resolution) means "give me the
/// native/full resolution", the only safe choice for callers whose output
/// pixels must stay in the same coordinate space as something already
/// stored (face detection bboxes, face-thumbnail crops) or that need true
/// full quality (serving the original image). `Some(n)` is only safe for
/// callers that immediately downscale the result themselves anyway (report
/// thumbnails, gallery posters, the embed path's double-the-tensor-size
/// renders); for them, requesting a smaller render up front avoids qlmanage
/// decoding/resizing/PNG-encoding pixels that get thrown away moments
/// later. Do NOT pass `Some` for the face-detection or face-thumbnail-crop
/// call sites: detection's bbox coordinates are stored in terms of whatever
/// image size detection ran on (see `face_detect.rs`), so shrinking that
/// decode would silently corrupt every later full-res thumbnail crop and
/// the `--min-face-size` quality gate, which measures bbox size in that
/// same (assumed-full-res) space.
///
/// Video wall time does not scale with file size (one seek, one frame):
/// timed against the 5 largest real videos of a real library (2.3-5.7GB)
/// on 2026-08-01, all poster extractions finished in 0.22-0.36s, so the 20s
/// ceiling has ~50-90x headroom even for the largest real files measured.
pub fn decode_via_quicklook(
    path: &Path,
    tag: &str,
    max_size: Option<u32>,
) -> anyhow::Result<DynamicImage> {
    if !cfg!(target_os = "macos") {
        // Long explanation once per run; short typed reason per file. The
        // warning has already fired by the time the caller sees this error.
        warn_quicklook_unavailable_once();
        return Err(anyhow::anyhow!("needs macOS QuickLook (`qlmanage`)")
            .context(crate::error_kind::ErrorKind::QuicklookUnavailable));
    }
    // `qlmanage -t` does not fail on a container with no video track, it
    // hangs. Extension-gated so HEIC, which shares this function, is
    // untouched, and placed before the semaphore so a skipped file never
    // holds a permit. See `video_probe` for the measurement behind this.
    let probe_ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase());
    if matches!(probe_ext.as_deref(), Some("mov") | Some("mp4"))
        && !crate::video_probe::has_video_track(path)
    {
        anyhow::bail!("no video track (audio-only file); skipped without calling QuickLook");
    }
    // A scratch directory of this call's own, removed when `scratch` drops,
    // so simultaneous decodes of one file cannot delete each other's output
    // (the gallery requesting an uncached video poster twice at once once
    // got a 404 when a shared, path-named directory made that possible).
    let scratch = tempfile::Builder::new()
        .prefix(&format!("videre_ql_{tag}_"))
        .tempdir()
        .context("create qlmanage temp dir")?;
    let out_dir = scratch.path();
    let _permit = qlmanage_semaphore().acquire();
    let size_arg = max_size.unwrap_or(10000).to_string();
    let mut child = std::process::Command::new("qlmanage")
        .args(["-t", "-s", &size_arg, "-o"])
        .arg(out_dir)
        .arg(path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .context("run qlmanage (requires macOS)")?;
    let outcome = wait_with_timeout(&mut child, QLMANAGE_TIMEOUT);
    // Returned, never logged here: a caller that reports its errors would log
    // it a second time. Callers that swallow the error pass it through
    // `warn_if_timeout` so a disconnected drive still reaches the log.
    if outcome == WaitOutcome::TimedOut {
        return Err(anyhow::anyhow!(
            "qlmanage timed out after {}s decoding {} (file may be unreachable - is its drive connected?)",
            QLMANAGE_TIMEOUT.as_secs(),
            path.display()
        )
        .context(crate::error_kind::ErrorKind::SourceUnavailable));
    }
    anyhow::ensure!(
        outcome == WaitOutcome::Success,
        "qlmanage failed for {}",
        path.display()
    );
    let file_name = path.file_name().context("path has no file name")?;
    let out_file = out_dir.join(quicklook_output_name(file_name));
    image::open(&out_file).with_context(|| format!("decode qlmanage output for {}", path.display()))
}

/// Encode `jpeg` into the library's cached original for `hash`, atomically:
/// every writer gets its own temporary file (`atomic_file::publish`), so two
/// concurrent savers in one gallery process can never rename a half-written
/// entry into place. Best-effort: a failure is logged and reported, never
/// fatal to the decode the bytes came from.
pub fn publish_cached_original(
    cache: &crate::library::CachePaths,
    hash: &str,
    jpeg: &[u8],
) -> bool {
    let destination = crate::thumb_cache::original_path_in(cache, hash);
    let written = crate::atomic_file::publish(&destination, |file| {
        use std::io::Write as _;
        file.write_all(jpeg).map_err(Into::into)
    });
    if let Err(error) = written {
        tracing::warn!("could not cache the full-resolution original for {hash}: {error:#}");
        return false;
    }
    true
}

/// Full-resolution HEIC decode for a library context: the cached original
/// (`thumb_cache::original_path_in`) when it is there, opened under the I/O
/// timeout; a fresh `decode_via_quicklook` render otherwise. A render is
/// published back to the cache, so the first request for a hash pays for it
/// and the rest read it: five faces on one photo render once, not five
/// times. `cache` `None`: render only. A missing, corrupt, or stale entry
/// is replaced by the render that follows it. The published entry is the
/// same upright, full-resolution QuickLook output the bbox geometry
/// expects; open it with plain `image::open`, never
/// `image_decode::decode_oriented_file` (double rotation).
pub fn decode_fullres_cached(
    path: &Path,
    tag: &str,
    cache: Option<(&crate::library::CachePaths, &str)>,
) -> anyhow::Result<DynamicImage> {
    if let Some((cache, hash)) = cache {
        let cached = crate::thumb_cache::original_path_in(cache, hash);
        let opened =
            crate::io_timeout::run_with_timeout(crate::io_timeout::DEFAULT_IO_TIMEOUT, move || {
                image::open(&cached)
            });
        if let Ok(Ok(img)) = opened {
            return Ok(img);
        }
    }
    // A QuickLook timeout is the caller's to log: the two callers either
    // swallow the error (`make_face_thumb`, via `warn_if_timeout`) or
    // propagate it to a worker that logs it (`load_image`), and calling it
    // here as well would log a detection timeout twice.
    let img = decode_via_quicklook(path, tag, None)?;
    if let Some((cache, hash)) = cache {
        let mut jpeg = Vec::new();
        if img
            .write_to(
                &mut std::io::Cursor::new(&mut jpeg),
                image::ImageFormat::Jpeg,
            )
            .is_ok()
        {
            publish_cached_original(cache, hash, &jpeg);
        }
    }
    Ok(img)
}

/// `qlmanage -t` writes `<file name>.png`, byte for byte. Built as an OS
/// string so a file name that is not valid UTF-8 still finds its output.
fn quicklook_output_name(file_name: &std::ffi::OsStr) -> std::ffi::OsString {
    let mut name = file_name.to_os_string();
    name.push(".png");
    name
}

/// For callers that swallow a QuickLook decode failure (a thumbnail, a
/// poster, a cache fill): a timeout still reaches the log, since it usually
/// means a disconnected drive. Other failures stay quiet there, as before.
/// Callers that return the error must not call this, or it is logged twice.
pub fn warn_if_timeout(error: &anyhow::Error) {
    if crate::error_kind::ErrorKind::in_chain(error)
        == Some(crate::error_kind::ErrorKind::SourceUnavailable)
    {
        crate::error_log::report(tracing::Level::WARN, error, None);
    }
}

/// Message shared by every QuickLook entry point, so a non-macOS user gets one
/// clear explanation instead of a stream of opaque per-file decode failures.
pub const QUICKLOOK_UNAVAILABLE: &str =
    "HEIC images and video frames are decoded via macOS QuickLook (`qlmanage`), \
     which has no equivalent on this platform - those files are skipped. \
     Scanning, dedupe, and search still work for jpg/jpeg/png/gif/webp/bmp/tiff.";

/// Prints `QUICKLOOK_UNAVAILABLE` at most once per process. Called on the
/// non-macOS path of the QuickLook decoder: without it a Linux user just sees
/// each HEIC/video silently fail to decode, with nothing saying why.
fn warn_quicklook_unavailable_once() {
    static WARNED: std::sync::Once = std::sync::Once::new();
    WARNED.call_once(|| {
        crate::error_log::report(tracing::Level::WARN, &quicklook_unavailable(), None)
    });
}

/// The QuickLook-unavailable warning, carrying its kind so the log records
/// why HEIC and video files were skipped on this platform.
fn quicklook_unavailable() -> anyhow::Error {
    anyhow::anyhow!(QUICKLOOK_UNAVAILABLE)
        .context(crate::error_kind::ErrorKind::QuicklookUnavailable)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_published_original_opens_as_a_jpeg() {
        let temp = tempfile::tempdir().unwrap();
        let ctx =
            crate::library::LibraryContext::new(temp.path(), &temp.path().join("cache")).unwrap();
        let img = image::RgbImage::from_pixel(2, 2, image::Rgb([255, 0, 0]));
        let mut jpeg = Vec::new();
        image::DynamicImage::ImageRgb8(img)
            .write_to(
                &mut std::io::Cursor::new(&mut jpeg),
                image::ImageFormat::Jpeg,
            )
            .unwrap();

        assert!(publish_cached_original(&ctx.cache, "pub-test", &jpeg));
        let opened =
            image::open(crate::thumb_cache::original_path_in(&ctx.cache, "pub-test")).unwrap();
        assert_eq!((opened.width(), opened.height()), (2, 2));
    }

    #[test]
    fn the_quicklook_warning_carries_its_kind_and_the_full_explanation() {
        let err = quicklook_unavailable();
        assert_eq!(
            crate::error_kind::ErrorKind::in_chain(&err),
            Some(crate::error_kind::ErrorKind::QuicklookUnavailable)
        );
        assert!(format!("{err:#}").contains("qlmanage"));
    }

    /// Simultaneous conversions of one file must not share a scratch
    /// directory. They did: it was named from the path and tag, and each call
    /// cleared it before and after running, so one call deleted another's
    /// output and failed. The gallery's tile view hit this whenever the page
    /// and a second request asked for the same uncached video poster.
    #[test]
    fn simultaneous_conversions_of_one_file_all_succeed() {
        if !cfg!(target_os = "macos") {
            return;
        }
        let video = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../videre/tests/fixtures/red_1s.mp4"
        );
        let handles: Vec<_> = (0..4)
            .map(|_| {
                std::thread::spawn(move || {
                    decode_via_quicklook(Path::new(video), "vposter480", Some(480))
                })
            })
            .collect();
        let ok = handles
            .into_iter()
            .map(|h| h.join().unwrap().is_ok())
            .filter(|ok| *ok)
            .count();
        assert_eq!(ok, 4, "every simultaneous conversion must produce an image");
    }

    /// An audio-only mov/mp4 must be rejected before `qlmanage` is spawned:
    /// qlmanage does not fail on such containers, it hangs until
    /// QLMANAGE_TIMEOUT (20s). Reached across crates the same way the
    /// simultaneous-conversions test above reaches red_1s.mp4.
    #[test]
    #[cfg(target_os = "macos")]
    fn an_audio_only_video_is_rejected_before_quicklook_is_called() {
        let audio_only = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../videre/tests/fixtures/audio_only.mov"
        );
        let err = decode_via_quicklook(Path::new(audio_only), "audio-only-test", None).unwrap_err();
        assert!(
            err.to_string().contains("no video track"),
            "unexpected error: {err:#}"
        );
    }

    /// Off macOS the shared decoder fails with the typed
    /// QuicklookUnavailable kind, so pipeline runs record the real reason
    /// and callers that swallow the error have already seen the long
    /// once-per-process warning.
    #[test]
    fn without_quicklook_the_decode_fails_with_the_quicklook_unavailable_kind() {
        if cfg!(target_os = "macos") {
            return;
        }
        let err =
            decode_via_quicklook(Path::new("/nonexistent.mov"), "kind-test", None).unwrap_err();
        assert_eq!(
            crate::error_kind::ErrorKind::in_chain(&err),
            Some(crate::error_kind::ErrorKind::QuicklookUnavailable)
        );
    }

    #[test]
    #[cfg(unix)]
    fn a_non_utf8_file_name_keeps_its_bytes_in_the_output_name() {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};
        let name = std::ffi::OsStr::from_bytes(b"caf\xe9.heic");
        assert_eq!(
            quicklook_output_name(name).into_vec(),
            b"caf\xe9.heic.png".to_vec()
        );
    }

    #[test]
    fn only_a_timeout_is_warned_for_callers_that_swallow_the_error() {
        #[derive(Clone, Default)]
        struct Buf(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
        impl std::io::Write for Buf {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(b);
                Ok(b.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let buf = Buf::default();
        let w = buf.clone();
        let sub = tracing_subscriber::fmt()
            .with_writer(move || w.clone())
            .finish();
        tracing::subscriber::with_default(sub, || {
            warn_if_timeout(&anyhow::anyhow!("decode failed"));
            warn_if_timeout(
                &anyhow::anyhow!("qlmanage timed out")
                    .context(crate::error_kind::ErrorKind::SourceUnavailable),
            );
        });
        let logged = String::from_utf8_lossy(&buf.0.lock().unwrap()).to_string();
        assert!(logged.contains("qlmanage timed out"), "{logged}");
        assert!(!logged.contains("decode failed"), "{logged}");
    }

    #[test]
    fn resolve_qlmanage_concurrency_uses_override_when_present() {
        assert_eq!(resolve_qlmanage_concurrency(Some(10)), 10);
    }

    #[test]
    fn resolve_qlmanage_concurrency_falls_back_to_default_when_absent() {
        assert_eq!(
            resolve_qlmanage_concurrency(None),
            QLMANAGE_MAX_CONCURRENT_DEFAULT
        );
    }

    #[test]
    fn resolve_qlmanage_concurrency_override_of_zero_is_honored_literally() {
        // Not clamped here, resolve_qlmanage_concurrency is pure plumbing;
        // Semaphore::new(0) blocking forever is a caller-input-validation
        // concern (see the --qlmanage-concurrency CLI flag), not this
        // function's job.
        assert_eq!(resolve_qlmanage_concurrency(Some(0)), 0);
    }

    // A real conversion through decode_via_quicklook is not unit tested:
    // qlmanage does not fail fast on a nonexistent path, it hangs until
    // QLMANAGE_TIMEOUT (20s), exactly the slow-path this module's own
    // timeout mechanism exists to bound. The audio-only test above exercises
    // the pre-spawn guard without spawning qlmanage; real conversions are
    // exercised in practice by videre faces/report/embed/watch against real
    // HEIC files.
}
