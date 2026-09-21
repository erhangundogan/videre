use crate::face_learning::LabeledFace;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum PairLabel {
    SameIdentity,
    DifferentIdentities,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PairExample {
    pub left_face_id: i64,
    pub right_face_id: i64,
    pub label: PairLabel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairSamplingConfig {
    pub seed: u64,
    pub max_positive_pairs_per_identity: usize,
    pub max_negative_pairs_per_identity_pair: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdentityFold {
    pub fold_index: usize,
    pub identities: Vec<String>,
    pub face_ids: Vec<i64>,
    pub face_count: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SamplingError {
    DuplicateFaceId(i64),
    ZeroPairCap,
    TooFewFolds,
    FewerIdentitiesThanFolds { identities: usize, folds: usize },
}

impl fmt::Display for SamplingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateFaceId(id) => write!(f, "duplicate labeled face id {id}"),
            Self::ZeroPairCap => write!(f, "pair sampling caps must be greater than zero"),
            Self::TooFewFolds => write!(f, "identity-held-out validation needs at least two folds"),
            Self::FewerIdentitiesThanFolds { identities, folds } => write!(
                f,
                "identity-held-out validation has {identities} identities for {folds} folds"
            ),
        }
    }
}

impl std::error::Error for SamplingError {}

fn grouped_faces(labels: &[LabeledFace]) -> Result<BTreeMap<String, Vec<i64>>, SamplingError> {
    let mut seen = BTreeSet::new();
    let mut groups: BTreeMap<String, Vec<i64>> = BTreeMap::new();
    for label in labels {
        if !seen.insert(label.face_id) {
            return Err(SamplingError::DuplicateFaceId(label.face_id));
        }
        groups
            .entry(label.identity.clone())
            .or_default()
            .push(label.face_id);
    }
    for face_ids in groups.values_mut() {
        face_ids.sort_unstable();
    }
    Ok(groups)
}

fn fnv1a(bytes: impl IntoIterator<Item = u8>) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn normalized_identity(identity: &str) -> String {
    crate::person::normalize(identity).unwrap_or_else(|| identity.to_owned())
}

fn stable_pair_hash(
    seed: u64,
    left_identity: &str,
    right_identity: Option<&str>,
    left_face_id: i64,
    right_face_id: i64,
) -> u64 {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&seed.to_le_bytes());
    bytes.push(0xff);
    bytes.extend_from_slice(normalized_identity(left_identity).as_bytes());
    if let Some(right_identity) = right_identity {
        bytes.push(0xfd);
        bytes.extend_from_slice(normalized_identity(right_identity).as_bytes());
    }
    bytes.push(0xfe);
    bytes.extend_from_slice(&left_face_id.to_le_bytes());
    bytes.extend_from_slice(&right_face_id.to_le_bytes());
    fnv1a(bytes)
}

fn stable_identity_hash(seed: u64, identity: &str) -> u64 {
    stable_pair_hash(seed, identity, None, 0, 0)
}

fn sampled_bucket(
    label: PairLabel,
    left_identity: &str,
    right_identity: Option<&str>,
    pairs: impl Iterator<Item = (i64, i64)>,
    cap: usize,
    seed: u64,
) -> Vec<PairExample> {
    let mut ranked: Vec<_> = pairs
        .map(|(left_face_id, right_face_id)| {
            let (left_face_id, right_face_id) = if left_face_id < right_face_id {
                (left_face_id, right_face_id)
            } else {
                (right_face_id, left_face_id)
            };
            (
                stable_pair_hash(
                    seed,
                    left_identity,
                    right_identity,
                    left_face_id,
                    right_face_id,
                ),
                PairExample {
                    left_face_id,
                    right_face_id,
                    label,
                },
            )
        })
        .collect();
    ranked.sort_by_key(|(hash, example)| (*hash, example.left_face_id, example.right_face_id));
    ranked.truncate(cap);
    ranked.into_iter().map(|(_, example)| example).collect()
}

pub fn sample_identity_balanced_pairs(
    labels: &[LabeledFace],
    config: &PairSamplingConfig,
) -> Result<Vec<PairExample>, SamplingError> {
    if config.max_positive_pairs_per_identity == 0
        || config.max_negative_pairs_per_identity_pair == 0
    {
        return Err(SamplingError::ZeroPairCap);
    }
    let groups = grouped_faces(labels)?;
    let identities: Vec<_> = groups.keys().cloned().collect();
    let mut examples = Vec::new();

    for identity in &identities {
        let face_ids = &groups[identity];
        let pairs = face_ids.iter().enumerate().flat_map(|(index, left)| {
            face_ids[index + 1..]
                .iter()
                .map(move |right| (*left, *right))
        });
        examples.extend(sampled_bucket(
            PairLabel::SameIdentity,
            identity,
            None,
            pairs,
            config.max_positive_pairs_per_identity,
            config.seed,
        ));
    }

    for (left_index, left_identity) in identities.iter().enumerate() {
        for right_identity in &identities[left_index + 1..] {
            let pairs = groups[left_identity].iter().flat_map(|left| {
                groups[right_identity]
                    .iter()
                    .map(move |right| (*left, *right))
            });
            examples.extend(sampled_bucket(
                PairLabel::DifferentIdentities,
                left_identity,
                Some(right_identity),
                pairs,
                config.max_negative_pairs_per_identity_pair,
                config.seed,
            ));
        }
    }

    examples.sort();
    Ok(examples)
}

pub fn identity_held_out_folds(
    labels: &[LabeledFace],
    fold_count: usize,
    seed: u64,
) -> Result<Vec<IdentityFold>, SamplingError> {
    if fold_count < 2 {
        return Err(SamplingError::TooFewFolds);
    }
    let groups = grouped_faces(labels)?;
    if groups.len() < fold_count {
        return Err(SamplingError::FewerIdentitiesThanFolds {
            identities: groups.len(),
            folds: fold_count,
        });
    }

    let mut identity_groups: Vec<_> = groups.into_iter().collect();
    identity_groups.sort_by(
        |(left_identity, left_faces), (right_identity, right_faces)| {
            right_faces
                .len()
                .cmp(&left_faces.len())
                .then_with(|| {
                    stable_identity_hash(seed, left_identity)
                        .cmp(&stable_identity_hash(seed, right_identity))
                })
                .then_with(|| left_identity.cmp(right_identity))
        },
    );

    let mut folds: Vec<_> = (0..fold_count)
        .map(|fold_index| IdentityFold {
            fold_index,
            identities: Vec::new(),
            face_ids: Vec::new(),
            face_count: 0,
        })
        .collect();
    for (identity, face_ids) in identity_groups {
        let target = folds
            .iter()
            .enumerate()
            .min_by_key(|(index, fold)| (fold.face_count, *index))
            .map(|(index, _)| index)
            .expect("fold_count was validated as at least two");
        folds[target].face_count += face_ids.len();
        folds[target].identities.push(identity);
        folds[target].face_ids.extend(face_ids);
    }
    for fold in &mut folds {
        fold.identities.sort();
        fold.face_ids.sort_unstable();
    }
    Ok(folds)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::face_learning::LabeledFace;
    use std::collections::{BTreeMap, BTreeSet};

    fn labels() -> Vec<LabeledFace> {
        let mut labels = Vec::new();
        let mut next_id = 1i64;
        for (identity, count) in [("alpha", 20), ("beta", 4), ("gamma", 3), ("delta", 2)] {
            for _ in 0..count {
                labels.push(LabeledFace::new(next_id, identity));
                next_id += 1;
            }
        }
        labels
    }

    fn config() -> PairSamplingConfig {
        PairSamplingConfig {
            seed: 0x5649_4445_5245,
            max_positive_pairs_per_identity: 3,
            max_negative_pairs_per_identity_pair: 2,
        }
    }

    fn identities_by_face(labels: &[LabeledFace]) -> BTreeMap<i64, &str> {
        labels
            .iter()
            .map(|label| (label.face_id, label.identity.as_str()))
            .collect()
    }

    #[test]
    fn sampling_caps_each_identity_and_identity_pair() {
        let labels = labels();
        let identity_by_face = identities_by_face(&labels);
        let examples = sample_identity_balanced_pairs(&labels, &config()).unwrap();

        let alpha_positive = examples
            .iter()
            .filter(|example| {
                example.label == PairLabel::SameIdentity
                    && identity_by_face[&example.left_face_id] == "alpha"
            })
            .count();
        assert_eq!(alpha_positive, 3);

        let mut negative_counts = BTreeMap::new();
        for example in examples
            .iter()
            .filter(|example| example.label == PairLabel::DifferentIdentities)
        {
            let mut identities = [
                identity_by_face[&example.left_face_id],
                identity_by_face[&example.right_face_id],
            ];
            identities.sort();
            *negative_counts.entry(identities).or_insert(0usize) += 1;
        }
        assert_eq!(negative_counts.len(), 6);
        assert!(negative_counts.values().all(|count| *count == 2));
    }

    #[test]
    fn sampling_is_order_independent_and_contains_no_duplicate_pair() {
        let labels = labels();
        let expected = sample_identity_balanced_pairs(&labels, &config()).unwrap();

        let mut reversed = labels.clone();
        reversed.reverse();
        assert_eq!(
            sample_identity_balanced_pairs(&reversed, &config()).unwrap(),
            expected
        );

        let mut rotated = labels.clone();
        rotated.rotate_left(7);
        assert_eq!(
            sample_identity_balanced_pairs(&rotated, &config()).unwrap(),
            expected
        );

        let pairs: BTreeSet<_> = expected
            .iter()
            .map(|example| (example.left_face_id, example.right_face_id))
            .collect();
        assert_eq!(pairs.len(), expected.len());
        assert!(expected
            .iter()
            .all(|example| example.left_face_id < example.right_face_id));
    }

    #[test]
    fn changing_seed_changes_the_sample() {
        let labels = labels();
        let first = sample_identity_balanced_pairs(&labels, &config()).unwrap();
        let mut other = config();
        other.seed += 1;
        let second = sample_identity_balanced_pairs(&labels, &other).unwrap();
        assert_ne!(first, second);
    }

    #[test]
    fn held_out_folds_keep_identities_whole_and_balance_greedily() {
        let labels = labels();
        let folds = identity_held_out_folds(&labels, 3, config().seed).unwrap();

        assert_eq!(folds.len(), 3);
        assert_eq!(
            folds.iter().map(|fold| fold.face_count).collect::<Vec<_>>(),
            vec![20, 4, 5]
        );
        assert_eq!(folds[0].identities, vec!["alpha"]);
        assert_eq!(folds[1].identities, vec!["beta"]);
        assert_eq!(folds[2].identities.len(), 2);

        let mut identity_folds = BTreeMap::new();
        for fold in &folds {
            assert_eq!(fold.face_count, fold.face_ids.len());
            for identity in &fold.identities {
                assert!(identity_folds.insert(identity, fold.fold_index).is_none());
            }
        }
        assert_eq!(identity_folds.len(), 4);
    }

    #[test]
    fn held_out_folds_are_order_independent() {
        let labels = labels();
        let expected = identity_held_out_folds(&labels, 3, config().seed).unwrap();
        let mut reversed = labels.clone();
        reversed.reverse();
        assert_eq!(
            identity_held_out_folds(&reversed, 3, config().seed).unwrap(),
            expected
        );
    }

    #[test]
    fn duplicate_face_ids_are_rejected() {
        let mut labels = labels();
        labels.push(LabeledFace::new(1, "another"));
        assert_eq!(
            sample_identity_balanced_pairs(&labels, &config()),
            Err(SamplingError::DuplicateFaceId(1))
        );
        assert_eq!(
            identity_held_out_folds(&labels, 3, config().seed),
            Err(SamplingError::DuplicateFaceId(1))
        );
    }

    #[test]
    fn zero_pair_caps_are_rejected() {
        let labels = labels();
        let mut zero_positive = config();
        zero_positive.max_positive_pairs_per_identity = 0;
        assert_eq!(
            sample_identity_balanced_pairs(&labels, &zero_positive),
            Err(SamplingError::ZeroPairCap)
        );

        let mut zero_negative = config();
        zero_negative.max_negative_pairs_per_identity_pair = 0;
        assert_eq!(
            sample_identity_balanced_pairs(&labels, &zero_negative),
            Err(SamplingError::ZeroPairCap)
        );
    }

    #[test]
    fn fold_count_must_leave_at_least_one_identity_per_fold() {
        let labels = labels();
        assert_eq!(
            identity_held_out_folds(&labels, 1, config().seed),
            Err(SamplingError::TooFewFolds)
        );
        assert_eq!(
            identity_held_out_folds(&labels, 5, config().seed),
            Err(SamplingError::FewerIdentitiesThanFolds {
                identities: 4,
                folds: 5,
            })
        );
    }
}
