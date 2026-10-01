//! Brute-force scoring. Inputs are L2-normalized so dot product = cosine.

/// How a SigLIP model turns an image-text cosine into a match probability:
/// `sigmoid(cos * scale + bias)`, with the model's own learned `logit_scale`
/// (already exponentiated into `scale`) and `logit_bias`.
///
/// SigLIP is trained with a sigmoid loss over exactly this, per image-text
/// pair, so the result is a calibrated "does this caption fit this image"
/// probability: a fixed cutoff means the same thing for every query and
/// every model, which a cosine does not. The same cosine of 0.10 is a 23%
/// match on `siglip-base-patch16-224` and 0.4% on `siglip2-base-patch16-224`.
///
/// Image against image has no trained bias, so this is for text queries only.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Calibration {
    pub scale: f32,
    pub bias: f32,
}

impl Calibration {
    /// The match probability of one cosine, in `[0, 1]`, monotonic in `cos`,
    /// so ranking by it is ranking by the cosine.
    pub fn probability(&self, cos: f32) -> f32 {
        1.0 / (1.0 + (-(cos * self.scale + self.bias)).exp())
    }
}

pub fn top_k(query: &[f32], corpus: &[(String, Vec<f32>)], k: usize) -> Vec<(String, f32)> {
    let mut scored: Vec<(String, f32)> = corpus
        .iter()
        .filter(|(_, v)| v.len() == query.len())
        .filter_map(|(hash, v)| {
            let dot: f32 = query.iter().zip(v.iter()).map(|(a, b)| a * b).sum();
            dot.is_finite().then(|| (hash.clone(), dot))
        })
        .collect();
    scored.sort_by(|a, b| b.1.total_cmp(&a.1));
    scored.truncate(k);
    scored
}

#[cfg(test)]
mod calibration_tests {
    use super::*;

    /// The default model's constants, read from its weights 2026-10-01.
    const SIGLIP2_BASE_224: Calibration = Calibration {
        scale: 112.67,
        bias: -16.7717,
    };

    #[test]
    fn a_cosine_becomes_the_trained_match_probability() {
        let c = SIGLIP2_BASE_224;
        assert!((c.probability(0.0) - 1.0 / (1.0 + 16.7717f32.exp())).abs() < 1e-9);
        let half = -c.bias / c.scale;
        assert!((c.probability(half) - 0.5).abs() < 1e-6);
        // The measured 10% point.
        assert!((c.probability(0.1294) - 0.1).abs() < 0.005);
    }

    #[test]
    fn the_probability_keeps_the_ranking_order() {
        let c = SIGLIP2_BASE_224;
        let cosines = [-0.2f32, 0.0, 0.08, 0.12, 0.15, 0.3];
        let p: Vec<f32> = cosines.iter().map(|&x| c.probability(x)).collect();
        assert!(p.windows(2).all(|w| w[0] <= w[1]), "{p:?}");
        assert!(p.iter().all(|x| (0.0..=1.0).contains(x)), "{p:?}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn top_k_orders_by_score_descending() {
        let corpus = vec![
            ("a".to_string(), vec![1.0f32, 0.0]),
            ("b".to_string(), vec![0.0f32, 1.0]),
            ("c".to_string(), vec![0.7f32, 0.7]),
        ];
        let query = vec![1.0f32, 0.0];
        let hits = top_k(&query, &corpus, 2);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].0, "a");
        assert!((hits[0].1 - 1.0).abs() < 1e-6);
        assert_eq!(hits[1].0, "c");
    }

    #[test]
    fn top_k_handles_k_larger_than_corpus() {
        let corpus = vec![("a".to_string(), vec![1.0f32])];
        let hits = top_k(&[1.0], &corpus, 10);
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn top_k_skips_nan_scores() {
        let corpus = vec![
            ("nan".to_string(), vec![f32::NAN, 0.0]),
            ("good".to_string(), vec![1.0f32, 0.0]),
            ("other".to_string(), vec![0.0f32, 1.0]),
        ];
        let hits = top_k(&[1.0, 0.0], &corpus, 10);
        assert_eq!(hits.len(), 2);
        assert!(hits.iter().all(|(name, _)| name != "nan"));
        assert!(hits.iter().all(|(_, score)| score.is_finite()));
        assert_eq!(hits[0].0, "good");
    }

    #[test]
    fn top_k_skips_dimension_mismatch() {
        let corpus = vec![
            ("bad".to_string(), vec![1.0f32]), // wrong dims
            ("good".to_string(), vec![1.0f32, 0.0]),
        ];
        let hits = top_k(&[1.0, 0.0], &corpus, 10);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0, "good");
    }
}
