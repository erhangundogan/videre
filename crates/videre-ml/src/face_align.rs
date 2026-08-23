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

/// How far a detected 5-point landmark set is from being a face, in template
/// pixels: the RMS residual after the best similarity fit onto `DST`.
///
/// :warning: **This is the question `--max-generic-sim` was reaching for and
/// could not ask.** ArcFace never sees the photo, only the 112x112 crop warped
/// so these five points land on the template. When the points are not really a
/// face, the warp produces a mangled image and the embedding encodes the
/// mangling rather than the person - so mangled faces resemble *each other* and
/// collect into their own cluster, while a correctly aligned face of the same
/// person sits somewhere else entirely.
///
/// Measured on a 92-face corpus with per-cluster ground truth: clusters the user
/// confirmed correct had a median residual of 2.5-6.2, the two they confirmed
/// wrong had 9.8 and 10.1, with one face at 33.0.
///
/// Unlike a cosine against the population mean, this is a property of one face.
/// It does not move when the library's composition changes, which is why the
/// threshold can transfer between libraries at all.
pub fn landmark_residual(landmarks: &[[f32; 2]; 5]) -> f32 {
    let m = umeyama(landmarks, &DST);
    let mut sum = 0.0f32;
    for (s, d) in landmarks.iter().zip(DST.iter()) {
        let x = m[0][0] * s[0] + m[0][1] * s[1] + m[0][2];
        let y = m[1][0] * s[0] + m[1][1] * s[1] + m[1][2];
        sum += (x - d[0]).powi(2) + (y - d[1]).powi(2);
    }
    (sum / landmarks.len() as f32).sqrt()
}

/// Parse the stored `"x1,y1,...,x5,y5"` landmark string.
pub fn parse_landmarks(s: &str) -> Option<[[f32; 2]; 5]> {
    let n: Vec<f32> = s.split(',').filter_map(|v| v.trim().parse().ok()).collect();
    if n.len() < 10 {
        return None;
    }
    let mut lm = [[0.0f32; 2]; 5];
    for (p, slot) in lm.iter_mut().enumerate() {
        *slot = [n[p * 2], n[p * 2 + 1]];
    }
    Some(lm)
}

/// Umeyama 2D similarity transform.
/// Returns 2x3 matrix M such that dst ≈ M * [src_x, src_y, 1]^T.
pub fn umeyama(src: &[[f32; 2]; 5], dst: &[[f32; 2]; 5]) -> [[f32; 3]; 2] {
    let n = src.len() as f32;

    let (mu_sx, mu_sy) = src
        .iter()
        .fold((0.0f32, 0.0f32), |(ax, ay), p| (ax + p[0], ay + p[1]));
    let (mu_dx, mu_dy) = dst
        .iter()
        .fold((0.0f32, 0.0f32), |(ax, ay), p| (ax + p[0], ay + p[1]));
    let (mu_sx, mu_sy) = (mu_sx / n, mu_sy / n);
    let (mu_dx, mu_dy) = (mu_dx / n, mu_dy / n);

    let var_s: f32 = src
        .iter()
        .map(|p| (p[0] - mu_sx).powi(2) + (p[1] - mu_sy).powi(2))
        .sum::<f32>()
        / n;

    let mut cov = [[0.0f32; 2]; 2];
    for (s, d) in src.iter().zip(dst.iter()) {
        let ds = [s[0] - mu_sx, s[1] - mu_sy];
        let dd = [d[0] - mu_dx, d[1] - mu_dy];
        cov[0][0] += dd[0] * ds[0];
        cov[0][1] += dd[0] * ds[1];
        cov[1][0] += dd[1] * ds[0];
        cov[1][1] += dd[1] * ds[1];
    }
    cov[0][0] /= n;
    cov[0][1] /= n;
    cov[1][0] /= n;
    cov[1][1] /= n;

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
        [scale * sin_a, scale * cos_a, ty],
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
            [
                m[1][1] * inv_det,
                -m[0][1] * inv_det,
                (m[0][1] * m[1][2] - m[1][1] * m[0][2]) * inv_det,
            ],
            [
                -m[1][0] * inv_det,
                m[0][0] * inv_det,
                (m[1][0] * m[0][2] - m[0][0] * m[1][2]) * inv_det,
            ],
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
        let src: [[f32; 2]; 5] = [[0.0, 0.0], [10.0, 0.0], [5.0, 5.0], [2.0, 9.0], [8.0, 9.0]];
        let mut dst = src;
        for p in dst.iter_mut() {
            p[0] += 20.0;
            p[1] += 30.0;
        }
        let m = umeyama(&src, &dst);
        assert!((m[0][0] - 1.0).abs() < 0.01, "scale_x={}", m[0][0]);
        assert!((m[0][2] - 20.0).abs() < 0.5, "tx={}", m[0][2]);
        assert!((m[1][2] - 30.0).abs() < 0.5, "ty={}", m[1][2]);
    }

    #[test]
    fn align_face_returns_112x112() {
        let img = DynamicImage::new_rgb8(200, 200);
        let lm: [[f32; 2]; 5] = [
            [40.0, 60.0],
            [80.0, 60.0],
            [60.0, 80.0],
            [45.0, 100.0],
            [75.0, 100.0],
        ];
        let out = align_face(&img, &lm);
        assert_eq!(out.width(), 112);
        assert_eq!(out.height(), 112);
    }
}

#[cfg(test)]
mod residual_tests {
    use super::*;

    /// The template fits itself perfectly, and a rotated, scaled, translated
    /// copy of it fits just as well: the residual measures *shape*, not pose.
    /// That is the property the gate depends on, since a tilted face is still a
    /// face and must not be held out for being tilted.
    #[test]
    fn residual_is_zero_for_a_face_shape_at_any_pose() {
        assert!(landmark_residual(&DST) < 1e-3);

        for deg in [15.0f32, 45.0, 90.0, 200.0] {
            let (s, c) = deg.to_radians().sin_cos();
            let mut moved = [[0.0f32; 2]; 5];
            for (i, p) in DST.iter().enumerate() {
                // scale 3.7 and an arbitrary offset, so only shape is left
                moved[i] = [
                    3.7 * (c * p[0] - s * p[1]) + 900.0,
                    3.7 * (s * p[0] + c * p[1]) - 40.0,
                ];
            }
            let r = landmark_residual(&moved);
            assert!(r < 1e-2, "a rotated face scored {r} at {deg} degrees");
        }
    }

    /// Points that are not a face score high. This is what separated the two
    /// clusters a user confirmed wrong from the three they confirmed correct.
    #[test]
    fn residual_is_large_when_the_points_are_not_a_face() {
        // Nose left of both eyes, mouth above them: no similarity transform
        // makes this a face.
        let scrambled = [
            [38.0, 52.0],
            [73.0, 52.0],
            [10.0, 40.0],
            [20.0, 20.0],
            [60.0, 15.0],
        ];
        assert!(
            landmark_residual(&scrambled) > 7.0,
            "scrambled points scored {}",
            landmark_residual(&scrambled)
        );
    }

    #[test]
    fn parse_landmarks_reads_the_stored_form() {
        let lm = parse_landmarks("1.0,2.0,3.0,4.0,5.0,6.0,7.0,8.0,9.0,10.0").unwrap();
        assert_eq!(lm[0], [1.0, 2.0]);
        assert_eq!(lm[4], [9.0, 10.0]);
        assert!(
            parse_landmarks("1.0,2.0").is_none(),
            "short input is not a face"
        );
        assert!(parse_landmarks("").is_none());
    }
}

/// How sharp an aligned crop is: the variance of its Laplacian.
///
/// :warning: **Measured on the aligned 112x112 crop, not the original photo.**
/// That is the image the model is actually given, so it is the one whose
/// sharpness decides whether the embedding means anything. A sharp photo
/// containing a tiny face produces a blurry crop once upscaled, and it is the
/// crop that matters.
///
/// Scale-free in the sense that matters here: it is a property of one image,
/// not of the library, so a threshold on it transfers between libraries in a way
/// that a cosine against the population mean never could.
pub fn blur_score(img: &RgbImage) -> f32 {
    let (w, h) = img.dimensions();
    if w < 3 || h < 3 {
        return 0.0;
    }
    let gray: Vec<f32> = img
        .pixels()
        .map(|p| 0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32)
        .collect();
    let at = |x: u32, y: u32| gray[(y * w + x) as usize];
    let mut vals = Vec::with_capacity(((w - 2) * (h - 2)) as usize);
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            // 4-neighbour Laplacian
            vals.push(at(x - 1, y) + at(x + 1, y) + at(x, y - 1) + at(x, y + 1) - 4.0 * at(x, y));
        }
    }
    let n = vals.len() as f32;
    let mean = vals.iter().sum::<f32>() / n;
    vals.iter().map(|v| (v - mean).powi(2)).sum::<f32>() / n
}
