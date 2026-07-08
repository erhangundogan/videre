//! Decode an image file into a SigLIP input tensor: resize to NxN,
//! scale to [0,1], normalize with mean 0.5 / std 0.5 per channel -> [-1,1].
//! HEIC is converted to a temp JPEG via sips (macOS), matching dupe-report.

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

fn decode_heic(path: &Path, size: usize) -> Result<image::DynamicImage> {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut h);
    let tmp = std::env::temp_dir().join(format!("dupe_embed_{:016x}.jpg", h.finish()));
    let status = std::process::Command::new("sips")
        .args(["-s", "format", "jpeg", "--resampleHeightWidthMax"])
        .arg((size * 2).to_string())
        .arg(path)
        .args(["--out".as_ref() as &std::ffi::OsStr, tmp.as_os_str()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .context("run sips (HEIC decode requires macOS)")?;
    anyhow::ensure!(status.success(), "sips failed for {}", path.display());
    let img = image::open(&tmp).with_context(|| format!("decode sips output for {}", path.display()));
    let _ = std::fs::remove_file(&tmp);
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
