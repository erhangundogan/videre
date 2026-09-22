use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// Current schema for persisted face-learning feature vectors.
pub const FEATURE_SCHEMA_VERSION: u32 = 1;
/// Canonical ArcFace 112x112 template landmarks in `(x, y)` order.
pub const ARCFACE_LANDMARK_TEMPLATE: [[f32; 2]; 5] = [
    [38.2946, 51.6963],
    [73.5318, 51.5014],
    [56.0252, 71.7366],
    [41.5493, 92.3655],
    [70.7299, 92.2041],
];

/// Complete, lexicographically ordered membership feature schema.
pub const MEMBERSHIP_FEATURE_NAMES: &[&str] = &[
    "blur_mean_subject",
    "blur_mean_target",
    "blur_missing_proportion_subject",
    "blur_missing_proportion_target",
    "centroid_similarity",
    "decision_stage",
    "detector_confidence_mean_subject",
    "detector_confidence_mean_target",
    "detector_confidence_missing_proportion_subject",
    "detector_confidence_missing_proportion_target",
    "face_size_mean_subject",
    "face_size_mean_target",
    "face_size_missing_proportion_subject",
    "face_size_missing_proportion_target",
    "landmark_residual_mean_subject",
    "landmark_residual_mean_target",
    "landmark_residual_missing_proportion_subject",
    "landmark_residual_missing_proportion_target",
    "reciprocal_rank_mean",
    "same_photo_proportion",
    "similarity_lower_quartile",
    "similarity_mean",
    "similarity_median",
    "similarity_spread",
    "similarity_strongest",
    "similarity_weakest",
    "subject_cohesion_mean",
    "subject_outlier_proportion",
    "subject_similarity_spread",
    "subject_size",
    "support_count_ge_0_50",
    "support_count_ge_0_65",
    "support_count_ge_0_75",
    "support_proportion_ge_0_50",
    "support_proportion_ge_0_65",
    "support_proportion_ge_0_75",
    "target_cohesion_mean",
    "target_outlier_proportion",
    "target_similarity_spread",
    "target_size",
];

/// Complete, lexicographically ordered cluster-quality feature schema.
pub const CLUSTER_QUALITY_FEATURE_NAMES: &[&str] = &[
    "blur_mean",
    "blur_missing_proportion",
    "cluster_size",
    "cohesion_mean",
    "decision_stage",
    "detector_confidence_mean",
    "detector_confidence_missing_proportion",
    "face_size_mean",
    "face_size_missing_proportion",
    "landmark_residual_mean",
    "landmark_residual_missing_proportion",
    "nearest_neighbor_mean",
    "outlier_proportion",
    "quality_missing_proportion",
    "same_photo_proportion",
    "similarity_lower_quartile",
    "similarity_median",
    "similarity_spread",
    "similarity_strongest",
    "similarity_weakest",
    "support_proportion_ge_0_50",
    "support_proportion_ge_0_65",
    "support_proportion_ge_0_75",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
/// Stage at which a face-learning decision was observed.
pub enum DecisionStage {
    GallerySingleton,
    GalleryCluster,
    Question,
    Merge,
    Attachment,
}

impl DecisionStage {
    fn numeric(self) -> f64 {
        match self {
            Self::GallerySingleton => 0.0,
            Self::GalleryCluster => 1.0,
            Self::Question => 2.0,
            Self::Merge => 3.0,
            Self::Attachment => 4.0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
/// Generic inputs needed to derive a decision feature vector for one face.
pub struct FaceObservation {
    pub face_id: i64,
    pub embedding: Vec<f32>,
    pub bbox_min_side: Option<f64>,
    pub blur: Option<f64>,
    pub det_score: Option<f64>,
    pub landmark_residual: Option<f64>,
    pub photo_hash: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
/// Versioned scalar inputs for an interpretable face-learning decision.
pub struct FeatureVector {
    pub schema_version: u32,
    pub values: BTreeMap<String, f64>,
}

#[derive(Clone, Debug, PartialEq)]
/// Validation and extraction failures that make an observation ineligible.
pub enum FeatureError {
    EmptySubject,
    EmptyTarget,
    TooFewClusterFaces,
    DuplicateFaceId(i64),
    EmbeddingDimensionMismatch,
    NonFiniteEmbedding(i64),
    ZeroNormEmbedding(i64),
    InvalidQuality { face_id: i64, field: &'static str },
    UnsupportedSchemaVersion(u32),
    InvalidFeatureSet,
    NonFiniteFeature(String),
    Json(String),
}

impl fmt::Display for FeatureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptySubject => write!(f, "membership subject is empty"),
            Self::EmptyTarget => write!(f, "membership target is empty"),
            Self::TooFewClusterFaces => write!(f, "cluster quality needs at least two faces"),
            Self::DuplicateFaceId(id) => write!(f, "face {id} appears more than once"),
            Self::EmbeddingDimensionMismatch => write!(f, "face embedding dimensions differ"),
            Self::NonFiniteEmbedding(id) => write!(f, "face {id} has a non-finite embedding"),
            Self::ZeroNormEmbedding(id) => write!(f, "face {id} has a zero-norm embedding"),
            Self::InvalidQuality { face_id, field } => {
                write!(f, "face {face_id} has invalid {field}")
            }
            Self::UnsupportedSchemaVersion(version) => {
                write!(f, "unsupported face feature schema version {version}")
            }
            Self::InvalidFeatureSet => write!(f, "face feature names do not match the schema"),
            Self::NonFiniteFeature(name) => write!(f, "face feature {name} is not finite"),
            Self::Json(message) => write!(f, "face feature JSON error: {message}"),
        }
    }
}

impl std::error::Error for FeatureError {}

impl FeatureVector {
    /// Reject feature vectors that do not exactly match the current finite schema.
    pub fn validate(&self) -> Result<(), FeatureError> {
        if self.schema_version != FEATURE_SCHEMA_VERSION {
            return Err(FeatureError::UnsupportedSchemaVersion(self.schema_version));
        }
        for (name, value) in &self.values {
            if !value.is_finite() {
                return Err(FeatureError::NonFiniteFeature(name.clone()));
            }
        }
        let names: Vec<_> = self.values.keys().map(String::as_str).collect();
        if names != MEMBERSHIP_FEATURE_NAMES && names != CLUSTER_QUALITY_FEATURE_NAMES {
            return Err(FeatureError::InvalidFeatureSet);
        }
        Ok(())
    }

    /// Serialize a validated vector with a deterministic key order.
    pub fn to_canonical_json(&self) -> Result<String, FeatureError> {
        self.validate()?;
        serde_json::to_string(self).map_err(|error| FeatureError::Json(error.to_string()))
    }
}

#[derive(Clone, Copy)]
struct Distribution {
    strongest: f64,
    median: f64,
    lower_quartile: f64,
    weakest: f64,
    mean: f64,
    spread: f64,
}

fn distribution(mut values: Vec<f64>) -> Distribution {
    values.sort_by(f64::total_cmp);
    let len = values.len();
    let median = if len.is_multiple_of(2) {
        (values[len / 2 - 1] + values[len / 2]) / 2.0
    } else {
        values[len / 2]
    };
    let lower_quartile = values[((len - 1) as f64 * 0.25).floor() as usize];
    let weakest = values[0];
    let strongest = values[len - 1];
    Distribution {
        strongest,
        median,
        lower_quartile,
        weakest,
        mean: values.iter().sum::<f64>() / len as f64,
        spread: strongest - weakest,
    }
}

fn validate_faces(groups: &[&[FaceObservation]]) -> Result<usize, FeatureError> {
    let mut ids = BTreeSet::new();
    let mut dimension = None;
    for face in groups.iter().flat_map(|group| group.iter()) {
        if !ids.insert(face.face_id) {
            return Err(FeatureError::DuplicateFaceId(face.face_id));
        }
        if face.embedding.is_empty() {
            return Err(FeatureError::ZeroNormEmbedding(face.face_id));
        }
        if dimension
            .replace(face.embedding.len())
            .is_some_and(|known| known != face.embedding.len())
        {
            return Err(FeatureError::EmbeddingDimensionMismatch);
        }
        if face.embedding.iter().any(|value| !value.is_finite()) {
            return Err(FeatureError::NonFiniteEmbedding(face.face_id));
        }
        let norm = face
            .embedding
            .iter()
            .map(|value| f64::from(*value).powi(2))
            .sum::<f64>()
            .sqrt();
        if norm <= f64::EPSILON {
            return Err(FeatureError::ZeroNormEmbedding(face.face_id));
        }
        for (field, value, probability) in [
            ("face_size", face.bbox_min_side, false),
            ("blur", face.blur, false),
            ("detector_confidence", face.det_score, true),
            ("landmark_residual", face.landmark_residual, false),
        ] {
            if value.is_some_and(|value| {
                !value.is_finite() || value < 0.0 || (probability && value > 1.0)
            }) {
                return Err(FeatureError::InvalidQuality {
                    face_id: face.face_id,
                    field,
                });
            }
        }
    }
    Ok(dimension.unwrap_or(0))
}

fn cosine(left: &FaceObservation, right: &FaceObservation) -> f64 {
    let dot = left
        .embedding
        .iter()
        .zip(&right.embedding)
        .map(|(left, right)| f64::from(*left) * f64::from(*right))
        .sum::<f64>();
    let left_norm = left
        .embedding
        .iter()
        .map(|value| f64::from(*value).powi(2))
        .sum::<f64>()
        .sqrt();
    let right_norm = right
        .embedding
        .iter()
        .map(|value| f64::from(*value).powi(2))
        .sum::<f64>()
        .sqrt();
    dot / (left_norm * right_norm)
}

fn centroid(group: &[FaceObservation], dimension: usize) -> Vec<f64> {
    let mut result = vec![0.0; dimension];
    for face in group {
        for (slot, value) in result.iter_mut().zip(&face.embedding) {
            *slot += f64::from(*value);
        }
    }
    for value in &mut result {
        *value /= group.len() as f64;
    }
    result
}

fn vector_cosine(left: &[f64], right: &[f64]) -> f64 {
    let dot = left.iter().zip(right).map(|(a, b)| a * b).sum::<f64>();
    let left_norm = left.iter().map(|value| value * value).sum::<f64>().sqrt();
    let right_norm = right.iter().map(|value| value * value).sum::<f64>().sqrt();
    dot / (left_norm * right_norm)
}

fn pair_similarities(group: &[FaceObservation]) -> Vec<f64> {
    group
        .iter()
        .enumerate()
        .flat_map(|(index, left)| {
            group[index + 1..]
                .iter()
                .map(move |right| cosine(left, right))
        })
        .collect()
}

fn cross_similarities(subject: &[FaceObservation], target: &[FaceObservation]) -> Vec<f64> {
    subject
        .iter()
        .flat_map(|left| target.iter().map(move |right| cosine(left, right)))
        .collect()
}

fn cohesion(group: &[FaceObservation]) -> (f64, f64, f64) {
    if group.len() < 2 {
        return (1.0, 0.0, 0.0);
    }
    let pairs = pair_similarities(group);
    let stats = distribution(pairs);
    let outliers = group
        .iter()
        .filter(|face| {
            let mean = group
                .iter()
                .filter(|other| other.face_id != face.face_id)
                .map(|other| cosine(face, other))
                .sum::<f64>()
                / (group.len() - 1) as f64;
            mean < 0.30
        })
        .count();
    (
        stats.mean,
        stats.spread,
        outliers as f64 / group.len() as f64,
    )
}

fn reciprocal_rank_mean(subject: &[FaceObservation], target: &[FaceObservation]) -> f64 {
    let mut sum = 0.0;
    for source in subject {
        let nearest = target
            .iter()
            .max_by(|left, right| {
                cosine(source, left)
                    .total_cmp(&cosine(source, right))
                    .then_with(|| right.face_id.cmp(&left.face_id))
            })
            .expect("target was validated as non-empty");
        let mut ranked: Vec<_> = subject
            .iter()
            .map(|candidate| (cosine(candidate, nearest), candidate.face_id))
            .collect();
        ranked.sort_by(|left, right| {
            right
                .0
                .total_cmp(&left.0)
                .then_with(|| left.1.cmp(&right.1))
        });
        let rank = ranked
            .iter()
            .position(|(_, id)| *id == source.face_id)
            .expect("source is present in its own ranking")
            + 1;
        sum += 1.0 / rank as f64;
    }
    sum / subject.len() as f64
}

fn insert_quality(values: &mut BTreeMap<String, f64>, group: &[FaceObservation], suffix: &str) {
    let selectors: [(&str, fn(&FaceObservation) -> Option<f64>); 4] = [
        ("face_size", |face: &FaceObservation| face.bbox_min_side),
        ("blur", |face: &FaceObservation| face.blur),
        ("detector_confidence", |face: &FaceObservation| {
            face.det_score
        }),
        ("landmark_residual", |face: &FaceObservation| {
            face.landmark_residual
        }),
    ];
    for (name, selector) in selectors {
        let present: Vec<_> = group.iter().filter_map(selector).collect();
        let mean = if present.is_empty() {
            0.0
        } else {
            present.iter().sum::<f64>() / present.len() as f64
        };
        values.insert(format!("{name}_mean_{suffix}"), mean);
        values.insert(
            format!("{name}_missing_proportion_{suffix}"),
            (group.len() - present.len()) as f64 / group.len() as f64,
        );
    }
}

fn same_photo_cross(subject: &[FaceObservation], target: &[FaceObservation]) -> f64 {
    let total = subject.len() * target.len();
    let same = subject
        .iter()
        .flat_map(|left| target.iter().map(move |right| (left, right)))
        .filter(|(left, right)| left.photo_hash == right.photo_hash)
        .count();
    same as f64 / total as f64
}

/// Derive generic scalar evidence for whether `subject` belongs with `target`.
pub fn extract_membership_features(
    subject: &[FaceObservation],
    target: &[FaceObservation],
    stage: DecisionStage,
) -> Result<FeatureVector, FeatureError> {
    if subject.is_empty() {
        return Err(FeatureError::EmptySubject);
    }
    if target.is_empty() {
        return Err(FeatureError::EmptyTarget);
    }
    let dimension = validate_faces(&[subject, target])?;
    let similarities = cross_similarities(subject, target);
    let stats = distribution(similarities.clone());
    let (subject_cohesion, subject_spread, subject_outliers) = cohesion(subject);
    let (target_cohesion, target_spread, target_outliers) = cohesion(target);
    let mut values = BTreeMap::new();
    values.insert("subject_size".into(), subject.len() as f64);
    values.insert("target_size".into(), target.len() as f64);
    values.insert("decision_stage".into(), stage.numeric());
    values.insert(
        "centroid_similarity".into(),
        vector_cosine(&centroid(subject, dimension), &centroid(target, dimension)),
    );
    values.insert("similarity_strongest".into(), stats.strongest);
    values.insert("similarity_median".into(), stats.median);
    values.insert("similarity_lower_quartile".into(), stats.lower_quartile);
    values.insert("similarity_weakest".into(), stats.weakest);
    values.insert("similarity_mean".into(), stats.mean);
    values.insert("similarity_spread".into(), stats.spread);
    values.insert(
        "reciprocal_rank_mean".into(),
        reciprocal_rank_mean(subject, target),
    );
    for (name, threshold) in [("0_50", 0.50), ("0_65", 0.65), ("0_75", 0.75)] {
        let count = similarities
            .iter()
            .filter(|similarity| **similarity >= threshold)
            .count();
        values.insert(format!("support_count_ge_{name}"), count as f64);
        values.insert(
            format!("support_proportion_ge_{name}"),
            count as f64 / similarities.len() as f64,
        );
    }
    values.insert("subject_cohesion_mean".into(), subject_cohesion);
    values.insert("subject_similarity_spread".into(), subject_spread);
    values.insert("subject_outlier_proportion".into(), subject_outliers);
    values.insert("target_cohesion_mean".into(), target_cohesion);
    values.insert("target_similarity_spread".into(), target_spread);
    values.insert("target_outlier_proportion".into(), target_outliers);
    values.insert(
        "same_photo_proportion".into(),
        same_photo_cross(subject, target),
    );
    insert_quality(&mut values, subject, "subject");
    insert_quality(&mut values, target, "target");
    let result = FeatureVector {
        schema_version: FEATURE_SCHEMA_VERSION,
        values,
    };
    result.validate()?;
    Ok(result)
}

/// Derive generic scalar evidence describing the quality of a proposed cluster.
pub fn extract_cluster_quality_features(
    cluster: &[FaceObservation],
    stage: DecisionStage,
) -> Result<FeatureVector, FeatureError> {
    if cluster.len() < 2 {
        return Err(FeatureError::TooFewClusterFaces);
    }
    validate_faces(&[cluster])?;
    let similarities = pair_similarities(cluster);
    let stats = distribution(similarities.clone());
    let (_, _, outlier_proportion) = cohesion(cluster);
    let nearest_neighbor_mean = cluster
        .iter()
        .map(|face| {
            cluster
                .iter()
                .filter(|other| other.face_id != face.face_id)
                .map(|other| cosine(face, other))
                .max_by(f64::total_cmp)
                .expect("cluster contains at least two faces")
        })
        .sum::<f64>()
        / cluster.len() as f64;
    let same_photo_pairs = cluster
        .iter()
        .enumerate()
        .flat_map(|(index, left)| cluster[index + 1..].iter().map(move |right| (left, right)))
        .filter(|(left, right)| left.photo_hash == right.photo_hash)
        .count();
    let mut values = BTreeMap::new();
    values.insert("cluster_size".into(), cluster.len() as f64);
    values.insert("decision_stage".into(), stage.numeric());
    values.insert("cohesion_mean".into(), stats.mean);
    values.insert("similarity_strongest".into(), stats.strongest);
    values.insert("similarity_median".into(), stats.median);
    values.insert("similarity_lower_quartile".into(), stats.lower_quartile);
    values.insert("similarity_weakest".into(), stats.weakest);
    values.insert("similarity_spread".into(), stats.spread);
    values.insert("nearest_neighbor_mean".into(), nearest_neighbor_mean);
    values.insert("outlier_proportion".into(), outlier_proportion);
    values.insert(
        "same_photo_proportion".into(),
        same_photo_pairs as f64 / similarities.len() as f64,
    );
    for (name, threshold) in [("0_50", 0.50), ("0_65", 0.65), ("0_75", 0.75)] {
        let count = similarities
            .iter()
            .filter(|similarity| **similarity >= threshold)
            .count();
        values.insert(
            format!("support_proportion_ge_{name}"),
            count as f64 / similarities.len() as f64,
        );
    }
    let quality: Vec<_> = cluster
        .iter()
        .map(|face| {
            [
                face.bbox_min_side,
                face.blur,
                face.det_score,
                face.landmark_residual,
            ]
        })
        .collect();
    let missing_faces = quality
        .iter()
        .filter(|values| values.iter().any(Option::is_none))
        .count();
    values.insert(
        "quality_missing_proportion".into(),
        missing_faces as f64 / cluster.len() as f64,
    );
    let selectors: [(&str, fn(&FaceObservation) -> Option<f64>); 4] = [
        ("face_size", |face: &FaceObservation| face.bbox_min_side),
        ("blur", |face: &FaceObservation| face.blur),
        ("detector_confidence", |face: &FaceObservation| {
            face.det_score
        }),
        ("landmark_residual", |face: &FaceObservation| {
            face.landmark_residual
        }),
    ];
    for (name, selector) in selectors {
        let present: Vec<_> = cluster.iter().filter_map(selector).collect();
        values.insert(
            format!("{name}_missing_proportion"),
            (cluster.len() - present.len()) as f64 / cluster.len() as f64,
        );
        values.insert(
            format!("{name}_mean"),
            if present.is_empty() {
                0.0
            } else {
                present.iter().sum::<f64>() / present.len() as f64
            },
        );
    }
    let result = FeatureVector {
        schema_version: FEATURE_SCHEMA_VERSION,
        values,
    };
    result.validate()?;
    Ok(result)
}

/// Parse an exact stored `"x1,y1,...,x5,y5"` landmark sequence.
pub fn parse_landmarks(value: &str) -> Option<[[f32; 2]; 5]> {
    let numbers: Vec<f32> = value
        .split(',')
        .map(|part| part.trim().parse::<f32>())
        .collect::<Result<_, _>>()
        .ok()?;
    if numbers.len() != 10 || numbers.iter().any(|value| !value.is_finite()) {
        return None;
    }
    let mut landmarks = [[0.0; 2]; 5];
    for (index, point) in landmarks.iter_mut().enumerate() {
        *point = [numbers[index * 2], numbers[index * 2 + 1]];
    }
    Some(landmarks)
}

/// Return the 2x3 similarity transform mapping `source` onto `destination`.
pub fn landmark_similarity_transform(
    source: &[[f32; 2]; 5],
    destination: &[[f32; 2]; 5],
) -> [[f32; 3]; 2] {
    let count = source.len() as f32;
    let (source_x, source_y) = source
        .iter()
        .fold((0.0, 0.0), |(x, y), point| (x + point[0], y + point[1]));
    let (destination_x, destination_y) = destination
        .iter()
        .fold((0.0, 0.0), |(x, y), point| (x + point[0], y + point[1]));
    let source_mean = [source_x / count, source_y / count];
    let destination_mean = [destination_x / count, destination_y / count];
    let variance = source
        .iter()
        .map(|point| (point[0] - source_mean[0]).powi(2) + (point[1] - source_mean[1]).powi(2))
        .sum::<f32>()
        / count;
    let mut covariance = [[0.0; 2]; 2];
    for (source, destination) in source.iter().zip(destination.iter()) {
        let source_delta = [source[0] - source_mean[0], source[1] - source_mean[1]];
        let destination_delta = [
            destination[0] - destination_mean[0],
            destination[1] - destination_mean[1],
        ];
        covariance[0][0] += destination_delta[0] * source_delta[0] / count;
        covariance[0][1] += destination_delta[0] * source_delta[1] / count;
        covariance[1][0] += destination_delta[1] * source_delta[0] / count;
        covariance[1][1] += destination_delta[1] * source_delta[1] / count;
    }
    let determinant = covariance[0][0] * covariance[1][1] - covariance[0][1] * covariance[1][0];
    let sign = if determinant >= 0.0 { 1.0 } else { -1.0 };
    let trace = covariance[0][0] + covariance[1][1];
    let skew = covariance[1][0] - covariance[0][1];
    let scale = if variance > 1e-8 {
        (trace.powi(2) + skew.powi(2)).sqrt() * sign / variance
    } else {
        1.0
    };
    let angle = skew.atan2(trace);
    let (sin, cos) = angle.sin_cos();
    let translate_x = destination_mean[0] - scale * (cos * source_mean[0] - sin * source_mean[1]);
    let translate_y = destination_mean[1] - scale * (sin * source_mean[0] + cos * source_mean[1]);
    [
        [scale * cos, -scale * sin, translate_x],
        [scale * sin, scale * cos, translate_y],
    ]
}

/// Measure landmark fit as RMS residual after alignment to the ArcFace template.
///
/// The score is a property of one detection and does not depend on the library's
/// population. A high residual indicates that the alignment warp is likely to
/// produce a distorted crop and therefore an unreliable identity embedding.
pub fn landmark_residual(landmarks: &[[f32; 2]; 5]) -> f32 {
    let transform = landmark_similarity_transform(landmarks, &ARCFACE_LANDMARK_TEMPLATE);
    let sum = landmarks
        .iter()
        .zip(ARCFACE_LANDMARK_TEMPLATE.iter())
        .map(|(source, destination)| {
            let x = transform[0][0] * source[0] + transform[0][1] * source[1] + transform[0][2];
            let y = transform[1][0] * source[0] + transform[1][1] * source[1] + transform[1][2];
            (x - destination[0]).powi(2) + (y - destination[1]).powi(2)
        })
        .sum::<f32>();
    (sum / landmarks.len() as f32).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < 1e-6,
            "expected {expected}, got {actual}"
        );
    }

    fn face(face_id: i64, embedding: [f32; 2], photo_hash: &str) -> FaceObservation {
        FaceObservation {
            face_id,
            embedding: embedding.to_vec(),
            bbox_min_side: Some(100.0 + face_id as f64),
            blur: Some(200.0 + face_id as f64),
            det_score: Some(0.9),
            landmark_residual: Some(2.0),
            photo_hash: photo_hash.to_owned(),
        }
    }

    #[test]
    fn membership_features_are_complete_and_order_independent() {
        let mut subject = vec![face(4, [1.0, 0.0], "photo-a")];
        let mut target = vec![
            face(3, [0.8, 0.6], "photo-b"),
            face(2, [1.0, 0.0], "photo-c"),
            face(1, [0.6, 0.8], "photo-a"),
        ];

        let expected =
            extract_membership_features(&subject, &target, DecisionStage::GallerySingleton)
                .unwrap();
        assert_eq!(expected.schema_version, FEATURE_SCHEMA_VERSION);
        assert_eq!(expected.values.len(), MEMBERSHIP_FEATURE_NAMES.len());
        assert_eq!(expected.values["subject_size"], 1.0);
        assert_eq!(expected.values["target_size"], 3.0);
        assert_close(
            expected.values["centroid_similarity"],
            0.8 / (0.8_f64.powi(2) + (1.4_f64 / 3.0).powi(2)).sqrt(),
        );
        assert_close(expected.values["similarity_strongest"], 1.0);
        assert_close(expected.values["similarity_median"], 0.8);
        assert_close(expected.values["similarity_lower_quartile"], 0.6);
        assert_close(expected.values["similarity_weakest"], 0.6);
        assert_close(expected.values["reciprocal_rank_mean"], 1.0);
        assert_eq!(expected.values["support_count_ge_0_50"], 3.0);
        assert_eq!(expected.values["support_proportion_ge_0_50"], 1.0);
        assert_eq!(expected.values["support_count_ge_0_65"], 2.0);
        assert_eq!(expected.values["support_proportion_ge_0_65"], 2.0 / 3.0);
        assert_eq!(expected.values["support_count_ge_0_75"], 2.0);
        assert_eq!(expected.values["support_proportion_ge_0_75"], 2.0 / 3.0);
        assert_close(expected.values["subject_cohesion_mean"], 1.0);
        assert_close(expected.values["subject_similarity_spread"], 0.0);
        assert_close(expected.values["subject_outlier_proportion"], 0.0);
        assert_close(expected.values["target_cohesion_mean"], 2.36 / 3.0);
        assert_close(expected.values["target_similarity_spread"], 0.36);
        assert_close(expected.values["target_outlier_proportion"], 0.0);
        assert_eq!(expected.values["same_photo_proportion"], 1.0 / 3.0);
        assert_eq!(expected.values["face_size_mean_subject"], 104.0);
        assert_eq!(expected.values["face_size_mean_target"], 102.0);
        assert_eq!(expected.values["face_size_missing_proportion_subject"], 0.0);
        assert_eq!(expected.values["face_size_missing_proportion_target"], 0.0);
        assert_eq!(expected.values["decision_stage"], 0.0);
        expected.validate().unwrap();

        subject.reverse();
        target.reverse();
        let reordered =
            extract_membership_features(&subject, &target, DecisionStage::GallerySingleton)
                .unwrap();
        assert_eq!(
            expected.to_canonical_json().unwrap(),
            reordered.to_canonical_json().unwrap()
        );
        let keys: Vec<_> = expected.values.keys().map(String::as_str).collect();
        let mut sorted = keys.clone();
        sorted.sort_unstable();
        assert_eq!(keys, sorted);
    }

    #[test]
    fn cluster_quality_features_pin_cohesion_spread_outliers_and_quality() {
        let cluster = vec![
            face(1, [1.0, 0.0], "photo-a"),
            face(2, [0.8, 0.6], "photo-a"),
            face(3, [0.0, 1.0], "photo-b"),
        ];

        let features =
            extract_cluster_quality_features(&cluster, DecisionStage::GalleryCluster).unwrap();
        assert_eq!(features.values.len(), CLUSTER_QUALITY_FEATURE_NAMES.len());
        assert_eq!(features.values["cluster_size"], 3.0);
        assert_close(features.values["cohesion_mean"], (0.8 + 0.0 + 0.6) / 3.0);
        assert_close(features.values["similarity_strongest"], 0.8);
        assert_close(features.values["similarity_lower_quartile"], 0.0);
        assert_close(features.values["similarity_weakest"], 0.0);
        assert_close(features.values["similarity_spread"], 0.8);
        assert_close(features.values["nearest_neighbor_mean"], 2.2 / 3.0);
        assert_close(features.values["outlier_proportion"], 0.0);
        assert_close(features.values["support_proportion_ge_0_50"], 2.0 / 3.0);
        assert_close(features.values["support_proportion_ge_0_65"], 1.0 / 3.0);
        assert_close(features.values["support_proportion_ge_0_75"], 1.0 / 3.0);
        assert_eq!(features.values["same_photo_proportion"], 1.0 / 3.0);
        assert_eq!(features.values["face_size_mean"], 102.0);
        assert_eq!(features.values["blur_mean"], 202.0);
        assert_eq!(features.values["detector_confidence_mean"], 0.9);
        assert_eq!(features.values["landmark_residual_mean"], 2.0);
        assert_eq!(features.values["quality_missing_proportion"], 0.0);
        assert_eq!(features.values["face_size_missing_proportion"], 0.0);
        assert_eq!(features.values["blur_missing_proportion"], 0.0);
        assert_eq!(
            features.values["detector_confidence_missing_proportion"],
            0.0
        );
        assert_eq!(features.values["landmark_residual_missing_proportion"], 0.0);
        assert_eq!(features.values["decision_stage"], 1.0);
        features.validate().unwrap();
    }

    #[test]
    fn missing_quality_is_explicit_and_uses_neutral_values() {
        let mut missing = face(1, [1.0, 0.0], "photo-a");
        missing.bbox_min_side = None;
        missing.blur = None;
        missing.det_score = None;
        missing.landmark_residual = None;
        let present = face(2, [0.8, 0.6], "photo-b");

        let features =
            extract_cluster_quality_features(&[missing, present], DecisionStage::GalleryCluster)
                .unwrap();
        assert_eq!(features.values["quality_missing_proportion"], 0.5);
        assert_eq!(features.values["face_size_missing_proportion"], 0.5);
        assert_eq!(features.values["blur_missing_proportion"], 0.5);
        assert_eq!(
            features.values["detector_confidence_missing_proportion"],
            0.5
        );
        assert_eq!(features.values["landmark_residual_missing_proportion"], 0.5);
        assert_eq!(features.values["face_size_mean"], 102.0);
        assert_eq!(features.values["blur_mean"], 202.0);
    }

    #[test]
    fn malformed_inputs_fail_closed_with_specific_errors() {
        let valid = face(1, [1.0, 0.0], "photo-a");
        assert_eq!(
            extract_membership_features(&[], std::slice::from_ref(&valid), DecisionStage::Question),
            Err(FeatureError::EmptySubject)
        );
        assert_eq!(
            extract_membership_features(std::slice::from_ref(&valid), &[], DecisionStage::Question),
            Err(FeatureError::EmptyTarget)
        );
        assert_eq!(
            extract_cluster_quality_features(
                std::slice::from_ref(&valid),
                DecisionStage::GalleryCluster,
            ),
            Err(FeatureError::TooFewClusterFaces)
        );
        assert_eq!(
            extract_membership_features(
                std::slice::from_ref(&valid),
                std::slice::from_ref(&valid),
                DecisionStage::Question
            ),
            Err(FeatureError::DuplicateFaceId(1))
        );

        let short = face(2, [1.0, 0.0], "photo-b");
        let mut long = face(3, [1.0, 0.0], "photo-c");
        long.embedding.push(0.0);
        assert_eq!(
            extract_membership_features(&[short], &[long], DecisionStage::Question),
            Err(FeatureError::EmbeddingDimensionMismatch)
        );

        let mut non_finite = face(4, [1.0, f32::NAN], "photo-d");
        assert_eq!(
            extract_cluster_quality_features(
                &[non_finite.clone(), face(5, [1.0, 0.0], "photo-e")],
                DecisionStage::GalleryCluster
            ),
            Err(FeatureError::NonFiniteEmbedding(4))
        );
        non_finite.embedding = vec![0.0, 0.0];
        assert_eq!(
            extract_cluster_quality_features(
                &[non_finite, face(5, [1.0, 0.0], "photo-e")],
                DecisionStage::GalleryCluster
            ),
            Err(FeatureError::ZeroNormEmbedding(4))
        );

        let mut invalid_quality = face(6, [1.0, 0.0], "photo-f");
        invalid_quality.blur = Some(f64::INFINITY);
        assert_eq!(
            extract_cluster_quality_features(
                &[invalid_quality, face(7, [1.0, 0.0], "photo-g")],
                DecisionStage::GalleryCluster
            ),
            Err(FeatureError::InvalidQuality {
                face_id: 6,
                field: "blur"
            })
        );
    }

    #[test]
    fn feature_vector_validation_rejects_schema_keys_and_non_finite_values() {
        let valid = extract_cluster_quality_features(
            &[face(1, [1.0, 0.0], "a"), face(2, [0.8, 0.6], "b")],
            DecisionStage::GalleryCluster,
        )
        .unwrap();

        let mut wrong_schema = valid.clone();
        wrong_schema.schema_version += 1;
        assert_eq!(
            wrong_schema.validate(),
            Err(FeatureError::UnsupportedSchemaVersion(2))
        );

        let mut missing = valid.clone();
        missing.values.remove("cohesion_mean");
        assert_eq!(missing.validate(), Err(FeatureError::InvalidFeatureSet));

        let mut unknown = valid.clone();
        unknown.values.insert("identity".into(), 1.0);
        assert_eq!(unknown.validate(), Err(FeatureError::InvalidFeatureSet));

        let mut non_finite = FeatureVector {
            schema_version: FEATURE_SCHEMA_VERSION,
            values: BTreeMap::from([("bad".to_owned(), f64::NAN)]),
        };
        assert_eq!(
            non_finite.validate(),
            Err(FeatureError::NonFiniteFeature("bad".into()))
        );
        non_finite.values.insert("bad".into(), 0.0);
        assert_eq!(non_finite.validate(), Err(FeatureError::InvalidFeatureSet));
    }
}
