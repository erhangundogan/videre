use super::{
    extract_cluster_quality_features, extract_membership_features, sample_identity_balanced_pairs,
    CalibrationModel, DecisionStage, FaceObservation, FeatureVector, LabeledFace,
    LearningDecisionKind, LearningOutcome, LogisticModel, LogisticScorer, ModelBundle, PairLabel,
    PairSamplingConfig, StoredLearningEvent, CLUSTER_QUALITY_FEATURE_NAMES, FEATURE_SCHEMA_VERSION,
    MEMBERSHIP_FEATURE_NAMES, MODEL_ARTIFACT_VERSION,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

const MAX_TRAINING_ITERATIONS: usize = 10_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExampleSource {
    ConfirmedLabels,
    ExplicitFeedback,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WeightedExample {
    pub source: ExampleSource,
    pub event_id: Option<i64>,
    pub identity_keys: Vec<String>,
    pub positive: bool,
    pub weight: f64,
    pub features: FeatureVector,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DecisionDataset {
    pub decision_kind: LearningDecisionKind,
    pub examples: Vec<WeightedExample>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrainingExclusion {
    pub event_id: i64,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TrainingSnapshot {
    pub generation: u64,
    pub embedding_model_id: String,
    pub feature_schema_version: u32,
    pub membership: DecisionDataset,
    pub cluster_quality: DecisionDataset,
    pub exclusions: Vec<TrainingExclusion>,
}

#[derive(Clone, Debug, PartialEq)]
struct HeldOutFold {
    held_out_identities: Vec<String>,
    training: Vec<WeightedExample>,
    validation: Vec<WeightedExample>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TrainingConfig {
    pub seed: u64,
    pub folds: usize,
    pub max_derived_pairs_per_identity: usize,
    pub max_negative_pairs_per_identity_pair: usize,
    pub max_explicit_examples_per_identity: usize,
    pub explicit_weight: f64,
    pub explicit_weight_cap: f64,
    pub l2_grid: Vec<f64>,
    pub positive_class_weight_grid: Vec<f64>,
    pub max_iterations: usize,
    pub tolerance: f64,
    pub min_positive_identities: usize,
    pub min_negative_identities: usize,
}

impl Default for TrainingConfig {
    fn default() -> Self {
        Self {
            seed: 0x5649_4445_5245,
            folds: 3,
            max_derived_pairs_per_identity: 16,
            max_negative_pairs_per_identity_pair: 4,
            max_explicit_examples_per_identity: 16,
            explicit_weight: 3.0,
            explicit_weight_cap: 4.0,
            l2_grid: vec![1.0, 0.1, 0.01],
            positive_class_weight_grid: vec![1.0, 2.0],
            max_iterations: 100,
            tolerance: 1e-8,
            min_positive_identities: 2,
            min_negative_identities: 2,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum TrainingError {
    InvalidConfig(String),
    InvalidInput(String),
    InsufficientEvidence {
        decision_kind: LearningDecisionKind,
        positive_identities: usize,
        negative_identities: usize,
    },
    TooFewIdentities {
        identities: usize,
        folds: usize,
    },
    NonConvergence,
}

impl fmt::Display for TrainingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfig(reason) => write!(f, "invalid training config: {reason}"),
            Self::InvalidInput(reason) => write!(f, "invalid training input: {reason}"),
            Self::InsufficientEvidence { decision_kind, positive_identities, negative_identities } => write!(f, "insufficient {decision_kind:?} evidence: {positive_identities} positive and {negative_identities} negative identities"),
            Self::TooFewIdentities { identities, folds } => write!(f, "only {identities} labeled identities for {folds} cross-validation folds; label more people or lower the fold count"),
            Self::NonConvergence => write!(f, "logistic optimization did not converge"),
        }
    }
}
impl std::error::Error for TrainingError {}

/// Total order over feature vectors: schema version first, then the maps'
/// entries in key order with `total_cmp` on values, which also handles NaN.
fn compare_feature_values(left: &FeatureVector, right: &FeatureVector) -> std::cmp::Ordering {
    left.schema_version
        .cmp(&right.schema_version)
        .then_with(|| {
            let mut left_values = left.values.iter();
            let mut right_values = right.values.iter();
            loop {
                match (left_values.next(), right_values.next()) {
                    (None, None) => return std::cmp::Ordering::Equal,
                    (None, Some(_)) => return std::cmp::Ordering::Less,
                    (Some(_), None) => return std::cmp::Ordering::Greater,
                    (Some((left_name, left_value)), Some((right_name, right_value))) => {
                        let order = left_name
                            .cmp(right_name)
                            .then(left_value.total_cmp(right_value));
                        if order != std::cmp::Ordering::Equal {
                            return order;
                        }
                    }
                }
            }
        })
}

fn compare_examples(left: &WeightedExample, right: &WeightedExample) -> std::cmp::Ordering {
    left.identity_keys
        .cmp(&right.identity_keys)
        .then(left.positive.cmp(&right.positive))
        .then(left.event_id.cmp(&right.event_id))
        .then_with(|| compare_feature_values(&left.features, &right.features))
}

fn stable_identity_hash(seed: u64, identity: &str) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in seed
        .to_le_bytes()
        .into_iter()
        .chain(std::iter::once(0xff))
        .chain(identity.bytes())
    {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn identity_held_out_splits(
    dataset: &DecisionDataset,
    fold_count: usize,
    seed: u64,
) -> Result<Vec<HeldOutFold>, TrainingError> {
    if fold_count < 2 {
        return Err(TrainingError::InvalidConfig(
            "identity-held-out training needs at least two folds".into(),
        ));
    }
    let mut identities = BTreeSet::new();
    for example in &dataset.examples {
        if example.identity_keys.is_empty() {
            return Err(TrainingError::InvalidInput(
                "training example has no identity key".into(),
            ));
        }
        identities.extend(example.identity_keys.iter().cloned());
    }
    if identities.len() < fold_count {
        return Err(TrainingError::TooFewIdentities {
            identities: identities.len(),
            folds: fold_count,
        });
    }
    let mut identities: Vec<_> = identities.into_iter().collect();
    identities.sort_by_key(|identity| (stable_identity_hash(seed, identity), identity.clone()));
    let mut held_out: Vec<Vec<String>> = vec![Vec::new(); fold_count];
    for (index, identity) in identities.into_iter().enumerate() {
        held_out[index % fold_count].push(identity);
    }
    let mut canonical_examples = dataset.examples.clone();
    canonical_examples.sort_by(compare_examples);
    let mut folds = Vec::with_capacity(fold_count);
    for mut identities in held_out {
        identities.sort();
        let held: BTreeSet<_> = identities.iter().cloned().collect();
        let training: Vec<_> = canonical_examples
            .iter()
            .filter(|example| example.identity_keys.iter().all(|key| !held.contains(key)))
            .cloned()
            .collect();
        let validation: Vec<_> = canonical_examples
            .iter()
            .filter(|example| example.identity_keys.iter().all(|key| held.contains(key)))
            .cloned()
            .collect();
        if training.is_empty() || validation.is_empty() {
            return Err(TrainingError::InvalidInput(
                "identity fold has no training or validation examples".into(),
            ));
        }
        folds.push(HeldOutFold {
            held_out_identities: identities,
            training,
            validation,
        });
    }
    Ok(folds)
}

pub fn build_training_snapshot(
    generation: u64,
    embedding_model_id: &str,
    labels: &[LabeledFace],
    observations: &[FaceObservation],
    events: &[StoredLearningEvent],
    config: &TrainingConfig,
) -> Result<TrainingSnapshot, TrainingError> {
    validate_config(config)?;
    if embedding_model_id.trim().is_empty() {
        return Err(TrainingError::InvalidInput(
            "embedding model id is empty".into(),
        ));
    }
    let observation_count = observations.len();
    let observations: BTreeMap<_, _> = observations
        .iter()
        .cloned()
        .map(|observation| (observation.face_id, observation))
        .collect();
    if observations.len() != observation_count {
        return Err(TrainingError::InvalidInput(
            "duplicate observation id".into(),
        ));
    }
    let identities: BTreeMap<_, _> = labels
        .iter()
        .map(|label| (label.face_id, label.identity.clone()))
        .collect();
    let pairs = sample_identity_balanced_pairs(
        labels,
        &PairSamplingConfig {
            seed: config.seed,
            max_positive_pairs_per_identity: config.max_derived_pairs_per_identity,
            max_negative_pairs_per_identity_pair: config.max_negative_pairs_per_identity_pair,
        },
    )
    .map_err(|error| TrainingError::InvalidInput(error.to_string()))?;
    let mut membership = Vec::new();
    for pair in pairs {
        let left = observations.get(&pair.left_face_id).ok_or_else(|| {
            TrainingError::InvalidInput(format!("missing observation {}", pair.left_face_id))
        })?;
        let right = observations.get(&pair.right_face_id).ok_or_else(|| {
            TrainingError::InvalidInput(format!("missing observation {}", pair.right_face_id))
        })?;
        let mut identity_keys = vec![
            identities[&pair.left_face_id].clone(),
            identities[&pair.right_face_id].clone(),
        ];
        identity_keys.sort();
        identity_keys.dedup();
        membership.push(WeightedExample {
            source: ExampleSource::ConfirmedLabels,
            event_id: None,
            identity_keys,
            positive: pair.label == PairLabel::SameIdentity,
            weight: 1.0,
            features: extract_membership_features(
                std::slice::from_ref(left),
                std::slice::from_ref(right),
                DecisionStage::Attachment,
            )
            .map_err(|error| TrainingError::InvalidInput(error.to_string()))?,
        });
    }
    let mut grouped: BTreeMap<String, Vec<FaceObservation>> = BTreeMap::new();
    for label in labels {
        grouped.entry(label.identity.clone()).or_default().push(
            observations
                .get(&label.face_id)
                .ok_or_else(|| {
                    TrainingError::InvalidInput(format!("missing observation {}", label.face_id))
                })?
                .clone(),
        );
    }
    let mut cluster_quality = Vec::new();
    for (identity, mut faces) in grouped {
        if faces.len() < 2 {
            continue;
        }
        faces.sort_by_key(|face| face.face_id);
        cluster_quality.push(WeightedExample {
            source: ExampleSource::ConfirmedLabels,
            event_id: None,
            identity_keys: vec![identity],
            positive: true,
            weight: 1.0,
            features: extract_cluster_quality_features(&faces, DecisionStage::GalleryCluster)
                .map_err(|error| TrainingError::InvalidInput(error.to_string()))?,
        });
    }
    let mut exclusions = Vec::new();
    let mut explicit_counts: BTreeMap<(LearningDecisionKind, String), usize> = BTreeMap::new();
    let mut events = events.to_vec();
    // The byte-level tiebreak only orders events sharing an id; it decides
    // which duplicate survives so input order cannot change the snapshot.
    events.sort_by(|left, right| {
        left.id.cmp(&right.id).then_with(|| {
            serde_json::to_vec(left)
                .unwrap_or_default()
                .cmp(&serde_json::to_vec(right).unwrap_or_default())
        })
    });
    let mut seen_event_ids = BTreeSet::new();
    for event in events {
        let reason = if !seen_event_ids.insert(event.id) {
            Some("duplicate_event")
        } else if !event.eligible {
            Some("ineligible")
        } else if event.embedding_model_id != embedding_model_id {
            Some("embedding_model_mismatch")
        } else if event.features.schema_version != FEATURE_SCHEMA_VERSION {
            Some("feature_schema_mismatch")
        } else if event.features.validate().is_err() {
            Some("invalid_feature_vector")
        } else if event
            .features
            .values
            .keys()
            .map(String::as_str)
            .ne(match event.decision_kind {
                LearningDecisionKind::Membership => MEMBERSHIP_FEATURE_NAMES,
                LearningDecisionKind::ClusterQuality => CLUSTER_QUALITY_FEATURE_NAMES,
            }
            .iter()
            .copied())
        {
            Some("decision_feature_mismatch")
        } else {
            None
        };
        if let Some(reason) = reason {
            exclusions.push(TrainingExclusion {
                event_id: event.id,
                reason: reason.into(),
            });
            continue;
        }
        let identity = event
            .target_identity
            .clone()
            .unwrap_or_else(|| format!("event:{}", event.id));
        let count = explicit_counts
            .entry((event.decision_kind, identity.clone()))
            .or_default();
        if *count >= config.max_explicit_examples_per_identity {
            exclusions.push(TrainingExclusion {
                event_id: event.id,
                reason: "per_identity_cap".into(),
            });
            continue;
        }
        *count += 1;
        let example = WeightedExample {
            source: ExampleSource::ExplicitFeedback,
            event_id: Some(event.id),
            identity_keys: vec![identity],
            positive: event.outcome == LearningOutcome::Positive,
            weight: config.explicit_weight.min(config.explicit_weight_cap),
            features: event.features,
        };
        match event.decision_kind {
            LearningDecisionKind::Membership => membership.push(example),
            LearningDecisionKind::ClusterQuality => cluster_quality.push(example),
        }
    }
    membership.sort_by(compare_examples);
    cluster_quality.sort_by(compare_examples);
    exclusions.sort_by(|left, right| {
        left.event_id
            .cmp(&right.event_id)
            .then(left.reason.cmp(&right.reason))
    });
    Ok(TrainingSnapshot {
        generation,
        embedding_model_id: embedding_model_id.into(),
        feature_schema_version: FEATURE_SCHEMA_VERSION,
        membership: DecisionDataset {
            decision_kind: LearningDecisionKind::Membership,
            examples: membership,
        },
        cluster_quality: DecisionDataset {
            decision_kind: LearningDecisionKind::ClusterQuality,
            examples: cluster_quality,
        },
        exclusions,
    })
}

fn validate_config(config: &TrainingConfig) -> Result<(), TrainingError> {
    if config.folds < 2
        || config.max_derived_pairs_per_identity == 0
        || config.max_negative_pairs_per_identity_pair == 0
        || config.max_explicit_examples_per_identity == 0
        || config.min_positive_identities == 0
        || config.min_negative_identities == 0
        || !config.explicit_weight.is_finite()
        || config.explicit_weight <= 0.0
        || !config.explicit_weight_cap.is_finite()
        || config.explicit_weight_cap <= 0.0
        || config.explicit_weight.min(config.explicit_weight_cap) <= 1.0
        || config.max_iterations == 0
        || config.max_iterations > MAX_TRAINING_ITERATIONS
        || config.tolerance <= 0.0
        || !config.tolerance.is_finite()
        || config.l2_grid.is_empty()
        || config.positive_class_weight_grid.is_empty()
        || config.l2_grid.iter().any(|v| !v.is_finite() || *v <= 0.0)
        || config
            .positive_class_weight_grid
            .iter()
            .any(|v| !v.is_finite() || *v <= 0.0)
    {
        return Err(TrainingError::InvalidConfig(
            "invalid folds, grid, iterations, or tolerance".into(),
        ));
    }
    Ok(())
}

fn identity_counts(dataset: &DecisionDataset) -> (usize, usize) {
    let mut positive = BTreeSet::new();
    let mut negative = BTreeSet::new();
    for example in &dataset.examples {
        if example.positive {
            positive.extend(example.identity_keys.iter().cloned());
        } else {
            negative.extend(example.identity_keys.iter().cloned());
        }
    }
    (positive.len(), negative.len())
}

pub fn train_logistic_bundle(
    snapshot: &TrainingSnapshot,
    config: &TrainingConfig,
) -> Result<ModelBundle, TrainingError> {
    validate_config(config)?;
    if snapshot.embedding_model_id.trim().is_empty()
        || snapshot.feature_schema_version != FEATURE_SCHEMA_VERSION
        || snapshot.membership.decision_kind != LearningDecisionKind::Membership
        || snapshot.cluster_quality.decision_kind != LearningDecisionKind::ClusterQuality
    {
        return Err(TrainingError::InvalidInput(
            "training snapshot model, schema, or decision kinds are incompatible".into(),
        ));
    }
    Ok(ModelBundle {
        artifact_version: MODEL_ARTIFACT_VERSION,
        embedding_model_id: snapshot.embedding_model_id.clone(),
        feature_schema_version: snapshot.feature_schema_version,
        membership: train_scorer(&snapshot.membership, config)?,
        cluster_quality: train_scorer(&snapshot.cluster_quality, config)?,
    })
}

fn train_scorer(
    dataset: &DecisionDataset,
    config: &TrainingConfig,
) -> Result<LogisticScorer, TrainingError> {
    let (positive_identities, negative_identities) = identity_counts(dataset);
    if positive_identities < config.min_positive_identities
        || negative_identities < config.min_negative_identities
    {
        return Err(TrainingError::InsufficientEvidence {
            decision_kind: dataset.decision_kind,
            positive_identities,
            negative_identities,
        });
    }
    let folds = identity_held_out_splits(dataset, config.folds, config.seed)?;
    let mut l2_grid = config.l2_grid.clone();
    l2_grid.sort_by(|left, right| right.total_cmp(left));
    l2_grid.dedup_by(|left, right| left.total_cmp(right).is_eq());
    let mut class_weight_grid = config.positive_class_weight_grid.clone();
    class_weight_grid.sort_by(f64::total_cmp);
    class_weight_grid.dedup_by(|left, right| left.total_cmp(right).is_eq());

    let mut selected: Option<(f64, f64, f64, f64, Vec<HeldOutScore>)> = None;
    for l2 in l2_grid {
        for class_weight in &class_weight_grid {
            let held_out = held_out_scores(
                &folds,
                l2,
                *class_weight,
                config.max_iterations,
                config.tolerance,
            )?;
            let uncalibrated: Vec<_> = held_out
                .iter()
                .map(|score| (sigmoid(score.logit), score.positive, score.weight))
                .collect();
            let (_, precision, recall) = select_threshold(&uncalibrated)?;
            let candidate = (precision, recall, l2, *class_weight, held_out);
            let replace = selected.as_ref().is_none_or(|best| {
                candidate
                    .0
                    .total_cmp(&best.0)
                    .then(candidate.1.total_cmp(&best.1))
                    .then(candidate.2.total_cmp(&best.2))
                    .then_with(|| best.3.total_cmp(&candidate.3))
                    .is_gt()
            });
            if replace {
                selected = Some(candidate);
            }
        }
    }
    let (_, _, l2, class_weight, held_out) = selected.ok_or_else(|| {
        TrainingError::InvalidConfig("the training parameter grid is empty".into())
    })?;
    let calibration = fit_calibration(&held_out, config.max_iterations, config.tolerance)?;
    let calibrated: Vec<_> = held_out
        .iter()
        .map(|score| {
            (
                sigmoid(calibration.intercept + calibration.slope * score.logit),
                score.positive,
                score.weight,
            )
        })
        .collect();
    let (threshold, _, _) = select_threshold(&calibrated)?;
    let model = fit_logistic(
        &dataset.examples,
        l2,
        class_weight,
        config.max_iterations,
        config.tolerance,
    )?;
    let scorer = LogisticScorer {
        model,
        calibration,
        threshold,
    };
    scorer
        .validate()
        .map_err(|error| TrainingError::InvalidInput(error.to_string()))?;
    Ok(scorer)
}

#[derive(Clone, Debug)]
struct HeldOutScore {
    identity_keys: Vec<String>,
    event_id: Option<i64>,
    positive: bool,
    weight: f64,
    logit: f64,
}

fn held_out_scores(
    folds: &[HeldOutFold],
    l2: f64,
    class_weight: f64,
    max_iterations: usize,
    tolerance: f64,
) -> Result<Vec<HeldOutScore>, TrainingError> {
    let mut scores = Vec::new();
    for fold in folds {
        let model = fit_logistic(&fold.training, l2, class_weight, max_iterations, tolerance)?;
        for example in &fold.validation {
            let (logit, _) = model
                .score(&example.features)
                .map_err(|error| TrainingError::InvalidInput(error.to_string()))?;
            scores.push(HeldOutScore {
                identity_keys: example.identity_keys.clone(),
                event_id: example.event_id,
                positive: example.positive,
                weight: example.weight,
                logit,
            });
        }
    }
    scores.sort_by(|left, right| {
        left.identity_keys
            .cmp(&right.identity_keys)
            .then(left.positive.cmp(&right.positive))
            .then(left.event_id.cmp(&right.event_id))
            .then(left.logit.total_cmp(&right.logit))
    });
    if !scores.iter().any(|score| score.positive) || !scores.iter().any(|score| !score.positive) {
        return Err(TrainingError::InvalidInput(
            "held-out predictions need both classes".into(),
        ));
    }
    Ok(scores)
}

fn select_threshold(scores: &[(f64, bool, f64)]) -> Result<(f64, f64, f64), TrainingError> {
    if scores.is_empty()
        || scores.iter().any(|(score, _, weight)| {
            !score.is_finite()
                || !(0.0..=1.0).contains(score)
                || !weight.is_finite()
                || *weight <= 0.0
        })
    {
        return Err(TrainingError::InvalidInput(
            "threshold selection received invalid scores".into(),
        ));
    }
    let positive_weight: f64 = scores
        .iter()
        .filter(|(_, positive, _)| *positive)
        .map(|(_, _, weight)| weight)
        .sum();
    if positive_weight == 0.0 || scores.iter().all(|(_, positive, _)| *positive) {
        return Err(TrainingError::InvalidInput(
            "threshold selection needs both classes".into(),
        ));
    }
    // One sweep over scores in descending order: walking down and grouping
    // equal scores means the cumulative counts at each distinct score are
    // exactly the counts the previous all-scores rescan produced, without the
    // quadratic cost.
    let mut ordered: Vec<(f64, bool, f64)> = scores.to_vec();
    for (score, _, _) in &mut ordered {
        if *score == 0.0 {
            // Fold negative zero into positive zero so equal scores land in
            // one threshold group.
            *score = 0.0;
        }
    }
    ordered.sort_by(|left, right| {
        right
            .0
            .total_cmp(&left.0)
            .then(left.1.cmp(&right.1))
            .then(left.2.total_cmp(&right.2))
    });
    let mut best: Option<(f64, f64, f64)> = None;
    let mut true_positive = 0.0;
    let mut false_positive = 0.0;
    let mut index = 0;
    while index < ordered.len() {
        let threshold = ordered[index].0;
        while index < ordered.len() && ordered[index].0 == threshold {
            let (_, positive, weight) = ordered[index];
            if positive {
                true_positive += weight;
            } else {
                false_positive += weight;
            }
            index += 1;
        }
        let predicted = true_positive + false_positive;
        if predicted == 0.0 {
            continue;
        }
        let precision = true_positive / predicted;
        let recall = true_positive / positive_weight;
        let candidate = (threshold, precision, recall);
        let replace = best.as_ref().is_none_or(|current| {
            candidate
                .1
                .total_cmp(&current.1)
                .then(candidate.2.total_cmp(&current.2))
                .then(candidate.0.total_cmp(&current.0))
                .is_gt()
        });
        if replace {
            best = Some(candidate);
        }
    }
    best.ok_or_else(|| TrainingError::InvalidInput("no usable threshold".into()))
}

fn fit_calibration(
    scores: &[HeldOutScore],
    max_iterations: usize,
    tolerance: f64,
) -> Result<CalibrationModel, TrainingError> {
    let mut parameters = [0.0, 1.0];
    let mut converged = false;
    for _ in 0..max_iterations {
        let mut gradient = [0.0, 0.0];
        let mut hessian = [[0.0, 0.0], [0.0, 0.0]];
        for score in scores {
            let probability = sigmoid(parameters[0] + parameters[1] * score.logit);
            let label = if score.positive { 1.0 } else { 0.0 };
            let row = [1.0, score.logit];
            for left in 0..2 {
                gradient[left] += score.weight * (probability - label) * row[left];
                for right in 0..2 {
                    hessian[left][right] +=
                        score.weight * probability * (1.0 - probability) * row[left] * row[right];
                }
            }
        }
        gradient[1] += 1e-3 * parameters[1];
        hessian[1][1] += 1e-3;
        hessian[0][0] += 1e-9;
        let delta = solve(
            hessian.into_iter().map(Vec::from).collect(),
            gradient.to_vec(),
        )
        .ok_or(TrainingError::NonConvergence)?;
        let max_delta = delta.iter().map(|value| value.abs()).fold(0.0, f64::max);
        parameters[0] -= delta[0];
        parameters[1] -= delta[1];
        if parameters.iter().any(|value| !value.is_finite()) {
            return Err(TrainingError::NonConvergence);
        }
        if max_delta <= tolerance {
            converged = true;
            break;
        }
    }
    if !converged || parameters[1] <= 0.0 {
        return Err(TrainingError::NonConvergence);
    }
    Ok(CalibrationModel {
        intercept: parameters[0],
        slope: parameters[1],
    })
}

fn sigmoid(value: f64) -> f64 {
    if value >= 0.0 {
        1.0 / (1.0 + (-value).exp())
    } else {
        let exp = value.exp();
        exp / (1.0 + exp)
    }
}

fn logistic_loss(logit: f64, positive: bool) -> f64 {
    if logit >= 0.0 {
        let tail = (-logit).exp().ln_1p();
        if positive {
            tail
        } else {
            logit + tail
        }
    } else {
        let tail = logit.exp().ln_1p();
        if positive {
            -logit + tail
        } else {
            tail
        }
    }
}

fn logistic_objective(
    examples: &[&WeightedExample],
    rows: &[Vec<f64>],
    beta: &[f64],
    l2: f64,
    class_weight: f64,
) -> f64 {
    let data_loss = examples
        .iter()
        .zip(rows)
        .map(|(example, row)| {
            let weight = example.weight * if example.positive { class_weight } else { 1.0 };
            let logit = row.iter().zip(beta).map(|(x, b)| x * b).sum();
            weight * logistic_loss(logit, example.positive)
        })
        .sum::<f64>();
    data_loss + 0.5 * l2 * beta[1..].iter().map(|value| value * value).sum::<f64>()
}

fn solve(mut matrix: Vec<Vec<f64>>, mut rhs: Vec<f64>) -> Option<Vec<f64>> {
    for column in 0..rhs.len() {
        let pivot = (column..rhs.len()).max_by(|left, right| {
            matrix[*left][column]
                .abs()
                .total_cmp(&matrix[*right][column].abs())
        })?;
        if matrix[pivot][column].abs() < 1e-12 {
            return None;
        }
        matrix.swap(column, pivot);
        rhs.swap(column, pivot);
        let divisor = matrix[column][column];
        for value in &mut matrix[column][column..] {
            *value /= divisor;
        }
        rhs[column] /= divisor;
        for row in 0..rhs.len() {
            if row == column {
                continue;
            }
            let factor = matrix[row][column];
            let pivot_values = matrix[column][column..].to_vec();
            for (value, pivot_value) in matrix[row][column..].iter_mut().zip(pivot_values) {
                *value -= factor * pivot_value;
            }
            rhs[row] -= factor * rhs[column];
        }
    }
    Some(rhs)
}

fn fit_logistic(
    examples: &[WeightedExample],
    l2: f64,
    class_weight: f64,
    max_iterations: usize,
    tolerance: f64,
) -> Result<LogisticModel, TrainingError> {
    let mut examples: Vec<_> = examples.iter().collect();
    examples.sort_by(|left, right| compare_examples(left, right));
    let first = examples
        .first()
        .ok_or_else(|| TrainingError::InvalidInput("empty dataset".into()))?;
    if !examples.iter().any(|example| example.positive)
        || !examples.iter().any(|example| !example.positive)
    {
        return Err(TrainingError::InvalidInput(
            "logistic fitting needs both classes".into(),
        ));
    }
    let feature_names: Vec<_> = first.features.values.keys().cloned().collect();
    if feature_names.is_empty()
        || examples.iter().any(|example| {
            example.features.values.keys().ne(feature_names.iter())
                || !example.weight.is_finite()
                || example.weight <= 0.0
                || example
                    .features
                    .values
                    .values()
                    .any(|value| !value.is_finite())
        })
    {
        return Err(TrainingError::InvalidInput("invalid example".into()));
    }
    let count = examples.len() as f64;
    let means: Vec<_> = feature_names
        .iter()
        .map(|name| {
            examples
                .iter()
                .map(|example| example.features.values[name])
                .sum::<f64>()
                / count
        })
        .collect();
    let scales: Vec<_> = feature_names
        .iter()
        .enumerate()
        .map(|(index, name)| {
            (examples
                .iter()
                .map(|example| (example.features.values[name] - means[index]).powi(2))
                .sum::<f64>()
                / count)
                .sqrt()
        })
        .collect();
    // A feature that is constant across the training set carries no signal,
    // and constant features are normal here: single-face derived pairs pin
    // size, cohesion, spread, and missing-count features to one value, and a
    // corpus with complete detector metadata pins the missing proportions.
    // Keep such features in the model so scoring keeps taking the full
    // schema: standardize by 1.0 and pin the weight to zero after the fit
    // instead of refusing to train.
    let degenerate: Vec<bool> = scales.iter().map(|scale| *scale <= 1e-12).collect();
    let scales: Vec<_> = scales
        .iter()
        .enumerate()
        .map(|(index, scale)| if degenerate[index] { 1.0 } else { *scale })
        .collect();
    let rows: Vec<Vec<f64>> = examples
        .iter()
        .map(|example| {
            std::iter::once(1.0)
                .chain(feature_names.iter().enumerate().map(|(index, name)| {
                    (example.features.values[name] - means[index]) / scales[index]
                }))
                .collect()
        })
        .collect();
    let mut beta = vec![0.0; feature_names.len() + 1];
    let mut converged = false;
    for _ in 0..max_iterations {
        let mut gradient = vec![0.0; beta.len()];
        let mut hessian = vec![vec![0.0; beta.len()]; beta.len()];
        for (example, row) in examples.iter().zip(&rows) {
            let label = if example.positive { 1.0 } else { 0.0 };
            let weight = example.weight * if example.positive { class_weight } else { 1.0 };
            let probability = sigmoid(row.iter().zip(&beta).map(|(x, b)| x * b).sum());
            for left in 0..beta.len() {
                gradient[left] += weight * (probability - label) * row[left];
                for right in 0..beta.len() {
                    hessian[left][right] +=
                        weight * probability * (1.0 - probability) * row[left] * row[right];
                }
            }
        }
        for index in 1..beta.len() {
            gradient[index] += l2 * beta[index];
            hessian[index][index] += l2;
        }
        hessian[0][0] += 1e-9;
        let delta = solve(hessian, gradient).ok_or(TrainingError::NonConvergence)?;
        let current_objective = logistic_objective(&examples, &rows, &beta, l2, class_weight);
        let mut step = 1.0;
        let mut candidate = beta.clone();
        let mut accepted = false;
        for _ in 0..24 {
            for ((value, parameter), change) in candidate.iter_mut().zip(&beta).zip(&delta) {
                *value = *parameter - step * change;
            }
            let candidate_objective =
                logistic_objective(&examples, &rows, &candidate, l2, class_weight);
            if candidate_objective.is_finite() && candidate_objective <= current_objective {
                accepted = true;
                break;
            }
            step *= 0.5;
        }
        if !accepted {
            return Err(TrainingError::NonConvergence);
        }
        let max_delta = delta
            .iter()
            .map(|value| step * value.abs())
            .fold(0.0, f64::max);
        beta = candidate;
        if beta.iter().any(|value| !value.is_finite()) {
            return Err(TrainingError::NonConvergence);
        }
        if max_delta <= tolerance {
            converged = true;
            break;
        }
    }
    if !converged {
        return Err(TrainingError::NonConvergence);
    }
    // The standardized column of a constant feature is identically zero, so
    // it receives no gradient; pin it anyway so the guarantee does not rely
    // on the algebra of the solve.
    for (index, degenerate) in degenerate.iter().enumerate() {
        if *degenerate {
            beta[index + 1] = 0.0;
        }
    }
    Ok(LogisticModel {
        feature_names,
        means,
        scales,
        intercept: beta[0],
        weights: beta[1..].to_vec(),
        l2,
        positive_class_weight: class_weight,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::face_learning::{
        DecisionKind, DecisionTarget, EventFaceRef, InvalidationReason, LearningAction,
        StoredLearningEvent, ValidationSummary, EVALUATION_PROTOCOL_VERSION,
    };
    use std::collections::BTreeMap;

    fn vector(x: f64, y: f64) -> FeatureVector {
        FeatureVector {
            schema_version: 1,
            values: BTreeMap::from([("x".into(), x), ("y".into(), y)]),
        }
    }
    fn example(identity: &str, positive: bool, x: f64, y: f64) -> WeightedExample {
        WeightedExample {
            source: ExampleSource::ConfirmedLabels,
            event_id: None,
            identity_keys: vec![identity.into()],
            positive,
            weight: 1.0,
            features: vector(x, y),
        }
    }

    fn config() -> TrainingConfig {
        TrainingConfig {
            folds: 4,
            l2_grid: vec![0.01, 1.0],
            positive_class_weight_grid: vec![2.0, 1.0],
            max_iterations: 100,
            tolerance: 1e-8,
            min_positive_identities: 2,
            min_negative_identities: 2,
            ..TrainingConfig::default()
        }
    }

    fn separable_dataset(decision_kind: LearningDecisionKind) -> DecisionDataset {
        let mut examples = Vec::new();
        for index in 0..8 {
            let identity = format!("identity-{index}");
            examples.push(example(
                &identity,
                false,
                -2.0 - index as f64 * 0.1,
                index as f64 * 0.2,
            ));
            examples.push(example(
                &identity,
                true,
                2.0 + index as f64 * 0.1,
                index as f64 * 0.2 + 0.5,
            ));
        }
        DecisionDataset {
            decision_kind,
            examples,
        }
    }

    fn observation(face_id: i64, embedding: [f32; 2], hash: &str) -> FaceObservation {
        FaceObservation {
            face_id,
            embedding: embedding.to_vec(),
            bbox_min_side: Some(100.0 + face_id as f64),
            blur: Some(500.0 + face_id as f64),
            det_score: Some(0.9),
            landmark_residual: Some(0.01),
            photo_hash: hash.to_owned(),
        }
    }

    /// Realistic metadata: sizes, blur (with gaps), confidence, and landmark
    /// residuals all vary the way they do in a scanned library.
    fn varied_observation(face_id: i64, embedding: [f32; 2], hash: &str) -> FaceObservation {
        FaceObservation {
            face_id,
            embedding: embedding.to_vec(),
            bbox_min_side: Some(80.0 + face_id as f64 * 7.0),
            blur: if face_id % 2 == 0 {
                Some(300.0 + face_id as f64 * 111.0)
            } else {
                None
            },
            det_score: Some(0.80 + face_id as f64 * 0.01),
            landmark_residual: Some(0.005 + face_id as f64 * 0.003),
            photo_hash: hash.to_owned(),
        }
    }

    fn stored_event(
        id: i64,
        action: LearningAction,
        decision_kind: LearningDecisionKind,
        outcome: LearningOutcome,
        target_identity: Option<&str>,
        features: FeatureVector,
    ) -> StoredLearningEvent {
        StoredLearningEvent {
            id,
            action,
            decision_kind,
            outcome,
            embedding_model_id: "arcface/model".into(),
            active_profile_id: None,
            target_identity: target_identity.map(str::to_owned),
            support_count: 2,
            scorer_confidence: None,
            eligible: true,
            invalidation_reason: None,
            created_at: "2026-01-01 00:00:00".into(),
            faces: Vec::<EventFaceRef>::new(),
            features,
        }
    }

    #[test]
    fn snapshot_is_balanced_capped_separated_and_order_independent() {
        let observations = vec![
            observation(1, [1.0, 0.0], "h1"),
            observation(2, [0.98, 0.2], "h2"),
            observation(3, [0.0, 1.0], "h3"),
            observation(4, [0.2, 0.98], "h4"),
            observation(5, [-1.0, 0.0], "h5"),
            observation(6, [-0.98, 0.2], "h6"),
            observation(7, [0.0, -1.0], "h7"),
            observation(8, [0.2, -0.98], "h8"),
        ];
        let labels = vec![
            LabeledFace::new(1, "alpha"),
            LabeledFace::new(2, "alpha"),
            LabeledFace::new(3, "beta"),
            LabeledFace::new(4, "beta"),
            LabeledFace::new(5, "gamma"),
            LabeledFace::new(6, "gamma"),
            LabeledFace::new(7, "delta"),
            LabeledFace::new(8, "delta"),
        ];
        let membership_features = extract_membership_features(
            &observations[0..1],
            &observations[1..2],
            DecisionStage::Question,
        )
        .unwrap();
        let cluster_features =
            extract_cluster_quality_features(&observations[0..2], DecisionStage::GalleryCluster)
                .unwrap();
        let mut events = vec![
            stored_event(
                1,
                LearningAction::AssignFace,
                LearningDecisionKind::Membership,
                LearningOutcome::Negative,
                Some("alpha"),
                membership_features.clone(),
            ),
            stored_event(
                2,
                LearningAction::AssignFace,
                LearningDecisionKind::Membership,
                LearningOutcome::Positive,
                Some("alpha"),
                membership_features.clone(),
            ),
            stored_event(
                3,
                LearningAction::AssignFace,
                LearningDecisionKind::Membership,
                LearningOutcome::Positive,
                Some("alpha"),
                membership_features.clone(),
            ),
            stored_event(
                4,
                LearningAction::DissolveCluster,
                LearningDecisionKind::ClusterQuality,
                LearningOutcome::Negative,
                None,
                cluster_features.clone(),
            ),
        ];
        let mut duplicate = events[0].clone();
        duplicate.outcome = LearningOutcome::Positive;
        events.push(duplicate);
        let mut incompatible = events[0].clone();
        incompatible.id = 5;
        incompatible.embedding_model_id = "other/model".into();
        events.push(incompatible);
        let mut wrong_schema = events[0].clone();
        wrong_schema.id = 6;
        wrong_schema.features.schema_version += 1;
        events.push(wrong_schema);
        let mut ineligible = events[0].clone();
        ineligible.id = 7;
        ineligible.eligible = false;
        ineligible.invalidation_reason = Some(InvalidationReason::PersonRemoved);
        events.push(ineligible);
        let mut wrong_feature_set = events[0].clone();
        wrong_feature_set.id = 8;
        wrong_feature_set.features = cluster_features.clone();
        events.push(wrong_feature_set);

        let mut config = config();
        config.max_derived_pairs_per_identity = 1;
        config.max_negative_pairs_per_identity_pair = 1;
        config.max_explicit_examples_per_identity = 2;
        config.explicit_weight = 9.0;
        config.explicit_weight_cap = 4.0;
        let snapshot =
            build_training_snapshot(9, "arcface/model", &labels, &observations, &events, &config)
                .unwrap();

        let explicit_membership: Vec<_> = snapshot
            .membership
            .examples
            .iter()
            .filter(|example| example.source == ExampleSource::ExplicitFeedback)
            .collect();
        assert_eq!(explicit_membership.len(), 2);
        assert!(explicit_membership
            .iter()
            .all(|example| example.weight == 4.0));
        assert_eq!(
            snapshot
                .cluster_quality
                .examples
                .iter()
                .filter(|example| example.source == ExampleSource::ExplicitFeedback
                    && !example.positive)
                .count(),
            1,
            "dissolve remains one cluster-quality example"
        );
        assert!(snapshot
            .membership
            .examples
            .iter()
            .all(|example| { example.features.values.contains_key("subject_size") }));
        assert!(snapshot
            .cluster_quality
            .examples
            .iter()
            .all(|example| { example.features.values.contains_key("cluster_size") }));
        let reasons: BTreeMap<_, _> = snapshot
            .exclusions
            .iter()
            .map(|exclusion| (exclusion.event_id, exclusion.reason.as_str()))
            .collect();
        assert_eq!(reasons[&1], "duplicate_event");
        assert_eq!(reasons[&3], "per_identity_cap");
        assert_eq!(reasons[&5], "embedding_model_mismatch");
        assert_eq!(reasons[&6], "feature_schema_mismatch");
        assert_eq!(reasons[&7], "ineligible");
        assert_eq!(reasons[&8], "decision_feature_mismatch");

        let mut reversed_labels = labels.clone();
        reversed_labels.reverse();
        let mut reversed_observations = observations.clone();
        reversed_observations.reverse();
        let mut reversed_events = events.clone();
        reversed_events.reverse();
        assert_eq!(
            snapshot,
            build_training_snapshot(
                9,
                "arcface/model",
                &reversed_labels,
                &reversed_observations,
                &reversed_events,
                &config,
            )
            .unwrap()
        );
    }

    #[test]
    fn held_out_folds_keep_each_identity_wholly_on_one_side() {
        let dataset = separable_dataset(LearningDecisionKind::Membership);
        let folds = identity_held_out_splits(&dataset, 4, config().seed).unwrap();
        assert_eq!(folds.len(), 4);
        for fold in &folds {
            let train: BTreeSet<_> = fold
                .training
                .iter()
                .flat_map(|example| example.identity_keys.iter())
                .collect();
            let validation: BTreeSet<_> = fold
                .validation
                .iter()
                .flat_map(|example| example.identity_keys.iter())
                .collect();
            assert!(train.is_disjoint(&validation));
            assert!(!fold.training.is_empty());
            assert!(!fold.validation.is_empty());
        }

        let mut reversed = dataset.clone();
        reversed.examples.reverse();
        assert_eq!(
            folds,
            identity_held_out_splits(&reversed, 4, config().seed).unwrap()
        );

        let two_identities = DecisionDataset {
            decision_kind: LearningDecisionKind::Membership,
            examples: vec![example("a", true, 1.0, 0.0), example("b", false, -1.0, 0.0)],
        };
        assert!(matches!(
            identity_held_out_splits(&two_identities, 4, config().seed),
            Err(TrainingError::TooFewIdentities {
                identities: 2,
                folds: 4
            })
        ));
    }

    #[test]
    fn fitting_uses_training_fold_statistics_and_deterministic_grid_order() {
        let mut dataset = separable_dataset(LearningDecisionKind::Membership);
        for example in &mut dataset.examples {
            if example.identity_keys == ["identity-7"] {
                example.features.values.insert("y".into(), 1_000_000.0);
            }
        }
        let folds = identity_held_out_splits(&dataset, 4, config().seed).unwrap();
        let fold = folds
            .iter()
            .find(|fold| {
                fold.validation
                    .iter()
                    .any(|example| example.identity_keys == ["identity-7"])
            })
            .unwrap();
        let fold_model = fit_logistic(&fold.training, 1.0, 1.0, 100, 1e-8).unwrap();
        assert!(fold_model.means[1] < 100.0);

        let scorer = train_scorer(
            &separable_dataset(LearningDecisionKind::Membership),
            &config(),
        )
        .unwrap();
        assert_eq!(scorer.model.l2, 1.0, "precision ties prefer stronger L2");
        assert_eq!(
            scorer.model.positive_class_weight, 1.0,
            "remaining ties use stable numeric parameter order"
        );
        assert!(scorer.calibration.slope.is_finite());
        assert!(scorer.calibration.slope > 0.0);
        assert_ne!(
            scorer.calibration,
            CalibrationModel {
                intercept: 0.0,
                slope: 1.0
            },
            "calibration must be fitted from held-out logits"
        );
        assert!((0.0..=1.0).contains(&scorer.threshold));
    }

    #[test]
    fn bundle_is_byte_deterministic_and_fail_closed() {
        let membership = separable_dataset(LearningDecisionKind::Membership);
        let cluster_quality = separable_dataset(LearningDecisionKind::ClusterQuality);
        let snapshot = TrainingSnapshot {
            generation: 4,
            embedding_model_id: "arcface/model".into(),
            feature_schema_version: FEATURE_SCHEMA_VERSION,
            membership: membership.clone(),
            cluster_quality: cluster_quality.clone(),
            exclusions: Vec::new(),
        };
        let expected = train_logistic_bundle(&snapshot, &config()).unwrap();
        expected.validate().unwrap();

        let mut permuted = snapshot.clone();
        permuted.membership.examples.reverse();
        permuted.cluster_quality.examples.rotate_left(5);
        assert_eq!(
            serde_json::to_vec(&expected).unwrap(),
            serde_json::to_vec(&train_logistic_bundle(&permuted, &config()).unwrap()).unwrap()
        );

        let one_class = DecisionDataset {
            decision_kind: LearningDecisionKind::Membership,
            examples: (0..4)
                .map(|index| example(&format!("p{index}"), true, index as f64, index as f64 + 1.0))
                .collect(),
        };
        assert!(matches!(
            train_scorer(&one_class, &config()),
            Err(TrainingError::InsufficientEvidence { .. })
        ));
        assert!(matches!(
            fit_logistic(&one_class.examples, 1.0, 1.0, 100, 1e-8),
            Err(TrainingError::InvalidInput(_))
        ));

        let too_few_independent = DecisionDataset {
            decision_kind: LearningDecisionKind::Membership,
            examples: vec![
                example("only-positive", true, 2.0, 3.0),
                example("only-negative", false, -2.0, -3.0),
            ],
        };
        assert!(matches!(
            train_scorer(&too_few_independent, &config()),
            Err(TrainingError::InsufficientEvidence {
                positive_identities: 1,
                negative_identities: 1,
                ..
            })
        ));

        let mut constant = membership.examples.clone();
        for example in &mut constant {
            example.features.values.insert("y".into(), 1.0);
        }
        let pinned = fit_logistic(&constant, 1.0, 1.0, 100, 1e-8).unwrap();
        let y = pinned
            .feature_names
            .iter()
            .position(|name| name == "y")
            .unwrap();
        assert_eq!(pinned.weights[y], 0.0, "a constant feature must not vote");
        assert_eq!(pinned.scales[y], 1.0);

        let mut non_finite = membership.examples.clone();
        non_finite[0].features.values.insert("x".into(), f64::NAN);
        assert!(matches!(
            fit_logistic(&non_finite, 1.0, 1.0, 100, 1e-8),
            Err(TrainingError::InvalidInput(_))
        ));
        assert!(matches!(
            fit_logistic(&membership.examples, 1.0, 1.0, 1, 1e-16),
            Err(TrainingError::NonConvergence)
        ));

        let mut incompatible = snapshot;
        incompatible.feature_schema_version += 1;
        assert!(matches!(
            train_logistic_bundle(&incompatible, &config()),
            Err(TrainingError::InvalidInput(_))
        ));

        let mut unbounded = config();
        unbounded.max_iterations = 10_001;
        assert!(matches!(
            train_logistic_bundle(&permuted, &unbounded),
            Err(TrainingError::InvalidConfig(_))
        ));

        let mut underweighted_feedback = config();
        underweighted_feedback.explicit_weight_cap = 1.0;
        assert!(matches!(
            train_logistic_bundle(&permuted, &underweighted_feedback),
            Err(TrainingError::InvalidConfig(_))
        ));
    }

    #[test]
    fn stable_log_loss_handles_extreme_logits() {
        for (logit, positive) in [
            (1_000.0, true),
            (1_000.0, false),
            (-1_000.0, true),
            (-1_000.0, false),
        ] {
            assert!(logistic_loss(logit, positive).is_finite());
            assert!(logistic_loss(logit, positive) >= 0.0);
        }
    }

    #[test]
    fn deterministic_fit_and_constant_feature_pin() {
        let examples = vec![
            example("a", false, -2.0, 0.0),
            example("b", false, -1.0, 1.0),
            example("c", true, 1.0, 2.0),
            example("d", true, 2.0, 3.0),
        ];
        let model = fit_logistic(&examples, 1.0, 1.0, 100, 1e-8).unwrap();
        let mut reversed = examples.clone();
        reversed.reverse();
        assert_eq!(model, fit_logistic(&reversed, 1.0, 1.0, 100, 1e-8).unwrap());
        let mut constant = examples;
        for example in &mut constant {
            example.features.values.insert("y".into(), 1.0);
        }
        let pinned = fit_logistic(&constant, 1.0, 1.0, 100, 1e-8).unwrap();
        let y = pinned
            .feature_names
            .iter()
            .position(|name| name == "y")
            .unwrap();
        assert_eq!(pinned.weights[y], 0.0);
        assert_eq!(pinned.scales[y], 1.0);
        assert!(pinned.weights[0] != 0.0, "the varying feature still votes");
    }

    #[test]
    fn example_ordering_breaks_ties_by_feature_values() {
        let first = example("a", true, 1.0, 2.0);
        let mut second = example("a", true, 1.0, 3.0);
        assert_eq!(
            compare_examples(&first, &second),
            std::cmp::Ordering::Less,
            "same keys and label order by the first differing feature value"
        );
        second.features.values.insert("x".into(), 0.5);
        assert_eq!(
            compare_examples(&first, &second),
            std::cmp::Ordering::Greater
        );
        second.features.values.insert("x".into(), 1.0);
        second.features.values.insert("y".into(), 2.0);
        assert_eq!(compare_examples(&first, &second), std::cmp::Ordering::Equal);
    }

    #[test]
    fn sweep_matches_brute_force_threshold_selection() {
        let scores: Vec<(f64, bool, f64)> = vec![
            (0.95, true, 1.0),
            (0.95, false, 2.0),
            (0.90, true, 1.0),
            (0.80, false, 1.0),
            (0.70, true, 3.0),
            (0.60, false, 1.5),
            (0.60, false, 0.5),
            (0.40, true, 1.0),
            (0.20, false, 1.0),
            (0.10, true, 1.0),
        ];
        let (threshold, precision, recall) = select_threshold(&scores).unwrap();

        let mut thresholds: Vec<_> = scores.iter().map(|(score, _, _)| *score).collect();
        thresholds.sort_by(|left, right| right.total_cmp(left));
        thresholds.dedup_by(|left, right| left.total_cmp(right).is_eq());
        let positive_weight: f64 = scores
            .iter()
            .filter(|(_, positive, _)| *positive)
            .map(|(_, _, weight)| weight)
            .sum();
        let mut reference: Option<(f64, f64, f64)> = None;
        for candidate_threshold in thresholds {
            let mut true_positive = 0.0;
            let mut false_positive = 0.0;
            for (score, positive, weight) in &scores {
                if *score >= candidate_threshold {
                    if *positive {
                        true_positive += *weight;
                    } else {
                        false_positive += *weight;
                    }
                }
            }
            let predicted = true_positive + false_positive;
            if predicted == 0.0 {
                continue;
            }
            let candidate = (
                candidate_threshold,
                true_positive / predicted,
                true_positive / positive_weight,
            );
            let replace = reference.as_ref().is_none_or(|current| {
                candidate
                    .1
                    .total_cmp(&current.1)
                    .then(candidate.2.total_cmp(&current.2))
                    .then(candidate.0.total_cmp(&current.0))
                    .is_gt()
            });
            if replace {
                reference = Some(candidate);
            }
        }
        let (expected_threshold, expected_precision, expected_recall) = reference.unwrap();
        assert!((threshold - expected_threshold).abs() < 1e-12);
        assert!((precision - expected_precision).abs() < 1e-12);
        assert!((recall - expected_recall).abs() < 1e-12);
    }

    #[test]
    fn labels_and_feedback_snapshot_trains_end_to_end() {
        let observations = vec![
            varied_observation(1, [1.0, 0.0], "h1"),
            varied_observation(2, [0.98, 0.2], "h2"),
            varied_observation(3, [0.0, 1.0], "h3"),
            varied_observation(4, [0.2, 0.98], "h4"),
            varied_observation(5, [-1.0, 0.0], "h5"),
            varied_observation(6, [-0.98, 0.2], "h6"),
            varied_observation(7, [0.0, -1.0], "h7"),
            varied_observation(8, [0.2, -0.98], "h8"),
        ];
        let labels = vec![
            LabeledFace::new(1, "alpha"),
            LabeledFace::new(2, "alpha"),
            LabeledFace::new(3, "beta"),
            LabeledFace::new(4, "beta"),
            LabeledFace::new(5, "gamma"),
            LabeledFace::new(6, "gamma"),
            LabeledFace::new(7, "delta"),
            LabeledFace::new(8, "delta"),
        ];
        let cluster_features =
            extract_cluster_quality_features(&observations[0..2], DecisionStage::Question).unwrap();
        let events = vec![
            stored_event(
                1,
                LearningAction::DissolveCluster,
                LearningDecisionKind::ClusterQuality,
                LearningOutcome::Negative,
                None,
                cluster_features.clone(),
            ),
            stored_event(
                2,
                LearningAction::DissolveCluster,
                LearningDecisionKind::ClusterQuality,
                LearningOutcome::Negative,
                None,
                cluster_features,
            ),
        ];
        let config = TrainingConfig::default();
        let snapshot =
            build_training_snapshot(3, "arcface/model", &labels, &observations, &events, &config)
                .unwrap();
        let bundle = train_logistic_bundle(&snapshot, &config).unwrap();
        bundle.validate().unwrap();

        let pinned = |model: &LogisticModel, name: &str| {
            let index = model
                .feature_names
                .iter()
                .position(|key| key == name)
                .unwrap();
            (model.weights[index], model.scales[index])
        };
        // Derived single-face pairs pin size, cohesion, and missing-count
        // features to one value; they must stay in the schema without voting.
        assert_eq!(pinned(&bundle.membership.model, "subject_size"), (0.0, 1.0));
        assert_eq!(pinned(&bundle.membership.model, "target_size"), (0.0, 1.0));
        assert_eq!(
            pinned(
                &bundle.membership.model,
                "detector_confidence_missing_proportion_subject"
            ),
            (0.0, 1.0)
        );
        assert!(
            pinned(&bundle.membership.model, "similarity_mean").0 != 0.0,
            "similarity must still carry the model"
        );
        assert_eq!(
            pinned(&bundle.cluster_quality.model, "cluster_size"),
            (0.0, 1.0)
        );

        let evidence = bundle
            .membership
            .score_with_evidence(
                &snapshot.membership.examples[0].features,
                1,
                FEATURE_SCHEMA_VERSION,
                DecisionKind::Membership,
                vec![1],
                DecisionTarget::Person("alpha".into()),
                Vec::new(),
                ValidationSummary {
                    protocol_version: EVALUATION_PROTOCOL_VERSION,
                    datasets: 1,
                    pair_precision: Some(0.9),
                    pair_recall: Some(0.9),
                    suggestion_precision: None,
                    suggestion_coverage: None,
                },
            )
            .unwrap();
        evidence.validate().unwrap();
    }

    #[test]
    fn labels_without_cluster_feedback_reports_what_is_missing() {
        // Derived cluster-quality examples are all positive, so teaching that
        // scorer needs explicit negative corrections. A labels-only corpus
        // must say exactly that instead of failing on an unrelated detail.
        let observations = vec![
            varied_observation(1, [1.0, 0.0], "h1"),
            varied_observation(2, [0.98, 0.2], "h2"),
            varied_observation(3, [0.0, 1.0], "h3"),
            varied_observation(4, [0.2, 0.98], "h4"),
            varied_observation(5, [-1.0, 0.0], "h5"),
            varied_observation(6, [-0.98, 0.2], "h6"),
            varied_observation(7, [0.0, -1.0], "h7"),
            varied_observation(8, [0.2, -0.98], "h8"),
        ];
        let labels = vec![
            LabeledFace::new(1, "alpha"),
            LabeledFace::new(2, "alpha"),
            LabeledFace::new(3, "beta"),
            LabeledFace::new(4, "beta"),
            LabeledFace::new(5, "gamma"),
            LabeledFace::new(6, "gamma"),
            LabeledFace::new(7, "delta"),
            LabeledFace::new(8, "delta"),
        ];
        let config = TrainingConfig::default();
        let snapshot =
            build_training_snapshot(1, "arcface/model", &labels, &observations, &[], &config)
                .unwrap();
        assert!(matches!(
            train_logistic_bundle(&snapshot, &config),
            Err(TrainingError::InsufficientEvidence {
                decision_kind: LearningDecisionKind::ClusterQuality,
                positive_identities: 4,
                negative_identities: 0,
            })
        ));
    }
}
