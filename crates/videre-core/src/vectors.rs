//! f32 <-> f16 BLOB conversion and L2 normalization for stored embeddings.
//! Storage format: little-endian f16, 2 bytes per dimension.

use half::f16;

pub fn to_f16_bytes(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 2);
    for &x in v {
        out.extend_from_slice(&f16::from_f32(x).to_le_bytes());
    }
    out
}

pub fn from_f16_bytes(bytes: &[u8]) -> Vec<f32> {
    debug_assert_eq!(bytes.len() % 2, 0, "f16 blob must have even length");
    bytes
        .chunks_exact(2)
        .map(|c| f16::from_le_bytes([c[0], c[1]]).to_f32())
        .collect()
}

pub fn l2_normalize(v: &mut [f32]) {
    debug_assert!(v.iter().all(|x| x.is_finite()), "vector must be finite");
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn f16_round_trip_preserves_values_within_tolerance() {
        let v = vec![0.1f32, -0.5, 0.999, 0.0];
        let bytes = to_f16_bytes(&v);
        assert_eq!(bytes.len(), 8);
        let back = from_f16_bytes(&bytes);
        for (a, b) in v.iter().zip(back.iter()) {
            assert!((a - b).abs() < 1e-3, "{a} vs {b}");
        }
    }

    #[test]
    fn l2_normalize_produces_unit_vector() {
        let mut v = vec![3.0f32, 4.0];
        l2_normalize(&mut v);
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-6);
        assert!((v[0] - 0.6).abs() < 1e-6);
    }

    #[test]
    fn l2_normalize_zero_vector_stays_zero() {
        let mut v = vec![0.0f32; 4];
        l2_normalize(&mut v);
        assert!(v.iter().all(|x| *x == 0.0));
    }

    #[test]
    fn realistic_scale_round_trip_preserves_dot_product() {
        // Pseudo-random 1152-dim vector via a simple LCG (no external deps).
        let mut state: u64 = 0x1234_5678_9abc_def0;
        let mut lcg = || {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            // Map top 24 bits to [-1.0, 1.0).
            ((state >> 40) as f32 / (1u32 << 23) as f32) - 1.0
        };
        let mut v: Vec<f32> = (0..1152).map(|_| lcg()).collect();
        l2_normalize(&mut v);

        let bytes = to_f16_bytes(&v);
        let back = from_f16_bytes(&bytes);

        let dot: f32 = v.iter().zip(back.iter()).map(|(a, b)| a * b).sum();
        assert!(dot > 0.999, "dot product after round trip: {dot}");
    }
}
