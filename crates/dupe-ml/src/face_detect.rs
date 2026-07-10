//! SCRFD face detector wrapper for ort 2.0.0-rc.12.
//!
//! Loads a SCRFD-10GF ONNX model and runs face detection on a single image.
//! The model expects 640x640 BGR float32 input normalised by `(pixel - 127.5) / 128.0`
//! and produces 9 output tensors (score/bbox/kps for strides 8, 16, 32).

use anyhow::{Context, Result};
use image::DynamicImage;
use ndarray::Array4;
use ort::{session::Session, value::TensorRef};
use std::path::Path;

const INPUT_SIZE: u32 = 640;
const CONF_THRESHOLD: f32 = 0.5;
const NMS_THRESHOLD: f32 = 0.4;
const STRIDES: [u32; 3] = [8, 16, 32];
const ANCHORS_PER_CELL: usize = 2;

/// A detected face with bounding box, confidence score, and 5 facial landmarks.
#[derive(Debug, Clone)]
pub struct Detection {
    /// Bounding box in original-image coordinates: [x1, y1, x2, y2].
    pub bbox: [f32; 4],
    /// Confidence score (already sigmoid-activated by the model).
    pub score: f32,
    /// 5 facial landmarks (left-eye, right-eye, nose, left-mouth, right-mouth)
    /// in original-image coordinates: `[[x, y]; 5]`.
    pub landmarks: [[f32; 2]; 5],
}

/// Wraps an ONNX SCRFD session for face detection.
pub struct FaceDetector {
    session: Session,
}

impl FaceDetector {
    /// Load a SCRFD ONNX model from `model_path`.
    ///
    /// Uses `Session::builder().commit_from_file()` as per ort 2.0.0-rc.12.
    pub fn new(model_path: &Path) -> Result<Self> {
        let session = Session::builder()
            .context("create ort SessionBuilder")?
            .commit_from_file(model_path)
            .context("load SCRFD ONNX model")?;
        Ok(Self { session })
    }

    /// Run face detection on `img`.
    ///
    /// Returns detected faces after NMS, in descending confidence order.
    pub fn detect(&mut self, img: &DynamicImage) -> Result<Vec<Detection>> {
        let orig_w = img.width();
        let orig_h = img.height();
        let input_tensor = preprocess(img);
        let outputs = self
            .session
            .run(ort::inputs![TensorRef::from_array_view(input_tensor.view())?])
            .context("SCRFD inference")?;
        postprocess(&outputs, orig_w, orig_h)
    }
}

/// Resize `img` to 640x640 and convert to a CHW BGR float32 tensor normalised
/// by `(pixel - 127.5) / 128.0`.
fn preprocess(img: &DynamicImage) -> Array4<f32> {
    let resized = img.resize_exact(INPUT_SIZE, INPUT_SIZE, image::imageops::FilterType::Triangle);
    let rgb = resized.to_rgb8();
    let mut tensor = Array4::<f32>::zeros([1, 3, INPUT_SIZE as usize, INPUT_SIZE as usize]);
    for (x, y, pix) in rgb.enumerate_pixels() {
        // SCRFD expects BGR channel order.
        tensor[[0, 0, y as usize, x as usize]] = (pix[2] as f32 - 127.5) / 128.0; // B
        tensor[[0, 1, y as usize, x as usize]] = (pix[1] as f32 - 127.5) / 128.0; // G
        tensor[[0, 2, y as usize, x as usize]] = (pix[0] as f32 - 127.5) / 128.0; // R
    }
    tensor
}

/// Decode SCRFD outputs into [`Detection`]s and apply NMS.
///
/// Output tensor layout (9 tensors total):
/// `[score_8, bbox_8, kps_8, score_16, bbox_16, kps_16, score_32, bbox_32, kps_32]`
///
/// For stride S, grid = 640/S, n = grid^2 * 2 (2 anchors per cell).
/// - score: [1, n, 1]
/// - bbox:  [1, n, 4]  — offsets in stride units from the anchor centre
/// - kps:   [1, n, 10] — 5-point landmark offsets in stride units
fn postprocess(
    outputs: &ort::session::SessionOutputs<'_>,
    orig_w: u32,
    orig_h: u32,
) -> Result<Vec<Detection>> {
    let scale_x = orig_w as f32 / INPUT_SIZE as f32;
    let scale_y = orig_h as f32 / INPUT_SIZE as f32;
    let mut detections: Vec<Detection> = Vec::new();

    for (stride_idx, &stride) in STRIDES.iter().enumerate() {
        let grid = (INPUT_SIZE / stride) as usize;
        let n = grid * grid * ANCHORS_PER_CELL;
        let base = stride_idx * 3;

        // Extract output tensors by index and collect into flat Vecs for easy indexing.
        // try_extract_array::<f32>() is available because ort's "ndarray" feature is on by default.
        let scores: Vec<f32> = outputs[base]
            .try_extract_array::<f32>()
            .context("extract score tensor")?
            .iter()
            .cloned()
            .collect();
        let bboxes: Vec<f32> = outputs[base + 1]
            .try_extract_array::<f32>()
            .context("extract bbox tensor")?
            .iter()
            .cloned()
            .collect();
        let kps: Vec<f32> = outputs[base + 2]
            .try_extract_array::<f32>()
            .context("extract kps tensor")?
            .iter()
            .cloned()
            .collect();

        for anchor_idx in 0..n {
            // scores is flattened from [1, n, 1] -> n elements
            let score = scores[anchor_idx];
            if score < CONF_THRESHOLD {
                continue;
            }

            // Anchor centre in the 640x640 input space
            let grid_y = (anchor_idx / ANCHORS_PER_CELL) / grid;
            let grid_x = (anchor_idx / ANCHORS_PER_CELL) % grid;
            let cx = (grid_x as f32 + 0.5) * stride as f32;
            let cy = (grid_y as f32 + 0.5) * stride as f32;

            // Decode bounding box (bboxes flattened from [1, n, 4])
            let bx = anchor_idx * 4;
            let x1 = (cx - bboxes[bx] * stride as f32) * scale_x;
            let y1 = (cy - bboxes[bx + 1] * stride as f32) * scale_y;
            let x2 = (cx + bboxes[bx + 2] * stride as f32) * scale_x;
            let y2 = (cy + bboxes[bx + 3] * stride as f32) * scale_y;

            // Decode 5-point landmarks (kps flattened from [1, n, 10])
            let kx = anchor_idx * 10;
            let mut landmarks = [[0.0f32; 2]; 5];
            for p in 0..5 {
                landmarks[p][0] = (cx + kps[kx + p * 2] * stride as f32) * scale_x;
                landmarks[p][1] = (cy + kps[kx + p * 2 + 1] * stride as f32) * scale_y;
            }

            detections.push(Detection {
                bbox: [x1, y1, x2, y2],
                score,
                landmarks,
            });
        }
    }

    Ok(nms(detections, NMS_THRESHOLD))
}

/// Apply greedy non-maximum suppression.
///
/// Sorts by confidence descending and suppresses boxes with IoU > `iou_thresh`.
pub fn nms(mut dets: Vec<Detection>, iou_thresh: f32) -> Vec<Detection> {
    dets.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap());
    let mut keep = Vec::new();
    let mut suppressed = vec![false; dets.len()];
    for i in 0..dets.len() {
        if suppressed[i] {
            continue;
        }
        keep.push(dets[i].clone());
        for j in (i + 1)..dets.len() {
            if iou(&dets[i].bbox, &dets[j].bbox) > iou_thresh {
                suppressed[j] = true;
            }
        }
    }
    keep
}

/// Intersection-over-union for two axis-aligned boxes `[x1, y1, x2, y2]`.
pub fn iou(a: &[f32; 4], b: &[f32; 4]) -> f32 {
    let ix1 = a[0].max(b[0]);
    let iy1 = a[1].max(b[1]);
    let ix2 = a[2].min(b[2]);
    let iy2 = a[3].min(b[3]);
    let inter = (ix2 - ix1).max(0.0) * (iy2 - iy1).max(0.0);
    let area_a = (a[2] - a[0]) * (a[3] - a[1]);
    let area_b = (b[2] - b[0]) * (b[3] - b[1]);
    inter / (area_a + area_b - inter)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iou_identical_boxes() {
        assert!((iou(&[0.0, 0.0, 10.0, 10.0], &[0.0, 0.0, 10.0, 10.0]) - 1.0).abs() < 1e-5);
    }

    #[test]
    fn iou_non_overlapping() {
        assert_eq!(iou(&[0.0, 0.0, 5.0, 5.0], &[10.0, 10.0, 20.0, 20.0]), 0.0);
    }

    #[test]
    fn nms_removes_duplicate() {
        let d1 = Detection { bbox: [0.0, 0.0, 10.0, 10.0], score: 0.9, landmarks: [[0.0; 2]; 5] };
        let d2 = Detection { bbox: [1.0, 1.0, 11.0, 11.0], score: 0.8, landmarks: [[0.0; 2]; 5] };
        let result = nms(vec![d1, d2], 0.4);
        assert_eq!(result.len(), 1);
        assert!((result[0].score - 0.9).abs() < 1e-5);
    }

    #[test]
    fn nms_keeps_non_overlapping() {
        let d1 = Detection { bbox: [0.0, 0.0, 5.0, 5.0], score: 0.9, landmarks: [[0.0; 2]; 5] };
        let d2 = Detection {
            bbox: [100.0, 100.0, 110.0, 110.0],
            score: 0.8,
            landmarks: [[0.0; 2]; 5],
        };
        let result = nms(vec![d1, d2], 0.4);
        assert_eq!(result.len(), 2);
    }
}
