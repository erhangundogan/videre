use super::{
    active_profile, extract_membership_features, DecisionEvidence, DecisionKind, DecisionStage,
    DecisionTarget, FeatureVector, ModelBundle, ValidationSummary, FEATURE_SCHEMA_VERSION,
};
use crate::face_db::load_face_observations;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// Maximum confirmed faces used as a question's target support, matching the
/// event provenance cap.
const MAX_SUPPORT_FACES: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuestionStatus {
    Pending,
    Answered,
    Skipped,
    Superseded,
}

impl QuestionStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Answered => "answered",
            Self::Skipped => "skipped",
            Self::Superseded => "superseded",
        }
    }

    fn parse(value: &str) -> Result<Self, QuestionError> {
        match value {
            "pending" => Ok(Self::Pending),
            "answered" => Ok(Self::Answered),
            "skipped" => Ok(Self::Skipped),
            "superseded" => Ok(Self::Superseded),
            other => Err(QuestionError::InvalidStoredValue(format!(
                "unknown question status {other}"
            ))),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuestionAnswer {
    Yes,
    No,
    Skip,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct QuestionPriority {
    /// Distance of the active scorer's confidence from its decision threshold;
    /// smaller is a higher-value question.
    pub uncertainty: f64,
    /// Absolute confidence gap between the two candidate model kinds.
    pub disagreement: f64,
    /// Fraction of the target's support faces the scorer would attach.
    pub coverage: f64,
    /// Quality of the subject representative (detector score).
    pub representativeness: f64,
    /// Confirmed support faces behind the target.
    pub support: usize,
    /// Recent question volume for this subject or target; smaller is better.
    pub diversity_penalty: f64,
    /// Missing subject quality metadata; smaller is better.
    pub quality_penalty: f64,
}

impl QuestionPriority {
    fn compare(&self, other: &Self) -> Ordering {
        self.uncertainty
            .total_cmp(&other.uncertainty)
            .then(other.disagreement.total_cmp(&self.disagreement))
            .then(other.coverage.total_cmp(&self.coverage))
            .then(other.representativeness.total_cmp(&self.representativeness))
            .then(other.support.cmp(&self.support))
            .then(self.diversity_penalty.total_cmp(&other.diversity_penalty))
            .then(self.quality_penalty.total_cmp(&other.quality_penalty))
    }
}

/// One proposed identity question: the subject cluster, the target person, the
/// validated evidence the question would show, and the priority that ranked it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct QuestionCandidate {
    pub subject_face_ids: Vec<i64>,
    pub representative_face_id: i64,
    /// The machine cluster the subjects formed when the question was built;
    /// answers revalidate it so moved faces are never labeled stale.
    pub cluster_id: i64,
    pub target_identity: String,
    pub target_display: String,
    pub profile_id: i64,
    pub model_kind: String,
    pub evidence: DecisionEvidence,
    pub evidence_revision: String,
    pub priority: QuestionPriority,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StoredQuestion {
    pub id: i64,
    pub status: QuestionStatus,
    pub subject_face_ids: Vec<i64>,
    pub support_face_ids: Vec<i64>,
    pub target_identity: String,
    pub target_display: String,
    pub profile_id: i64,
    pub model_kind: String,
    pub representative_face_id: i64,
    pub cluster_id: i64,
    pub evidence_revision: String,
    pub evidence: DecisionEvidence,
    pub created_at: String,
    pub decided_at: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct QuestionSelectionConfig {
    /// Per-page budget: at most this many candidates are returned.
    pub limit: usize,
    /// Targets with fewer confirmed support faces are never asked about.
    pub min_support: usize,
    /// Subjects below this detector score, or without one, are excluded.
    pub min_det_score: f64,
    /// How many unassigned clusters to consider, in stable id order.
    pub max_cluster_candidates: usize,
    /// How many people to consider as targets, in stable name order.
    pub max_person_candidates: usize,
    /// Recent question rows scanned for the diversity penalty.
    pub diversity_window: usize,
}

impl Default for QuestionSelectionConfig {
    fn default() -> Self {
        Self {
            limit: 8,
            min_support: 2,
            min_det_score: 0.5,
            max_cluster_candidates: 64,
            max_person_candidates: 64,
            diversity_window: 64,
        }
    }
}

#[derive(Debug)]
pub enum QuestionError {
    InvalidConfig(String),
    InvalidStoredValue(String),
    InvalidProfile(String),
    Db(rusqlite::Error),
    Serialization(String),
}

impl fmt::Display for QuestionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfig(reason) => write!(f, "invalid question config: {reason}"),
            Self::InvalidStoredValue(reason) => write!(f, "invalid stored question: {reason}"),
            Self::InvalidProfile(reason) => write!(f, "invalid profile for questions: {reason}"),
            Self::Db(error) => write!(f, "question database error: {error}"),
            Self::Serialization(error) => write!(f, "question serialization error: {error}"),
        }
    }
}

impl std::error::Error for QuestionError {}

impl From<rusqlite::Error> for QuestionError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Db(error)
    }
}

impl From<super::ProfileError> for QuestionError {
    fn from(error: super::ProfileError) -> Self {
        Self::InvalidProfile(error.to_string())
    }
}

/// Question delivery state: what was asked, about which faces, against which
/// profile, with the validated evidence shown to the user.
pub fn ensure_question_tables(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS face_learning_questions (
            id INTEGER PRIMARY KEY,
            status TEXT NOT NULL CHECK(status IN ('pending','answered','skipped','superseded')),
            target_identity TEXT NOT NULL,
            profile_id INTEGER NOT NULL,
            model_kind TEXT NOT NULL,
            representative_face_id INTEGER NOT NULL,
            cluster_id INTEGER NOT NULL,
            evidence_revision TEXT NOT NULL,
            evidence_json TEXT NOT NULL,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            decided_at TEXT
        );
        CREATE TABLE IF NOT EXISTS face_learning_question_faces (
            question_id INTEGER NOT NULL REFERENCES face_learning_questions(id),
            face_id INTEGER NOT NULL,
            role TEXT NOT NULL CHECK(role IN ('subject','support')),
            ordinal INTEGER NOT NULL CHECK(ordinal >= 0),
            PRIMARY KEY(question_id, role, ordinal)
        );
        CREATE INDEX IF NOT EXISTS face_learning_questions_status
            ON face_learning_questions(status);
        CREATE INDEX IF NOT EXISTS face_learning_question_faces_face
            ON face_learning_question_faces(face_id);",
    )
}

/// Stable digest of everything the question's evidence depends on. Two
/// selections that produce the same digest ask the same question; a changed
/// profile, subject set, target, features, threshold, or support set changes
/// the digest and makes the question eligible again.
pub fn question_evidence_revision(
    profile_id: i64,
    model_kind: &str,
    subject_face_ids: &[i64],
    target_identity: &str,
    features: &FeatureVector,
    threshold: f64,
    support_face_ids: &[i64],
) -> String {
    let subject: BTreeSet<i64> = subject_face_ids.iter().copied().collect();
    let canonical = format!(
        "v1|{profile_id}|{model_kind}|{:?}|{target_identity}|{}|{threshold:?}|{:?}",
        subject.iter().collect::<Vec<_>>(),
        serde_json::to_string(features).unwrap_or_default(),
        support_face_ids,
    );
    blake3::hash(canonical.as_bytes()).to_string()
}

fn decode_bundle(model_kind: &str, parameters: &[u8]) -> Result<ModelBundle, QuestionError> {
    let bundle: ModelBundle = serde_json::from_slice(parameters)
        .map_err(|error| QuestionError::InvalidProfile(error.to_string()))?;
    if bundle.model_kind() != model_kind {
        return Err(QuestionError::InvalidProfile(
            "stored model kind does not match the decoded artifact".into(),
        ));
    }
    bundle
        .validate()
        .map_err(|error| QuestionError::InvalidProfile(error.to_string()))?;
    Ok(bundle)
}

fn score_confidence(
    bundle: &ModelBundle,
    features: &FeatureVector,
) -> Result<(f64, f64), QuestionError> {
    let score = match bundle {
        ModelBundle::Logistic { membership, .. } => membership
            .score(features)
            .map_err(|error| QuestionError::InvalidProfile(error.to_string()))?,
        ModelBundle::Additive { membership, .. } => membership
            .score(features)
            .map_err(|error| QuestionError::InvalidProfile(error.to_string()))?,
    };
    Ok((score.confidence, score.threshold))
}

fn evidence_summary(profile: &super::StoredProfile) -> ValidationSummary {
    ValidationSummary {
        protocol_version: profile.validation_report.protocol_version,
        datasets: profile.validation_report.datasets.len(),
        pair_precision: None,
        pair_recall: None,
        suggestion_precision: None,
        suggestion_coverage: None,
    }
}

fn validate_config(config: &QuestionSelectionConfig) -> Result<(), QuestionError> {
    if config.limit == 0
        || config.max_cluster_candidates == 0
        || config.max_person_candidates == 0
        || !config.min_det_score.is_finite()
        || config.min_det_score < 0.0
        || config.min_det_score > 1.0
    {
        return Err(QuestionError::InvalidConfig(
            "limit, candidate caps, and detector floor must be positive and in range".into(),
        ));
    }
    Ok(())
}

struct SubjectCluster {
    cluster_id: i64,
    face_ids: Vec<i64>,
    representative: i64,
    det_score: Option<f64>,
    blur: Option<f64>,
}

fn load_subject_clusters(
    conn: &Connection,
    config: &QuestionSelectionConfig,
) -> Result<Vec<SubjectCluster>, QuestionError> {
    let cluster_ids: Vec<i64> = {
        let mut statement = conn.prepare(&format!(
            "SELECT DISTINCT cluster_id FROM faces
             WHERE cluster_id IS NOT NULL AND confirmed = 0 AND person_label IS NULL
               AND {}
             ORDER BY cluster_id LIMIT ?1",
            crate::face_db::HAS_PHOTO
        ))?;
        let rows = statement
            .query_map([config.max_cluster_candidates as i64], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<i64>>>()?;
        rows
    };
    let mut clusters = Vec::with_capacity(cluster_ids.len());
    for cluster_id in cluster_ids {
        let members: Vec<(i64, Option<f64>, Option<f64>, i64)> = {
            let mut statement = conn.prepare(&format!(
                "SELECT id, det_score, blur, is_primary FROM faces
                 WHERE cluster_id = ?1 AND confirmed = 0 AND person_label IS NULL
                   AND {}
                 ORDER BY is_primary DESC, det_score DESC, id ASC",
                crate::face_db::HAS_PHOTO
            ))?;
            let rows = statement
                .query_map([cluster_id], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        if members.is_empty() {
            continue;
        }
        let (representative, det_score, blur, _) = members[0];
        // The detector-score floor keeps unknown-quality faces from being
        // asked about; a missing blur stays eligible but is penalized.
        if !det_score.is_some_and(|score| score >= config.min_det_score) {
            continue;
        }
        clusters.push(SubjectCluster {
            cluster_id,
            face_ids: members.into_iter().map(|(id, _, _, _)| id).collect(),
            representative,
            det_score,
            blur,
        });
    }
    Ok(clusters)
}

struct TargetPerson {
    identity: String,
    display: String,
    support_ids: Vec<i64>,
}

fn load_target_people(
    conn: &Connection,
    config: &QuestionSelectionConfig,
) -> Result<Vec<TargetPerson>, QuestionError> {
    let people: Vec<(String, String)> = {
        let mut statement =
            conn.prepare("SELECT name, full_name FROM people ORDER BY name LIMIT ?1")?;
        let rows = statement
            .query_map([config.max_person_candidates as i64], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    let mut targets = Vec::with_capacity(people.len());
    for (identity, display) in people {
        let support_ids: Vec<i64> = {
            let mut statement = conn.prepare(&format!(
                "SELECT id FROM faces
                 WHERE person_label = ?1 AND confirmed = 1 AND cluster_id IS NULL
                   AND {}
                 ORDER BY is_primary DESC, id ASC LIMIT ?2",
                crate::face_db::HAS_PHOTO
            ))?;
            let rows = statement
                .query_map(params![identity, MAX_SUPPORT_FACES as i64], |row| {
                    row.get(0)
                })?
                .collect::<rusqlite::Result<Vec<i64>>>()?;
            rows
        };
        if support_ids.len() < config.min_support {
            continue;
        }
        targets.push(TargetPerson {
            identity,
            display,
            support_ids,
        });
    }
    Ok(targets)
}

fn diversity_penalties(
    conn: &Connection,
    config: &QuestionSelectionConfig,
) -> Result<(BTreeSet<String>, BTreeSet<i64>), QuestionError> {
    let mut asked_targets = BTreeSet::new();
    {
        let mut statement = conn.prepare(
            "SELECT target_identity FROM face_learning_questions
             ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = statement
            .query_map([config.diversity_window as i64], |row| {
                row.get::<_, String>(0)
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        asked_targets.extend(rows);
    }
    let mut asked_faces = BTreeSet::new();
    {
        let mut statement = conn.prepare(
            "SELECT f.face_id FROM face_learning_question_faces AS f
             JOIN face_learning_questions AS q ON q.id = f.question_id
             ORDER BY q.id DESC LIMIT ?1",
        )?;
        let rows = statement
            .query_map([config.diversity_window as i64], |row| row.get::<_, i64>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        asked_faces.extend(rows);
    }
    Ok((asked_targets, asked_faces))
}

fn known_revisions(conn: &Connection) -> Result<BTreeSet<String>, QuestionError> {
    let mut statement = conn.prepare("SELECT evidence_revision FROM face_learning_questions")?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows.into_iter().collect())
}

/// Rank bounded Yes/No/Skip questions against the active profile. Both
/// candidate model kinds share folds and inputs; the most recent stored
/// profile of the other kind (if any) powers the disagreement signal.
pub fn select_questions(
    conn: &Connection,
    config: &QuestionSelectionConfig,
) -> Result<Vec<QuestionCandidate>, QuestionError> {
    validate_config(config)?;
    let Some(active) = active_profile(conn).map_err(QuestionError::from)? else {
        return Ok(Vec::new());
    };
    if active.model_kind != "logistic" && active.model_kind != "additive" {
        return Err(QuestionError::InvalidProfile(format!(
            "unknown model kind {}",
            active.model_kind
        )));
    }
    let bundle = decode_bundle(&active.model_kind, &active.parameters)?;
    let challenger: Option<ModelBundle> = {
        let other_kind = if active.model_kind == "logistic" {
            "additive"
        } else {
            "logistic"
        };
        let row: Option<(Vec<u8>, String)> = conn
            .query_row(
                "SELECT parameters, model_kind FROM face_learning_profiles
                 WHERE model_kind = ?1 AND id != ?2 ORDER BY id DESC LIMIT 1",
                params![other_kind, active.id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        row.map(|(parameters, model_kind)| decode_bundle(&model_kind, &parameters))
            .transpose()?
    };
    let summary = evidence_summary(&active);
    let clusters = load_subject_clusters(conn, config)?;
    let people = load_target_people(conn, config)?;
    let (asked_targets, asked_faces) = diversity_penalties(conn, config)?;
    let known = known_revisions(conn)?;

    // Preload every observation once; feature extraction stays connection-free.
    let mut face_ids: BTreeSet<i64> = BTreeSet::new();
    for cluster in &clusters {
        face_ids.extend(cluster.face_ids.iter().copied());
    }
    for person in &people {
        face_ids.extend(person.support_ids.iter().copied());
    }
    let observations: BTreeMap<i64, super::FaceObservation> =
        load_face_observations(conn, &face_ids.into_iter().collect::<Vec<_>>())?
            .into_iter()
            .map(|observation| (observation.face_id, observation))
            .collect();

    let mut candidates = Vec::new();
    for cluster in &clusters {
        if !cluster
            .face_ids
            .iter()
            .all(|id| observations.contains_key(id))
        {
            return Err(QuestionError::InvalidStoredValue(format!(
                "cluster face {} is missing observation data",
                cluster.representative
            )));
        }
        let representative_observation = &observations[&cluster.representative];
        let diversity_penalty = asked_targets.len() as f64
            + cluster
                .face_ids
                .iter()
                .filter(|id| asked_faces.contains(id))
                .count() as f64;
        for person in &people {
            let support: Vec<super::FaceObservation> = person
                .support_ids
                .iter()
                .map(|id| observations[id].clone())
                .collect();
            let features = extract_membership_features(
                std::slice::from_ref(representative_observation),
                &support,
                DecisionStage::Question,
            )
            .map_err(|error| QuestionError::InvalidStoredValue(error.to_string()))?;
            let (confidence, threshold) = score_confidence(&bundle, &features)?;
            let disagreement = match &challenger {
                Some(challenger) => {
                    let (other, _) = score_confidence(challenger, &features)?;
                    (confidence - other).abs()
                }
                None => 0.0,
            };
            let mut attached = 0usize;
            for face in &support {
                let pair = extract_membership_features(
                    std::slice::from_ref(representative_observation),
                    std::slice::from_ref(face),
                    DecisionStage::Question,
                )
                .map_err(|error| QuestionError::InvalidStoredValue(error.to_string()))?;
                let (pair_confidence, _) = score_confidence(&bundle, &pair)?;
                if pair_confidence >= threshold {
                    attached += 1;
                }
            }
            let evidence_revision = question_evidence_revision(
                active.id,
                active.model_kind.as_str(),
                &cluster.face_ids,
                &person.identity,
                &features,
                threshold,
                &person.support_ids,
            );
            if known.contains(&evidence_revision) {
                continue;
            }
            let mut scorer_evidence = match &bundle {
                ModelBundle::Logistic { membership, .. } => membership.score_with_evidence(
                    &features,
                    active.id,
                    FEATURE_SCHEMA_VERSION,
                    DecisionKind::Membership,
                    vec![cluster.representative],
                    DecisionTarget::Person(person.identity.clone()),
                    person.support_ids.clone(),
                    summary.clone(),
                ),
                ModelBundle::Additive { membership, .. } => membership.score_with_evidence(
                    &features,
                    active.id,
                    FEATURE_SCHEMA_VERSION,
                    DecisionKind::Membership,
                    vec![cluster.representative],
                    DecisionTarget::Person(person.identity.clone()),
                    person.support_ids.clone(),
                    summary.clone(),
                ),
            }
            .map_err(|error| QuestionError::InvalidProfile(error.to_string()))?;
            scorer_evidence.subject_face_ids = cluster.face_ids.clone();
            scorer_evidence
                .validate()
                .map_err(|error| QuestionError::InvalidProfile(error.to_string()))?;
            candidates.push(QuestionCandidate {
                subject_face_ids: cluster.face_ids.clone(),
                representative_face_id: cluster.representative,
                cluster_id: cluster.cluster_id,
                target_identity: person.identity.clone(),
                target_display: person.display.clone(),
                profile_id: active.id,
                model_kind: active.model_kind.clone(),
                evidence: scorer_evidence,
                evidence_revision,
                priority: QuestionPriority {
                    uncertainty: (confidence - threshold).abs(),
                    disagreement,
                    coverage: attached as f64 / support.len() as f64,
                    representativeness: cluster.det_score.unwrap_or(0.0),
                    support: person.support_ids.len(),
                    diversity_penalty,
                    quality_penalty: (1.0 - cluster.det_score.unwrap_or(0.0))
                        + cluster.blur.map_or(1.0, |_| 0.0),
                },
            });
        }
    }
    candidates.sort_by(|left, right| {
        left.priority
            .compare(&right.priority)
            .then(left.subject_face_ids.cmp(&right.subject_face_ids))
            .then(left.target_identity.cmp(&right.target_identity))
    });
    candidates.truncate(config.limit);
    Ok(candidates)
}

/// Replace the pending set: every still-pending question is superseded and the
/// ranked candidates become the new pending page, in priority order.
pub fn replace_pending_questions(
    conn: &Connection,
    candidates: &[QuestionCandidate],
) -> Result<Vec<StoredQuestion>, QuestionError> {
    for candidate in candidates {
        candidate
            .evidence
            .validate()
            .map_err(|error| QuestionError::InvalidProfile(error.to_string()))?;
    }
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| {
        conn.execute(
            "UPDATE face_learning_questions SET status = 'superseded' WHERE status = 'pending'",
            [],
        )?;
        let mut stored = Vec::with_capacity(candidates.len());
        for candidate in candidates {
            let evidence_json = serde_json::to_string(&candidate.evidence)
                .map_err(|error| QuestionError::Serialization(error.to_string()))?;
            conn.execute(
                "INSERT INTO face_learning_questions (
                    status, target_identity, profile_id, model_kind,
                    representative_face_id, cluster_id, evidence_revision, evidence_json
                 ) VALUES ('pending', ?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    candidate.target_identity,
                    candidate.profile_id,
                    candidate.model_kind,
                    candidate.representative_face_id,
                    candidate.cluster_id,
                    candidate.evidence_revision,
                    evidence_json,
                ],
            )?;
            let question_id = conn.last_insert_rowid();
            store_question_faces(conn, question_id, candidate)?;
            stored.push(stored_question(conn, question_id)?.ok_or_else(|| {
                QuestionError::InvalidStoredValue("just-stored question vanished".into())
            })?);
        }
        Ok(stored)
    })();
    match result {
        Ok(stored) => match conn.execute_batch("COMMIT") {
            Ok(()) => Ok(stored),
            Err(error) => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(QuestionError::Db(error))
            }
        },
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

fn store_question_faces(
    conn: &Connection,
    question_id: i64,
    candidate: &QuestionCandidate,
) -> Result<(), QuestionError> {
    let mut statement = conn.prepare(
        "INSERT INTO face_learning_question_faces (question_id, face_id, role, ordinal)
         VALUES (?1, ?2, ?3, ?4)",
    )?;
    for (ordinal, face_id) in candidate.subject_face_ids.iter().enumerate() {
        statement.execute(params![question_id, face_id, "subject", ordinal as i64])?;
    }
    let support: BTreeSet<i64> = candidate
        .evidence
        .support_face_ids
        .iter()
        .copied()
        .collect();
    for (ordinal, face_id) in support.iter().enumerate() {
        statement.execute(params![question_id, face_id, "support", ordinal as i64])?;
    }
    Ok(())
}

fn question_face_ids(
    conn: &Connection,
    question_id: i64,
    role: &str,
) -> Result<Vec<i64>, QuestionError> {
    let mut statement = conn.prepare(
        "SELECT face_id FROM face_learning_question_faces
         WHERE question_id = ?1 AND role = ?2 ORDER BY ordinal",
    )?;
    let rows = statement
        .query_map(params![question_id, role], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<i64>>>()?;
    Ok(rows)
}

pub fn stored_question(
    conn: &Connection,
    question_id: i64,
) -> Result<Option<StoredQuestion>, QuestionError> {
    let row: Option<(
        i64,
        String,
        String,
        i64,
        String,
        i64,
        i64,
        String,
        String,
        String,
        Option<String>,
    )> = conn
        .query_row(
            "SELECT id, status, target_identity, profile_id, model_kind,
                    representative_face_id, cluster_id, evidence_revision,
                    evidence_json, created_at, decided_at
             FROM face_learning_questions WHERE id = ?1",
            [question_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                    row.get(9)?,
                    row.get(10)?,
                ))
            },
        )
        .optional()?;
    let Some((
        id,
        status,
        target_identity,
        profile_id,
        model_kind,
        representative_face_id,
        cluster_id,
        evidence_revision,
        evidence_json,
        created_at,
        decided_at,
    )) = row
    else {
        return Ok(None);
    };
    let target_display: String = conn
        .query_row(
            "SELECT full_name FROM people WHERE name = ?1",
            params![target_identity],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or_else(|| target_identity.clone());
    Ok(Some(StoredQuestion {
        id,
        status: QuestionStatus::parse(&status)?,
        subject_face_ids: question_face_ids(conn, id, "subject")?,
        support_face_ids: question_face_ids(conn, id, "support")?,
        target_identity,
        target_display,
        profile_id,
        model_kind,
        representative_face_id,
        cluster_id,
        evidence_revision,
        evidence: serde_json::from_str(&evidence_json)
            .map_err(|error| QuestionError::InvalidStoredValue(error.to_string()))?,
        created_at,
        decided_at,
    }))
}

pub fn list_pending_questions(
    conn: &Connection,
    limit: usize,
) -> Result<Vec<StoredQuestion>, QuestionError> {
    let ids: Vec<i64> = {
        let mut statement = conn.prepare(
            "SELECT id FROM face_learning_questions WHERE status = 'pending' ORDER BY id LIMIT ?1",
        )?;
        let rows = statement
            .query_map([limit as i64], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<i64>>>()?;
        rows
    };
    ids.into_iter()
        .map(|id| {
            stored_question(conn, id)?.ok_or_else(|| {
                QuestionError::InvalidStoredValue("pending question vanished".into())
            })
        })
        .collect()
}

/// The active profile facts an answer transaction revalidates against: which
/// profile asked the question and what threshold its membership scorer used.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct QuestionContext {
    pub profile_id: i64,
    pub model_kind: String,
    pub membership_threshold: f64,
}

/// Load and decode the active profile for answer-time revalidation.
pub fn active_question_context(
    conn: &Connection,
) -> Result<Option<QuestionContext>, QuestionError> {
    let Some(active) = active_profile(conn)? else {
        return Ok(None);
    };
    let bundle = decode_bundle(&active.model_kind, &active.parameters)?;
    let membership_threshold = match &bundle {
        ModelBundle::Logistic { membership, .. } => membership.threshold,
        ModelBundle::Additive { membership, .. } => membership.threshold,
    };
    Ok(Some(QuestionContext {
        profile_id: active.id,
        model_kind: active.model_kind,
        membership_threshold,
    }))
}

/// Move one question to a decided state. Assumes the caller's transaction.
pub fn finish_question_in_transaction(
    conn: &Connection,
    question_id: i64,
    status: QuestionStatus,
) -> Result<(), QuestionError> {
    let changed = conn.execute(
        "UPDATE face_learning_questions
         SET status = ?1, decided_at = datetime('now')
         WHERE id = ?2 AND status = 'pending'",
        params![status.as_str(), question_id],
    )?;
    if changed == 0 {
        return Err(QuestionError::InvalidStoredValue(format!(
            "question {question_id} is not pending"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::face_learning::{
        AdditiveCurve, AdditiveModel, AdditiveScorer, CalibrationModel, LogisticModel,
        LogisticScorer, MEMBERSHIP_FEATURE_NAMES, MODEL_ARTIFACT_VERSION,
    };
    use rusqlite::Connection;

    fn embedding_blob(x: f32, y: f32) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(4);
        bytes.extend_from_slice(&half::f16::from_f32(x).to_le_bytes());
        bytes.extend_from_slice(&half::f16::from_f32(y).to_le_bytes());
        bytes
    }

    fn library() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        // Foreign keys enforced, with labels referencing people, as the
        // library schema will require; question children then check their
        // parent question on every insert.
        conn.execute_batch(
            "PRAGMA foreign_keys = ON;
             CREATE TABLE people (name TEXT PRIMARY KEY, full_name TEXT NOT NULL);
             CREATE TABLE faces (id INTEGER PRIMARY KEY, hash TEXT NOT NULL,
             bbox TEXT NOT NULL, landmark TEXT, embedding BLOB NOT NULL,
             cluster_id INTEGER,
             person_label TEXT REFERENCES people(name) ON DELETE RESTRICT ON UPDATE RESTRICT,
             confirmed INTEGER DEFAULT 0,
             is_primary INTEGER DEFAULT 0, det_score REAL, blur REAL, oriented INTEGER);",
        )
        .unwrap();
        super::super::ensure_learning_tables(&conn).unwrap();
        super::super::ensure_profile_table(&conn).unwrap();
        ensure_question_tables(&conn).unwrap();
        // Every face's photo exists: these tests are not about missing photos,
        // and the readers list only faces some file still has.
        conn.execute_batch(
            "CREATE VIEW file_hashes AS SELECT DISTINCT hash, '/p/' || hash AS path FROM faces",
        )
        .unwrap();
        conn
    }

    fn insert_face(
        conn: &Connection,
        id: i64,
        cluster: Option<i64>,
        embedding: (f32, f32),
        det_score: Option<f64>,
    ) {
        conn.execute(
            "INSERT INTO faces (id, hash, bbox, embedding, cluster_id, confirmed, det_score, blur)
             VALUES (?1, 'h' || ?1, '0,0,80,80', ?2, ?3, 0, ?4, 600.0)",
            params![
                id,
                embedding_blob(embedding.0, embedding.1),
                cluster,
                det_score
            ],
        )
        .unwrap();
    }

    fn confirm_person(conn: &Connection, ids: &[i64], identity: &str) {
        conn.execute(
            "INSERT INTO people (name, full_name) VALUES (?1, ?2) ON CONFLICT(name) DO NOTHING",
            params![identity, identity],
        )
        .unwrap();
        for id in ids {
            conn.execute(
                "UPDATE faces SET person_label = ?1, confirmed = 1, cluster_id = NULL WHERE id = ?2",
                params![identity, id],
            )
            .unwrap();
        }
    }

    fn logistic_scorer_for(weights: Vec<(String, f64)>) -> LogisticScorer {
        let names: Vec<String> = MEMBERSHIP_FEATURE_NAMES
            .iter()
            .map(|name| name.to_string())
            .collect();
        let means: Vec<f64> = names
            .iter()
            .map(|name| if name == "similarity_mean" { 1.0 } else { 0.0 })
            .collect();
        let scales: Vec<f64> = names
            .iter()
            .map(|name| if name == "similarity_mean" { 0.5 } else { 1.0 })
            .collect();
        let weights: Vec<f64> = names
            .iter()
            .map(|name| {
                weights
                    .iter()
                    .find(|(key, _)| key == name)
                    .map_or(0.0, |(_, value)| *value)
            })
            .collect();
        LogisticScorer {
            model: LogisticModel {
                feature_names: names,
                means,
                scales,
                intercept: 0.0,
                weights,
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

    fn additive_scorer_for(effects: [f64; 3]) -> AdditiveScorer {
        // Every membership feature needs a curve; only similarity_mean has a
        // shaped response, the rest are flat single-knot zero effects.
        let curves: Vec<AdditiveCurve> = MEMBERSHIP_FEATURE_NAMES
            .iter()
            .map(|name| {
                if *name == "similarity_mean" {
                    AdditiveCurve {
                        name: name.to_string(),
                        knots: vec![0.0, 0.5, 1.0],
                        effects: effects.to_vec(),
                    }
                } else {
                    AdditiveCurve {
                        name: name.to_string(),
                        knots: vec![0.0],
                        effects: vec![0.0],
                    }
                }
            })
            .collect();
        AdditiveScorer {
            model: AdditiveModel {
                intercept: 0.0,
                curves,
                l2: 0.5,
                smoothing: 0.0,
                max_abs_effect: 4.0,
            },
            calibration: CalibrationModel {
                intercept: 0.0,
                slope: 1.0,
            },
            threshold: 0.5,
        }
    }

    fn profile_report() -> crate::face_learning::ValidationReport {
        crate::face_learning::ValidationReport {
            protocol_version: 1,
            evidence_schema_version: 1,
            feature_schema_version: FEATURE_SCHEMA_VERSION,
            datasets: Vec::new(),
        }
    }

    fn insert_profile_with_status(conn: &Connection, bundle: &ModelBundle, status: &str) -> i64 {
        let parameters = serde_json::to_vec(bundle).unwrap();
        let evidence = serde_json::to_string(&crate::face_learning::TrainingEvidenceCounts {
            positive_pairs: 20,
            negative_pairs: 20,
            explicit_negative_pairs: 0,
        })
        .unwrap();
        let report = serde_json::to_string(&profile_report()).unwrap();
        conn.execute(
            "INSERT INTO face_learning_profiles (
                artifact_version, embedding_model_id, feature_schema_version, model_kind,
                parameters, training_evidence_json, validation_report_json, stage, status
             ) VALUES (1, 'arcface/test', ?1, ?2, ?3, ?4, ?5, 'suggestion', ?6)",
            params![
                FEATURE_SCHEMA_VERSION,
                bundle.model_kind(),
                parameters,
                evidence,
                report,
                status
            ],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn insert_profile(conn: &Connection, bundle: &ModelBundle) -> i64 {
        insert_profile_with_status(conn, bundle, "active")
    }

    fn logistic_bundle_of(scorer: LogisticScorer) -> ModelBundle {
        ModelBundle::Logistic {
            artifact_version: MODEL_ARTIFACT_VERSION,
            embedding_model_id: "arcface/test".into(),
            feature_schema_version: FEATURE_SCHEMA_VERSION,
            membership: scorer.clone(),
            cluster_quality: scorer,
        }
    }

    fn additive_bundle_of(scorer: AdditiveScorer) -> ModelBundle {
        ModelBundle::Additive {
            artifact_version: MODEL_ARTIFACT_VERSION,
            embedding_model_id: "arcface/test".into(),
            feature_schema_version: FEATURE_SCHEMA_VERSION,
            membership: scorer.clone(),
            cluster_quality: scorer,
        }
    }

    fn unit(angle_degrees: f64) -> (f32, f32) {
        let radians = angle_degrees.to_radians();
        (radians.cos() as f32, radians.sin() as f32)
    }

    fn active_logistic_profile(conn: &Connection) -> i64 {
        // weight 2 on standardized similarity_mean around mean 1.0: a subject
        // identical to its support lands exactly on the threshold, an
        // orthogonal one two standard deviations below it.
        insert_profile(
            conn,
            &logistic_bundle_of(logistic_scorer_for(vec![("similarity_mean".into(), 2.0)])),
        )
    }

    #[test]
    fn question_faces_require_an_existing_question() {
        let conn = library();
        ensure_question_tables(&conn).unwrap();
        let orphan = conn.execute(
            "INSERT INTO face_learning_question_faces (question_id, face_id, role, ordinal)
             VALUES (999, 10, 'subject', 0)",
            [],
        );
        assert!(
            orphan.is_err(),
            "a question face must reference an existing question"
        );
    }

    #[test]
    fn selection_needs_a_profile_and_enough_support() {
        let conn = library();
        insert_face(&conn, 10, Some(1), unit(0.0), Some(0.9));
        insert_face(&conn, 11, Some(1), unit(0.0), Some(0.9));
        insert_face(&conn, 12, None, unit(0.0), Some(0.9));
        confirm_person(&conn, &[12], "alice");
        assert!(select_questions(&conn, &QuestionSelectionConfig::default())
            .unwrap()
            .is_empty());

        active_logistic_profile(&conn);
        let candidates = select_questions(&conn, &QuestionSelectionConfig::default()).unwrap();
        assert!(
            candidates.is_empty(),
            "support below the floor must not ask"
        );

        insert_face(&conn, 13, None, unit(0.0), Some(0.9));
        confirm_person(&conn, &[13], "alice");
        let candidates = select_questions(&conn, &QuestionSelectionConfig::default()).unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].subject_face_ids, vec![10, 11]);
        assert_eq!(candidates[0].evidence.support_face_ids, vec![12, 13]);
    }

    #[test]
    fn selection_ranks_uncertainty_and_is_deterministic() {
        let conn = library();
        insert_face(&conn, 10, Some(1), unit(0.0), Some(0.9));
        insert_face(&conn, 11, Some(1), unit(0.0), Some(0.9));
        insert_face(&conn, 12, None, unit(0.0), Some(0.9));
        insert_face(&conn, 13, None, unit(0.0), Some(0.9));
        confirm_person(&conn, &[12, 13], "alice");
        insert_face(&conn, 20, Some(2), unit(90.0), Some(0.9));
        active_logistic_profile(&conn);

        let candidates = select_questions(&conn, &QuestionSelectionConfig::default()).unwrap();
        assert_eq!(candidates.len(), 2, "two clusters against one person");
        for candidate in &candidates {
            candidate.evidence.validate().unwrap();
        }
        // The identical subject lands exactly on the threshold, the orthogonal
        // one far below it: closest-to-threshold is the more valuable question.
        assert_eq!(candidates[0].subject_face_ids, vec![10, 11]);
        assert!(candidates[0].priority.uncertainty < candidates[1].priority.uncertainty);
        assert!(candidates[1].priority.uncertainty > 0.3);

        let again = select_questions(&conn, &QuestionSelectionConfig::default()).unwrap();
        assert_eq!(candidates, again, "selection must be deterministic");
    }

    #[test]
    fn selection_excludes_low_quality_subjects() {
        let conn = library();
        insert_face(&conn, 10, Some(1), unit(0.0), Some(0.2));
        insert_face(&conn, 11, None, unit(0.0), Some(0.9));
        insert_face(&conn, 12, None, unit(0.0), Some(0.9));
        confirm_person(&conn, &[11, 12], "alice");
        // Representative below the detector floor.
        insert_face(&conn, 20, Some(2), unit(0.0), Some(0.2));
        active_logistic_profile(&conn);
        let candidates = select_questions(&conn, &QuestionSelectionConfig::default()).unwrap();
        assert!(candidates.is_empty(), "low-quality subjects must not ask");

        // A missing detector score is also excluded.
        insert_face(&conn, 21, Some(3), unit(0.0), None);
        let candidates = select_questions(&conn, &QuestionSelectionConfig::default()).unwrap();
        assert!(candidates.is_empty());
    }

    #[test]
    fn disagreement_uses_the_latest_profile_of_the_other_kind() {
        let conn = library();
        insert_face(&conn, 10, Some(1), unit(0.0), Some(0.9));
        insert_face(&conn, 11, None, unit(0.0), Some(0.9));
        insert_face(&conn, 12, None, unit(0.0), Some(0.9));
        confirm_person(&conn, &[11, 12], "alice");
        insert_face(&conn, 20, Some(2), unit(90.0), Some(0.9));
        active_logistic_profile(&conn);

        let without = select_questions(&conn, &QuestionSelectionConfig::default()).unwrap();
        assert!(without
            .iter()
            .all(|candidate| candidate.priority.disagreement == 0.0));

        // A strong additive curve on the orthogonal subject flips nothing about
        // uncertainty, but the two scorers now disagree measurably.
        insert_profile_with_status(
            &conn,
            &additive_bundle_of(additive_scorer_for([4.0, 0.0, 0.0])),
            "candidate",
        );
        let with = select_questions(&conn, &QuestionSelectionConfig::default()).unwrap();
        assert!(with
            .iter()
            .any(|candidate| candidate.priority.disagreement > 0.0));
    }

    #[test]
    fn replace_stores_pages_and_known_revisions_are_not_repeated() {
        let conn = library();
        insert_face(&conn, 10, Some(1), unit(0.0), Some(0.9));
        insert_face(&conn, 11, None, unit(0.0), Some(0.9));
        insert_face(&conn, 12, None, unit(0.0), Some(0.9));
        confirm_person(&conn, &[11, 12], "alice");
        insert_face(&conn, 20, Some(2), unit(90.0), Some(0.9));
        active_logistic_profile(&conn);

        let candidates = select_questions(&conn, &QuestionSelectionConfig::default()).unwrap();
        assert_eq!(candidates.len(), 2);
        let stored = replace_pending_questions(&conn, &candidates).unwrap();
        assert_eq!(stored.len(), 2);
        let listed = list_pending_questions(&conn, 10).unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].status, QuestionStatus::Pending);
        assert!(!listed[0].subject_face_ids.is_empty());
        assert!(!listed[0].support_face_ids.is_empty());
        assert_eq!(listed[0].target_display, "alice");

        // Unchanged inputs select nothing new: every revision is known.
        let repeat = select_questions(&conn, &QuestionSelectionConfig::default()).unwrap();
        assert!(repeat.is_empty(), "unchanged questions must not repeat");

        // New confirmed support changes the evidence and re-opens questions.
        insert_face(&conn, 13, None, unit(0.0), Some(0.9));
        confirm_person(&conn, &[13], "alice");
        let changed = select_questions(&conn, &QuestionSelectionConfig::default()).unwrap();
        assert_eq!(changed.len(), 2, "changed evidence is eligible again");

        // Replacing supersedes still-pending questions only.
        let superseded = replace_pending_questions(&conn, &changed).unwrap();
        assert_eq!(superseded.len(), 2);
        let statuses: Vec<QuestionStatus> = list_pending_questions(&conn, 10)
            .unwrap()
            .iter()
            .map(|question| question.status)
            .collect();
        assert_eq!(statuses.len(), 2);
        let all: Vec<QuestionStatus> = {
            let mut statement = conn
                .prepare("SELECT status FROM face_learning_questions ORDER BY id")
                .unwrap();
            let rows = statement
                .query_map([], |row| row.get::<_, String>(0))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            rows.iter()
                .map(|status| QuestionStatus::parse(status).unwrap())
                .collect()
        };
        assert_eq!(
            all.iter()
                .filter(|status| **status == QuestionStatus::Superseded)
                .count(),
            2,
            "the earlier page must be superseded, not deleted"
        );
    }

    #[test]
    fn revision_tracks_every_input_it_names() {
        let features = FeatureVector {
            schema_version: FEATURE_SCHEMA_VERSION,
            values: std::collections::BTreeMap::from([("x".into(), 1.0)]),
        };
        let base =
            question_evidence_revision(1, "logistic", &[1, 2], "alice", &features, 0.5, &[3]);
        assert_ne!(
            question_evidence_revision(1, "logistic", &[2], "alice", &features, 0.5, &[3]),
            base,
            "a subject face whose id matches the profile id must affect the revision"
        );
        assert_eq!(
            base,
            question_evidence_revision(1, "logistic", &[2, 1], "alice", &features, 0.5, &[3]),
            "subject order must not matter"
        );
        for (label, revision) in [
            (
                "profile",
                question_evidence_revision(2, "logistic", &[1, 2], "alice", &features, 0.5, &[3]),
            ),
            (
                "kind",
                question_evidence_revision(1, "additive", &[1, 2], "alice", &features, 0.5, &[3]),
            ),
            (
                "subject",
                question_evidence_revision(1, "logistic", &[1, 9], "alice", &features, 0.5, &[3]),
            ),
            (
                "target",
                question_evidence_revision(1, "logistic", &[1, 2], "bob", &features, 0.5, &[3]),
            ),
            (
                "features",
                question_evidence_revision(
                    1,
                    "logistic",
                    &[1, 2],
                    "alice",
                    &FeatureVector {
                        schema_version: FEATURE_SCHEMA_VERSION,
                        values: std::collections::BTreeMap::from([("x".into(), 2.0)]),
                    },
                    0.5,
                    &[3],
                ),
            ),
            (
                "threshold",
                question_evidence_revision(1, "logistic", &[1, 2], "alice", &features, 0.6, &[3]),
            ),
            (
                "support",
                question_evidence_revision(
                    1,
                    "logistic",
                    &[1, 2],
                    "alice",
                    &features,
                    0.5,
                    &[3, 4],
                ),
            ),
        ] {
            assert_ne!(base, revision, "{label} must change the revision");
        }
    }

    #[test]
    fn stored_question_round_trip_fails_closed_on_unknown_status() {
        let conn = library();
        insert_face(&conn, 10, Some(1), unit(0.0), Some(0.9));
        insert_face(&conn, 11, None, unit(0.0), Some(0.9));
        insert_face(&conn, 12, None, unit(0.0), Some(0.9));
        confirm_person(&conn, &[11, 12], "alice");
        active_logistic_profile(&conn);
        let candidates = select_questions(&conn, &QuestionSelectionConfig::default()).unwrap();
        replace_pending_questions(&conn, &candidates).unwrap();
        let stored = list_pending_questions(&conn, 1).unwrap().remove(0);
        assert_eq!(stored_question(&conn, stored.id).unwrap().unwrap(), stored);
        assert!(stored_question(&conn, 99_999).unwrap().is_none());

        assert!(
            conn.execute(
                "UPDATE face_learning_questions SET status = 'bogus' WHERE id = ?1",
                params![stored.id],
            )
            .is_err(),
            "the status CHECK constraint must reject unknown values"
        );
        assert_eq!(
            stored_question(&conn, stored.id).unwrap().unwrap().status,
            QuestionStatus::Pending
        );
    }
}
