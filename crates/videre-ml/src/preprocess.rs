//! Decode an image file into a SigLIP input tensor: resize to NxN,
//! scale to [0,1], normalize with mean 0.5 / std 0.5 per channel -> [-1,1].
//! HEIC is converted via QuickLook (macOS), matching dupe-report and
//! dupe-faces. See `decode_via_quicklook` for why `sips` alone isn't used.

use anyhow::{Context, Result};
use candle_core::{DType, Device, Tensor};
use std::path::Path;

pub fn image_to_tensor(path: &Path, size: usize, device: &Device) -> Result<Tensor> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .unwrap_or_default();

    let img = if ext == "heic" {
        decode_via_quicklook(path, size, "embed-heic")?
    } else if ext == "mov" || ext == "mp4" {
        decode_via_quicklook(path, size, "embed-video")?
    } else {
        let timeout_path = path.to_path_buf();
        videre_core::io_timeout::run_with_timeout(videre_core::io_timeout::DEFAULT_IO_TIMEOUT, move || {
            // Orientation-aware decode: the tensor must describe the photo as
            // a person sees it, not the sensor canvas (see
            // `videre_core::image_decode` for why HEIC never reaches here).
            videre_core::image_decode::decode_oriented_file(&timeout_path)
        })
        .map_err(|_| {
            anyhow::anyhow!(
                "timed out reading {} after {}s (file may be unreachable - is its drive connected?)",
                path.display(),
                videre_core::io_timeout::DEFAULT_IO_TIMEOUT.as_secs()
            )
            .context(videre_core::error_kind::ErrorKind::SourceUnavailable)
        })?
        .map_err(|e| {
            videre_core::error_kind::from_image(e).context(format!("decode {}", path.display()))
        })?
    };

    let img = img
        .resize_exact(
            size as u32,
            size as u32,
            image::imageops::FilterType::Triangle,
        )
        .to_rgb8();

    let data: Vec<f32> = img.into_raw().iter().map(|&b| b as f32 / 255.0).collect();
    // HWC -> CHW, then (x - 0.5) / 0.5
    let t = Tensor::from_vec(data, (size, size, 3), device)?
        .permute((2, 0, 1))?
        .to_dtype(DType::F32)?;
    let t = ((t - 0.5)? / 0.5)?;
    Ok(t)
}

/// Convert an image/video file to a `DynamicImage` via QuickLook (`qlmanage -t`).
/// Used for HEIC (see the orientation note below) and for `.mov`/`.mp4`.
/// QuickLook already generates a poster-frame thumbnail for video files the
/// same way it does for HEIC, so this is one shared mechanism for both
/// rather than a second implementation.
///
/// `sips -s format jpeg` copies the raw sensor-buffer pixels unrotated for
/// HEIC files where the iPhone camera encoded rotation via the HEIF `irot`
/// transform box rather than a classic EXIF Orientation tag, the same
/// rotation Finder/Preview/Photos apply via QuickLook. Since the resulting
/// tensor is immediately resized square anyway, exact resolution doesn't
/// matter here as much as getting the orientation right so the embedding
/// represents the photo as a person actually sees it.
///
/// `tag` disambiguates temp-directory names between call sites (mirrors
/// `videre_core::heic::heic_via_quicklook`'s existing `tag` parameter) so a
/// HEIC conversion and a video conversion of files that happen to hash the
/// same never collide.
///
/// Ceiling for a single `qlmanage` conversion. See `videre_core::heic` for
/// why this is needed: a disconnected/stale external volume can make
/// `qlmanage` block indefinitely on macOS rather than fail fast, which
/// otherwise freezes `videre embed` silently on that one file.
///
/// Confirmed well-tuned for video specifically (2026-08-01): timed raw
/// `qlmanage -t` poster-frame extraction directly against the 5 largest real
/// video files in a real library (2.3-5.7GB, `.mov`/`.mp4`), all completed
/// in 0.22-0.36s. Video extraction only seeks to one point and decodes a
/// single frame, so unlike HEIC's full-image decode, wall time doesn't scale
/// with file size/duration, this 20s ceiling has ~50-90x headroom even for
/// the largest real files measured, not just typical ones.
const QLMANAGE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

// Sibling: `videre_core::heic::heic_via_quicklook` does nearly the same thing
// for the scan path. See the note there for why the two are separate; a change
// to the qlmanage invocation, the timeout, or the temp-file handling probably
// belongs in both.
pub fn decode_via_quicklook(path: &Path, size: usize, tag: &str) -> Result<image::DynamicImage> {
    if !cfg!(target_os = "macos") {
        // Long explanation once per run; short reason per file. Callers already
        // prefix their own `skip <path>:`, so repeating the full paragraph (and
        // the path) for every video would bury the signal in a real library.
        videre_core::heic::warn_quicklook_unavailable_once();
        anyhow::bail!("needs macOS QuickLook (`qlmanage`)");
    }
    // `qlmanage -t` does not fail on a container with no video track, it
    // hangs; videre kills it at QLMANAGE_TIMEOUT and pays 20s per file, on
    // every run, because nothing marks the file permanently unembeddable.
    // Measured: 60s per `videre embed` and another 60s per `scan --similar`
    // on a real library with three audio-only Live Photo companions.
    //
    // Gated on extension so HEIC, which shares this function, is untouched.
    // Placed before the semaphore so a skipped file never holds a permit.
    let probe_ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase());
    if matches!(probe_ext.as_deref(), Some("mov") | Some("mp4"))
        && !videre_core::video_probe::has_video_track(path)
    {
        anyhow::bail!("no video track (audio-only file); skipped without calling QuickLook");
    }

    use videre_core::io_timeout::{wait_with_timeout, WaitOutcome};
    // A scratch directory of this call's own, removed when `scratch` drops, so
    // simultaneous decodes of one file cannot delete each other's output. See
    // `videre_core::heic::heic_via_quicklook`, which had the same shared,
    // path-named directory.
    let scratch = tempfile::Builder::new()
        .prefix(&format!("videre_embed_ql_{tag}_"))
        .tempdir()
        .context("create qlmanage temp dir")?;
    let out_dir = scratch.path();
    let target = (size * 2).to_string();
    let _permit = videre_core::heic::qlmanage_semaphore().acquire();
    let mut child = std::process::Command::new("qlmanage")
        .args(["-t", "-s", &target, "-o"])
        .arg(out_dir)
        .arg(path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .context("run qlmanage (requires macOS)")?;
    let outcome = wait_with_timeout(&mut child, QLMANAGE_TIMEOUT);
    anyhow::ensure!(
        outcome != WaitOutcome::TimedOut,
        "qlmanage timed out after {}s decoding {} (file may be unreachable - is its drive connected?)",
        QLMANAGE_TIMEOUT.as_secs(),
        path.display()
    );
    anyhow::ensure!(
        outcome == WaitOutcome::Success,
        "qlmanage failed for {}",
        path.display()
    );
    let file_name = path.file_name().context("path has no file name")?;
    let out_file = out_dir.join(format!("{}.png", file_name.to_string_lossy()));
    image::open(&out_file).with_context(|| format!("decode qlmanage output for {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use candle_core::Device;

    #[test]
    fn preprocess_produces_correct_shape_and_range() {
        let t = image_to_tensor(
            std::path::Path::new("tests/fixtures/red_2x2.png"),
            384,
            &Device::Cpu,
        )
        .unwrap();
        assert_eq!(t.dims(), &[3, 384, 384]);
        // SigLIP normalization maps [0,1] to [-1,1]; red pixel -> R channel ~ 1.0
        let flat: Vec<f32> = t.flatten_all().unwrap().to_vec1().unwrap();
        assert!(flat.iter().all(|v| *v >= -1.001 && *v <= 1.001));
        assert!((flat[0] - 1.0).abs() < 0.02); // first value is R channel of red image
    }

    #[test]
    fn preprocess_missing_file_is_err_not_panic() {
        let r = image_to_tensor(std::path::Path::new("/nonexistent.jpg"), 384, &Device::Cpu);
        let err = r.unwrap_err();
        assert_eq!(
            videre_core::error_kind::ErrorKind::in_chain(&err),
            Some(videre_core::error_kind::ErrorKind::SourceUnavailable),
            "a missing file is not a decode failure"
        );
    }

    #[test]
    fn preprocess_corrupt_file_is_a_decode_failure() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Çağla_bozuk.jpg");
        std::fs::write(&path, b"\xff\xd8 not really a jpeg").unwrap();
        let err = image_to_tensor(&path, 384, &Device::Cpu).unwrap_err();
        assert_eq!(
            videre_core::error_kind::ErrorKind::in_chain(&err),
            Some(videre_core::error_kind::ErrorKind::DecodeFailed)
        );
    }

    #[test]
    fn tagged_file_embeds_as_the_rotated_display_canvas() {
        // The o6 fixture is the untagged original with EXIF Orientation = 6
        // added by exiftool; the pixels are identical. Orientation 6 displays
        // as a 90-degree clockwise rotation, so the tensor of the tagged file
        // must equal the tensor of the original rotated 90 CW. Before the
        // orientation fix this failed: both decoded to the same raw canvas.
        let raw = image::open("tests/fixtures/ai-generated-couple.jpg").unwrap();
        let rotated = image::imageops::rotate90(&raw);
        let mut png = Vec::new();
        rotated
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let png_path = std::env::temp_dir().join("videre_o6_expected.png");
        std::fs::write(&png_path, &png).unwrap();

        let expected = image_to_tensor(&png_path, 64, &Device::Cpu).unwrap();
        let actual = image_to_tensor(
            std::path::Path::new("tests/fixtures/ai-generated-couple_o6.jpg"),
            64,
            &Device::Cpu,
        )
        .unwrap();
        let a: Vec<f32> = expected.flatten_all().unwrap().to_vec1().unwrap();
        let b: Vec<f32> = actual.flatten_all().unwrap().to_vec1().unwrap();
        let diff: f32 = a.iter().zip(&b).map(|(x, y)| (x - y).abs()).sum();
        assert!(
            diff < 1e-2,
            "tagged file must decode to its rotated display canvas, sum |diff| = {diff}"
        );
        std::fs::remove_file(&png_path).ok();
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn preprocess_extracts_a_frame_from_a_real_mp4() {
        let t = image_to_tensor(
            std::path::Path::new("tests/fixtures/red_1s.mp4"),
            384,
            &Device::Cpu,
        )
        .unwrap();
        assert_eq!(t.dims(), &[3, 384, 384]);
        // The fixture is a solid red frame; SigLIP normalization maps [0,1] to
        // [-1,1], so the R channel of the extracted frame should be ~1.0, same
        // assertion shape as the existing red_2x2.png test above.
        let flat: Vec<f32> = t.flatten_all().unwrap().to_vec1().unwrap();
        assert!(flat.iter().all(|v| *v >= -1.001 && *v <= 1.001));
        assert!((flat[0] - 1.0).abs() < 0.1); // video compression allows more slack than a lossless PNG
    }
}
