use super::{ClusteringMetrics, SuggestionMetrics};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PromotionGates {
    pub protocol_version: u32,
    pub evidence_schema_version: u32,
    pub feature_schema_version: u32,
    pub min_datasets: usize,
    pub max_hard_rule_violations: usize,
    pub max_invalid_explanations: usize,
    pub min_suggestion_precision: f64,
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

fn validate_gates(gates: &PromotionGates) -> Result<(), ProfileError> {
    if gates.min_datasets == 0 {
        return Err(ProfileError::InvalidGate("min_datasets"));
    }
    for (value, name) in [
        (gates.min_suggestion_precision, "min_suggestion_precision"),
        (gates.min_suggestion_coverage, "min_suggestion_coverage"),
        (gates.max_wall_time_ms, "max_wall_time_ms"),
        (gates.max_peak_memory_mib, "max_peak_memory_mib"),
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
    ] {
        validate_finite_nonnegative(value, name)?;
    }
    if gates.min_suggestion_precision > 1.0 || gates.min_suggestion_coverage > 1.0 {
        return Err(ProfileError::InvalidGate("suggestion_threshold"));
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
    }
    Ok(())
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

pub fn insert_candidate(conn: &Connection, profile: &NewProfile) -> Result<i64, ProfileError> {
    ensure_profile_table(conn)?;
    validate_report(&profile.validation_report)?;
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
        let active = active_profile(conn)?;
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
            conn.execute_batch("COMMIT")?;
            Ok(outcome)
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
    use crate::face_learning::{ClusteringMetrics, SuggestionMetrics};
    use rusqlite::Connection;

    fn clustering(recall: f64, fragmented: usize) -> ClusteringMetrics {
        ClusteringMetrics {
            labeled_faces: 100,
            labeled_identities: 10,
            predicted_clusters: 10,
            true_positive_pairs: 70,
            false_positive_pairs: 2,
            false_negative_pairs: 30,
            pair_precision: Some(0.98),
            pair_recall: Some(recall),
            pair_f1: Some(0.81),
            mixed_clusters: 1,
            fragmented_identities: fragmented,
            unassigned_labeled_faces: 10,
            unassigned_rate: 0.10,
        }
    }

    fn suggestions() -> SuggestionMetrics {
        SuggestionMetrics {
            correct: 40,
            incorrect: 0,
            not_suggested: 60,
            precision: Some(1.0),
            coverage: 0.40,
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
        }
    }

    fn candidate(stage: ProfileStage, report: ValidationReport) -> NewProfile {
        NewProfile {
            artifact_version: 1,
            embedding_model_id: "arcface-test".into(),
            feature_schema_version: 1,
            model_kind: "deterministic-test".into(),
            parameters: vec![1, 2, 3],
            training_evidence: TrainingEvidenceCounts {
                positive_pairs: 20,
                negative_pairs: 20,
                explicit_negative_pairs: 0,
            },
            validation_report: report,
            stage,
        }
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
    fn stage_specific_metrics_are_required() {
        let active = report(0.70, 3);
        let mut candidate = report(0.72, 3);
        candidate.datasets[0].suggestions = None;
        candidate.datasets[1].clustering.pair_precision = None;
        candidate.datasets[1].clustering.pair_recall = None;

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
        regressed.datasets[0].clustering.pair_precision = Some(0.97);
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

        let failed_id =
            insert_candidate(&conn, &candidate(ProfileStage::Grouping, report(0.70, 3))).unwrap();
        let outcome = evaluate_and_promote(&conn, failed_id, &gates()).unwrap();
        let PromotionOutcome::Rejected(failures) = outcome else {
            panic!("candidate must be rejected")
        };
        assert!(failure_keys(&failures).contains(&("", "quality_gain")));
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
            insert_candidate(&conn, &candidate(ProfileStage::Grouping, report(0.72, 3))).unwrap();
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
            insert_candidate(&conn, &candidate(ProfileStage::Grouping, report(0.72, 3))).unwrap();
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
}
