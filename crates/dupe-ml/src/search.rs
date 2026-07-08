//! Brute-force scoring. Inputs are L2-normalized so dot product = cosine.

pub fn top_k(query: &[f32], corpus: &[(String, Vec<f32>)], k: usize) -> Vec<(String, f32)> {
    let mut scored: Vec<(String, f32)> = corpus
        .iter()
        .filter(|(_, v)| v.len() == query.len())
        .map(|(hash, v)| {
            let dot: f32 = query.iter().zip(v.iter()).map(|(a, b)| a * b).sum();
            (hash.clone(), dot)
        })
        .collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(k);
    scored
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
    fn top_k_skips_dimension_mismatch() {
        let corpus = vec![
            ("bad".to_string(), vec![1.0f32]),          // wrong dims
            ("good".to_string(), vec![1.0f32, 0.0]),
        ];
        let hits = top_k(&[1.0, 0.0], &corpus, 10);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0, "good");
    }
}
