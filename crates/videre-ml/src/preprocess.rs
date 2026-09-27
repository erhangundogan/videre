//! Decode an image file into a SigLIP input tensor: resize to NxN,
//! scale to [0,1], normalize with mean 0.5 / std 0.5 per channel -> [-1,1].
//! HEIC and video frames are converted via QuickLook (macOS) through
//! `videre_core::heic::decode_via_quicklook`.

use anyhow::Result;
use candle_core::{DType, Device, Tensor};
use std::path::Path;

pub fn image_to_tensor(path: &Path, size: usize, device: &Device) -> Result<Tensor> {
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
