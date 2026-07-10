use image::{DynamicImage, Rgb, RgbImage};

/// Canonical ArcFace 112x112 template landmarks (x, y).
const DST: [[f32; 2]; 5] = [
    [38.2946, 51.6963],
    [73.5318, 51.5014],
    [56.0252, 71.7366],
    [41.5493, 92.3655],
    [70.7299, 92.2041],
];

/// Warp src image so detected landmarks map to the 112x112 ArcFace template.
pub fn align_face(img: &DynamicImage, landmarks: &[[f32; 2]; 5]) -> RgbImage {
    let m = umeyama(landmarks, &DST);
    warp_affine(img, m, 112, 112)
}

/// Umeyama 2D similarity transform.
/// Returns 2x3 matrix M such that dst ≈ M * [src_x, src_y, 1]^T.
pub fn umeyama(src: &[[f32; 2]; 5], dst: &[[f32; 2]; 5]) -> [[f32; 3]; 2] {
    let n = src.len() as f32;

    let (mu_sx, mu_sy) = src.iter().fold((0.0f32, 0.0f32), |(ax, ay), p| (ax + p[0], ay + p[1]));
    let (mu_dx, mu_dy) = dst.iter().fold((0.0f32, 0.0f32), |(ax, ay), p| (ax + p[0], ay + p[1]));
    let (mu_sx, mu_sy) = (mu_sx / n, mu_sy / n);
    let (mu_dx, mu_dy) = (mu_dx / n, mu_dy / n);

    let var_s: f32 = src.iter().map(|p| (p[0] - mu_sx).powi(2) + (p[1] - mu_sy).powi(2)).sum::<f32>() / n;

    let mut cov = [[0.0f32; 2]; 2];
    for (s, d) in src.iter().zip(dst.iter()) {
        let ds = [s[0] - mu_sx, s[1] - mu_sy];
        let dd = [d[0] - mu_dx, d[1] - mu_dy];
        cov[0][0] += dd[0] * ds[0];
        cov[0][1] += dd[0] * ds[1];
        cov[1][0] += dd[1] * ds[0];
        cov[1][1] += dd[1] * ds[1];
    }
    cov[0][0] /= n; cov[0][1] /= n; cov[1][0] /= n; cov[1][1] /= n;

    let det = cov[0][0] * cov[1][1] - cov[0][1] * cov[1][0];
    let s_sign = if det >= 0.0 { 1.0f32 } else { -1.0 };

    // Closed-form 2D similarity via complex number: c = (trace + i*skew) / var_s
    let trace_cov = cov[0][0] + cov[1][1];
    let skew = cov[1][0] - cov[0][1];
    let scale = if var_s > 1e-8 {
        (trace_cov.powi(2) + skew.powi(2)).sqrt() * s_sign / var_s
    } else {
        1.0
    };

    let angle = skew.atan2(trace_cov);
    let (sin_a, cos_a) = angle.sin_cos();

    let tx = mu_dx - scale * (cos_a * mu_sx - sin_a * mu_sy);
    let ty = mu_dy - scale * (sin_a * mu_sx + cos_a * mu_sy);

    [
        [scale * cos_a, -scale * sin_a, tx],
        [scale * sin_a,  scale * cos_a, ty],
    ]
}

fn warp_affine(img: &DynamicImage, m: [[f32; 3]; 2], out_w: u32, out_h: u32) -> RgbImage {
    let rgb = img.to_rgb8();
    let mut out = RgbImage::new(out_w, out_h);

    // Invert M: for each output pixel, find the corresponding source pixel
    let det = m[0][0] * m[1][1] - m[0][1] * m[1][0];
    let inv = if det.abs() > 1e-8 {
        let inv_det = 1.0 / det;
        [
            [m[1][1] * inv_det, -m[0][1] * inv_det, (m[0][1]*m[1][2] - m[1][1]*m[0][2]) * inv_det],
            [-m[1][0] * inv_det, m[0][0] * inv_det, (m[1][0]*m[0][2] - m[0][0]*m[1][2]) * inv_det],
        ]
    } else {
        [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0]]
    };

    for dy in 0..out_h {
        for dx in 0..out_w {
            let sx = inv[0][0] * dx as f32 + inv[0][1] * dy as f32 + inv[0][2];
            let sy = inv[1][0] * dx as f32 + inv[1][1] * dy as f32 + inv[1][2];
            *out.get_pixel_mut(dx, dy) = bilinear(&rgb, sx, sy);
        }
    }
    out
}

fn bilinear(img: &RgbImage, x: f32, y: f32) -> Rgb<u8> {
    let (w, h) = img.dimensions();
    let x0 = x.floor() as i32;
    let y0 = y.floor() as i32;
    let fx = x - x0 as f32;
    let fy = y - y0 as f32;

    let get = |xi: i32, yi: i32| -> [f32; 3] {
        let xi = xi.clamp(0, w as i32 - 1) as u32;
        let yi = yi.clamp(0, h as i32 - 1) as u32;
        let p = img.get_pixel(xi, yi);
        [p[0] as f32, p[1] as f32, p[2] as f32]
    };

    let p00 = get(x0, y0);
    let p10 = get(x0 + 1, y0);
    let p01 = get(x0, y0 + 1);
    let p11 = get(x0 + 1, y0 + 1);

    let r = |i: usize| -> u8 {
        let v = p00[i] * (1.0 - fx) * (1.0 - fy)
              + p10[i] * fx * (1.0 - fy)
              + p01[i] * (1.0 - fx) * fy
              + p11[i] * fx * fy;
        v.round().clamp(0.0, 255.0) as u8
    };
    Rgb([r(0), r(1), r(2)])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn umeyama_identity_when_src_equals_dst() {
        let m = umeyama(&DST, &DST);
        assert!((m[0][0] - 1.0).abs() < 1e-3, "m00={}", m[0][0]);
        assert!((m[1][1] - 1.0).abs() < 1e-3, "m11={}", m[1][1]);
        assert!(m[0][1].abs() < 1e-3);
        assert!(m[1][0].abs() < 1e-3);
        assert!(m[0][2].abs() < 1e-3, "tx={}", m[0][2]);
        assert!(m[1][2].abs() < 1e-3, "ty={}", m[1][2]);
    }

    #[test]
    fn umeyama_pure_translation() {
        let src: [[f32; 2]; 5] = [[0.0,0.0],[10.0,0.0],[5.0,5.0],[2.0,9.0],[8.0,9.0]];
        let mut dst = src;
        for p in dst.iter_mut() { p[0] += 20.0; p[1] += 30.0; }
        let m = umeyama(&src, &dst);
        assert!((m[0][0] - 1.0).abs() < 0.01, "scale_x={}", m[0][0]);
        assert!((m[0][2] - 20.0).abs() < 0.5, "tx={}", m[0][2]);
        assert!((m[1][2] - 30.0).abs() < 0.5, "ty={}", m[1][2]);
    }

    #[test]
    fn align_face_returns_112x112() {
        let img = DynamicImage::new_rgb8(200, 200);
        let lm: [[f32; 2]; 5] = [[40.0,60.0],[80.0,60.0],[60.0,80.0],[45.0,100.0],[75.0,100.0]];
        let out = align_face(&img, &lm);
        assert_eq!(out.width(), 112);
        assert_eq!(out.height(), 112);
    }
}
