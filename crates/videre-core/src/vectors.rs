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

/// Cosine similarity, normalizing as it goes.
///
/// Unlike the several private dot-product loops around the workspace
/// (`face_cluster::cosine_dist`, `pipeline::cosine_sim`, `search::top_k`),
/// this does **not** assume its inputs are already L2-normalized. Those
/// callers work on stored embeddings, which are normalized on write; this one
/// exists for comparing freshly computed vectors, where that guarantee does
/// not hold.
///
/// Returns 0.0 when either vector has zero norm, rather than a NaN from
/// dividing by zero. A zero-norm embedding is itself a symptom worth
/// surfacing (batch 128 produced 30 of them), and NaN would silently poison
/// any comparison downstream.
///
/// Returns 0.0 on a length mismatch rather than panicking or comparing a
/// prefix, since two different-width embeddings are not meaningfully similar.
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    dot / (na * nb)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosine_of_identical_vectors_is_one() {
        let v = vec![1.0, 2.0, 3.0];
        assert!((cosine(&v, &v) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_ignores_magnitude() {
        // The whole point of normalizing: a scaled copy is the same direction.
        let a = vec![1.0, 2.0, 3.0];
        let b: Vec<f32> = a.iter().map(|x| x * 7.5).collect();
        assert!((cosine(&a, &b) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_of_orthogonal_is_zero_and_opposite_is_minus_one() {
        assert!(cosine(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-6);
        assert!((cosine(&[1.0, 0.0], &[-1.0, 0.0]) + 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_with_a_zero_vector_is_zero_not_nan() {
        let c = cosine(&[0.0, 0.0], &[1.0, 2.0]);
        assert!(c.is_finite(), "must not be NaN");
        assert_eq!(c, 0.0);
    }

    #[test]
    fn cosine_of_mismatched_lengths_is_zero() {
        assert_eq!(cosine(&[1.0, 2.0], &[1.0, 2.0, 3.0]), 0.0);
    }

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
