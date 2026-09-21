use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fmt;

pub const EVIDENCE_SCHEMA_VERSION: u32 = 1;
const RECONSTRUCTION_TOLERANCE: f64 = 1e-9;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DecisionKind {
    PairScore,
    ClusterMerge,
    PersonSuggestion,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DecisionOutcome {
    Allowed,
    RejectedByScore,
    VetoedByRule,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DecisionTarget {
    Face(i64),
    Cluster(i64),
    Person(String),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FeatureContribution {
    pub name: String,
    pub value: f64,
    pub contribution: f64,
}

impl FeatureContribution {
    pub fn new(name: impl Into<String>, value: f64, contribution: f64) -> Self {
        Self {
            name: name.into(),
            value,
            contribution,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleVeto {
    pub rule_id: i64,
    pub kind: String,
}

impl RuleVeto {
    pub fn new(rule_id: i64, kind: impl Into<String>) -> Self {
        Self {
            rule_id,
            kind: kind.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Calibration {
    pub intercept: f64,
    pub slope: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ValidationSummary {
    pub protocol_version: u32,
    pub datasets: usize,
    pub pair_precision: Option<f64>,
    pub pair_recall: Option<f64>,
    pub suggestion_precision: Option<f64>,
    pub suggestion_coverage: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DecisionEvidence {
    pub schema_version: u32,
    pub profile_id: i64,
    pub feature_schema_version: u32,
    pub decision_kind: DecisionKind,
    pub outcome: DecisionOutcome,
    pub subject_face_ids: Vec<i64>,
    pub target: DecisionTarget,
    pub intercept: f64,
    pub raw_logit: f64,
    pub calibration: Calibration,
    pub calibrated_confidence: f64,
    pub threshold: f64,
    pub margin: f64,
    pub features: Vec<FeatureContribution>,
    pub support_face_ids: Vec<i64>,
    pub rule_vetoes: Vec<RuleVeto>,
    pub validation: ValidationSummary,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EvidenceError {
    UnsupportedSchemaVersion(u32),
    NonFinite(String),
    ProbabilityOutOfRange(String),
    ContributionMismatch,
    CalibratedScoreMismatch,
    MarginMismatch,
    OutcomeMismatch,
    DuplicateFeatureName(String),
    DuplicateRuleId(i64),
}

impl fmt::Display for EvidenceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedSchemaVersion(version) => {
                write!(f, "unsupported face evidence schema version {version}")
            }
            Self::NonFinite(field) => write!(f, "face evidence field {field} is not finite"),
            Self::ProbabilityOutOfRange(field) => {
                write!(f, "face evidence probability {field} is outside 0..=1")
            }
            Self::ContributionMismatch => {
                write!(
                    f,
                    "face evidence contributions do not reconstruct the raw logit"
                )
            }
            Self::CalibratedScoreMismatch => {
                write!(
                    f,
                    "face evidence calibration does not reconstruct confidence"
                )
            }
            Self::MarginMismatch => {
                write!(
                    f,
                    "face evidence margin does not match confidence minus threshold"
                )
            }
            Self::OutcomeMismatch => {
                write!(
                    f,
                    "face evidence outcome contradicts its score or rule vetoes"
                )
            }
            Self::DuplicateFeatureName(name) => {
                write!(f, "face evidence repeats feature {name}")
            }
            Self::DuplicateRuleId(id) => write!(f, "face evidence repeats rule {id}"),
        }
    }
}

impl std::error::Error for EvidenceError {}

fn ensure_finite(field: &str, value: f64) -> Result<(), EvidenceError> {
    if value.is_finite() {
        Ok(())
    } else {
        Err(EvidenceError::NonFinite(field.to_owned()))
    }
}

fn ensure_probability(field: &str, value: f64) -> Result<(), EvidenceError> {
    ensure_finite(field, value)?;
    if (0.0..=1.0).contains(&value) {
        Ok(())
    } else {
        Err(EvidenceError::ProbabilityOutOfRange(field.to_owned()))
    }
}

fn logistic(value: f64) -> f64 {
    if value >= 0.0 {
        1.0 / (1.0 + (-value).exp())
    } else {
        let exp = value.exp();
        exp / (1.0 + exp)
    }
}

impl DecisionEvidence {
    pub fn validate(&self) -> Result<(), EvidenceError> {
        if self.schema_version != EVIDENCE_SCHEMA_VERSION {
            return Err(EvidenceError::UnsupportedSchemaVersion(self.schema_version));
        }

        ensure_finite("intercept", self.intercept)?;
        ensure_finite("raw_logit", self.raw_logit)?;
        ensure_finite("calibration_intercept", self.calibration.intercept)?;
        ensure_finite("calibration_slope", self.calibration.slope)?;
        ensure_probability("calibrated_confidence", self.calibrated_confidence)?;
        ensure_probability("threshold", self.threshold)?;
        ensure_finite("margin", self.margin)?;

        let mut feature_names = BTreeSet::new();
        for feature in &self.features {
            ensure_finite("feature_value", feature.value)?;
            ensure_finite("feature_contribution", feature.contribution)?;
            if !feature_names.insert(feature.name.as_str()) {
                return Err(EvidenceError::DuplicateFeatureName(feature.name.clone()));
            }
        }
        let mut rule_ids = BTreeSet::new();
        for rule in &self.rule_vetoes {
            if !rule_ids.insert(rule.rule_id) {
                return Err(EvidenceError::DuplicateRuleId(rule.rule_id));
            }
        }
        for (field, value) in [
            ("validation_metric", self.validation.pair_precision),
            ("validation_metric", self.validation.pair_recall),
            ("validation_metric", self.validation.suggestion_precision),
            ("validation_metric", self.validation.suggestion_coverage),
        ] {
            if let Some(value) = value {
                ensure_probability(field, value)?;
            }
        }

        let reconstructed_logit = self.intercept
            + self
                .features
                .iter()
                .map(|feature| feature.contribution)
                .sum::<f64>();
        if (reconstructed_logit - self.raw_logit).abs() > RECONSTRUCTION_TOLERANCE {
            return Err(EvidenceError::ContributionMismatch);
        }

        let reconstructed_confidence =
            logistic(self.calibration.intercept + self.calibration.slope * self.raw_logit);
        if (reconstructed_confidence - self.calibrated_confidence).abs() > RECONSTRUCTION_TOLERANCE
        {
            return Err(EvidenceError::CalibratedScoreMismatch);
        }
        if (self.calibrated_confidence - self.threshold - self.margin).abs()
            > RECONSTRUCTION_TOLERANCE
        {
            return Err(EvidenceError::MarginMismatch);
        }
        let outcome_matches = match self.outcome {
            DecisionOutcome::Allowed => {
                self.calibrated_confidence >= self.threshold && self.rule_vetoes.is_empty()
            }
            DecisionOutcome::RejectedByScore => {
                self.calibrated_confidence < self.threshold && self.rule_vetoes.is_empty()
            }
            DecisionOutcome::VetoedByRule => !self.rule_vetoes.is_empty(),
        };
        if !outcome_matches {
            return Err(EvidenceError::OutcomeMismatch);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> DecisionEvidence {
        DecisionEvidence {
            schema_version: EVIDENCE_SCHEMA_VERSION,
            profile_id: 7,
            feature_schema_version: 1,
            decision_kind: DecisionKind::PersonSuggestion,
            outcome: DecisionOutcome::Allowed,
            subject_face_ids: vec![11],
            target: DecisionTarget::Person("ada".into()),
            intercept: -1.0,
            raw_logit: 0.85,
            calibration: Calibration {
                intercept: 0.0,
                slope: 1.0,
            },
            calibrated_confidence: 0.7005671424739729,
            threshold: 0.65,
            margin: 0.05056714247397287,
            features: vec![
                FeatureContribution::new("cosine_similarity", 0.8, 2.0),
                FeatureContribution::new("blur_penalty", 0.2, -0.25),
                FeatureContribution::new("support_count", 3.0, 0.10),
            ],
            support_face_ids: vec![21, 22, 23],
            rule_vetoes: Vec::new(),
            validation: ValidationSummary {
                protocol_version: 1,
                datasets: 2,
                pair_precision: Some(0.98),
                pair_recall: Some(0.72),
                suggestion_precision: Some(0.99),
                suggestion_coverage: Some(0.40),
            },
        }
    }

    #[test]
    fn contributions_reconstruct_logit_and_calibrated_score() {
        assert_eq!(fixture().validate(), Ok(()));
    }

    #[test]
    fn contribution_mismatch_is_rejected() {
        let mut evidence = fixture();
        evidence.raw_logit = 0.86;
        assert_eq!(
            evidence.validate(),
            Err(EvidenceError::ContributionMismatch)
        );
    }

    #[test]
    fn calibrated_score_mismatch_is_rejected() {
        let mut evidence = fixture();
        evidence.calibrated_confidence = 0.71;
        evidence.margin = 0.06;
        assert_eq!(
            evidence.validate(),
            Err(EvidenceError::CalibratedScoreMismatch)
        );
    }

    #[test]
    fn confidence_and_threshold_must_be_probabilities() {
        let mut evidence = fixture();
        evidence.calibrated_confidence = 1.01;
        assert_eq!(
            evidence.validate(),
            Err(EvidenceError::ProbabilityOutOfRange(
                "calibrated_confidence".into()
            ))
        );

        let mut evidence = fixture();
        evidence.threshold = -0.01;
        assert_eq!(
            evidence.validate(),
            Err(EvidenceError::ProbabilityOutOfRange("threshold".into()))
        );
    }

    #[test]
    fn margin_must_match_confidence_minus_threshold() {
        let mut evidence = fixture();
        evidence.margin += 0.01;
        assert_eq!(evidence.validate(), Err(EvidenceError::MarginMismatch));
    }

    #[test]
    fn non_finite_values_are_rejected() {
        let mutations: Vec<(&str, Box<dyn Fn(&mut DecisionEvidence)>)> = vec![
            ("intercept", Box::new(|e| e.intercept = f64::NAN)),
            ("raw_logit", Box::new(|e| e.raw_logit = f64::INFINITY)),
            (
                "calibration_intercept",
                Box::new(|e| e.calibration.intercept = f64::NAN),
            ),
            (
                "calibration_slope",
                Box::new(|e| e.calibration.slope = f64::INFINITY),
            ),
            (
                "calibrated_confidence",
                Box::new(|e| e.calibrated_confidence = f64::NAN),
            ),
            ("threshold", Box::new(|e| e.threshold = f64::INFINITY)),
            ("margin", Box::new(|e| e.margin = f64::NAN)),
            (
                "feature_value",
                Box::new(|e| e.features[0].value = f64::NAN),
            ),
            (
                "feature_contribution",
                Box::new(|e| e.features[0].contribution = f64::INFINITY),
            ),
            (
                "validation_metric",
                Box::new(|e| e.validation.pair_precision = Some(f64::NAN)),
            ),
        ];

        for (field, mutate) in mutations {
            let mut evidence = fixture();
            mutate(&mut evidence);
            assert_eq!(
                evidence.validate(),
                Err(EvidenceError::NonFinite(field.into())),
                "field {field}"
            );
        }
    }

    #[test]
    fn duplicate_feature_names_and_rule_ids_are_rejected() {
        let mut evidence = fixture();
        evidence
            .features
            .push(FeatureContribution::new("cosine_similarity", 0.7, 0.0));
        assert_eq!(
            evidence.validate(),
            Err(EvidenceError::DuplicateFeatureName(
                "cosine_similarity".into()
            ))
        );

        let mut evidence = fixture();
        evidence.rule_vetoes = vec![
            RuleVeto::new(4, "not_person"),
            RuleVeto::new(4, "separate_sets"),
        ];
        assert_eq!(evidence.validate(), Err(EvidenceError::DuplicateRuleId(4)));
    }

    #[test]
    fn hard_rule_veto_stays_separate_from_model_contributions() {
        let mut evidence = fixture();
        let model_score = evidence.raw_logit;
        let contributions = evidence.features.clone();
        evidence.outcome = DecisionOutcome::VetoedByRule;
        evidence.rule_vetoes = vec![RuleVeto::new(9, "not_person")];

        assert_eq!(evidence.validate(), Ok(()));
        assert_eq!(evidence.raw_logit, model_score);
        assert_eq!(evidence.features, contributions);
        assert_eq!(evidence.rule_vetoes.len(), 1);
    }

    #[test]
    fn outcome_must_match_threshold_and_rule_vetoes() {
        let mut allowed_below_threshold = fixture();
        allowed_below_threshold.threshold = 0.80;
        allowed_below_threshold.margin =
            allowed_below_threshold.calibrated_confidence - allowed_below_threshold.threshold;
        assert_eq!(
            allowed_below_threshold.validate(),
            Err(EvidenceError::OutcomeMismatch)
        );

        let mut allowed_with_veto = fixture();
        allowed_with_veto.rule_vetoes = vec![RuleVeto::new(9, "not_person")];
        assert_eq!(
            allowed_with_veto.validate(),
            Err(EvidenceError::OutcomeMismatch)
        );

        let mut rejected_above_threshold = fixture();
        rejected_above_threshold.outcome = DecisionOutcome::RejectedByScore;
        assert_eq!(
            rejected_above_threshold.validate(),
            Err(EvidenceError::OutcomeMismatch)
        );

        let mut vetoed_without_rule = fixture();
        vetoed_without_rule.outcome = DecisionOutcome::VetoedByRule;
        assert_eq!(
            vetoed_without_rule.validate(),
            Err(EvidenceError::OutcomeMismatch)
        );
    }

    #[test]
    fn validation_summary_rates_must_be_probabilities() {
        let mut evidence = fixture();
        evidence.validation.pair_precision = Some(1.01);
        assert_eq!(
            evidence.validate(),
            Err(EvidenceError::ProbabilityOutOfRange(
                "validation_metric".into()
            ))
        );
    }

    #[test]
    fn json_round_trip_preserves_schema_and_evidence() {
        let evidence = fixture();
        let json = serde_json::to_string(&evidence).unwrap();
        assert!(json.contains("\"schema_version\":1"));
        let decoded: DecisionEvidence = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, evidence);
        assert_eq!(serde_json::to_string(&decoded).unwrap(), json);
    }

    #[test]
    fn future_schema_version_is_rejected() {
        let mut evidence = fixture();
        evidence.schema_version += 1;
        assert_eq!(
            evidence.validate(),
            Err(EvidenceError::UnsupportedSchemaVersion(2))
        );
    }
}
