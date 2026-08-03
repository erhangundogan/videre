use std::collections::BinaryHeap;

/// Default centroid cosine-similarity threshold for the centroid-merge pass.
/// Calibrated on this project's real data: confirmed *different* people never
/// exceed ~0.29 centroid similarity, while a single person's fragmented
/// sub-clusters run 0.37-0.76, so 0.35 reunites fragments with a safe margin
/// above the different-person ceiling.
pub const DEFAULT_MERGE_SIM: f32 = 0.35;

/// Default minimum face bbox side (pixels) for a face to take part in
/// clustering. Faces smaller than this embed into near-degenerate ArcFace
/// vectors that cluster together regardless of identity; they are held out of
/// clustering (left as unassigned singletons) rather than forming a large
/// mixed junk cluster. On this project's real data, genuine person clusters
/// are essentially all >100px per side while the degenerate junk cluster sat
/// at ~60px, so 80 cleanly separates the two.
pub const DEFAULT_MIN_FACE_PX: f32 = 80.0;

/// Default distinctiveness gate: faces whose embedding cosine-similarity to the
/// population-average embedding exceeds this are held out of clustering as
/// low-quality (occluded, non-frontal, blurry, or false detections all embed
/// close to the generic average and carry little identity information). On this
/// project's real data, 0.40 removed ~78% of a mixed junk cluster while
/// touching 0% of confirmed real-person clusters.
pub const DEFAULT_MAX_GENERIC_SIM: f32 = 0.40;

/// Average-linkage (UPGMA) agglomerative clustering on L2-normalized
/// embeddings using cosine distance. Repeatedly merges the two closest
/// clusters, where the distance between two clusters is the size-weighted
/// average cosine distance across every pair of members (not the worst-case
/// pair). This sits deliberately between two failure modes:
///
/// - DBSCAN's density-reachability chains a long sequence of pairwise-close
///   points into one cluster even when the endpoints are nowhere near each
///   other (thousands of unrelated faces merged into one cluster).
/// - Complete-linkage (the max pairwise distance) refuses to ever merge two
///   groups if a *single* pair is far apart, even when every other pair
///   overwhelmingly agrees - in practice this fractures one person's photos
///   into dozens of separate clusters because of a handful of odd-angle or
///   blurry face crops that happen to embed poorly.
///
/// Averaging is far more robust to that kind of single-pair noise while
/// still requiring broad agreement across most pairs, not just one lucky
/// bridge - real measurements on this project's data show confirmed
/// different people average well under 0.2 cosine similarity even in the
/// worst case, while a real person's separate clusters commonly average
/// 0.5-0.7, so an eps of 0.6 cosine distance (0.4 similarity) sits safely in
/// that gap.
///
/// Clusters smaller than `min_samples` are reported as outliers (`None`)
/// rather than kept as small clusters.
///
/// Uses a lazily-invalidated min-heap of candidate merges (classic
/// nearest-neighbor agglomerative clustering) rather than rescanning every
/// active pair on every merge, so this runs in O(n^2 log n) instead of
/// O(n^3) - `videre watch` re-runs this every cycle over every embedding in
/// the database, so the naive cubic scan becomes a real bottleneck once the
/// face count reaches a few thousand.
///
/// Returns Vec<(face_id, cluster_id)> where cluster_id=None means outlier.
pub fn average_linkage_cosine(
    points: &[(i64, Vec<f32>)],
    eps: f32,
    min_samples: usize,
    silent: bool,
) -> Vec<(i64, Option<i64>)> {
    let n = points.len();
    if n == 0 { return Vec::new(); }
    let clusters = agglomerate_average(points, eps, silent);
    label_clusters(points, &clusters, min_samples)
}

/// Average-linkage agglomeration with no `min_samples` filtering: returns the
/// member index-lists of every resulting cluster, every point included. Shared
/// by [`average_linkage_cosine`] and [`cluster_faces`].
fn agglomerate_average(points: &[(i64, Vec<f32>)], eps: f32, silent: bool) -> Vec<Vec<usize>> {
    let n = points.len();
    if n == 0 {
        return Vec::new();
    }

    // Running per-cluster embedding sum (a singleton's sum is its own
    // embedding). Replaces the old O(n^2) dense `dist` matrix (~13.7GB at
    // n=58,555) with O(n * dim) memory (~120MB at that scale) - see
    // cluster_dist_from_sums's doc comment for the exact identity this
    // relies on. Any pair's distance is now computed on demand in O(dim)
    // from two small, cache-resident vectors, instead of looked up in a
    // multi-gigabyte matrix.
    let mut sums: Vec<Vec<f32>> = points.iter().map(|(_, v)| v.clone()).collect();
    let mut members: Vec<Vec<usize>> = (0..n).map(|i| vec![i]).collect();
    let mut alive = vec![true; n];

    let mut heap: BinaryHeap<HeapEntry> = BinaryHeap::new();
    let progress = crate::progress::Progress::new(n as u64, silent);
    for i in 0..n {
        for j in (i + 1)..n {
            let d = cosine_dist(&points[i].1, &points[j].1);
            if d <= eps {
                heap.push(HeapEntry { dist: d, i, j });
            }
        }
        progress.tick();
    }
    progress.finish();

    // Unlike the old dense-matrix version, entries are only ever pushed when
    // they are actually eps-eligible (both here and in the merge-update loop
    // below) - so there is no need for a separate "if d > eps { break }"
    // check: every entry the heap can ever produce already satisfies
    // d <= eps at push time, and the loop ends naturally once the heap is
    // exhausted. (This is provably equivalent to the old break: HeapEntry's
    // Ord is reversed so the heap pops in ascending distance order, and
    // pushes only ever happen from within this same loop, so the old code's
    // "break on the first non-stale d > eps" and "never push d > eps in the
    // first place" both stop merging at the exact same logical point.)
    while let Some(HeapEntry { dist: d, i, j }) = heap.pop() {
        if !alive[i] || !alive[j] {
            continue;
        }
        let size_i = members[i].len() as f32;
        let size_j = members[j].len() as f32;
        let current_d = cluster_dist_from_sums(&sums[i], &sums[j], size_i, size_j);
        if current_d != d {
            continue; // stale: superseded since push, either i or j has since absorbed another cluster
        }

        // Merge cluster j into cluster i (average linkage: size-weighted mean of the two).
        let moved = std::mem::take(&mut members[j]);
        members[i].extend(moved);
        let moved_sum = std::mem::take(&mut sums[j]);
        for (s, v) in sums[i].iter_mut().zip(&moved_sum) {
            *s += v;
        }
        alive[j] = false;

        let new_size_i = members[i].len() as f32;
        for k in 0..n {
            if k == i || k == j || !alive[k] {
                continue;
            }
            let size_k = members[k].len() as f32;
            let new_d = cluster_dist_from_sums(&sums[i], &sums[k], new_size_i, size_k);
            if new_d <= eps {
                heap.push(HeapEntry { dist: new_d, i: i.min(k), j: i.max(k) });
            }
        }
    }

    (0..n).filter(|&r| alive[r]).map(|r| std::mem::take(&mut members[r])).collect()
}

/// L2-normalized mean of the given members' embeddings. A zero-length sum
/// (antipodal members that cancel) is left un-normalized; its similarity to
/// anything is 0, which correctly blocks merging.
fn centroid(points: &[(i64, Vec<f32>)], member_idxs: &[usize]) -> Vec<f32> {
    let dim = points[member_idxs[0]].1.len();
    let mut sum = vec![0.0f32; dim];
    for &idx in member_idxs {
        for (s, v) in sum.iter_mut().zip(&points[idx].1) { *s += v; }
    }
    let norm = sum.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 1e-12 { for s in &mut sum { *s /= norm; } }
    sum
}

/// Repeatedly merges the two clusters whose centroids are most cosine-similar,
/// stopping once no pair reaches `merge_sim`. Centroids are recomputed from the
/// full member set after each merge, so this is centroid-linkage over the
/// clusters produced by [`agglomerate_average`].
fn merge_by_centroid(
    points: &[(i64, Vec<f32>)],
    mut clusters: Vec<Vec<usize>>,
    merge_sim: f32,
) -> Vec<Vec<usize>> {
    let mut centroids: Vec<Vec<f32>> = clusters.iter().map(|c| centroid(points, c)).collect();

    loop {
        let mut best: Option<(f32, usize, usize)> = None;
        for i in 0..clusters.len() {
            for j in (i + 1)..clusters.len() {
                let s = centroids[i].iter().zip(&centroids[j]).map(|(a, b)| a * b).sum::<f32>();
                if best.is_none_or(|(bs, _, _)| s > bs) {
                    best = Some((s, i, j));
                }
            }
        }
        let Some((s, i, j)) = best else { break };
        if s < merge_sim { break; }

        // Merge j into i, drop j (swap_remove keeps indices tidy), recompute i's centroid.
        let moved = std::mem::take(&mut clusters[j]);
        clusters[i].extend(moved);
        clusters.swap_remove(j);
        centroids.swap_remove(j);
        centroids[i] = centroid(points, &clusters[i]);
    }

    clusters
}

/// Assigns ascending cluster ids to clusters with at least `min_samples`
/// members; every other point (smaller clusters) becomes noise (`None`).
/// Returns labels in the original `points` order.
fn label_clusters(
    points: &[(i64, Vec<f32>)],
    clusters: &[Vec<usize>],
    min_samples: usize,
) -> Vec<(i64, Option<i64>)> {
    let mut labels: Vec<Option<i64>> = vec![None; points.len()];
    let mut cluster_id: i64 = 0;
    for members in clusters {
        if members.len() < min_samples { continue; }
        for &idx in members {
            labels[idx] = Some(cluster_id);
        }
        cluster_id += 1;
    }
    points.iter().zip(labels).map(|((id, _), lbl)| (*id, lbl)).collect()
}

#[derive(Copy, Clone, PartialEq)]
struct HeapEntry {
    dist: f32,
    i: usize,
    j: usize,
}
impl Eq for HeapEntry {}
impl Ord for HeapEntry {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Reverse so BinaryHeap (a max-heap) pops the smallest distance first.
        other.dist.total_cmp(&self.dist)
    }
}
impl PartialOrd for HeapEntry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> { Some(self.cmp(other)) }
}

fn cosine_dist(a: &[f32], b: &[f32]) -> f32 {
    1.0 - a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>()
}

/// Exact average-linkage distance between two clusters, computed from their
/// running embedding sums rather than a stored pairwise distance matrix.
///
/// For L2-normalized (unit) vectors, cosine similarity is a plain dot
/// product, and average-linkage cluster distance decomposes exactly:
///
/// avg_distance(A, B) = mean over all (a in A, b in B) of (1 - a.b)
///                     = 1 - mean(a.b)
///                     = 1 - (sum_A . sum_B) / (|A| * |B|)
///
/// by bilinearity of the dot product (the sum of pairwise dot products over
/// a cartesian product of two sets equals the dot product of the two sets'
/// sums). This lets cluster distance be computed on demand in O(dim) from
/// two running sums, instead of maintaining/looking up an O(n^2) matrix -
/// see docs/superpowers/specs/2026-08-03-face-clustering-performance-design.md.
fn cluster_dist_from_sums(sum_a: &[f32], sum_b: &[f32], size_a: f32, size_b: f32) -> f32 {
    let dot: f32 = sum_a.iter().zip(sum_b).map(|(x, y)| x * y).sum();
    1.0 - dot / (size_a * size_b)
}

#[cfg(test)]
mod sums_distance_tests {
    use super::*;

    #[test]
    fn singleton_clusters_match_plain_cosine_distance() {
        // Both vectors here are already unit-length, matching the
        // precondition cluster_dist_from_sums is documented against
        // (real ArcFace embeddings are always L2-normalized).
        let a = vec![1.0f32, 0.0, 0.0];
        let b = vec![0.6f32, 0.8, 0.0]; // 0.6^2 + 0.8^2 = 1.0, already unit
        let expected = cosine_dist(&a, &b);
        let actual = cluster_dist_from_sums(&a, &b, 1.0, 1.0);
        assert!((actual - expected).abs() < 1e-6, "expected {expected}, got {actual}");
    }

    #[test]
    fn merged_cluster_distance_matches_direct_average_over_all_cross_pairs() {
        // Cluster A = {a1, a2}, cluster B = {b1}, all unit-length. The
        // sums-based formula for dist(A, B) must equal the direct average of
        // cosine_dist(a1, b1) and cosine_dist(a2, b1) - the definition of
        // average-linkage.
        let a1 = vec![1.0f32, 0.0, 0.0];
        let a2 = vec![0.6f32, 0.8, 0.0]; // unit-length, close to a1's direction
        let b1 = vec![0.0f32, 1.0, 0.0];

        let sum_a: Vec<f32> = a1.iter().zip(&a2).map(|(x, y)| x + y).collect();
        let direct_avg = (cosine_dist(&a1, &b1) + cosine_dist(&a2, &b1)) / 2.0;
        let via_sums = cluster_dist_from_sums(&sum_a, &b1, 2.0, 1.0);

        assert!(
            (via_sums - direct_avg).abs() < 1e-6,
            "sums-based distance {via_sums} must match direct average {direct_avg}"
        );
    }
}

/// Two-stage clustering: average-linkage agglomeration, then a centroid-merge
/// pass that joins whole clusters whose L2-normalized mean embeddings
/// ("centroids") are at least `merge_sim` cosine-similar.
///
/// Why the second pass exists: average-linkage decides on the *average
/// pairwise* distance between two clusters' raw members. One person's photos
/// legitimately spread wide in embedding space (pose, lighting, age), so two
/// genuine sub-clusters of the same person can have an average cross-pair
/// similarity well below the merge threshold even though they are clearly the
/// same identity. Averaging each cluster down to a single centroid first
/// cancels that per-face spread, and the centroid-to-centroid signal is far
/// cleaner: on this project's real data, confirmed *different* people never
/// exceed ~0.29 centroid similarity, while a real person's fragmented
/// sub-clusters sit at 0.37-0.76 - so a `merge_sim` around 0.35 reunites a
/// person's fragments without risking merging two different people.
///
/// This only ever operates on cluster grouping (the returned cluster ids) -
/// it never inspects or writes human labels. `min_samples` is applied *after*
/// the merge, so small fragments that join a larger cluster are kept.
///
/// Returns Vec<(face_id, cluster_id)> where cluster_id=None means outlier.
pub fn cluster_faces(
    points: &[(i64, Vec<f32>)],
    eps: f32,
    min_samples: usize,
    merge_sim: f32,
    silent: bool,
) -> Vec<(i64, Option<i64>)> {
    let n = points.len();
    if n == 0 { return Vec::new(); }
    let clusters = agglomerate_average(points, eps, silent);

    // Only real clusters (>= min_samples) take part in the centroid-merge.
    // Two reasons, both important:
    //   * Correctness: a lone face is a noisy identity signal - a single bad
    //     crop can sit within merge_sim of a *different* person's centroid,
    //     whereas a whole cluster's averaged centroid cannot. Merging only
    //     established clusters keeps the pass from mis-attaching stray faces.
    //   * Speed: the merge is O(iterations * clusters^2 * dim); feeding it the
    //     hundreds of singletons agglomeration leaves behind blows the cost up
    //     by orders of magnitude, which matters because `videre watch`
    //     re-clusters on every cycle.
    // Sub-min_samples clusters keep flowing through to label_clusters, where
    // they fall out as noise exactly as they did before this pass existed.
    let (mergeable, rest): (Vec<Vec<usize>>, Vec<Vec<usize>>) =
        clusters.into_iter().partition(|c| c.len() >= min_samples);
    let mut merged = merge_by_centroid(points, mergeable, merge_sim);
    merged.extend(rest);
    label_clusters(points, &merged, min_samples)
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
        let result = average_linkage_cosine(&[(1, v1), (2, v2), (3, v3)], 0.1, 2, true);
        let map: std::collections::HashMap<_, _> = result.into_iter().collect();
        assert_eq!(map[&1], map[&2], "close vectors must share cluster");
        assert_eq!(map[&3], None, "distant vector must be outlier");
    }

    #[test]
    fn identical_vectors_cluster_together() {
        let v = l2(vec![1.0f32, 0.0, 0.0]);
        let result = average_linkage_cosine(&[(1, v.clone()), (2, v.clone()), (3, v)], 0.05, 2, true);
        let ids: Vec<_> = result.iter().map(|(_, c)| *c).collect();
        assert!(ids.iter().all(|c| c.is_some()), "all must be clustered");
        assert_eq!(ids[0], ids[1]);
        assert_eq!(ids[1], ids[2]);
    }

    #[test]
    fn all_noise_when_min_samples_too_high() {
        let v = l2(vec![1.0f32, 0.0]);
        let result = average_linkage_cosine(&[(1, v.clone()), (2, v)], 0.05, 10, true);
        assert!(result.iter().all(|(_, c)| c.is_none()));
    }

    #[test]
    fn empty_input_returns_empty() {
        let result = average_linkage_cosine(&[], 0.4, 2, true);
        assert!(result.is_empty());
    }

    #[test]
    fn chain_of_similar_pairs_does_not_merge_into_one_cluster() {
        // 5 points around a circle, 60 degrees apart: each point is close to
        // its immediate neighbor (dist 0.5, within eps 0.6) but the chain
        // endpoints are far apart (dist 1.5, well outside eps). DBSCAN's
        // density-reachability chains all 5 into a single cluster via the
        // neighbor-of-a-neighbor links; that's the real-world bug where
        // thousands of unrelated faces end up in one cluster.
        let angles = [0.0f32, 60.0, 120.0, 180.0, 240.0];
        let points: Vec<(i64, Vec<f32>)> = angles
            .iter()
            .enumerate()
            .map(|(i, deg)| {
                let rad = deg.to_radians();
                (i as i64, vec![rad.cos(), rad.sin()])
            })
            .collect();
        let result = average_linkage_cosine(&points, 0.6, 2, true);
        let map: std::collections::HashMap<_, _> = result.into_iter().collect();
        let cluster_ids: std::collections::HashSet<_> =
            map.values().filter_map(|c| *c).collect();
        assert!(
            cluster_ids.len() > 1,
            "chain must not collapse into a single cluster, got cluster ids {cluster_ids:?}"
        );
        assert_ne!(map[&0], map[&4], "chain endpoints must not share a cluster");
    }

    #[test]
    fn one_bad_pair_does_not_block_an_otherwise_strong_merge() {
        // Two real clusters of the same person: A = 3 points at 0 degrees,
        // S = 2 points at 20 degrees (dist(A,S)=0.060, clearly the same
        // identity, they merge into a 5-member cluster). A single extra
        // photo `o` at 70 degrees is a bad crop of the SAME person: its
        // worst pairwise distance (to A, 0.658) exceeds eps, but its
        // distance to S (0.357) is fine, and the size-weighted average
        // across all 5 existing members (0.538) is comfortably within eps.
        // Complete-linkage (whichever single worst pair) would refuse to
        // ever merge `o` in, no matter how large/confident the surrounding
        // cluster gets - that's the real-world bug where a person's photos
        // fracture into dozens of separate clusters because of a handful of
        // odd-angle or blurry faces.
        let deg = |d: f32| { let r = d.to_radians(); vec![r.cos(), r.sin()] };
        let points: Vec<(i64, Vec<f32>)> = vec![
            (1, deg(0.0)), (2, deg(0.0)), (3, deg(0.0)),
            (4, deg(20.0)), (5, deg(20.0)),
            (6, deg(70.0)),
        ];
        let result = average_linkage_cosine(&points, 0.6, 2, true);
        let map: std::collections::HashMap<_, _> = result.into_iter().collect();
        assert!(map[&1].is_some(), "the core group must still cluster");
        assert_eq!(map[&1], map[&6], "the odd-angle photo must join the same person's cluster");
    }

    #[test]
    fn two_distinct_clusters() {
        let a1 = l2(vec![1.0f32, 0.0, 0.0]);
        let a2 = l2(vec![0.99f32, 0.01, 0.0]);
        let b1 = l2(vec![0.0f32, 1.0, 0.0]);
        let b2 = l2(vec![0.0f32, 0.99, 0.01]);
        let result = average_linkage_cosine(&[(1, a1), (2, a2), (3, b1), (4, b2)], 0.1, 2, true);
        let map: std::collections::HashMap<_, _> = result.into_iter().collect();
        assert_ne!(map[&1], map[&3]);
        assert_eq!(map[&1], map[&2]);
        assert_eq!(map[&3], map[&4]);
    }

    // Two sub-clusters of the SAME identity: a shared identity axis (dim 0),
    // a per-sub-cluster axis (dims 1 vs 2), and tiny distinct noise (dims
    // 3-5). Every within-sub-cluster pair is nearly identical (shares the
    // sub-cluster axis) while every cross pair only shares the identity axis,
    // so average-linkage sees the cross distance as too large and leaves them
    // split - but the two centroids both collapse to ~(identity+sub) and are
    // 0.5 cosine-similar, which is exactly the "one person fragmented into
    // several clusters" case the centroid-merge pass is built to reunite.
    fn same_identity_two_subclusters() -> Vec<(i64, Vec<f32>)> {
        vec![
            (1, l2(vec![1.0, 1.0, 0.0, 0.15, 0.0, 0.0])),
            (2, l2(vec![1.0, 1.0, 0.0, 0.0, 0.15, 0.0])),
            (3, l2(vec![1.0, 1.0, 0.0, 0.0, 0.0, 0.15])),
            (4, l2(vec![1.0, 0.0, 1.0, 0.15, 0.0, 0.0])),
            (5, l2(vec![1.0, 0.0, 1.0, 0.0, 0.15, 0.0])),
            (6, l2(vec![1.0, 0.0, 1.0, 0.0, 0.0, 0.15])),
        ]
    }

    #[test]
    fn average_linkage_alone_splits_the_two_subclusters() {
        // Guard on the premise of the next test: without the centroid-merge
        // pass, these six faces of one person land in (at least) two clusters.
        let result = average_linkage_cosine(&same_identity_two_subclusters(), 0.3, 1, true);
        let clusters: std::collections::HashSet<_> =
            result.iter().filter_map(|(_, c)| *c).collect();
        assert!(clusters.len() >= 2, "premise: average-linkage should split them, got {clusters:?}");
    }

    #[test]
    fn centroid_merge_reunites_one_persons_fragmented_subclusters() {
        let result = cluster_faces(&same_identity_two_subclusters(), 0.3, 1, 0.4, true);
        let map: std::collections::HashMap<_, _> = result.into_iter().collect();
        let c1 = map[&1];
        assert!(c1.is_some(), "faces must be clustered, not left as noise");
        for id in 2..=6 {
            assert_eq!(map[&id], c1, "all six same-identity faces must share one cluster");
        }
    }

    #[test]
    fn centroid_merge_keeps_distinct_identities_apart() {
        // Cluster A (dims 0+1) and cluster C (negative dim 0) have centroids
        // pointing in opposite directions on the identity axis: centroid
        // similarity is negative, far below merge_sim, so they must NOT merge
        // even though the merge pass is active - guarding against a return to
        // the mega-blob failure.
        let mut pts = same_identity_two_subclusters();
        pts.truncate(3); // just cluster A (identity axis 0, sub axis 1)
        pts.push((7, l2(vec![-1.0, 0.0, 0.0, 0.15, 0.0, 0.0])));
        pts.push((8, l2(vec![-1.0, 0.0, 0.0, 0.0, 0.15, 0.0])));
        pts.push((9, l2(vec![-1.0, 0.0, 0.0, 0.0, 0.0, 0.15])));
        let result = cluster_faces(&pts, 0.3, 1, 0.4, true);
        let map: std::collections::HashMap<_, _> = result.into_iter().collect();
        assert_eq!(map[&1], map[&2], "cluster A stays together");
        assert_eq!(map[&7], map[&8], "cluster C stays together");
        assert_ne!(map[&1], map[&7], "different identities must not merge");
    }

    #[test]
    fn centroid_merge_still_drops_small_clusters_below_min_samples() {
        // A lone point far from the identity clusters: after merging, it is
        // still a size-1 cluster and must fall out as noise under min_samples=2.
        let mut pts = same_identity_two_subclusters();
        pts.push((99, l2(vec![0.0, 0.0, 0.0, 0.0, 1.0, 0.0])));
        let result = cluster_faces(&pts, 0.3, 2, 0.4, true);
        let map: std::collections::HashMap<_, _> = result.into_iter().collect();
        assert_eq!(map[&99], None, "isolated singleton must remain noise");
    }

    #[test]
    fn agglomerate_average_matches_dense_matrix_reference_on_synthetic_data() {
        // Byte-for-byte copy of the ORIGINAL (pre-2026-08-03) dense-matrix
        // agglomerate_average, kept only as a reference oracle for this test.
        fn reference_agglomerate_average(
            points: &[(i64, Vec<f32>)],
            eps: f32,
        ) -> Vec<Vec<usize>> {
            let n = points.len();
            let mut dist: Vec<Vec<f32>> = vec![vec![0.0f32; n]; n];
            let mut heap: BinaryHeap<HeapEntry> = BinaryHeap::new();
            for i in 0..n {
                for j in (i + 1)..n {
                    let d = cosine_dist(&points[i].1, &points[j].1);
                    dist[i][j] = d;
                    dist[j][i] = d;
                    if d <= eps {
                        heap.push(HeapEntry { dist: d, i, j });
                    }
                }
            }
            let mut members: Vec<Vec<usize>> = (0..n).map(|i| vec![i]).collect();
            let mut alive = vec![true; n];
            while let Some(HeapEntry { dist: d, i, j }) = heap.pop() {
                if !alive[i] || !alive[j] {
                    continue;
                }
                if dist[i][j] != d {
                    continue;
                }
                if d > eps {
                    break;
                }
                let size_i = members[i].len() as f32;
                let size_j = members[j].len() as f32;
                let moved = std::mem::take(&mut members[j]);
                members[i].extend(moved);
                alive[j] = false;
                for k in 0..n {
                    if k == i || k == j || !alive[k] {
                        continue;
                    }
                    let new_d = (size_i * dist[i][k] + size_j * dist[j][k]) / (size_i + size_j);
                    if new_d != dist[i][k] {
                        dist[i][k] = new_d;
                        dist[k][i] = new_d;
                        heap.push(HeapEntry { dist: new_d, i: i.min(k), j: i.max(k) });
                    }
                }
            }
            (0..n).filter(|&r| alive[r]).map(|r| std::mem::take(&mut members[r])).collect()
        }

        // Deterministic synthetic fixture: a small seeded LCG generates ~300
        // points in 64 dims, clustered around a handful of random unit
        // "identity" centers with small per-point noise (mimicking how one
        // real person's face embeddings cluster around a rough centroid),
        // then every point is L2-normalized (matching real ArcFace output).
        fn lcg_next(state: &mut u64) -> f32 {
            *state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((*state >> 33) as f32 / (1u64 << 31) as f32) - 1.0 // roughly in [-1, 1)
        }

        fn normalize(v: &mut [f32]) {
            let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
            if norm > 1e-12 {
                for x in v.iter_mut() {
                    *x /= norm;
                }
            }
        }

        let dim = 64;
        let num_identities = 8;
        let points_per_identity = 38; // 8 * 38 = 304 points total
        let mut state = 0xC0FFEEu64;

        let mut centers: Vec<Vec<f32>> = Vec::new();
        for _ in 0..num_identities {
            let mut c: Vec<f32> = (0..dim).map(|_| lcg_next(&mut state)).collect();
            normalize(&mut c);
            centers.push(c);
        }

        let mut points: Vec<(i64, Vec<f32>)> = Vec::new();
        let mut next_id = 1i64;
        for center in &centers {
            for _ in 0..points_per_identity {
                let mut v: Vec<f32> = center
                    .iter()
                    .map(|c| c + 0.15 * lcg_next(&mut state))
                    .collect();
                normalize(&mut v);
                points.push((next_id, v));
                next_id += 1;
            }
        }

        for &eps in &[0.3f32, 0.6f32, 0.9f32] {
            let via_new = agglomerate_average(&points, eps, true);
            let via_reference = reference_agglomerate_average(&points, eps);

            let mut new_sorted: Vec<Vec<usize>> =
                via_new.into_iter().map(|mut c| { c.sort(); c }).collect();
            let mut reference_sorted: Vec<Vec<usize>> =
                via_reference.into_iter().map(|mut c| { c.sort(); c }).collect();
            new_sorted.sort();
            reference_sorted.sort();

            assert_eq!(
                new_sorted, reference_sorted,
                "partition mismatch at eps={eps} between new and reference agglomerate_average"
            );
        }
    }
}
