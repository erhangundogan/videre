use super::{ClusteringMetrics, ModelBundle, SuggestionMetrics};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

pub const PROFILE_ARTIFACT_VERSION: u32 = 1;
const METRIC_TOLERANCE: f64 = 1e-9;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProfileStage {
    Shadow,
    Suggestion,
    Grouping,
}

impl ProfileStage {
    fn as_str(self) -> &'static str {
        match self {
            Self::Shadow => "shadow",
            Self::Suggestion => "suggestion",
            Self::Grouping => "grouping",
        }
    }

    fn rank(self) -> u8 {
        match self {
            Self::Shadow => 0,
            Self::Suggestion => 1,
            Self::Grouping => 2,
        }
    }

    fn parse(value: &str) -> Result<Self, ProfileError> {
        match value {
            "shadow" => Ok(Self::Shadow),
            "suggestion" => Ok(Self::Suggestion),
            "grouping" => Ok(Self::Grouping),
            other => Err(ProfileError::InvalidStoredValue(format!(
                "unknown profile stage {other}"
            ))),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProfileStatus {
    Candidate,
    Active,
    Rejected,
    Retired,
}

impl ProfileStatus {
    fn parse(value: &str) -> Result<Self, ProfileError> {
        match value {
            "candidate" => Ok(Self::Candidate),
            "active" => Ok(Self::Active),
            "rejected" => Ok(Self::Rejected),
            "retired" => Ok(Self::Retired),
            other => Err(ProfileError::InvalidStoredValue(format!(
                "unknown profile status {other}"
            ))),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrainingEvidenceCounts {
    pub positive_pairs: usize,
    pub negative_pairs: usize,
    pub explicit_negative_pairs: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DatasetValidation {
    pub dataset_key: String,
    pub clustering: ClusteringMetrics,
    pub suggestions: Option<SuggestionMetrics>,
    pub hard_rule_violations: usize,
    pub invalid_explanations: usize,
    pub wall_time_ms: f64,
    pub peak_memory_mib: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ValidationReport {
    pub protocol_version: u32,
    pub evidence_schema_version: u32,
    pub feature_schema_version: u32,
    pub datasets: Vec<DatasetValidation>,
}

impl Default for PromotionGates {
    fn default() -> Self {
        Self::shipped()
    }
}

impl PromotionGates {
    /// The gates this protocol ships with. They match the frozen evaluation
    /// protocol versions, and every profile must pass them at promotion time.
    pub fn shipped() -> Self {
        Self {
            protocol_version: 1,
            evidence_schema_version: super::evidence::EVIDENCE_SCHEMA_VERSION,
            feature_schema_version: super::features::FEATURE_SCHEMA_VERSION,
            min_datasets: 2,
            max_hard_rule_violations: 0,
            max_invalid_explanations: 0,
            min_suggestion_precision: 0.85,
            min_suggestion_precision_wilson_lower_bound: 0.70,
            min_suggestion_coverage: 0.02,
            max_wall_time_ms: 60_000.0,
            max_peak_memory_mib: 2_048.0,
            max_pair_precision_drop: 0.01,
            max_pair_recall_drop: 0.01,
            max_mixed_cluster_rate_increase: 0.0,
            max_fragmented_identity_rate_increase: 0.0,
            max_unassigned_rate_increase: 0.01,
            min_pair_recall_gain: 0.0,
            min_fragmented_identity_reduction: 0,
            min_additive_gain: 0.02,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PromotionGates {
    pub protocol_version: u32,
    pub evidence_schema_version: u32,
    pub feature_schema_version: u32,
    pub min_datasets: usize,
    pub max_hard_rule_violations: usize,
    pub max_invalid_explanations: usize,
    pub min_suggestion_precision: f64,
    pub min_suggestion_precision_wilson_lower_bound: f64,
    pub min_suggestion_coverage: f64,
    pub max_wall_time_ms: f64,
    pub max_peak_memory_mib: f64,
    pub max_pair_precision_drop: f64,
    pub max_pair_recall_drop: f64,
    pub max_mixed_cluster_rate_increase: f64,
    pub max_fragmented_identity_rate_increase: f64,
    pub max_unassigned_rate_increase: f64,
    pub min_pair_recall_gain: f64,
    pub min_fragmented_identity_reduction: usize,
    pub min_additive_gain: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GateFailure {
    pub dataset_key: String,
    pub gate: String,
    pub observed: Option<f64>,
    pub required: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NewProfile {
    pub artifact_version: u32,
    pub embedding_model_id: String,
    pub feature_schema_version: u32,
    pub model_kind: String,
    pub parameters: Vec<u8>,
    pub training_evidence: TrainingEvidenceCounts,
    pub validation_report: ValidationReport,
    pub stage: ProfileStage,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StoredProfile {
    pub id: i64,
    pub artifact_version: u32,
    pub embedding_model_id: String,
    pub feature_schema_version: u32,
    pub model_kind: String,
    pub parameters: Vec<u8>,
    pub training_evidence: TrainingEvidenceCounts,
    pub validation_report: ValidationReport,
    pub stage: ProfileStage,
    pub status: ProfileStatus,
    pub promotion_result: Option<Vec<GateFailure>>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PromotionOutcome {
    Promoted,
    Rejected(Vec<GateFailure>),
}

#[derive(Debug)]
pub enum ProfileError {
    InvalidGate(&'static str),
    InvalidReport(String),
    InvalidProfile(String),
    IncompatibleProfile(String),
    InvalidStoredValue(String),
    CandidateNotFound(i64),
    NotCandidate(i64),
    Sql(rusqlite::Error),
    Json(serde_json::Error),
}

impl fmt::Display for ProfileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidGate(name) => write!(f, "invalid promotion gate {name}"),
            Self::InvalidReport(reason) => write!(f, "invalid validation report: {reason}"),
            Self::InvalidProfile(reason) => write!(f, "invalid face profile: {reason}"),
            Self::IncompatibleProfile(reason) => {
                write!(f, "incompatible face profile: {reason}")
            }
            Self::InvalidStoredValue(reason) => write!(f, "invalid stored profile: {reason}"),
            Self::CandidateNotFound(id) => write!(f, "profile candidate {id} was not found"),
            Self::NotCandidate(id) => write!(f, "profile {id} is not a candidate"),
            Self::Sql(error) => write!(f, "profile database error: {error}"),
            Self::Json(error) => write!(f, "profile JSON error: {error}"),
        }
    }
}

impl std::error::Error for ProfileError {}

impl From<rusqlite::Error> for ProfileError {
    fn from(value: rusqlite::Error) -> Self {
        Self::Sql(value)
    }
}

impl From<serde_json::Error> for ProfileError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

pub fn ensure_profile_table(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS face_learning_profiles (
            id INTEGER PRIMARY KEY,
            artifact_version INTEGER NOT NULL,
            embedding_model_id TEXT NOT NULL,
            feature_schema_version INTEGER NOT NULL,
            model_kind TEXT NOT NULL,
            parameters BLOB NOT NULL,
            training_evidence_json TEXT NOT NULL,
            validation_report_json TEXT NOT NULL,
            stage TEXT NOT NULL CHECK(stage IN ('shadow','suggestion','grouping')),
            status TEXT NOT NULL CHECK(status IN ('candidate','active','rejected','retired')),
            promotion_result_json TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        CREATE UNIQUE INDEX IF NOT EXISTS face_learning_one_active
            ON face_learning_profiles(status) WHERE status='active';",
    )
}

fn validate_finite_nonnegative(value: f64, name: &'static str) -> Result<(), ProfileError> {
    if !value.is_finite() || value < 0.0 {
        return Err(ProfileError::InvalidGate(name));
    }
    Ok(())
}

fn validate_probability(value: f64, name: &'static str) -> Result<(), ProfileError> {
    if !value.is_finite() || !(0.0..=1.0).contains(&value) {
        return Err(ProfileError::InvalidGate(name));
    }
    Ok(())
}

fn validate_gates(gates: &PromotionGates) -> Result<(), ProfileError> {
    if gates.min_datasets == 0 {
        return Err(ProfileError::InvalidGate("min_datasets"));
    }
    for (value, name) in [
        (gates.min_suggestion_precision, "min_suggestion_precision"),
        (
            gates.min_suggestion_precision_wilson_lower_bound,
            "min_suggestion_precision_wilson_lower_bound",
        ),
        (gates.min_suggestion_coverage, "min_suggestion_coverage"),
        (gates.max_pair_precision_drop, "max_pair_precision_drop"),
        (gates.max_pair_recall_drop, "max_pair_recall_drop"),
        (
            gates.max_mixed_cluster_rate_increase,
            "max_mixed_cluster_rate_increase",
        ),
        (
            gates.max_fragmented_identity_rate_increase,
            "max_fragmented_identity_rate_increase",
        ),
        (
            gates.max_unassigned_rate_increase,
            "max_unassigned_rate_increase",
        ),
        (gates.min_pair_recall_gain, "min_pair_recall_gain"),
        (gates.min_additive_gain, "min_additive_gain"),
    ] {
        validate_probability(value, name)?;
    }
    for (value, name) in [
        (gates.max_wall_time_ms, "max_wall_time_ms"),
        (gates.max_peak_memory_mib, "max_peak_memory_mib"),
    ] {
        validate_finite_nonnegative(value, name)?;
    }
    Ok(())
}

fn choose_two(value: usize) -> u64 {
    let value = value as u64;
    value.saturating_mul(value.saturating_sub(1)) / 2
}

fn balanced_pair_minimum(faces: usize, buckets: usize) -> u64 {
    if buckets == 0 {
        return 0;
    }
    let quotient = faces / buckets;
    let remainder = faces % buckets;
    remainder as u64 * choose_two(quotient + 1)
        + (buckets - remainder) as u64 * choose_two(quotient)
}

fn close(left: f64, right: f64) -> bool {
    (left - right).abs() <= METRIC_TOLERANCE
}

fn expected_ratio(numerator: u64, denominator: u64) -> Option<f64> {
    (denominator != 0).then(|| numerator as f64 / denominator as f64)
}

fn validate_optional_metric(
    dataset_key: &str,
    name: &str,
    observed: Option<f64>,
    expected: Option<f64>,
) -> Result<(), ProfileError> {
    let valid = match (observed, expected) {
        (None, None) => true,
        (Some(observed), Some(expected)) => {
            observed.is_finite() && (0.0..=1.0).contains(&observed) && close(observed, expected)
        }
        _ => false,
    };
    if !valid {
        return Err(ProfileError::InvalidReport(format!(
            "{dataset_key} has inconsistent {name}"
        )));
    }
    Ok(())
}

fn validate_clustering(dataset_key: &str, metrics: &ClusteringMetrics) -> Result<(), ProfileError> {
    if metrics.labeled_faces == 0
        || metrics.labeled_identities == 0
        || metrics.labeled_identities > metrics.labeled_faces
        || metrics.unassigned_labeled_faces > metrics.labeled_faces
        || metrics.predicted_clusters > metrics.labeled_faces - metrics.unassigned_labeled_faces
        || (metrics.predicted_clusters == 0
            && metrics.unassigned_labeled_faces != metrics.labeled_faces)
        || metrics.mixed_clusters > metrics.predicted_clusters
        || metrics.fragmented_identities > metrics.labeled_identities
    {
        return Err(ProfileError::InvalidReport(format!(
            "{dataset_key} has impossible clustering counts"
        )));
    }

    let actual_pairs = metrics
        .true_positive_pairs
        .checked_add(metrics.false_negative_pairs)
        .ok_or_else(|| {
            ProfileError::InvalidReport(format!("{dataset_key} pair counts overflow"))
        })?;
    let predicted_pairs = metrics
        .true_positive_pairs
        .checked_add(metrics.false_positive_pairs)
        .ok_or_else(|| {
            ProfileError::InvalidReport(format!("{dataset_key} pair counts overflow"))
        })?;
    let max_pairs = choose_two(metrics.labeled_faces);
    let min_actual = balanced_pair_minimum(metrics.labeled_faces, metrics.labeled_identities);
    let assigned = metrics.labeled_faces - metrics.unassigned_labeled_faces;
    let min_predicted = balanced_pair_minimum(assigned, metrics.predicted_clusters);
    if actual_pairs < min_actual
        || actual_pairs > max_pairs
        || predicted_pairs < min_predicted
        || predicted_pairs > choose_two(assigned)
    {
        return Err(ProfileError::InvalidReport(format!(
            "{dataset_key} has impossible pair counts"
        )));
    }

    let expected_precision = expected_ratio(metrics.true_positive_pairs, predicted_pairs);
    let expected_recall = expected_ratio(metrics.true_positive_pairs, actual_pairs);
    validate_optional_metric(
        dataset_key,
        "pair_precision",
        metrics.pair_precision,
        expected_precision,
    )?;
    validate_optional_metric(
        dataset_key,
        "pair_recall",
        metrics.pair_recall,
        expected_recall,
    )?;
    let expected_f1 = match (expected_precision, expected_recall) {
        (Some(precision), Some(recall)) if precision + recall > 0.0 => {
            Some(2.0 * precision * recall / (precision + recall))
        }
        (Some(_), Some(_)) => Some(0.0),
        _ => None,
    };
    validate_optional_metric(dataset_key, "pair_f1", metrics.pair_f1, expected_f1)?;

    let expected_unassigned =
        metrics.unassigned_labeled_faces as f64 / metrics.labeled_faces as f64;
    if !metrics.unassigned_rate.is_finite()
        || !(0.0..=1.0).contains(&metrics.unassigned_rate)
        || !close(metrics.unassigned_rate, expected_unassigned)
    {
        return Err(ProfileError::InvalidReport(format!(
            "{dataset_key} has inconsistent unassigned_rate"
        )));
    }
    Ok(())
}

fn validate_suggestions(
    dataset_key: &str,
    labeled_faces: usize,
    metrics: &SuggestionMetrics,
) -> Result<(), ProfileError> {
    let suggested = metrics
        .correct
        .checked_add(metrics.incorrect)
        .ok_or_else(|| {
            ProfileError::InvalidReport(format!("{dataset_key} suggestion counts overflow"))
        })?;
    let total = suggested
        .checked_add(metrics.not_suggested)
        .ok_or_else(|| {
            ProfileError::InvalidReport(format!("{dataset_key} suggestion counts overflow"))
        })?;
    if total != labeled_faces {
        return Err(ProfileError::InvalidReport(format!(
            "{dataset_key} suggestion counts do not cover labeled faces"
        )));
    }
    validate_optional_metric(
        dataset_key,
        "suggestion precision",
        metrics.precision,
        expected_ratio(metrics.correct as u64, suggested as u64),
    )?;
    let expected_coverage = suggested as f64 / labeled_faces as f64;
    if !metrics.coverage.is_finite()
        || !(0.0..=1.0).contains(&metrics.coverage)
        || !close(metrics.coverage, expected_coverage)
    {
        return Err(ProfileError::InvalidReport(format!(
            "{dataset_key} has inconsistent suggestion coverage"
        )));
    }
    Ok(())
}

fn validate_report(report: &ValidationReport) -> Result<(), ProfileError> {
    let mut keys = BTreeSet::new();
    for dataset in &report.datasets {
        if dataset.dataset_key.is_empty() || !keys.insert(dataset.dataset_key.as_str()) {
            return Err(ProfileError::InvalidReport(format!(
                "duplicate or empty dataset key {:?}",
                dataset.dataset_key
            )));
        }
        if !dataset.wall_time_ms.is_finite()
            || dataset.wall_time_ms < 0.0
            || !dataset.peak_memory_mib.is_finite()
            || dataset.peak_memory_mib < 0.0
        {
            return Err(ProfileError::InvalidReport(format!(
                "non-finite resource metric for {}",
                dataset.dataset_key
            )));
        }
        validate_clustering(&dataset.dataset_key, &dataset.clustering)?;
        if let Some(suggestions) = &dataset.suggestions {
            validate_suggestions(
                &dataset.dataset_key,
                dataset.clustering.labeled_faces,
                suggestions,
            )?;
        }
    }
    Ok(())
}

fn validate_profile_metadata(
    artifact_version: u32,
    embedding_model_id: &str,
    feature_schema_version: u32,
    model_kind: &str,
    report: &ValidationReport,
) -> Result<(), ProfileError> {
    if artifact_version != PROFILE_ARTIFACT_VERSION {
        return Err(ProfileError::InvalidProfile(format!(
            "unsupported artifact version {artifact_version}"
        )));
    }
    if embedding_model_id.trim().is_empty() {
        return Err(ProfileError::InvalidProfile(
            "embedding model id is empty".into(),
        ));
    }
    if model_kind.trim().is_empty() {
        return Err(ProfileError::InvalidProfile("model kind is empty".into()));
    }
    if feature_schema_version != report.feature_schema_version {
        return Err(ProfileError::InvalidProfile(format!(
            "profile feature schema {feature_schema_version} does not match report feature schema {}",
            report.feature_schema_version
        )));
    }
    validate_report(report)
}

fn wilson_lower_bound(correct: usize, total: usize) -> Option<f64> {
    if total == 0 {
        return None;
    }
    const Z: f64 = 1.959_963_984_540_054;
    let total = total as f64;
    let proportion = correct as f64 / total;
    let z_squared = Z * Z;
    let center = proportion + z_squared / (2.0 * total);
    let margin = Z * ((proportion * (1.0 - proportion) + z_squared / (4.0 * total)) / total).sqrt();
    Some((center - margin) / (1.0 + z_squared / total))
}

fn failure(
    dataset_key: &str,
    gate: &str,
    observed: Option<f64>,
    required: Option<f64>,
) -> GateFailure {
    GateFailure {
        dataset_key: dataset_key.into(),
        gate: gate.into(),
        observed,
        required,
    }
}

fn rate(numerator: usize, denominator: usize) -> Option<f64> {
    (denominator != 0).then(|| numerator as f64 / denominator as f64)
}

pub fn evaluate_promotion(
    active: Option<&ValidationReport>,
    candidate: &ValidationReport,
    gates: &PromotionGates,
    stage: ProfileStage,
) -> Result<Vec<GateFailure>, ProfileError> {
    validate_gates(gates)?;
    validate_report(candidate)?;
    if let Some(active) = active {
        validate_report(active)?;
        for (observed, required, name) in [
            (
                active.protocol_version,
                gates.protocol_version,
                "protocol_version",
            ),
            (
                active.evidence_schema_version,
                gates.evidence_schema_version,
                "evidence_schema_version",
            ),
            (
                active.feature_schema_version,
                gates.feature_schema_version,
                "feature_schema_version",
            ),
        ] {
            if observed != required {
                return Err(ProfileError::IncompatibleProfile(format!(
                    "active {name} {observed} does not match required version {required}"
                )));
            }
        }
    }

    let mut failures = Vec::new();
    for (observed, required, gate) in [
        (
            candidate.protocol_version,
            gates.protocol_version,
            "protocol_version",
        ),
        (
            candidate.evidence_schema_version,
            gates.evidence_schema_version,
            "evidence_schema_version",
        ),
        (
            candidate.feature_schema_version,
            gates.feature_schema_version,
            "feature_schema_version",
        ),
    ] {
        if observed != required {
            failures.push(failure(
                "",
                gate,
                Some(observed as f64),
                Some(required as f64),
            ));
        }
    }
    if candidate.datasets.len() < gates.min_datasets {
        failures.push(failure(
            "",
            "min_datasets",
            Some(candidate.datasets.len() as f64),
            Some(gates.min_datasets as f64),
        ));
    }

    let active_by_key: BTreeMap<_, _> = active
        .into_iter()
        .flat_map(|report| &report.datasets)
        .map(|dataset| (dataset.dataset_key.as_str(), dataset))
        .collect();
    if active.is_some() {
        let candidate_keys: BTreeSet<_> = candidate
            .datasets
            .iter()
            .map(|dataset| dataset.dataset_key.as_str())
            .collect();
        for key in active_by_key.keys() {
            if !candidate_keys.contains(key) {
                failures.push(failure(key, "missing_dataset", None, None));
            }
        }
        for key in &candidate_keys {
            if !active_by_key.contains_key(key) {
                failures.push(failure(key, "unexpected_dataset", None, None));
            }
        }
    }
    let mut quality_gain = false;

    for dataset in &candidate.datasets {
        let key = dataset.dataset_key.as_str();
        if dataset.hard_rule_violations > gates.max_hard_rule_violations {
            failures.push(failure(
                key,
                "hard_rule_violations",
                Some(dataset.hard_rule_violations as f64),
                Some(gates.max_hard_rule_violations as f64),
            ));
        }
        if dataset.invalid_explanations > gates.max_invalid_explanations {
            failures.push(failure(
                key,
                "invalid_explanations",
                Some(dataset.invalid_explanations as f64),
                Some(gates.max_invalid_explanations as f64),
            ));
        }
        if dataset.wall_time_ms > gates.max_wall_time_ms {
            failures.push(failure(
                key,
                "wall_time_ms",
                Some(dataset.wall_time_ms),
                Some(gates.max_wall_time_ms),
            ));
        }
        if dataset.peak_memory_mib > gates.max_peak_memory_mib {
            failures.push(failure(
                key,
                "peak_memory_mib",
                Some(dataset.peak_memory_mib),
                Some(gates.max_peak_memory_mib),
            ));
        }

        if stage.rank() >= ProfileStage::Suggestion.rank() {
            match &dataset.suggestions {
                None => failures.push(failure(key, "suggestion_metrics", None, None)),
                Some(metrics) => {
                    match metrics.precision {
                        Some(value) if value >= gates.min_suggestion_precision => {}
                        observed => failures.push(failure(
                            key,
                            "suggestion_precision",
                            observed,
                            Some(gates.min_suggestion_precision),
                        )),
                    }
                    let suggested = metrics.correct + metrics.incorrect;
                    match wilson_lower_bound(metrics.correct, suggested) {
                        Some(value)
                            if value >= gates.min_suggestion_precision_wilson_lower_bound => {}
                        observed => failures.push(failure(
                            key,
                            "suggestion_precision_wilson_lower_bound",
                            observed,
                            Some(gates.min_suggestion_precision_wilson_lower_bound),
                        )),
                    }
                    if metrics.coverage < gates.min_suggestion_coverage {
                        failures.push(failure(
                            key,
                            "suggestion_coverage",
                            Some(metrics.coverage),
                            Some(gates.min_suggestion_coverage),
                        ));
                    }
                }
            }
        }

        if stage == ProfileStage::Grouping {
            if dataset.clustering.pair_precision.is_none() {
                failures.push(failure(key, "pair_precision", None, None));
            }
            if dataset.clustering.pair_recall.is_none() {
                failures.push(failure(key, "pair_recall", None, None));
            }
            if let Some(previous) = active_by_key.get(key) {
                if let (Some(current), Some(old)) = (
                    dataset.clustering.pair_precision,
                    previous.clustering.pair_precision,
                ) {
                    if current + gates.max_pair_precision_drop < old {
                        failures.push(failure(
                            key,
                            "pair_precision_regression",
                            Some(current),
                            Some(old - gates.max_pair_precision_drop),
                        ));
                    }
                }
                if let (Some(current), Some(old)) = (
                    dataset.clustering.pair_recall,
                    previous.clustering.pair_recall,
                ) {
                    if current + gates.max_pair_recall_drop < old {
                        failures.push(failure(
                            key,
                            "pair_recall_regression",
                            Some(current),
                            Some(old - gates.max_pair_recall_drop),
                        ));
                    }
                    quality_gain |= current - old >= gates.min_pair_recall_gain;
                }
                for (current, old, allowed, gate) in [
                    (
                        rate(
                            dataset.clustering.mixed_clusters,
                            dataset.clustering.predicted_clusters,
                        ),
                        rate(
                            previous.clustering.mixed_clusters,
                            previous.clustering.predicted_clusters,
                        ),
                        gates.max_mixed_cluster_rate_increase,
                        "mixed_cluster_rate_regression",
                    ),
                    (
                        rate(
                            dataset.clustering.fragmented_identities,
                            dataset.clustering.labeled_identities,
                        ),
                        rate(
                            previous.clustering.fragmented_identities,
                            previous.clustering.labeled_identities,
                        ),
                        gates.max_fragmented_identity_rate_increase,
                        "fragmented_identity_rate_regression",
                    ),
                    (
                        Some(dataset.clustering.unassigned_rate),
                        Some(previous.clustering.unassigned_rate),
                        gates.max_unassigned_rate_increase,
                        "unassigned_rate_regression",
                    ),
                ] {
                    if let (Some(current), Some(old)) = (current, old) {
                        if current > old + allowed {
                            failures.push(failure(key, gate, Some(current), Some(old + allowed)));
                        }
                    }
                }
                quality_gain |= previous
                    .clustering
                    .fragmented_identities
                    .saturating_sub(dataset.clustering.fragmented_identities)
                    >= gates.min_fragmented_identity_reduction;
            }
        }
    }

    if stage == ProfileStage::Grouping && active.is_some() && !quality_gain {
        failures.push(failure("", "quality_gain", None, None));
    }
    failures.sort_by(|left, right| {
        (&left.dataset_key, &left.gate).cmp(&(&right.dataset_key, &right.gate))
    });
    Ok(failures)
}

/// Only decodable logistic or additive artifacts may be stored, the declared
/// model kind must match the decoded bundle, and the row's identity metadata
/// must agree with what the artifact itself carries.
fn decode_artifact(
    artifact_version: u32,
    embedding_model_id: &str,
    feature_schema_version: u32,
    model_kind: &str,
    parameters: &[u8],
) -> Result<(), ProfileError> {
    let bundle: ModelBundle = serde_json::from_slice(parameters).map_err(|error| {
        ProfileError::InvalidProfile(format!(
            "profile parameters are not a decodable model artifact: {error}"
        ))
    })?;
    if bundle.model_kind() != model_kind {
        return Err(ProfileError::InvalidProfile(format!(
            "profile model kind {model_kind} does not match the decoded {} artifact",
            bundle.model_kind()
        )));
    }
    if bundle.artifact_version() != artifact_version {
        return Err(ProfileError::InvalidProfile(format!(
            "profile artifact version {artifact_version} does not match the decoded {} artifact",
            bundle.artifact_version()
        )));
    }
    if bundle.embedding_model_id() != embedding_model_id {
        return Err(ProfileError::InvalidProfile(format!(
            "profile embedding model {embedding_model_id} does not match the decoded {} artifact",
            bundle.embedding_model_id()
        )));
    }
    if bundle.feature_schema_version() != feature_schema_version {
        return Err(ProfileError::InvalidProfile(format!(
            "profile feature schema {feature_schema_version} does not match the decoded {} artifact",
            bundle.feature_schema_version()
        )));
    }
    bundle
        .validate()
        .map_err(|error| ProfileError::InvalidProfile(error.to_string()))
}

pub fn insert_candidate(conn: &Connection, profile: &NewProfile) -> Result<i64, ProfileError> {
    ensure_profile_table(conn)?;
    if profile.stage == ProfileStage::Grouping {
        return Err(ProfileError::InvalidProfile(
            "grouping profiles are outside this entry point".into(),
        ));
    }
    validate_profile_metadata(
        profile.artifact_version,
        &profile.embedding_model_id,
        profile.feature_schema_version,
        &profile.model_kind,
        &profile.validation_report,
    )?;
    // A candidate that cannot explain its own validation evidence never
    // reaches storage; promotion gates re-check explanations later.
    if profile
        .validation_report
        .datasets
        .iter()
        .any(|dataset| dataset.invalid_explanations > 0)
    {
        return Err(ProfileError::InvalidProfile(
            "candidate reports with invalid explanations are not stored".into(),
        ));
    }
    decode_artifact(
        profile.artifact_version,
        &profile.embedding_model_id,
        profile.feature_schema_version,
        &profile.model_kind,
        &profile.parameters,
    )?;
    let evidence = serde_json::to_string(&profile.training_evidence)?;
    let report = serde_json::to_string(&profile.validation_report)?;
    conn.execute(
        "INSERT INTO face_learning_profiles (
            artifact_version, embedding_model_id, feature_schema_version, model_kind,
            parameters, training_evidence_json, validation_report_json, stage, status
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'candidate')",
        params![
            profile.artifact_version,
            profile.embedding_model_id,
            profile.feature_schema_version,
            profile.model_kind,
            profile.parameters,
            evidence,
            report,
            profile.stage.as_str(),
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

fn stored_profile(
    conn: &Connection,
    clause: &str,
    value: i64,
) -> Result<Option<StoredProfile>, ProfileError> {
    let sql = format!(
        "SELECT id, artifact_version, embedding_model_id, feature_schema_version,
                model_kind, parameters, training_evidence_json, validation_report_json,
                stage, status, promotion_result_json
         FROM face_learning_profiles WHERE {clause} LIMIT 1"
    );
    let raw = conn
        .query_row(&sql, [value], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, u32>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, u32>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Vec<u8>>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, String>(8)?,
                row.get::<_, String>(9)?,
                row.get::<_, Option<String>>(10)?,
            ))
        })
        .optional()?;
    raw.map(
        |(
            id,
            artifact_version,
            embedding_model_id,
            feature_schema_version,
            model_kind,
            parameters,
            evidence,
            report,
            stage,
            status,
            result,
        )| {
            Ok(StoredProfile {
                id,
                artifact_version,
                embedding_model_id,
                feature_schema_version,
                model_kind,
                parameters,
                training_evidence: serde_json::from_str(&evidence)?,
                validation_report: serde_json::from_str(&report)?,
                stage: ProfileStage::parse(&stage)?,
                status: ProfileStatus::parse(&status)?,
                promotion_result: result.map(|json| serde_json::from_str(&json)).transpose()?,
            })
        },
    )
    .transpose()
}

pub fn active_profile(conn: &Connection) -> Result<Option<StoredProfile>, ProfileError> {
    ensure_profile_table(conn)?;
    stored_profile(conn, "status = 'active' AND ?1 = 1", 1)
}

pub fn evaluate_and_promote(
    conn: &Connection,
    candidate_id: i64,
    gates: &PromotionGates,
) -> Result<PromotionOutcome, ProfileError> {
    ensure_profile_table(conn)?;
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| {
        let candidate = stored_profile(conn, "id = ?1", candidate_id)?
            .ok_or(ProfileError::CandidateNotFound(candidate_id))?;
        if candidate.status != ProfileStatus::Candidate {
            return Err(ProfileError::NotCandidate(candidate_id));
        }
        validate_profile_metadata(
            candidate.artifact_version,
            &candidate.embedding_model_id,
            candidate.feature_schema_version,
            &candidate.model_kind,
            &candidate.validation_report,
        )?;
        let active = active_profile(conn)?;
        if let Some(active) = &active {
            validate_profile_metadata(
                active.artifact_version,
                &active.embedding_model_id,
                active.feature_schema_version,
                &active.model_kind,
                &active.validation_report,
            )?;
            if active.embedding_model_id != candidate.embedding_model_id {
                return Err(ProfileError::IncompatibleProfile(format!(
                    "embedding model {} does not match active model {}",
                    candidate.embedding_model_id, active.embedding_model_id
                )));
            }
        }
        let failures = evaluate_promotion(
            active.as_ref().map(|profile| &profile.validation_report),
            &candidate.validation_report,
            gates,
            candidate.stage,
        )?;
        let result_json = serde_json::to_string(&failures)?;
        if !failures.is_empty() {
            conn.execute(
                "UPDATE face_learning_profiles
                 SET status = 'rejected', promotion_result_json = ?1 WHERE id = ?2",
                params![result_json, candidate_id],
            )?;
            return Ok(PromotionOutcome::Rejected(failures));
        }
        conn.execute(
            "UPDATE face_learning_profiles SET status = 'retired' WHERE status = 'active'",
            [],
        )?;
        conn.execute(
            "UPDATE face_learning_profiles
             SET status = 'active', promotion_result_json = ?1 WHERE id = ?2",
            params![result_json, candidate_id],
        )?;
        Ok(PromotionOutcome::Promoted)
    })();
    match result {
        Ok(outcome) => {
            if let Err(error) = conn.execute_batch("COMMIT") {
                let _ = conn.execute_batch("ROLLBACK");
                Err(error.into())
            } else {
                Ok(outcome)
            }
        }
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::face_learning::{
        AdditiveCurve, AdditiveModel, AdditiveScorer, CalibrationModel, ClusteringMetrics,
        LogisticModel, LogisticScorer, ModelBundle, SuggestionMetrics, MODEL_ARTIFACT_VERSION,
    };
    use rusqlite::Connection;

    fn clustering(recall: f64, fragmented: usize) -> ClusteringMetrics {
        let true_positive_pairs = (recall * 1000.0).round() as u64;
        ClusteringMetrics {
            labeled_faces: 100,
            labeled_identities: 10,
            predicted_clusters: 10,
            true_positive_pairs,
            false_positive_pairs: 0,
            false_negative_pairs: 1000 - true_positive_pairs,
            pair_precision: Some(1.0),
            pair_recall: Some(recall),
            pair_f1: Some(2.0 * recall / (1.0 + recall)),
            mixed_clusters: 1,
            fragmented_identities: fragmented,
            unassigned_labeled_faces: 10,
            unassigned_rate: 0.10,
        }
    }

    fn suggestions() -> SuggestionMetrics {
        SuggestionMetrics {
            correct: 100,
            incorrect: 0,
            not_suggested: 0,
            precision: Some(1.0),
            coverage: 1.0,
        }
    }

    fn report(recall: f64, fragmented: usize) -> ValidationReport {
        ValidationReport {
            protocol_version: 1,
            evidence_schema_version: 1,
            feature_schema_version: 1,
            datasets: vec![
                DatasetValidation {
                    dataset_key: "library-a".into(),
                    clustering: clustering(recall, fragmented),
                    suggestions: Some(suggestions()),
                    hard_rule_violations: 0,
                    invalid_explanations: 0,
                    wall_time_ms: 100.0,
                    peak_memory_mib: 50.0,
                },
                DatasetValidation {
                    dataset_key: "library-b".into(),
                    clustering: clustering(recall, fragmented),
                    suggestions: Some(suggestions()),
                    hard_rule_violations: 0,
                    invalid_explanations: 0,
                    wall_time_ms: 120.0,
                    peak_memory_mib: 60.0,
                },
            ],
        }
    }

    fn gates() -> PromotionGates {
        PromotionGates {
            protocol_version: 1,
            evidence_schema_version: 1,
            feature_schema_version: 1,
            min_datasets: 2,
            max_hard_rule_violations: 0,
            max_invalid_explanations: 0,
            min_suggestion_precision: 0.99,
            min_suggestion_precision_wilson_lower_bound: 0.95,
            min_suggestion_coverage: 0.01,
            max_wall_time_ms: 200.0,
            max_peak_memory_mib: 100.0,
            max_pair_precision_drop: 0.0,
            max_pair_recall_drop: 0.0,
            max_mixed_cluster_rate_increase: 0.0,
            max_fragmented_identity_rate_increase: 0.0,
            max_unassigned_rate_increase: 0.0,
            min_pair_recall_gain: 0.01,
            min_fragmented_identity_reduction: 1,
            min_additive_gain: 0.01,
        }
    }

    fn logistic_scorer() -> LogisticScorer {
        LogisticScorer {
            model: LogisticModel {
                feature_names: vec!["x".into()],
                means: vec![0.0],
                scales: vec![1.0],
                intercept: 0.0,
                weights: vec![1.0],
                l2: 1.0,
                positive_class_weight: 1.0,
            },
            calibration: CalibrationModel {
                intercept: 0.0,
                slope: 1.0,
            },
            threshold: 0.5,
        }
    }

    fn logistic_bundle() -> ModelBundle {
        ModelBundle::Logistic {
            artifact_version: MODEL_ARTIFACT_VERSION,
            embedding_model_id: "arcface-test".into(),
            feature_schema_version: 1,
            membership: logistic_scorer(),
            cluster_quality: logistic_scorer(),
        }
    }

    fn additive_bundle() -> ModelBundle {
        let scorer = AdditiveScorer {
            model: AdditiveModel {
                intercept: 0.0,
                curves: vec![AdditiveCurve {
                    name: "x".into(),
                    knots: vec![-1.0, 1.0],
                    effects: vec![-0.5, 0.5],
                }],
                l2: 1.0,
                smoothing: 0.25,
                max_abs_effect: 1.0,
            },
            calibration: CalibrationModel {
                intercept: 0.0,
                slope: 1.0,
            },
            threshold: 0.5,
        };
        ModelBundle::Additive {
            artifact_version: MODEL_ARTIFACT_VERSION,
            embedding_model_id: "arcface-test".into(),
            feature_schema_version: 1,
            membership: scorer.clone(),
            cluster_quality: scorer,
        }
    }

    fn candidate_for_bundle(
        stage: ProfileStage,
        report: ValidationReport,
        bundle: &ModelBundle,
    ) -> NewProfile {
        NewProfile {
            artifact_version: 1,
            embedding_model_id: "arcface-test".into(),
            feature_schema_version: 1,
            model_kind: bundle.model_kind().into(),
            parameters: serde_json::to_vec(bundle).unwrap(),
            training_evidence: TrainingEvidenceCounts {
                positive_pairs: 20,
                negative_pairs: 20,
                explicit_negative_pairs: 0,
            },
            validation_report: report,
            stage,
        }
    }

    fn candidate(stage: ProfileStage, report: ValidationReport) -> NewProfile {
        candidate_for_bundle(stage, report, &logistic_bundle())
    }

    fn failure_keys(failures: &[GateFailure]) -> Vec<(&str, &str)> {
        failures
            .iter()
            .map(|failure| (failure.dataset_key.as_str(), failure.gate.as_str()))
            .collect()
    }

    #[test]
    fn schema_and_dataset_count_mismatches_reject() {
        let active = report(0.70, 3);
        let mut wrong_schema = report(0.72, 3);
        wrong_schema.protocol_version = 2;
        let failures = evaluate_promotion(
            Some(&active),
            &wrong_schema,
            &gates(),
            ProfileStage::Grouping,
        )
        .unwrap();
        assert!(failure_keys(&failures).contains(&("", "protocol_version")));

        let mut too_small = report(0.72, 3);
        too_small.datasets.pop();
        let failures =
            evaluate_promotion(Some(&active), &too_small, &gates(), ProfileStage::Grouping)
                .unwrap();
        assert!(failure_keys(&failures).contains(&("", "min_datasets")));
    }

    #[test]
    fn active_baseline_schema_must_match_the_promotion_protocol() {
        for field in ["protocol", "evidence", "feature"] {
            let mut active = report(0.70, 3);
            match field {
                "protocol" => active.protocol_version = 2,
                "evidence" => active.evidence_schema_version = 2,
                "feature" => active.feature_schema_version = 2,
                _ => unreachable!(),
            }

            assert!(matches!(
                evaluate_promotion(
                    Some(&active),
                    &report(0.72, 3),
                    &gates(),
                    ProfileStage::Grouping,
                ),
                Err(ProfileError::IncompatibleProfile(_))
            ));
        }
    }

    #[test]
    fn stage_specific_metrics_are_required() {
        let active = report(0.70, 3);
        let mut candidate = report(0.72, 3);
        candidate.datasets[0].suggestions = None;
        candidate.datasets[1].clustering.labeled_identities = 100;
        candidate.datasets[1].clustering.predicted_clusters = 90;
        candidate.datasets[1].clustering.true_positive_pairs = 0;
        candidate.datasets[1].clustering.false_positive_pairs = 0;
        candidate.datasets[1].clustering.false_negative_pairs = 0;
        candidate.datasets[1].clustering.pair_precision = None;
        candidate.datasets[1].clustering.pair_recall = None;
        candidate.datasets[1].clustering.pair_f1 = None;
        candidate.datasets[1].clustering.mixed_clusters = 0;

        let failures =
            evaluate_promotion(Some(&active), &candidate, &gates(), ProfileStage::Grouping)
                .unwrap();
        assert_eq!(
            failure_keys(&failures),
            vec![
                ("library-a", "suggestion_metrics"),
                ("library-b", "pair_precision"),
                ("library-b", "pair_recall"),
            ]
        );
    }

    #[test]
    fn hard_rules_explanations_and_resources_fail_in_stable_order() {
        let active = report(0.70, 3);
        let mut candidate = report(0.72, 3);
        candidate.datasets[0].hard_rule_violations = 1;
        candidate.datasets[0].wall_time_ms = 201.0;
        candidate.datasets[1].invalid_explanations = 1;
        candidate.datasets[1].peak_memory_mib = 101.0;

        let failures =
            evaluate_promotion(Some(&active), &candidate, &gates(), ProfileStage::Shadow).unwrap();
        assert_eq!(
            failure_keys(&failures),
            vec![
                ("library-a", "hard_rule_violations"),
                ("library-a", "wall_time_ms"),
                ("library-b", "invalid_explanations"),
                ("library-b", "peak_memory_mib"),
            ]
        );
    }

    #[test]
    fn grouping_rejects_regressions_and_requires_one_quality_gain() {
        let active = report(0.70, 3);
        let unchanged = report(0.70, 3);
        let failures =
            evaluate_promotion(Some(&active), &unchanged, &gates(), ProfileStage::Grouping)
                .unwrap();
        assert!(failure_keys(&failures).contains(&("", "quality_gain")));

        let mut regressed = report(0.72, 3);
        regressed.datasets[0].clustering.false_positive_pairs = 1;
        let precision = 720.0 / 721.0;
        regressed.datasets[0].clustering.pair_precision = Some(precision);
        regressed.datasets[0].clustering.pair_f1 =
            Some(2.0 * precision * 0.72 / (precision + 0.72));
        regressed.datasets[1].clustering.mixed_clusters = 2;
        let failures =
            evaluate_promotion(Some(&active), &regressed, &gates(), ProfileStage::Grouping)
                .unwrap();
        assert!(failure_keys(&failures).contains(&("library-a", "pair_precision_regression")));
        assert!(failure_keys(&failures).contains(&("library-b", "mixed_cluster_rate_regression")));

        assert!(evaluate_promotion(
            Some(&active),
            &report(0.72, 3),
            &gates(),
            ProfileStage::Grouping,
        )
        .unwrap()
        .is_empty());
        assert!(evaluate_promotion(
            Some(&active),
            &report(0.70, 2),
            &gates(),
            ProfileStage::Grouping,
        )
        .unwrap()
        .is_empty());
    }

    #[test]
    fn non_finite_gate_values_are_rejected_before_comparison() {
        let mut invalid = gates();
        invalid.max_wall_time_ms = f64::NAN;
        assert!(matches!(
            evaluate_promotion(
                Some(&report(0.70, 3)),
                &report(0.72, 3),
                &invalid,
                ProfileStage::Grouping,
            ),
            Err(ProfileError::InvalidGate("max_wall_time_ms"))
        ));
    }

    #[test]
    fn rate_gates_must_be_probabilities() {
        let mut invalid = gates();
        invalid.max_pair_precision_drop = 1.01;
        assert!(matches!(
            evaluate_promotion(
                Some(&report(0.70, 3)),
                &report(0.72, 3),
                &invalid,
                ProfileStage::Grouping,
            ),
            Err(ProfileError::InvalidGate("max_pair_precision_drop"))
        ));

        let mut invalid = gates();
        invalid.min_pair_recall_gain = 1.01;
        assert!(matches!(
            evaluate_promotion(
                Some(&report(0.70, 3)),
                &report(0.72, 3),
                &invalid,
                ProfileStage::Grouping,
            ),
            Err(ProfileError::InvalidGate("min_pair_recall_gain"))
        ));
    }

    #[test]
    fn malformed_aggregate_metrics_are_rejected_before_gating() {
        let mut invalid = report(0.72, 3);
        invalid.datasets[0].clustering.pair_precision = Some(1.01);
        assert!(matches!(
            evaluate_promotion(None, &invalid, &gates(), ProfileStage::Shadow),
            Err(ProfileError::InvalidReport(_))
        ));

        let mut invalid = report(0.72, 3);
        invalid.datasets[0].suggestions.as_mut().unwrap().correct = 99;
        assert!(matches!(
            evaluate_promotion(None, &invalid, &gates(), ProfileStage::Shadow),
            Err(ProfileError::InvalidReport(_))
        ));

        let mut invalid = report(0.72, 3);
        invalid.datasets[0].clustering.unassigned_rate = f64::NAN;
        assert!(matches!(
            evaluate_promotion(None, &invalid, &gates(), ProfileStage::Shadow),
            Err(ProfileError::InvalidReport(_))
        ));

        let mut invalid = report(0.72, 3);
        let clustering = &mut invalid.datasets[0].clustering;
        clustering.predicted_clusters = 0;
        clustering.true_positive_pairs = 0;
        clustering.false_positive_pairs = 0;
        clustering.false_negative_pairs = 1_000;
        clustering.mixed_clusters = 0;
        clustering.pair_precision = None;
        clustering.pair_recall = Some(0.0);
        clustering.pair_f1 = None;
        assert!(matches!(
            evaluate_promotion(None, &invalid, &gates(), ProfileStage::Shadow),
            Err(ProfileError::InvalidReport(_))
        ));
    }

    #[test]
    fn candidate_must_cover_the_same_frozen_datasets() {
        let active = report(0.70, 3);
        let mut candidate = report(0.72, 3);
        candidate.datasets[1].dataset_key = "library-c".into();
        let failures =
            evaluate_promotion(Some(&active), &candidate, &gates(), ProfileStage::Grouping)
                .unwrap();
        assert!(failure_keys(&failures).contains(&("library-b", "missing_dataset")));
        assert!(failure_keys(&failures).contains(&("library-c", "unexpected_dataset")));
    }

    #[test]
    fn suggestion_precision_requires_enough_evidence() {
        let mut candidate = report(0.72, 3);
        for dataset in &mut candidate.datasets {
            dataset.suggestions = Some(SuggestionMetrics {
                correct: 1,
                incorrect: 0,
                not_suggested: 99,
                precision: Some(1.0),
                coverage: 0.01,
            });
        }
        let failures =
            evaluate_promotion(None, &candidate, &gates(), ProfileStage::Suggestion).unwrap();
        assert!(failure_keys(&failures)
            .contains(&("library-a", "suggestion_precision_wilson_lower_bound")));
    }

    #[test]
    fn profile_metadata_must_match_the_report_and_active_embedding_model() {
        let conn = Connection::open_in_memory().unwrap();
        ensure_profile_table(&conn).unwrap();
        let mut mismatch = candidate(ProfileStage::Shadow, report(0.70, 3));
        mismatch.feature_schema_version = 2;
        assert!(matches!(
            insert_candidate(&conn, &mismatch),
            Err(ProfileError::InvalidProfile(_))
        ));

        let active_id =
            insert_candidate(&conn, &candidate(ProfileStage::Shadow, report(0.70, 3))).unwrap();
        evaluate_and_promote(&conn, active_id, &gates()).unwrap();
        let mut other_bundle = logistic_bundle();
        match &mut other_bundle {
            ModelBundle::Logistic {
                embedding_model_id, ..
            } => *embedding_model_id = "other/test-model".into(),
            _ => unreachable!(),
        }
        let mut other_model =
            candidate_for_bundle(ProfileStage::Suggestion, report(0.72, 3), &other_bundle);
        other_model.embedding_model_id = "other/test-model".into();
        let other_id = insert_candidate(&conn, &other_model).unwrap();
        assert!(matches!(
            evaluate_and_promote(&conn, other_id, &gates()),
            Err(ProfileError::IncompatibleProfile(_))
        ));
    }

    #[test]
    fn promotion_revalidates_metadata_loaded_inside_the_transaction() {
        let conn = Connection::open_in_memory().unwrap();
        ensure_profile_table(&conn).unwrap();
        let id =
            insert_candidate(&conn, &candidate(ProfileStage::Shadow, report(0.70, 3))).unwrap();
        conn.execute(
            "UPDATE face_learning_profiles SET feature_schema_version = 2 WHERE id = ?1",
            [id],
        )
        .unwrap();
        assert!(matches!(
            evaluate_and_promote(&conn, id, &gates()),
            Err(ProfileError::InvalidProfile(_))
        ));
        assert!(conn.is_autocommit());
    }

    #[test]
    fn failed_and_successful_promotions_preserve_one_active_profile() {
        let conn = Connection::open_in_memory().unwrap();
        ensure_profile_table(&conn).unwrap();

        let active_id =
            insert_candidate(&conn, &candidate(ProfileStage::Shadow, report(0.70, 3))).unwrap();
        assert_eq!(
            evaluate_and_promote(&conn, active_id, &gates()).unwrap(),
            PromotionOutcome::Promoted
        );
        assert_eq!(active_profile(&conn).unwrap().unwrap().id, active_id);

        let mut weak = report(0.70, 3);
        weak.datasets[0].suggestions = Some(SuggestionMetrics {
            correct: 50,
            incorrect: 50,
            not_suggested: 0,
            precision: Some(0.5),
            coverage: 1.0,
        });
        let failed_id =
            insert_candidate(&conn, &candidate(ProfileStage::Suggestion, weak)).unwrap();
        let outcome = evaluate_and_promote(&conn, failed_id, &gates()).unwrap();
        let PromotionOutcome::Rejected(failures) = outcome else {
            panic!("candidate must be rejected")
        };
        assert!(failure_keys(&failures).contains(&("library-a", "suggestion_precision")));
        assert_eq!(active_profile(&conn).unwrap().unwrap().id, active_id);
        let failed_status: String = conn
            .query_row(
                "SELECT status FROM face_learning_profiles WHERE id = ?1",
                [failed_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(failed_status, "rejected");

        let passing_id =
            insert_candidate(&conn, &candidate(ProfileStage::Suggestion, report(0.72, 3))).unwrap();
        assert_eq!(
            evaluate_and_promote(&conn, passing_id, &gates()).unwrap(),
            PromotionOutcome::Promoted
        );
        assert_eq!(active_profile(&conn).unwrap().unwrap().id, passing_id);
        let old_status: String = conn
            .query_row(
                "SELECT status FROM face_learning_profiles WHERE id = ?1",
                [active_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(old_status, "retired");
    }

    #[test]
    fn sqlite_failure_rolls_back_retirement_and_activation() {
        let conn = Connection::open_in_memory().unwrap();
        ensure_profile_table(&conn).unwrap();
        let active_id =
            insert_candidate(&conn, &candidate(ProfileStage::Shadow, report(0.70, 3))).unwrap();
        evaluate_and_promote(&conn, active_id, &gates()).unwrap();

        let candidate_id =
            insert_candidate(&conn, &candidate(ProfileStage::Suggestion, report(0.72, 3))).unwrap();
        conn.execute_batch(&format!(
            "CREATE TRIGGER abort_face_profile_activation
             BEFORE UPDATE OF status ON face_learning_profiles
             WHEN NEW.id = {candidate_id} AND NEW.status = 'active'
             BEGIN SELECT RAISE(ABORT, 'injected activation failure'); END;"
        ))
        .unwrap();

        assert!(evaluate_and_promote(&conn, candidate_id, &gates()).is_err());
        assert_eq!(active_profile(&conn).unwrap().unwrap().id, active_id);
        let candidate_status: String = conn
            .query_row(
                "SELECT status FROM face_learning_profiles WHERE id = ?1",
                [candidate_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(candidate_status, "candidate");
    }

    #[test]
    fn commit_failure_rolls_back_and_closes_the_transaction() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON").unwrap();
        ensure_profile_table(&conn).unwrap();
        let active_id =
            insert_candidate(&conn, &candidate(ProfileStage::Shadow, report(0.70, 3))).unwrap();
        evaluate_and_promote(&conn, active_id, &gates()).unwrap();
        let candidate_id =
            insert_candidate(&conn, &candidate(ProfileStage::Suggestion, report(0.72, 3))).unwrap();
        conn.execute_batch(&format!(
            "CREATE TABLE profile_commit_parent (id INTEGER PRIMARY KEY);
             CREATE TABLE profile_commit_child (
                 parent_id INTEGER REFERENCES profile_commit_parent(id)
                     DEFERRABLE INITIALLY DEFERRED
             );
             CREATE TRIGGER fail_profile_commit
             AFTER UPDATE OF status ON face_learning_profiles
             WHEN NEW.id = {candidate_id} AND NEW.status = 'active'
             BEGIN INSERT INTO profile_commit_child(parent_id) VALUES (999); END;"
        ))
        .unwrap();

        assert!(evaluate_and_promote(&conn, candidate_id, &gates()).is_err());
        assert!(conn.is_autocommit());
        assert_eq!(active_profile(&conn).unwrap().unwrap().id, active_id);
        let status: String = conn
            .query_row(
                "SELECT status FROM face_learning_profiles WHERE id = ?1",
                [candidate_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "candidate");
    }

    fn stored_profile_count(conn: &Connection) -> usize {
        conn.query_row("SELECT count(*) FROM face_learning_profiles", [], |row| {
            row.get::<_, i64>(0)
        })
        .map(|count| count as usize)
        .unwrap()
    }

    fn face_rows(conn: &Connection) -> Vec<(i64, String)> {
        let mut statement = conn
            .prepare("SELECT id, state FROM faces ORDER BY id")
            .unwrap();
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    }

    #[test]
    fn insert_candidate_rejects_artifacts_it_cannot_decode() {
        let conn = Connection::open_in_memory().unwrap();
        ensure_profile_table(&conn).unwrap();

        let mut garbage = candidate(ProfileStage::Suggestion, report(0.70, 3));
        garbage.parameters = vec![1, 2, 3];
        assert!(matches!(
            insert_candidate(&conn, &garbage),
            Err(ProfileError::InvalidProfile(_))
        ));

        let mut mislabeled = candidate_for_bundle(
            ProfileStage::Suggestion,
            report(0.70, 3),
            &logistic_bundle(),
        );
        mislabeled.model_kind = "deterministic-test".into();
        assert!(matches!(
            insert_candidate(&conn, &mislabeled),
            Err(ProfileError::InvalidProfile(_))
        ));

        let mut wrong_kind = candidate_for_bundle(
            ProfileStage::Suggestion,
            report(0.70, 3),
            &additive_bundle(),
        );
        wrong_kind.model_kind = "logistic".into();
        assert!(matches!(
            insert_candidate(&conn, &wrong_kind),
            Err(ProfileError::InvalidProfile(_))
        ));

        assert_eq!(
            stored_profile_count(&conn),
            0,
            "rejected artifacts must not be stored"
        );
    }

    #[test]
    fn insert_candidate_rejects_row_metadata_that_disagrees_with_the_artifact() {
        let conn = Connection::open_in_memory().unwrap();
        ensure_profile_table(&conn).unwrap();

        // The row names a different embedding model than the decoded bundle.
        let mut wrong_model = candidate_for_bundle(
            ProfileStage::Suggestion,
            report(0.70, 3),
            &logistic_bundle(),
        );
        wrong_model.embedding_model_id = "other/model".into();
        assert!(matches!(
            insert_candidate(&conn, &wrong_model),
            Err(ProfileError::InvalidProfile(_))
        ));

        // The row claims a newer feature schema than the artifact carries;
        // the report must be moved with it so metadata checks pass and the
        // artifact comparison is what rejects the insert.
        let mut wrong_schema = candidate_for_bundle(
            ProfileStage::Suggestion,
            report(0.70, 3),
            &logistic_bundle(),
        );
        wrong_schema.feature_schema_version = 2;
        wrong_schema.validation_report.feature_schema_version = 2;
        assert!(matches!(
            insert_candidate(&conn, &wrong_schema),
            Err(ProfileError::InvalidProfile(_))
        ));

        assert_eq!(
            stored_profile_count(&conn),
            0,
            "metadata mismatches must not be stored"
        );
    }

    #[test]
    fn insert_candidate_rejects_grouping_and_unexplainable_candidates() {
        let conn = Connection::open_in_memory().unwrap();
        ensure_profile_table(&conn).unwrap();

        assert!(matches!(
            insert_candidate(&conn, &candidate(ProfileStage::Grouping, report(0.70, 3))),
            Err(ProfileError::InvalidProfile(_))
        ));

        let mut unexplainable = candidate(ProfileStage::Suggestion, report(0.70, 3));
        unexplainable.validation_report.datasets[0].invalid_explanations = 1;
        assert!(matches!(
            insert_candidate(&conn, &unexplainable),
            Err(ProfileError::InvalidProfile(_))
        ));

        assert_eq!(
            stored_profile_count(&conn),
            0,
            "rejected candidates must not be stored"
        );
    }

    #[test]
    fn suggestion_promotion_never_touches_face_rows_or_the_active_profile_on_failure() {
        let conn = Connection::open_in_memory().unwrap();
        ensure_profile_table(&conn).unwrap();
        conn.execute_batch(
            "CREATE TABLE faces (id INTEGER PRIMARY KEY, state TEXT);
             INSERT INTO faces(state) VALUES ('cluster-42');",
        )
        .unwrap();
        let faces_before = face_rows(&conn);

        let active_id =
            insert_candidate(&conn, &candidate(ProfileStage::Suggestion, report(0.70, 3))).unwrap();
        assert_eq!(
            evaluate_and_promote(&conn, active_id, &gates()).unwrap(),
            PromotionOutcome::Promoted
        );

        let mut weak = report(0.72, 3);
        weak.datasets[0].suggestions = Some(SuggestionMetrics {
            correct: 50,
            incorrect: 50,
            not_suggested: 0,
            precision: Some(0.5),
            coverage: 1.0,
        });
        let weak_id = insert_candidate(&conn, &candidate(ProfileStage::Suggestion, weak)).unwrap();
        assert!(matches!(
            evaluate_and_promote(&conn, weak_id, &gates()).unwrap(),
            PromotionOutcome::Rejected(_)
        ));

        assert_eq!(active_profile(&conn).unwrap().unwrap().id, active_id);
        assert_eq!(face_rows(&conn), faces_before);
    }
}
