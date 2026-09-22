use super::{
    compare_examples, fit_calibration, identity_held_out_splits, select_threshold, sigmoid,
    Calibration, CalibrationModel, DecisionEvidence, DecisionKind, DecisionOutcome, DecisionTarget,
    FeatureContribution, FeatureVector, HeldOutScore, LearningDecisionKind, ModelBundle,
    ModelError, TrainingConfig, TrainingError, TrainingSnapshot, ValidationSummary,
    WeightedExample, EVIDENCE_SCHEMA_VERSION, FEATURE_SCHEMA_VERSION, MODEL_ARTIFACT_VERSION,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AdditiveCurve {
    pub name: String,
    pub knots: Vec<f64>,
    pub effects: Vec<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AdditiveModel {
    pub intercept: f64,
    pub curves: Vec<AdditiveCurve>,
    pub l2: f64,
    pub smoothing: f64,
    pub max_abs_effect: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AdditiveScorer {
    pub model: AdditiveModel,
    pub calibration: CalibrationModel,
    pub threshold: f64,
}

impl AdditiveCurve {
    fn validate(&self, max_abs_effect: f64) -> Result<(), ModelError> {
        if self.name.trim().is_empty()
            || self.knots.is_empty()
            || self.knots.len() != self.effects.len()
            || self.knots.iter().any(|value| !value.is_finite())
            || self
                .effects
                .iter()
                .any(|value| !value.is_finite() || value.abs() > max_abs_effect + f64::EPSILON)
            || self.knots.windows(2).any(|pair| pair[0] >= pair[1])
        {
            return Err(ModelError::InvalidModel(
                "additive curve knots or effects are invalid".into(),
            ));
        }
        Ok(())
    }

    fn contribution(&self, value: f64) -> Result<f64, ModelError> {
        if !value.is_finite() {
            return Err(ModelError::InvalidFeatureVector(format!(
                "feature {} is not finite",
                self.name
            )));
        }
        if self.knots.len() == 1 || value <= self.knots[0] {
            return Ok(self.effects[0]);
        }
        let last = self.knots.len() - 1;
        if value >= self.knots[last] {
            return Ok(self.effects[last]);
        }
        let right = self.knots.partition_point(|knot| *knot < value);
        let left = right - 1;
        let width = self.knots[right] - self.knots[left];
        let fraction = (value - self.knots[left]) / width;
        Ok(self.effects[left] + fraction * (self.effects[right] - self.effects[left]))
    }
}

impl AdditiveModel {
    pub fn validate(&self) -> Result<(), ModelError> {
        if !self.intercept.is_finite()
            || !self.l2.is_finite()
            || self.l2 < 0.0
            || !self.smoothing.is_finite()
            || !(0.0..=1.0).contains(&self.smoothing)
            || !self.max_abs_effect.is_finite()
            || self.max_abs_effect <= 0.0
            || self.curves.is_empty()
            || self
                .curves
                .windows(2)
                .any(|pair| pair[0].name >= pair[1].name)
            || self
                .curves
                .iter()
                .map(|curve| curve.name.as_str())
                .collect::<BTreeSet<_>>()
                .len()
                != self.curves.len()
        {
            return Err(ModelError::InvalidModel(
                "additive model parameters are invalid".into(),
            ));
        }
        for curve in &self.curves {
            curve.validate(self.max_abs_effect)?;
        }
        Ok(())
    }

    pub fn score(
        &self,
        features: &FeatureVector,
    ) -> Result<(f64, Vec<(String, f64, f64)>), ModelError> {
        self.validate()?;
        if features.values.len() != self.curves.len()
            || self
                .curves
                .iter()
                .any(|curve| !features.values.contains_key(&curve.name))
        {
            return Err(ModelError::InvalidFeatureVector(
                "feature names do not match the additive model".into(),
            ));
        }
        let mut raw_logit = self.intercept;
        let mut contributions = Vec::with_capacity(self.curves.len());
        for curve in &self.curves {
            let value = features.values[&curve.name];
            let contribution = curve.contribution(value)?;
            raw_logit += contribution;
            contributions.push((curve.name.clone(), value, contribution));
        }
        if !raw_logit.is_finite() {
            return Err(ModelError::InvalidFeatureVector(
                "additive score is not finite".into(),
            ));
        }
        Ok((raw_logit, contributions))
    }
}

impl AdditiveScorer {
    pub fn validate(&self) -> Result<(), ModelError> {
        self.model.validate()?;
        if !self.calibration.intercept.is_finite()
            || !self.calibration.slope.is_finite()
            || self.calibration.slope <= 0.0
            || !self.threshold.is_finite()
            || !(0.0..=1.0).contains(&self.threshold)
        {
            return Err(ModelError::InvalidModel(
                "additive calibration or threshold is invalid".into(),
            ));
        }
        Ok(())
    }

    pub fn score(&self, features: &FeatureVector) -> Result<super::DecisionScore, ModelError> {
        self.validate()?;
        let (raw_logit, contributions) = self.model.score(features)?;
        Ok(super::DecisionScore {
            raw_logit,
            confidence: sigmoid(self.calibration.intercept + self.calibration.slope * raw_logit),
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

fn logit(probability: f64) -> f64 {
    (probability / (1.0 - probability)).ln()
}

fn quantile_knots(examples: &[&WeightedExample], name: &str, bins: usize) -> Vec<f64> {
    let mut values: Vec<_> = examples
        .iter()
        .map(|example| example.features.values[name])
        .collect();
    values.sort_by(f64::total_cmp);
    values.dedup_by(|left, right| left.total_cmp(right).is_eq());
    if values.len() <= bins {
        return values;
    }
    let mut knots = Vec::with_capacity(bins);
    for index in 0..bins {
        let position = index * (values.len() - 1) / (bins - 1);
        knots.push(values[position]);
    }
    knots.dedup_by(|left, right| left.total_cmp(right).is_eq());
    knots
}

fn nearest_knot(knots: &[f64], value: f64) -> usize {
    knots
        .iter()
        .enumerate()
        .min_by(|(left_index, left), (right_index, right)| {
            (value - **left)
                .abs()
                .total_cmp(&(value - **right).abs())
                .then(left_index.cmp(right_index))
        })
        .map(|(index, _)| index)
        .expect("curves always contain at least one knot")
}

fn fit_additive_model(
    examples: &[WeightedExample],
    config: &TrainingConfig,
) -> Result<AdditiveModel, TrainingError> {
    let mut examples: Vec<_> = examples.iter().collect();
    examples.sort_by(|left, right| compare_examples(left, right));
    let first = examples
        .first()
        .ok_or_else(|| TrainingError::InvalidInput("empty additive dataset".into()))?;
    if !examples.iter().any(|example| example.positive)
        || !examples.iter().any(|example| !example.positive)
    {
        return Err(TrainingError::InvalidInput(
            "additive fitting needs both classes".into(),
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
        return Err(TrainingError::InvalidInput(
            "invalid additive example".into(),
        ));
    }
    let total_weight: f64 = examples.iter().map(|example| example.weight).sum();
    let positive_weight: f64 = examples
        .iter()
        .filter(|example| example.positive)
        .map(|example| example.weight)
        .sum();
    let prevalence = ((positive_weight + 0.5) / (total_weight + 1.0)).clamp(1e-9, 1.0 - 1e-9);
    let intercept = logit(prevalence);
    let mut curves: Vec<_> = feature_names
        .iter()
        .map(|name| {
            let knots = quantile_knots(&examples, name, config.additive_bins);
            AdditiveCurve {
                name: name.clone(),
                effects: vec![0.0; knots.len()],
                knots,
            }
        })
        .collect();

    for _ in 0..config.additive_backfitting_iterations {
        for curve_index in 0..curves.len() {
            let knot_count = curves[curve_index].knots.len();
            if knot_count == 1 {
                curves[curve_index].effects[0] = 0.0;
                continue;
            }
            let mut positive = vec![0.0; knot_count];
            let mut total = vec![0.0; knot_count];
            let mut offsets = vec![0.0; knot_count];
            for example in &examples {
                let value = example.features.values[&curves[curve_index].name];
                let bin = nearest_knot(&curves[curve_index].knots, value);
                let mut offset = intercept;
                for (other_index, other) in curves.iter().enumerate() {
                    if other_index != curve_index {
                        offset += other
                            .contribution(example.features.values[&other.name])
                            .map_err(|error| TrainingError::InvalidInput(error.to_string()))?;
                    }
                }
                total[bin] += example.weight;
                offsets[bin] += example.weight * offset;
                if example.positive {
                    positive[bin] += example.weight;
                }
            }
            let mut effects = vec![0.0; knot_count];
            for index in 0..knot_count {
                if total[index] > 0.0 {
                    let probability = ((positive[index] + config.additive_l2 * prevalence)
                        / (total[index] + config.additive_l2))
                        .clamp(1e-9, 1.0 - 1e-9);
                    effects[index] = logit(probability) - offsets[index] / total[index];
                }
            }
            if config.additive_smoothing > 0.0 {
                let unsmoothed = effects.clone();
                for index in 0..knot_count {
                    let mut neighbor_sum = 0.0;
                    let mut neighbor_count = 0.0;
                    if index > 0 {
                        neighbor_sum += unsmoothed[index - 1];
                        neighbor_count += 1.0;
                    }
                    if index + 1 < knot_count {
                        neighbor_sum += unsmoothed[index + 1];
                        neighbor_count += 1.0;
                    }
                    if neighbor_count > 0.0 {
                        effects[index] = (1.0 - config.additive_smoothing) * unsmoothed[index]
                            + config.additive_smoothing * neighbor_sum / neighbor_count;
                    }
                }
            }
            for effect in &mut effects {
                *effect /= 1.0 + config.additive_l2;
            }
            let effect_weight: f64 = total.iter().sum();
            let center = if effect_weight > 0.0 {
                effects
                    .iter()
                    .zip(&total)
                    .map(|(effect, weight)| effect * weight)
                    .sum::<f64>()
                    / effect_weight
            } else {
                0.0
            };
            for effect in &mut effects {
                *effect -= center;
            }
            let largest = effects
                .iter()
                .map(|effect| effect.abs())
                .fold(0.0, f64::max);
            if largest > config.additive_max_abs_effect {
                let scale = config.additive_max_abs_effect / largest;
                for effect in &mut effects {
                    *effect *= scale;
                }
            }
            curves[curve_index].effects = effects;
        }
    }
    let model = AdditiveModel {
        intercept,
        curves,
        l2: config.additive_l2,
        smoothing: config.additive_smoothing,
        max_abs_effect: config.additive_max_abs_effect,
    };
    model
        .validate()
        .map_err(|error| TrainingError::InvalidInput(error.to_string()))?;
    Ok(model)
}

pub(crate) fn additive_held_out_scores(
    dataset: &super::DecisionDataset,
    config: &TrainingConfig,
) -> Result<Vec<HeldOutScore>, TrainingError> {
    let folds = identity_held_out_splits(dataset, config.folds, config.seed)?;
    let mut held_out = Vec::new();
    for (fold_index, fold) in folds.into_iter().enumerate() {
        let model = fit_additive_model(&fold.training, config)?;
        for example in fold.validation {
            let (logit, _) = model
                .score(&example.features)
                .map_err(|error| TrainingError::InvalidInput(error.to_string()))?;
            held_out.push(HeldOutScore {
                fold_index,
                identity_keys: example.identity_keys,
                event_id: example.event_id,
                positive: example.positive,
                weight: example.weight,
                logit,
            });
        }
    }
    held_out.sort_by(|left, right| {
        left.identity_keys
            .cmp(&right.identity_keys)
            .then(left.positive.cmp(&right.positive))
            .then(left.event_id.cmp(&right.event_id))
            .then(left.logit.total_cmp(&right.logit))
    });
    if !held_out.iter().any(|score| score.positive) || !held_out.iter().any(|score| !score.positive)
    {
        return Err(TrainingError::InvalidInput(
            "additive held-out predictions need both classes".into(),
        ));
    }
    Ok(held_out)
}

fn train_additive_scorer(
    dataset: &super::DecisionDataset,
    config: &TrainingConfig,
) -> Result<AdditiveScorer, TrainingError> {
    let held_out = additive_held_out_scores(dataset, config)?;
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
    let scorer = AdditiveScorer {
        model: fit_additive_model(&dataset.examples, config)?,
        calibration,
        threshold,
    };
    scorer
        .validate()
        .map_err(|error| TrainingError::InvalidInput(error.to_string()))?;
    Ok(scorer)
}

pub fn train_additive_bundle(
    snapshot: &TrainingSnapshot,
    config: &TrainingConfig,
) -> Result<ModelBundle, TrainingError> {
    super::validate_config(config)?;
    if snapshot.embedding_model_id.trim().is_empty()
        || snapshot.feature_schema_version != FEATURE_SCHEMA_VERSION
        || snapshot.membership.decision_kind != LearningDecisionKind::Membership
        || snapshot.cluster_quality.decision_kind != LearningDecisionKind::ClusterQuality
    {
        return Err(TrainingError::InvalidInput(
            "training snapshot model, schema, or decision kinds are incompatible".into(),
        ));
    }
    Ok(ModelBundle::Additive {
        artifact_version: MODEL_ARTIFACT_VERSION,
        embedding_model_id: snapshot.embedding_model_id.clone(),
        feature_schema_version: snapshot.feature_schema_version,
        membership: train_additive_scorer(&snapshot.membership, config)?,
        cluster_quality: train_additive_scorer(&snapshot.cluster_quality, config)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::face_learning::{
        DecisionDataset, DecisionKind, DecisionTarget, ExampleSource, FeatureVector,
        LearningDecisionKind, ModelBundle, TrainingConfig, TrainingSnapshot, ValidationSummary,
        WeightedExample, EVALUATION_PROTOCOL_VERSION, FEATURE_SCHEMA_VERSION,
    };
    use std::collections::BTreeMap;

    fn vector(x: f64, tied: f64) -> FeatureVector {
        FeatureVector {
            schema_version: FEATURE_SCHEMA_VERSION,
            values: BTreeMap::from([("tied".into(), tied), ("x".into(), x)]),
        }
    }

    fn scorer() -> AdditiveScorer {
        AdditiveScorer {
            model: AdditiveModel {
                intercept: 0.25,
                curves: vec![
                    AdditiveCurve {
                        name: "tied".into(),
                        knots: vec![0.0],
                        effects: vec![0.0],
                    },
                    AdditiveCurve {
                        name: "x".into(),
                        knots: vec![0.0, 1.0, 2.0],
                        effects: vec![-1.0, 0.0, 1.0],
                    },
                ],
                l2: 0.1,
                smoothing: 0.25,
                max_abs_effect: 2.0,
            },
            calibration: crate::face_learning::CalibrationModel {
                intercept: 0.1,
                slope: 0.8,
            },
            threshold: 0.5,
        }
    }

    fn example(identity: &str, positive: bool, x: f64) -> WeightedExample {
        WeightedExample {
            source: ExampleSource::ConfirmedLabels,
            event_id: None,
            identity_keys: vec![identity.into()],
            positive,
            weight: 1.0,
            features: vector(x, 1.0),
        }
    }

    fn nonlinear_dataset(kind: LearningDecisionKind) -> DecisionDataset {
        let mut examples = Vec::new();
        for index in 0usize..8 {
            let identity = format!("identity-{index}");
            examples.push(example(&identity, false, -0.25 + index as f64 * 0.05));
            let sign = if index.is_multiple_of(2) { -1.0 } else { 1.0 };
            examples.push(example(&identity, true, sign * (2.5 + index as f64 * 0.05)));
        }
        DecisionDataset {
            decision_kind: kind,
            examples,
        }
    }

    fn snapshot() -> TrainingSnapshot {
        TrainingSnapshot {
            generation: 7,
            embedding_model_id: "arcface/test".into(),
            feature_schema_version: FEATURE_SCHEMA_VERSION,
            membership: nonlinear_dataset(LearningDecisionKind::Membership),
            cluster_quality: nonlinear_dataset(LearningDecisionKind::ClusterQuality),
            exclusions: Vec::new(),
        }
    }

    fn config() -> TrainingConfig {
        TrainingConfig {
            folds: 4,
            min_positive_identities: 2,
            min_negative_identities: 2,
            additive_bins: 5,
            additive_backfitting_iterations: 20,
            additive_l2: 0.25,
            additive_smoothing: 0.25,
            additive_max_abs_effect: 4.0,
            ..TrainingConfig::default()
        }
    }

    #[test]
    fn interpolation_and_explanation_reconstruct_the_additive_logit() {
        let scorer = scorer();
        let score = scorer.score(&vector(1.5, 0.0)).unwrap();
        assert!((score.raw_logit - 0.75).abs() < 1e-12);
        assert!((score.contributions[1].2 - 0.5).abs() < 1e-12);

        let evidence = scorer
            .score_with_evidence(
                &vector(1.5, 0.0),
                4,
                FEATURE_SCHEMA_VERSION,
                DecisionKind::Membership,
                vec![11],
                DecisionTarget::Person("candidate".into()),
                vec![20, 21],
                ValidationSummary {
                    protocol_version: EVALUATION_PROTOCOL_VERSION,
                    datasets: 2,
                    pair_precision: Some(0.99),
                    pair_recall: Some(0.8),
                    suggestion_precision: Some(0.99),
                    suggestion_coverage: Some(0.5),
                },
            )
            .unwrap();
        evidence.validate().unwrap();
        assert_eq!(evidence.features.len(), 2);
    }

    #[test]
    fn malformed_additive_artifacts_fail_closed() {
        let mut invalid = scorer();
        invalid.model.curves[1].knots = vec![0.0, 0.0, 2.0];
        assert!(invalid.validate().is_err());

        let mut invalid = scorer();
        invalid.model.curves[1].effects.pop();
        assert!(invalid.validate().is_err());

        let mut invalid = scorer();
        invalid.model.curves[1].name = "tied".into();
        assert!(invalid.validate().is_err());

        let mut invalid = scorer();
        invalid.model.curves[1].effects[0] = 3.0;
        assert!(invalid.validate().is_err());

        let mut wrong_version = snapshot();
        wrong_version.feature_schema_version += 1;
        assert!(train_additive_bundle(&wrong_version, &config()).is_err());
    }

    #[test]
    fn quantile_training_handles_ties_and_is_byte_deterministic() {
        let expected = train_additive_bundle(&snapshot(), &config()).unwrap();
        let ModelBundle::Additive {
            membership,
            cluster_quality,
            ..
        } = &expected
        else {
            panic!("additive training returned the wrong artifact kind")
        };
        for scorer in [membership, cluster_quality] {
            scorer.validate().unwrap();
            let tied = scorer
                .model
                .curves
                .iter()
                .find(|curve| curve.name == "tied")
                .unwrap();
            assert_eq!(tied.knots, vec![1.0]);
            assert_eq!(tied.effects, vec![0.0]);
        }

        let mut permuted = snapshot();
        permuted.membership.examples.reverse();
        permuted.cluster_quality.examples.rotate_left(5);
        assert_eq!(
            serde_json::to_vec(&expected).unwrap(),
            serde_json::to_vec(&train_additive_bundle(&permuted, &config()).unwrap()).unwrap()
        );
    }
}
