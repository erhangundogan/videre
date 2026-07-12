//! Decode an image file into a SigLIP input tensor: resize to NxN,
//! scale to [0,1], normalize with mean 0.5 / std 0.5 per channel -> [-1,1].
//! HEIC is converted via QuickLook (macOS), matching dupe-report and
//! dupe-faces - see `decode_heic` for why `sips` alone isn't used.

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
        decode_heic(path, size)?
    } else {
        image::open(path).with_context(|| format!("decode {}", path.display()))?
    };

    let img = img
        .resize_exact(size as u32, size as u32, image::imageops::FilterType::Triangle)
        .to_rgb8();

    let data: Vec<f32> = img.into_raw().iter().map(|&b| b as f32 / 255.0).collect();
    // HWC -> CHW, then (x - 0.5) / 0.5
    let t = Tensor::from_vec(data, (size, size, 3), device)?
        .permute((2, 0, 1))?
        .to_dtype(DType::F32)?;
    let t = ((t - 0.5)? / 0.5)?;
    Ok(t)
}

/// Convert a HEIC file to a `DynamicImage` via QuickLook (`qlmanage -t`).
///
/// `sips -s format jpeg` copies the raw sensor-buffer pixels unrotated for
/// HEIC files where the iPhone camera encoded rotation via the HEIF `irot`
/// transform box rather than a classic EXIF Orientation tag - the same
/// rotation Finder/Preview/Photos apply via QuickLook. Since the resulting
/// tensor is immediately resized square anyway, exact resolution doesn't
/// matter here as much as getting the orientation right so the embedding
/// represents the photo as a person actually sees it.
fn decode_heic(path: &Path, size: usize) -> Result<image::DynamicImage> {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut h);
    let out_dir = std::env::temp_dir().join(format!("dupe_embed_ql_{:016x}", h.finish()));
    let _ = std::fs::remove_dir_all(&out_dir);
    std::fs::create_dir_all(&out_dir).context("create qlmanage temp dir")?;
    let target = (size * 2).to_string();
    let status = std::process::Command::new("qlmanage")
        .args(["-t", "-s", &target, "-o"])
        .arg(&out_dir)
        .arg(path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .context("run qlmanage (HEIC decode requires macOS)")?;
    anyhow::ensure!(status.success(), "qlmanage failed for {}", path.display());
    let file_name = path.file_name().context("path has no file name")?;
    let out_file = out_dir.join(format!("{}.png", file_name.to_string_lossy()));
    let img = image::open(&out_file)
        .with_context(|| format!("decode qlmanage output for {}", path.display()));
    let _ = std::fs::remove_dir_all(&out_dir);
    img
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
        assert!(r.is_err());
    }
}
