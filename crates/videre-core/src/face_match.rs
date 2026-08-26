//! Bounding-box geometry for matching XMP face regions to detected faces.
//! Pure: no I/O, no database, so the matching rule is tested in isolation.

/// A top-left-origin pixel box, the common space both a stored face bbox and a
/// denormalized MWG region are converted into before comparison.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PixelBox {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// Intersection-over-union of two boxes. 0.0 when they do not overlap or when
/// either has zero (or negative) area.
pub fn iou(a: &PixelBox, b: &PixelBox) -> f64 {
    let ix = a.x.max(b.x);
    let iy = a.y.max(b.y);
    let ix2 = (a.x + a.w).min(b.x + b.w);
    let iy2 = (a.y + a.h).min(b.y + b.h);
    let iw = (ix2 - ix).max(0.0);
    let ih = (iy2 - iy).max(0.0);
    let inter = iw * ih;
    let union = a.w * a.h + b.w * b.h - inter;
    if union <= 0.0 {
        0.0
    } else {
        inter / union
    }
}

/// For each region (by index), the id of the detected face it matches, or None.
/// Greedy: consider all (region, face) pairs with IoU >= `threshold`, take them
/// highest IoU first, and never reuse a region or a face. Deterministic: ties
/// break by region index then face id.
pub fn greedy_match(
    regions: &[PixelBox],
    faces: &[(i64, PixelBox)],
    threshold: f64,
) -> Vec<Option<i64>> {
    // (iou, region_idx, face_id, face_idx)
    let mut pairs: Vec<(f64, usize, i64, usize)> = Vec::new();
    for (ri, r) in regions.iter().enumerate() {
        for (fi, (fid, fb)) in faces.iter().enumerate() {
            let s = iou(r, fb);
            if s >= threshold {
                pairs.push((s, ri, *fid, fi));
            }
        }
    }
    // Highest IoU first; deterministic tie-break by region index then face id.
    pairs.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.1.cmp(&b.1))
            .then(a.2.cmp(&b.2))
    });
    let mut out = vec![None; regions.len()];
    let mut used_face = vec![false; faces.len()];
    for (_s, ri, fid, fi) in pairs {
        if out[ri].is_none() && !used_face[fi] {
            out[ri] = Some(fid);
            used_face[fi] = true;
        }
    }
    out
}

/// The default IoU acceptance threshold. Tuneable; 0.5 is the usual face-match
/// floor and can be revisited against real digiKam/Lightroom exports.
pub const DEFAULT_IOU_THRESHOLD: f64 = 0.5;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iou_of_identical_boxes_is_one() {
        let a = PixelBox {
            x: 10.0,
            y: 10.0,
            w: 100.0,
            h: 100.0,
        };
        assert!((iou(&a, &a) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn iou_of_disjoint_boxes_is_zero() {
        let a = PixelBox {
            x: 0.0,
            y: 0.0,
            w: 10.0,
            h: 10.0,
        };
        let b = PixelBox {
            x: 100.0,
            y: 100.0,
            w: 10.0,
            h: 10.0,
        };
        assert_eq!(iou(&a, &b), 0.0);
    }

    #[test]
    fn iou_of_half_overlap() {
        // Two 10x10 boxes overlapping in a 10x5 strip: inter=50, union=150.
        let a = PixelBox {
            x: 0.0,
            y: 0.0,
            w: 10.0,
            h: 10.0,
        };
        let b = PixelBox {
            x: 0.0,
            y: 5.0,
            w: 10.0,
            h: 10.0,
        };
        assert!((iou(&a, &b) - (50.0 / 150.0)).abs() < 1e-6);
    }

    #[test]
    fn greedy_matches_best_first_one_to_one() {
        let regions = vec![
            PixelBox {
                x: 0.0,
                y: 0.0,
                w: 10.0,
                h: 10.0,
            },
            PixelBox {
                x: 100.0,
                y: 100.0,
                w: 10.0,
                h: 10.0,
            },
        ];
        let faces = vec![
            (
                7i64,
                PixelBox {
                    x: 1.0,
                    y: 1.0,
                    w: 10.0,
                    h: 10.0,
                },
            ),
            (
                9i64,
                PixelBox {
                    x: 101.0,
                    y: 101.0,
                    w: 10.0,
                    h: 10.0,
                },
            ),
        ];
        let m = greedy_match(&regions, &faces, 0.3);
        assert_eq!(m, vec![Some(7), Some(9)]);
    }

    #[test]
    fn below_threshold_is_unmatched() {
        let regions = vec![PixelBox {
            x: 0.0,
            y: 0.0,
            w: 10.0,
            h: 10.0,
        }];
        let faces = vec![(
            7i64,
            PixelBox {
                x: 50.0,
                y: 50.0,
                w: 10.0,
                h: 10.0,
            },
        )];
        assert_eq!(greedy_match(&regions, &faces, 0.5), vec![None]);
    }
}
