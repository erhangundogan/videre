use super::{
    Calibration, DecisionEvidence, DecisionKind, DecisionOutcome, DecisionTarget,
    FeatureContribution, FeatureVector, ValidationSummary, EVIDENCE_SCHEMA_VERSION,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fmt;

pub const MODEL_ARTIFACT_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LogisticModel {
    pub feature_names: Vec<String>,
    pub means: Vec<f64>,
    pub scales: Vec<f64>,
    pub intercept: f64,
    pub weights: Vec<f64>,
    pub l2: f64,
    pub positive_class_weight: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CalibrationModel {
    pub intercept: f64,
    pub slope: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DecisionScore {
    pub raw_logit: f64,
    pub confidence: f64,
    pub threshold: f64,
    pub contributions: Vec<(String, f64, f64)>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LogisticScorer {
    pub model: LogisticModel,
    pub calibration: CalibrationModel,
    pub threshold: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelBundle {
    pub artifact_version: u32,
    pub embedding_model_id: String,
    pub feature_schema_version: u32,
    pub membership: LogisticScorer,
    pub cluster_quality: LogisticScorer,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModelError {
    InvalidModel(String),
    InvalidFeatureVector(String),
}

impl fmt::Display for ModelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidModel(reason) => write!(f, "invalid logistic model: {reason}"),
            Self::InvalidFeatureVector(reason) => write!(f, "invalid feature vector: {reason}"),
        }
    }
}

impl std::error::Error for ModelError {}

fn sigmoid(value: f64) -> f64 {
    if value >= 0.0 {
        1.0 / (1.0 + (-value).exp())
    } else {
        let exp = value.exp();
        exp / (1.0 + exp)
    }
}

impl LogisticModel {
    pub fn validate(&self) -> Result<(), ModelError> {
        let length = self.feature_names.len();
        if length == 0
            || self.means.len() != length
            || self.scales.len() != length
            || self.weights.len() != length
        {
            return Err(ModelError::InvalidModel(
                "feature parameter lengths disagree".to_owned(),
            ));
        }
        if self
            .feature_names
            .windows(2)
            .any(|names| names[0] >= names[1])
            || self.feature_names.iter().collect::<BTreeSet<_>>().len() != length
        {
            return Err(ModelError::InvalidModel(
                "feature names are not unique and sorted".to_owned(),
            ));
        }
        if !self.intercept.is_finite()
            || !self.l2.is_finite()
            || self.l2 < 0.0
            || !self.positive_class_weight.is_finite()
            || self.positive_class_weight <= 0.0
            || self
                .means
                .iter()
                .chain(&self.weights)
                .any(|value| !value.is_finite())
            || self
                .scales
                .iter()
                .any(|value| !value.is_finite() || *value <= 0.0)
        {
            return Err(ModelError::InvalidModel(
                "parameters must be finite with valid scales and penalties".to_owned(),
            ));
        }
        Ok(())
    }

    pub fn score(
        &self,
        features: &FeatureVector,
    ) -> Result<(f64, Vec<(String, f64, f64)>), ModelError> {
        self.validate()?;
        if features.values.len() != self.feature_names.len()
            || self
                .feature_names
                .iter()
                .any(|name| !features.values.contains_key(name))
        {
            return Err(ModelError::InvalidFeatureVector(
                "feature names do not match the model".to_owned(),
            ));
        }
        let mut raw_logit = self.intercept;
        let mut contributions = Vec::with_capacity(self.feature_names.len());
        for index in 0..self.feature_names.len() {
            let name = &self.feature_names[index];
            let value = features.values[name];
            if !value.is_finite() {
                return Err(ModelError::InvalidFeatureVector(format!(
                    "feature {name} is not finite"
                )));
            }
            let contribution =
                self.weights[index] * ((value - self.means[index]) / self.scales[index]);
            if !contribution.is_finite() {
                return Err(ModelError::InvalidFeatureVector(format!(
                    "feature {name} produced a non-finite contribution"
                )));
            }
            raw_logit += contribution;
            contributions.push((name.clone(), value, contribution));
        }
        if !raw_logit.is_finite() {
            return Err(ModelError::InvalidFeatureVector(
                "raw logit is not finite".to_owned(),
            ));
        }
        Ok((raw_logit, contributions))
    }
}

impl LogisticScorer {
    pub fn validate(&self) -> Result<(), ModelError> {
        self.model.validate()?;
        if !self.calibration.intercept.is_finite()
            || !self.calibration.slope.is_finite()
            || self.calibration.slope <= 0.0
            || !self.threshold.is_finite()
            || !(0.0..=1.0).contains(&self.threshold)
        {
            return Err(ModelError::InvalidModel(
                "calibration or threshold is invalid".to_owned(),
            ));
        }
        Ok(())
    }

    pub fn score(&self, features: &FeatureVector) -> Result<DecisionScore, ModelError> {
        self.validate()?;
        let (raw_logit, contributions) = self.model.score(features)?;
        let confidence = sigmoid(self.calibration.intercept + self.calibration.slope * raw_logit);
        Ok(DecisionScore {
            raw_logit,
            confidence,
            threshold: self.threshold,
            contributions,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn score_with_evidence(
        &self,
        features: &FeatureVector,
        profile_id: i64,
        feature_schema_version: u32,
        decision_kind: DecisionKind,
        subject_face_ids: Vec<i64>,
        target: DecisionTarget,
        support_face_ids: Vec<i64>,
        validation: ValidationSummary,
    ) -> Result<DecisionEvidence, ModelError> {
        let score = self.score(features)?;
        let evidence = DecisionEvidence {
            schema_version: EVIDENCE_SCHEMA_VERSION,
            profile_id,
            feature_schema_version,
            decision_kind,
            outcome: if score.confidence >= score.threshold {
                DecisionOutcome::Allowed
            } else {
                DecisionOutcome::RejectedByScore
            },
            subject_face_ids,
            target,
            intercept: self.model.intercept,
            raw_logit: score.raw_logit,
            calibration: Calibration {
                intercept: self.calibration.intercept,
                slope: self.calibration.slope,
            },
            calibrated_confidence: score.confidence,
            threshold: score.threshold,
            margin: score.confidence - score.threshold,
            features: score
                .contributions
                .into_iter()
                .map(|(name, value, contribution)| FeatureContribution {
                    name,
                    value,
                    contribution,
                })
                .collect(),
            support_face_ids,
            rule_vetoes: Vec::new(),
            validation,
        };
        evidence
            .validate()
            .map_err(|error| ModelError::InvalidModel(error.to_string()))?;
        Ok(evidence)
    }
}

impl ModelBundle {
    pub fn validate(&self) -> Result<(), ModelError> {
        if self.artifact_version != MODEL_ARTIFACT_VERSION {
            return Err(ModelError::InvalidModel(
                "unsupported artifact version".to_owned(),
            ));
        }
        if self.embedding_model_id.trim().is_empty() || self.feature_schema_version == 0 {
            return Err(ModelError::InvalidModel(
                "model identity or feature schema is missing".to_owned(),
            ));
        }
        self.membership.validate()?;
        self.cluster_quality.validate()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::face_learning::{
        DecisionKind, DecisionTarget, ValidationSummary, EVALUATION_PROTOCOL_VERSION,
    };
    use std::collections::BTreeMap;

    fn model() -> LogisticModel {
        LogisticModel {
            feature_names: vec!["a".into(), "b".into()],
            means: vec![1.0, 2.0],
            scales: vec![2.0, 4.0],
            intercept: -0.25,
            weights: vec![0.5, -1.0],
            l2: 0.1,
            positive_class_weight: 2.0,
        }
    }

    fn features() -> FeatureVector {
        FeatureVector {
            schema_version: 1,
            values: BTreeMap::from([("a".into(), 3.0), ("b".into(), 6.0)]),
        }
    }

    #[test]
    fn score_is_finite_and_contributions_reconstruct_the_logit() {
        let model = model();
        let features = features();
        let (logit, contributions) = model.score(&features).unwrap();
        assert_eq!(logit, -0.75);
        assert_eq!(
            model.intercept + contributions.iter().map(|item| item.2).sum::<f64>(),
            logit
        );
    }

    #[test]
    fn invalid_parameters_and_feature_shapes_fail_closed() {
        let mut invalid_model = model();
        invalid_model.weights[0] = f64::NAN;
        assert!(invalid_model.validate().is_err());

        let mut missing = features();
        missing.values.remove("b");
        assert!(model().score(&missing).is_err());

        let mut invalid_scorer = LogisticScorer {
            model: model(),
            calibration: CalibrationModel {
                intercept: 0.0,
                slope: 0.0,
            },
            threshold: 0.5,
        };
        assert!(invalid_scorer.validate().is_err());
        invalid_scorer.calibration.slope = 1.0;
        invalid_scorer.threshold = f64::NAN;
        assert!(invalid_scorer.validate().is_err());
    }

    #[test]
    fn scorer_emits_evidence_that_reconstructs_the_decision() {
        let scorer = LogisticScorer {
            model: model(),
            calibration: CalibrationModel {
                intercept: 0.1,
                slope: 0.8,
            },
            threshold: 0.4,
        };
        let evidence = scorer
            .score_with_evidence(
                &features(),
                17,
                1,
                DecisionKind::Membership,
                vec![11],
                DecisionTarget::Person("candidate".into()),
                vec![20, 21],
                ValidationSummary {
                    protocol_version: EVALUATION_PROTOCOL_VERSION,
                    datasets: 3,
                    pair_precision: Some(0.95),
                    pair_recall: Some(0.8),
                    suggestion_precision: Some(0.9),
                    suggestion_coverage: Some(0.7),
                },
            )
            .unwrap();

        evidence.validate().unwrap();
        assert_eq!(evidence.profile_id, 17);
        assert_eq!(evidence.decision_kind, DecisionKind::Membership);
        assert_eq!(evidence.support_face_ids, vec![20, 21]);
        assert_eq!(evidence.features.len(), 2);
    }
}
