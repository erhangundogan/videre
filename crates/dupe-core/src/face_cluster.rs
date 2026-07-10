/// DBSCAN on L2-normalized embeddings using cosine distance.
/// Returns Vec<(face_id, cluster_id)> where cluster_id=None means outlier.
pub fn dbscan_cosine(
    points: &[(i64, Vec<f32>)],
    eps: f32,
    min_samples: usize,
) -> Vec<(i64, Option<i64>)> {
    let n = points.len();
    if n == 0 { return Vec::new(); }

    // Precompute neighbor lists (indices within eps)
    let neighbors: Vec<Vec<usize>> = (0..n)
        .map(|i| {
            (0..n)
                .filter(|&j| i != j && cosine_dist(&points[i].1, &points[j].1) <= eps)
                .collect()
        })
        .collect();

    let mut labels: Vec<Option<i64>> = vec![None; n];
    let mut visited = vec![false; n];
    let mut cluster_id: i64 = 0;

    for i in 0..n {
        if visited[i] { continue; }
        visited[i] = true;
        // Need self + neighbors >= min_samples to be a core point
        if neighbors[i].len() + 1 < min_samples { continue; }
        labels[i] = Some(cluster_id);
        let mut queue = neighbors[i].clone();
        let mut qi = 0;
        while qi < queue.len() {
            let q = queue[qi];
            qi += 1;
            if !visited[q] {
                visited[q] = true;
                if neighbors[q].len() + 1 >= min_samples {
                    for &nb in &neighbors[q] {
                        if !queue.contains(&nb) { queue.push(nb); }
                    }
                }
            }
            if labels[q].is_none() { labels[q] = Some(cluster_id); }
        }
        cluster_id += 1;
    }

    points.iter().zip(labels).map(|((id, _), lbl)| (*id, lbl)).collect()
}

fn cosine_dist(a: &[f32], b: &[f32]) -> f32 {
    1.0 - a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn l2(v: Vec<f32>) -> Vec<f32> {
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        v.into_iter().map(|x| x / norm).collect()
    }

    #[test]
    fn two_close_vectors_form_cluster() {
        let v1 = l2(vec![1.0f32, 0.01, 0.0]);
        let v2 = l2(vec![1.0f32, 0.02, 0.0]);
        let v3 = l2(vec![0.0f32, 1.0, 0.0]);
        let result = dbscan_cosine(&[(1, v1), (2, v2), (3, v3)], 0.1, 2);
        let map: std::collections::HashMap<_, _> = result.into_iter().collect();
        assert_eq!(map[&1], map[&2], "close vectors must share cluster");
        assert_eq!(map[&3], None, "distant vector must be outlier");
    }

    #[test]
    fn identical_vectors_cluster_together() {
        let v = l2(vec![1.0f32, 0.0, 0.0]);
        let result = dbscan_cosine(&[(1, v.clone()), (2, v.clone()), (3, v)], 0.05, 2);
        let ids: Vec<_> = result.iter().map(|(_, c)| *c).collect();
        assert!(ids.iter().all(|c| c.is_some()), "all must be clustered");
        assert_eq!(ids[0], ids[1]);
        assert_eq!(ids[1], ids[2]);
    }

    #[test]
    fn all_noise_when_min_samples_too_high() {
        let v = l2(vec![1.0f32, 0.0]);
        let result = dbscan_cosine(&[(1, v.clone()), (2, v)], 0.05, 10);
        assert!(result.iter().all(|(_, c)| c.is_none()));
    }

    #[test]
    fn empty_input_returns_empty() {
        let result = dbscan_cosine(&[], 0.4, 2);
        assert!(result.is_empty());
    }

    #[test]
    fn two_distinct_clusters() {
        let a1 = l2(vec![1.0f32, 0.0, 0.0]);
        let a2 = l2(vec![0.99f32, 0.01, 0.0]);
        let b1 = l2(vec![0.0f32, 1.0, 0.0]);
        let b2 = l2(vec![0.0f32, 0.99, 0.01]);
        let result = dbscan_cosine(&[(1, a1), (2, a2), (3, b1), (4, b2)], 0.1, 2);
        let map: std::collections::HashMap<_, _> = result.into_iter().collect();
        assert_ne!(map[&1], map[&3]);
        assert_eq!(map[&1], map[&2]);
        assert_eq!(map[&3], map[&4]);
    }
}
