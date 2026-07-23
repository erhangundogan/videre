//! ArcFace w600k_r50 embedding wrapper for ort 2.0.0-rc.12.
//!
//! Takes aligned 112x112 face crops and returns L2-normalized 512-dim embeddings.
//! The model expects `[N, 3, 112, 112]` float32 input normalized by `(pixel/255.0 - 0.5) / 0.5`.

use anyhow::{Context, Result};
use image::RgbImage;
use ndarray::Array4;
use ort::{session::Session, value::TensorRef};
use std::path::Path;

/// Wraps an ONNX ArcFace session for face embedding.
pub struct FaceEmbedder {
    session: Session,
}

impl FaceEmbedder {
    /// Load an ArcFace ONNX model from `model_path`.
    ///
    /// Uses `Session::builder().commit_from_file()` as per ort 2.0.0-rc.12.
    pub fn new(model_path: &Path) -> Result<Self> {
        let session = crate::face_models::build_session(model_path)
            .context("load ArcFace ONNX model")?;
        Ok(Self { session })
    }

    /// Embed a batch of 112x112 aligned face crops.
    ///
    /// Returns L2-normalized 512-dim f32 vectors, one per input face.
    pub fn embed_batch(&mut self, faces: &[RgbImage]) -> Result<Vec<Vec<f32>>> {
        if faces.is_empty() {
            return Ok(Vec::new());
        }
        let n = faces.len();
        let tensor = preprocess_batch(faces);
        let outputs = self
            .session
            .run(ort::inputs![TensorRef::from_array_view(tensor.view())?])
            .context("ArcFace inference")?;

        // First output is [N, 512] embeddings.
        let flat: Vec<f32> = outputs[0]
            .try_extract_array::<f32>()
            .context("extract embedding tensor")?
            .iter()
            .cloned()
            .collect();

        let mut result = Vec::with_capacity(n);
        for i in 0..n {
            let mut emb: Vec<f32> = flat[i * 512..(i + 1) * 512].to_vec();
            l2_normalize(&mut emb);
            result.push(emb);
        }
        Ok(result)
    }
}

/// Normalize a 512-dim vector to unit length in-place.
pub fn l2_normalize(v: &mut Vec<f32>) {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 1e-8 {
        v.iter_mut().for_each(|x| *x /= norm);
    }
}

/// Pack RgbImage batch into NCHW float tensor, normalize to [-1, 1].
pub fn preprocess_batch(faces: &[RgbImage]) -> Array4<f32> {
    let n = faces.len();
    let mut tensor = Array4::<f32>::zeros([n, 3, 112, 112]);
    for (i, img) in faces.iter().enumerate() {
        for (x, y, pix) in img.enumerate_pixels() {
            tensor[[i, 0, y as usize, x as usize]] = (pix[0] as f32 / 255.0 - 0.5) / 0.5;
            tensor[[i, 1, y as usize, x as usize]] = (pix[1] as f32 / 255.0 - 0.5) / 0.5;
            tensor[[i, 2, y as usize, x as usize]] = (pix[2] as f32 / 255.0 - 0.5) / 0.5;
        }
    }
    tensor
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn l2_normalize_unit_vector_unchanged() {
        let mut v = vec![1.0f32, 0.0, 0.0];
        l2_normalize(&mut v);
        assert!((v[0] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn l2_normalize_produces_unit_length() {
        let mut v = vec![3.0f32, 4.0, 0.0];
        l2_normalize(&mut v);
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-6);
    }

    #[test]
    fn l2_normalize_zero_vector_safe() {
        let mut v = vec![0.0f32; 512];
        l2_normalize(&mut v);
    }

    #[test]
    fn preprocess_batch_shape() {
        let imgs = vec![RgbImage::new(112, 112), RgbImage::new(112, 112)];
        let t = preprocess_batch(&imgs);
        assert_eq!(t.shape(), &[2, 3, 112, 112]);
    }

    #[test]
    fn preprocess_batch_white_pixel_maps_to_one() {
        let mut img = RgbImage::new(112, 112);
        for p in img.pixels_mut() {
            *p = image::Rgb([255, 255, 255]);
        }
        let t = preprocess_batch(&[img]);
        // (255/255 - 0.5) / 0.5 = 1.0
        assert!((t[[0, 0, 0, 0]] - 1.0).abs() < 1e-5);
    }
}
