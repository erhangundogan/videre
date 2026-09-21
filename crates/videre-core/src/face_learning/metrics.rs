use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

pub const EVALUATION_PROTOCOL_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LabeledFace {
    pub face_id: i64,
    pub identity: String,
}

impl LabeledFace {
    pub fn new(face_id: i64, identity: impl Into<String>) -> Self {
        Self {
            face_id,
            identity: identity.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClusterPrediction {
    pub face_id: i64,
    pub cluster_id: Option<i64>,
}

impl ClusterPrediction {
    pub fn new(face_id: i64, cluster_id: Option<i64>) -> Self {
        Self {
            face_id,
            cluster_id,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SuggestionPrediction {
    pub face_id: i64,
    pub identity: Option<String>,
    pub confidence: f64,
}

impl SuggestionPrediction {
    pub fn new<S: Into<String>>(face_id: i64, identity: Option<S>, confidence: f64) -> Self {
        Self {
            face_id,
            identity: identity.map(Into::into),
            confidence,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CandidatePredictions {
    pub clusters: Vec<ClusterPrediction>,
    pub suggestions: Vec<SuggestionPrediction>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClusteringMetrics {
    pub labeled_faces: usize,
    pub labeled_identities: usize,
    pub predicted_clusters: usize,
    pub true_positive_pairs: u64,
    pub false_positive_pairs: u64,
    pub false_negative_pairs: u64,
    pub pair_precision: Option<f64>,
    pub pair_recall: Option<f64>,
    pub pair_f1: Option<f64>,
    pub mixed_clusters: usize,
    pub fragmented_identities: usize,
    pub unassigned_labeled_faces: usize,
    pub unassigned_rate: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SuggestionMetrics {
    pub correct: usize,
    pub incorrect: usize,
    pub not_suggested: usize,
    pub precision: Option<f64>,
    pub coverage: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DatasetEvaluation {
    pub protocol_version: u32,
    pub clustering: ClusteringMetrics,
    pub suggestions: Option<SuggestionMetrics>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EvaluationError {
    NoLabeledFaces,
    DuplicateTruthFaceId(i64),
    DuplicateClusterPrediction(i64),
    DuplicateSuggestionPrediction(i64),
    MissingClusterPrediction(i64),
    UnknownPredictionFaceId(i64),
    NonFiniteSuggestionConfidence(i64),
    SuggestionConfidenceOutOfRange(i64),
}

impl fmt::Display for EvaluationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoLabeledFaces => write!(f, "no confirmed labeled faces are available"),
            Self::DuplicateTruthFaceId(id) => write!(f, "duplicate truth face id {id}"),
            Self::DuplicateClusterPrediction(id) => {
                write!(f, "duplicate cluster prediction for face {id}")
            }
            Self::DuplicateSuggestionPrediction(id) => {
                write!(f, "duplicate suggestion prediction for face {id}")
            }
            Self::MissingClusterPrediction(id) => {
                write!(f, "missing cluster prediction for labeled face {id}")
            }
            Self::UnknownPredictionFaceId(id) => {
                write!(f, "prediction references unknown face {id}")
            }
            Self::NonFiniteSuggestionConfidence(id) => {
                write!(f, "suggestion confidence for face {id} is not finite")
            }
            Self::SuggestionConfidenceOutOfRange(id) => {
                write!(f, "suggestion confidence for face {id} is outside 0..=1")
            }
        }
    }
}

impl std::error::Error for EvaluationError {}

fn choose_two(n: usize) -> u64 {
    let n = n as u64;
    n.saturating_mul(n.saturating_sub(1)) / 2
}

fn ratio(numerator: u64, denominator: u64) -> Option<f64> {
    (denominator != 0).then(|| numerator as f64 / denominator as f64)
}

pub fn evaluate_candidate(
    labels: &[LabeledFace],
    predictions: &CandidatePredictions,
) -> Result<DatasetEvaluation, EvaluationError> {
    if labels.is_empty() {
        return Err(EvaluationError::NoLabeledFaces);
    }

    let mut truth = BTreeMap::new();
    for label in labels {
        if truth
            .insert(label.face_id, label.identity.as_str())
            .is_some()
        {
            return Err(EvaluationError::DuplicateTruthFaceId(label.face_id));
        }
    }

    let mut clusters = BTreeMap::new();
    for prediction in &predictions.clusters {
        if !truth.contains_key(&prediction.face_id) {
            return Err(EvaluationError::UnknownPredictionFaceId(prediction.face_id));
        }
        if clusters
            .insert(prediction.face_id, prediction.cluster_id)
            .is_some()
        {
            return Err(EvaluationError::DuplicateClusterPrediction(
                prediction.face_id,
            ));
        }
    }
    for face_id in truth.keys() {
        if !clusters.contains_key(face_id) {
            return Err(EvaluationError::MissingClusterPrediction(*face_id));
        }
    }

    let mut suggestions = BTreeMap::new();
    for prediction in &predictions.suggestions {
        if !truth.contains_key(&prediction.face_id) {
            return Err(EvaluationError::UnknownPredictionFaceId(prediction.face_id));
        }
        if !prediction.confidence.is_finite() {
            return Err(EvaluationError::NonFiniteSuggestionConfidence(
                prediction.face_id,
            ));
        }
        if !(0.0..=1.0).contains(&prediction.confidence) {
            return Err(EvaluationError::SuggestionConfidenceOutOfRange(
                prediction.face_id,
            ));
        }
        if suggestions.insert(prediction.face_id, prediction).is_some() {
            return Err(EvaluationError::DuplicateSuggestionPrediction(
                prediction.face_id,
            ));
        }
    }

    let mut identity_counts: BTreeMap<&str, usize> = BTreeMap::new();
    let mut cluster_counts: BTreeMap<i64, usize> = BTreeMap::new();
    let mut cell_counts: BTreeMap<(i64, &str), usize> = BTreeMap::new();
    let mut cluster_identities: BTreeMap<i64, BTreeSet<&str>> = BTreeMap::new();
    let mut identity_outcomes: BTreeMap<&str, BTreeSet<Option<i64>>> = BTreeMap::new();
    let mut unassigned_labeled_faces = 0usize;

    for (face_id, identity) in &truth {
        *identity_counts.entry(identity).or_default() += 1;
        let cluster_id = clusters[face_id];
        identity_outcomes
            .entry(identity)
            .or_default()
            .insert(cluster_id);
        match cluster_id {
            Some(cluster_id) => {
                *cluster_counts.entry(cluster_id).or_default() += 1;
                *cell_counts.entry((cluster_id, identity)).or_default() += 1;
                cluster_identities
                    .entry(cluster_id)
                    .or_default()
                    .insert(identity);
            }
            None => unassigned_labeled_faces += 1,
        }
    }

    let true_positive_pairs = cell_counts.values().map(|count| choose_two(*count)).sum();
    let predicted_positive_pairs: u64 = cluster_counts
        .values()
        .map(|count| choose_two(*count))
        .sum();
    let actual_positive_pairs: u64 = identity_counts
        .values()
        .map(|count| choose_two(*count))
        .sum();
    let false_positive_pairs = predicted_positive_pairs - true_positive_pairs;
    let false_negative_pairs = actual_positive_pairs - true_positive_pairs;
    let pair_precision = ratio(true_positive_pairs, predicted_positive_pairs);
    let pair_recall = ratio(true_positive_pairs, actual_positive_pairs);
    let pair_f1 = match (pair_precision, pair_recall) {
        (Some(precision), Some(recall)) if precision + recall > 0.0 => {
            Some(2.0 * precision * recall / (precision + recall))
        }
        (Some(_), Some(_)) => Some(0.0),
        _ => None,
    };

    let clustering = ClusteringMetrics {
        labeled_faces: truth.len(),
        labeled_identities: identity_counts.len(),
        predicted_clusters: cluster_counts.len(),
        true_positive_pairs,
        false_positive_pairs,
        false_negative_pairs,
        pair_precision,
        pair_recall,
        pair_f1,
        mixed_clusters: cluster_identities
            .values()
            .filter(|identities| identities.len() > 1)
            .count(),
        fragmented_identities: identity_outcomes
            .values()
            .filter(|outcomes| outcomes.len() > 1)
            .count(),
        unassigned_labeled_faces,
        unassigned_rate: unassigned_labeled_faces as f64 / truth.len() as f64,
    };

    let suggestion_metrics = if predictions.suggestions.is_empty() {
        None
    } else {
        let mut correct = 0usize;
        let mut incorrect = 0usize;
        let mut explicit_none = 0usize;
        for (face_id, prediction) in &suggestions {
            match prediction.identity.as_deref() {
                Some(identity) if Some(identity) == truth.get(face_id).copied() => correct += 1,
                Some(_) => incorrect += 1,
                None => explicit_none += 1,
            }
        }
        let suggested = correct + incorrect;
        let not_suggested = truth.len() - suggestions.len() + explicit_none;
        Some(SuggestionMetrics {
            correct,
            incorrect,
            not_suggested,
            precision: (suggested != 0).then(|| correct as f64 / suggested as f64),
            coverage: suggested as f64 / truth.len() as f64,
        })
    };

    Ok(DatasetEvaluation {
        protocol_version: EVALUATION_PROTOCOL_VERSION,
        clustering,
        suggestions: suggestion_metrics,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (Vec<LabeledFace>, CandidatePredictions) {
        (
            vec![
                LabeledFace::new(1, "ada"),
                LabeledFace::new(2, "ada"),
                LabeledFace::new(3, "grace"),
                LabeledFace::new(4, "grace"),
            ],
            CandidatePredictions {
                clusters: vec![
                    ClusterPrediction::new(1, Some(10)),
                    ClusterPrediction::new(2, Some(10)),
                    ClusterPrediction::new(3, Some(10)),
                    ClusterPrediction::new(4, None),
                ],
                suggestions: vec![
                    SuggestionPrediction::new(1, Some("ada"), 0.95),
                    SuggestionPrediction::new(2, None::<String>, 0.40),
                    SuggestionPrediction::new(3, Some("ada"), 0.80),
                    SuggestionPrediction::new(4, None::<String>, 0.30),
                ],
            },
        )
    }

    #[test]
    fn evaluates_cluster_and_suggestion_predictions() {
        let (labels, predictions) = fixture();
        let report = evaluate_candidate(&labels, &predictions).unwrap();

        assert_eq!(report.clustering.true_positive_pairs, 1);
        assert_eq!(report.clustering.false_positive_pairs, 2);
        assert_eq!(report.clustering.false_negative_pairs, 1);
        assert_eq!(report.clustering.mixed_clusters, 1);
        assert_eq!(report.clustering.fragmented_identities, 1);
        assert_eq!(report.clustering.unassigned_labeled_faces, 1);
        assert_eq!(report.clustering.pair_precision, Some(1.0 / 3.0));
        assert_eq!(report.clustering.pair_recall, Some(0.5));
        assert_eq!(report.clustering.unassigned_rate, 0.25);

        let suggestions = report.suggestions.unwrap();
        assert_eq!(suggestions.correct, 1);
        assert_eq!(suggestions.incorrect, 1);
        assert_eq!(suggestions.not_suggested, 2);
        assert_eq!(suggestions.precision, Some(0.5));
        assert_eq!(suggestions.coverage, 0.5);
    }

    #[test]
    fn evaluation_is_independent_of_input_order() {
        let (labels, predictions) = fixture();
        let expected = evaluate_candidate(&labels, &predictions).unwrap();

        let mut reversed_labels = labels.clone();
        reversed_labels.reverse();
        let mut reversed_predictions = predictions.clone();
        reversed_predictions.clusters.reverse();
        reversed_predictions.suggestions.rotate_left(1);

        assert_eq!(
            evaluate_candidate(&reversed_labels, &reversed_predictions).unwrap(),
            expected
        );
    }

    #[test]
    fn duplicate_truth_ids_are_rejected() {
        let (mut labels, predictions) = fixture();
        labels.push(LabeledFace::new(1, "ada"));
        assert_eq!(
            evaluate_candidate(&labels, &predictions),
            Err(EvaluationError::DuplicateTruthFaceId(1))
        );
    }

    #[test]
    fn duplicate_cluster_predictions_are_rejected() {
        let (labels, mut predictions) = fixture();
        predictions
            .clusters
            .push(ClusterPrediction::new(1, Some(99)));
        assert_eq!(
            evaluate_candidate(&labels, &predictions),
            Err(EvaluationError::DuplicateClusterPrediction(1))
        );
    }

    #[test]
    fn duplicate_suggestion_predictions_are_rejected() {
        let (labels, mut predictions) = fixture();
        predictions
            .suggestions
            .push(SuggestionPrediction::new(1, Some("ada"), 0.99));
        assert_eq!(
            evaluate_candidate(&labels, &predictions),
            Err(EvaluationError::DuplicateSuggestionPrediction(1))
        );
    }

    #[test]
    fn missing_cluster_prediction_is_rejected() {
        let (labels, mut predictions) = fixture();
        predictions
            .clusters
            .retain(|prediction| prediction.face_id != 4);
        assert_eq!(
            evaluate_candidate(&labels, &predictions),
            Err(EvaluationError::MissingClusterPrediction(4))
        );
    }

    #[test]
    fn unknown_prediction_face_is_rejected() {
        let (labels, mut predictions) = fixture();
        predictions.clusters.push(ClusterPrediction::new(99, None));
        assert_eq!(
            evaluate_candidate(&labels, &predictions),
            Err(EvaluationError::UnknownPredictionFaceId(99))
        );
    }

    #[test]
    fn non_finite_suggestion_confidence_is_rejected() {
        let (labels, mut predictions) = fixture();
        predictions.suggestions[0].confidence = f64::NAN;
        assert_eq!(
            evaluate_candidate(&labels, &predictions),
            Err(EvaluationError::NonFiniteSuggestionConfidence(1))
        );
    }

    #[test]
    fn no_labeled_faces_is_rejected() {
        assert_eq!(
            evaluate_candidate(
                &[],
                &CandidatePredictions {
                    clusters: Vec::new(),
                    suggestions: Vec::new(),
                }
            ),
            Err(EvaluationError::NoLabeledFaces)
        );
    }

    #[test]
    fn pair_metrics_are_undefined_without_positive_or_predicted_pairs() {
        let labels = vec![LabeledFace::new(1, "ada"), LabeledFace::new(2, "grace")];
        let predictions = CandidatePredictions {
            clusters: vec![
                ClusterPrediction::new(1, None),
                ClusterPrediction::new(2, None),
            ],
            suggestions: vec![
                SuggestionPrediction::new(1, Some("ada"), 0.9),
                SuggestionPrediction::new(2, None::<String>, 0.1),
            ],
        };

        let report = evaluate_candidate(&labels, &predictions).unwrap();
        assert_eq!(report.clustering.pair_precision, None);
        assert_eq!(report.clustering.pair_recall, None);
        assert_eq!(report.suggestions.unwrap().precision, Some(1.0));
    }

    #[test]
    fn empty_suggestion_input_serializes_as_null() {
        let labels = vec![LabeledFace::new(1, "ada")];
        let predictions = CandidatePredictions {
            clusters: vec![ClusterPrediction::new(1, None)],
            suggestions: Vec::new(),
        };

        let report = evaluate_candidate(&labels, &predictions).unwrap();
        assert_eq!(report.suggestions, None);
        let json = serde_json::to_value(report).unwrap();
        assert_eq!(json["suggestions"], serde_json::Value::Null);
    }
}
