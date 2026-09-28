//! Decode an image file into a SigLIP input tensor: resize to NxN,
//! scale to [0,1], normalize with mean 0.5 / std 0.5 per channel -> [-1,1].
//! HEIC and video frames are converted via QuickLook (macOS) through
//! `videre_core::heic::decode_via_quicklook`.

use anyhow::Result;
use candle_core::{DType, Device, Tensor};
use std::path::Path;

pub fn image_to_tensor(path: &Path, size: usize, device: &Device) -> Result<Tensor> {
    to_tensor(&decode(path, size)?, size, device)
}

/// The photo as a person sees it, decoded for a `size`-pixel model input:
/// HEIC and video through QuickLook at twice that size, everything else
/// through the orientation-aware `image` decode at full resolution. Embed also
/// takes the near-duplicate fingerprint from this image, before any resize.
pub fn decode(path: &Path, size: usize) -> Result<image::DynamicImage> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .unwrap_or_default();

    let img = if ext == "heic" {
        videre_core::heic::decode_via_quicklook(path, "embed-heic", Some((size * 2) as u32))?
    } else if ext == "mov" || ext == "mp4" {
        videre_core::heic::decode_via_quicklook(path, "embed-video", Some((size * 2) as u32))?
    } else {
        let timeout_path = path.to_path_buf();
        videre_core::io_timeout::run_with_timeout(
            videre_core::io_timeout::DEFAULT_IO_TIMEOUT,
            move || {
                // Orientation-aware decode: the tensor must describe the photo as
                // a person sees it, not the sensor canvas (see
                // `videre_core::image_decode` for why HEIC never reaches here).
                videre_core::image_decode::decode_oriented_file(&timeout_path)
            },
        )
        .map_err(|e| {
            videre_core::error_kind::from_io(e.into_io_error())
                .context(format!("could not read {}", path.display()))
        })?
        .map_err(|e| {
            videre_core::error_kind::from_image(e).context(format!("decode {}", path.display()))
        })?
    };
    Ok(img)
}

/// A decoded image as a SigLIP input tensor: `size`x`size`, CHW, in [-1, 1].
pub fn to_tensor(img: &image::DynamicImage, size: usize, device: &Device) -> Result<Tensor> {
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

#[cfg(test)]
mod tests {
    #[test]
    #[cfg(target_os = "macos")]
    fn embed_source_dhash_stays_within_the_similarity_threshold_of_the_64px_one() {
        // Fingerprints stored by the old `scan --similar` path came from a 64px
        // QuickLook render; embed now hashes its own, larger render. They must
        // stay within dedupe's 10-bit threshold of each other, or keeping the
        // stored values would split existing near-duplicate groups.
        use videre_core::image_decode::dhash;
        let base = concat!(env!("CARGO_MANIFEST_DIR"), "/../videre/tests/fixtures");
        let heic = std::path::Path::new(base).join("content_key/tiny.heic");
        let video = std::path::Path::new(base).join("testsrc_1s.mp4");
        // The old scan renders: HEIC capped at 64px, video at 128px (its
        // double-the-64px-target convention).
        let old_heic =
            videre_core::heic::decode_via_quicklook(&heic, "test-old-heic", Some(64)).unwrap();
        let old_video =
            videre_core::heic::decode_via_quicklook(&video, "test-old-video", Some(128)).unwrap();
        for size in [224, 384] {
            for (name, path, old) in [("heic", &heic, &old_heic), ("video", &video, &old_video)] {
                let new = decode(path, size).unwrap();
                let bits = (dhash(old) ^ dhash(&new)).count_ones();
                assert!(bits <= 10, "{name} at {size}: {bits} bits apart");
            }
        }
    }

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
