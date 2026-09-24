use super::{
    FeatureVector, CLUSTER_QUALITY_FEATURE_NAMES, FEATURE_SCHEMA_VERSION, MEMBERSHIP_FEATURE_NAMES,
};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LearningAction {
    LabelCluster,
    CreatePerson,
    AssignFace,
    AssignCluster,
    RemoveFaceFromCluster,
    RemoveFaceFromPerson,
    DissolveCluster,
    QuestionYes,
    QuestionNo,
}

impl LearningAction {
    fn as_str(self) -> &'static str {
        match self {
            Self::LabelCluster => "label_cluster",
            Self::CreatePerson => "create_person",
            Self::AssignFace => "assign_face",
            Self::AssignCluster => "assign_cluster",
            Self::RemoveFaceFromCluster => "remove_face_from_cluster",
            Self::RemoveFaceFromPerson => "remove_face_from_person",
            Self::DissolveCluster => "dissolve_cluster",
            Self::QuestionYes => "question_yes",
            Self::QuestionNo => "question_no",
        }
    }

    fn parse(value: &str) -> Result<Self, LearningEventError> {
        match value {
            "label_cluster" => Ok(Self::LabelCluster),
            "create_person" => Ok(Self::CreatePerson),
            "assign_face" => Ok(Self::AssignFace),
            "assign_cluster" => Ok(Self::AssignCluster),
            "remove_face_from_cluster" => Ok(Self::RemoveFaceFromCluster),
            "remove_face_from_person" => Ok(Self::RemoveFaceFromPerson),
            "dissolve_cluster" => Ok(Self::DissolveCluster),
            "question_yes" => Ok(Self::QuestionYes),
            "question_no" => Ok(Self::QuestionNo),
            other => Err(LearningEventError::InvalidStoredValue(format!(
                "unknown learning action {other}"
            ))),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LearningDecisionKind {
    Membership,
    ClusterQuality,
}

impl LearningDecisionKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Membership => "membership",
            Self::ClusterQuality => "cluster_quality",
        }
    }

    fn parse(value: &str) -> Result<Self, LearningEventError> {
        match value {
            "membership" => Ok(Self::Membership),
            "cluster_quality" => Ok(Self::ClusterQuality),
            other => Err(LearningEventError::InvalidStoredValue(format!(
                "unknown learning decision kind {other}"
            ))),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LearningOutcome {
    Positive,
    Negative,
}

impl LearningOutcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::Positive => "positive",
            Self::Negative => "negative",
        }
    }

    fn parse(value: &str) -> Result<Self, LearningEventError> {
        match value {
            "positive" => Ok(Self::Positive),
            "negative" => Ok(Self::Negative),
            other => Err(LearningEventError::InvalidStoredValue(format!(
                "unknown learning outcome {other}"
            ))),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventFaceRole {
    Subject,
    ClusterMember,
    TargetSupport,
}

impl EventFaceRole {
    fn as_str(self) -> &'static str {
        match self {
            Self::Subject => "subject",
            Self::ClusterMember => "cluster_member",
            Self::TargetSupport => "target_support",
        }
    }

    fn parse(value: &str) -> Result<Self, LearningEventError> {
        match value {
            "subject" => Ok(Self::Subject),
            "cluster_member" => Ok(Self::ClusterMember),
            "target_support" => Ok(Self::TargetSupport),
            other => Err(LearningEventError::InvalidStoredValue(format!(
                "unknown event face role {other}"
            ))),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvalidationReason {
    PersonRemoved,
    MissingRequiredContext,
}

impl InvalidationReason {
    fn parse(value: &str) -> Result<Self, LearningEventError> {
        match value {
            "person_removed" => Ok(Self::PersonRemoved),
            "missing_required_context" => Ok(Self::MissingRequiredContext),
            other => Err(LearningEventError::InvalidStoredValue(format!(
                "unknown invalidation reason {other}"
            ))),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LearningStatus {
    Current,
    Stale,
    Training,
    Failed,
    /// The last run found too little feedback to train on. Not a failure:
    /// the generation counts as evaluated, and new feedback retries it. The
    /// gallery also retries it once when it starts, so a newer build's
    /// thresholds and wording apply without waiting for feedback.
    Waiting,
}

impl LearningStatus {
    fn parse(value: &str) -> Result<Self, LearningEventError> {
        match value {
            "current" => Ok(Self::Current),
            "stale" => Ok(Self::Stale),
            "training" => Ok(Self::Training),
            "failed" => Ok(Self::Failed),
            "waiting" => Ok(Self::Waiting),
            other => Err(LearningEventError::InvalidStoredValue(format!(
                "unknown learning status {other}"
            ))),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventFaceRef {
    pub face_id: i64,
    pub role: EventFaceRole,
    pub ordinal: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NewLearningEvent {
    pub action: LearningAction,
    pub decision_kind: LearningDecisionKind,
    pub outcome: LearningOutcome,
    pub embedding_model_id: String,
    pub active_profile_id: Option<i64>,
    pub target_identity: Option<String>,
    pub features: FeatureVector,
    pub support_count: u32,
    pub scorer_confidence: Option<f64>,
    pub faces: Vec<EventFaceRef>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StoredLearningEvent {
    pub id: i64,
    pub action: LearningAction,
    pub decision_kind: LearningDecisionKind,
    pub outcome: LearningOutcome,
    pub embedding_model_id: String,
    pub active_profile_id: Option<i64>,
    pub target_identity: Option<String>,
    pub features: FeatureVector,
    pub support_count: u32,
    pub scorer_confidence: Option<f64>,
    pub eligible: bool,
    pub invalidation_reason: Option<InvalidationReason>,
    pub created_at: String,
    pub faces: Vec<EventFaceRef>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LearningState {
    pub generation: u64,
    pub trained_generation: u64,
    pub status: LearningStatus,
    pub training_generation: Option<u64>,
    pub last_profile_id: Option<i64>,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LearningBatchReceipt {
    pub generation: u64,
    pub event_ids: Vec<i64>,
}

#[derive(Debug)]
pub enum LearningEventError {
    InvalidEvent(String),
    InvalidStoredValue(String),
    TransactionRequired,
    StateConflict(String),
    Sql(rusqlite::Error),
}

impl fmt::Display for LearningEventError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidEvent(reason) => write!(f, "invalid learning event: {reason}"),
            Self::InvalidStoredValue(reason) => {
                write!(f, "invalid stored learning value: {reason}")
            }
            Self::TransactionRequired => {
                write!(f, "learning event batches require an existing transaction")
            }
            Self::StateConflict(reason) => write!(f, "learning state conflict: {reason}"),
            Self::Sql(error) => write!(f, "learning database error: {error}"),
        }
    }
}

impl std::error::Error for LearningEventError {}

impl From<rusqlite::Error> for LearningEventError {
    fn from(value: rusqlite::Error) -> Self {
        Self::Sql(value)
    }
}

pub fn ensure_learning_tables(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS face_learning_events (
            id INTEGER PRIMARY KEY,
            action_kind TEXT NOT NULL,
            decision_kind TEXT NOT NULL,
            outcome TEXT NOT NULL,
            embedding_model_id TEXT NOT NULL,
            feature_schema_version INTEGER NOT NULL,
            active_profile_id INTEGER,
            target_identity TEXT,
            feature_snapshot_json TEXT NOT NULL,
            support_count INTEGER NOT NULL CHECK(support_count >= 0),
            scorer_confidence REAL,
            eligible INTEGER NOT NULL DEFAULT 1 CHECK(eligible IN (0, 1)),
            invalidation_reason TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            CHECK((eligible = 1 AND invalidation_reason IS NULL) OR
                  (eligible = 0 AND invalidation_reason IS NOT NULL))
        );
        CREATE TABLE IF NOT EXISTS face_learning_event_faces (
            event_id INTEGER NOT NULL REFERENCES face_learning_events(id),
            face_id INTEGER NOT NULL,
            role TEXT NOT NULL,
            ordinal INTEGER NOT NULL CHECK(ordinal >= 0),
            PRIMARY KEY(event_id, role, ordinal),
            UNIQUE(event_id, face_id)
        );
        CREATE INDEX IF NOT EXISTS face_learning_event_faces_face
            ON face_learning_event_faces(face_id);
        CREATE TABLE IF NOT EXISTS face_learning_state (
            id INTEGER PRIMARY KEY CHECK(id = 1),
            generation INTEGER NOT NULL DEFAULT 0 CHECK(generation >= 0),
            trained_generation INTEGER NOT NULL DEFAULT 0 CHECK(trained_generation >= 0),
            status TEXT NOT NULL,
            training_generation INTEGER,
            last_profile_id INTEGER,
            last_error TEXT
        );
        INSERT OR IGNORE INTO face_learning_state
            (id, generation, trained_generation, status)
            VALUES (1, 0, 0, 'current');
        CREATE TRIGGER IF NOT EXISTS face_learning_events_immutable_update
        BEFORE UPDATE ON face_learning_events
        WHEN NEW.id IS NOT OLD.id
          OR NEW.action_kind IS NOT OLD.action_kind
          OR NEW.decision_kind IS NOT OLD.decision_kind
          OR NEW.outcome IS NOT OLD.outcome
          OR NEW.embedding_model_id IS NOT OLD.embedding_model_id
          OR NEW.feature_schema_version IS NOT OLD.feature_schema_version
          OR NEW.active_profile_id IS NOT OLD.active_profile_id
          OR NEW.target_identity IS NOT OLD.target_identity
          OR NEW.feature_snapshot_json IS NOT OLD.feature_snapshot_json
          OR NEW.support_count IS NOT OLD.support_count
          OR NEW.scorer_confidence IS NOT OLD.scorer_confidence
          OR NEW.created_at IS NOT OLD.created_at
        BEGIN SELECT RAISE(ABORT, 'face learning event content is immutable'); END;
        CREATE TRIGGER IF NOT EXISTS face_learning_events_immutable_delete
        BEFORE DELETE ON face_learning_events
        BEGIN SELECT RAISE(ABORT, 'face learning events are immutable'); END;
        CREATE TRIGGER IF NOT EXISTS face_learning_event_faces_immutable_update
        BEFORE UPDATE ON face_learning_event_faces
        BEGIN SELECT RAISE(ABORT, 'face learning provenance is immutable'); END;
        CREATE TRIGGER IF NOT EXISTS face_learning_event_faces_immutable_delete
        BEFORE DELETE ON face_learning_event_faces
        BEGIN SELECT RAISE(ABORT, 'face learning provenance is immutable'); END;",
    )
}

fn validate_new_event(event: &NewLearningEvent) -> Result<(), LearningEventError> {
    if event.embedding_model_id.trim().is_empty() {
        return Err(LearningEventError::InvalidEvent(
            "embedding model id is empty".to_owned(),
        ));
    }
    event
        .features
        .validate()
        .map_err(|error| LearningEventError::InvalidEvent(error.to_string()))?;
    let feature_names: Vec<_> = event.features.values.keys().map(String::as_str).collect();
    let expected_names = match event.decision_kind {
        LearningDecisionKind::Membership => MEMBERSHIP_FEATURE_NAMES,
        LearningDecisionKind::ClusterQuality => CLUSTER_QUALITY_FEATURE_NAMES,
    };
    if feature_names != expected_names {
        return Err(LearningEventError::InvalidEvent(
            "feature schema does not match decision kind".to_owned(),
        ));
    }
    if event
        .scorer_confidence
        .is_some_and(|value| !value.is_finite() || !(0.0..=1.0).contains(&value))
    {
        return Err(LearningEventError::InvalidEvent(
            "scorer confidence is not a probability".to_owned(),
        ));
    }
    if let Some(identity) = &event.target_identity {
        if crate::person::normalize(identity).as_deref() != Some(identity.as_str()) {
            return Err(LearningEventError::InvalidEvent(
                "target identity is not normalized".to_owned(),
            ));
        }
    }
    if event.faces.is_empty() {
        return Err(LearningEventError::InvalidEvent(
            "event provenance is empty".to_owned(),
        ));
    }
    let mut face_ids = BTreeSet::new();
    let mut ordinals: BTreeMap<EventFaceRole, BTreeSet<u32>> = BTreeMap::new();
    for face in &event.faces {
        if face.face_id <= 0 {
            return Err(LearningEventError::InvalidEvent(format!(
                "face id {} is not positive",
                face.face_id
            )));
        }
        if !face_ids.insert(face.face_id) {
            return Err(LearningEventError::InvalidEvent(format!(
                "face {} appears more than once",
                face.face_id
            )));
        }
        if !ordinals.entry(face.role).or_default().insert(face.ordinal) {
            return Err(LearningEventError::InvalidEvent(format!(
                "duplicate {:?} ordinal {}",
                face.role, face.ordinal
            )));
        }
    }
    let subject_count = ordinals
        .get(&EventFaceRole::Subject)
        .map_or(0, BTreeSet::len);
    let support_count = ordinals
        .get(&EventFaceRole::TargetSupport)
        .map_or(0, BTreeSet::len);
    let cluster_count = ordinals
        .get(&EventFaceRole::ClusterMember)
        .map_or(0, BTreeSet::len);
    let provenance_matches = match event.decision_kind {
        LearningDecisionKind::Membership => {
            subject_count > 0
                && support_count > 0
                && cluster_count == 0
                && event.support_count as usize == support_count
        }
        LearningDecisionKind::ClusterQuality => {
            subject_count == 0
                && support_count == 0
                && cluster_count >= 2
                && event.support_count as usize == cluster_count
        }
    };
    if !provenance_matches {
        return Err(LearningEventError::InvalidEvent(
            "provenance does not match decision kind or support count".to_owned(),
        ));
    }
    for (role, values) in ordinals {
        if values.iter().copied().ne(0..values.len() as u32) {
            return Err(LearningEventError::InvalidEvent(format!(
                "{:?} ordinals are not contiguous",
                role
            )));
        }
    }
    Ok(())
}

pub fn append_event_batch_in_transaction(
    conn: &Connection,
    events: &[NewLearningEvent],
) -> Result<LearningBatchReceipt, LearningEventError> {
    if conn.is_autocommit() {
        return Err(LearningEventError::TransactionRequired);
    }
    if events.is_empty() {
        return Err(LearningEventError::InvalidEvent(
            "event batch is empty".to_owned(),
        ));
    }
    for event in events {
        validate_new_event(event)?;
    }

    let mut event_ids = Vec::with_capacity(events.len());
    for event in events {
        let feature_json = event
            .features
            .to_canonical_json()
            .map_err(|error| LearningEventError::InvalidEvent(error.to_string()))?;
        conn.execute(
            "INSERT INTO face_learning_events (
                action_kind, decision_kind, outcome, embedding_model_id,
                feature_schema_version, active_profile_id, target_identity,
                feature_snapshot_json, support_count, scorer_confidence,
                eligible, invalidation_reason
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 1, NULL)",
            params![
                event.action.as_str(),
                event.decision_kind.as_str(),
                event.outcome.as_str(),
                event.embedding_model_id,
                event.features.schema_version,
                event.active_profile_id,
                event.target_identity,
                feature_json,
                event.support_count,
                event.scorer_confidence,
            ],
        )?;
        let event_id = conn.last_insert_rowid();
        let mut faces = event.faces.clone();
        faces.sort_by_key(|face| (face.role, face.ordinal, face.face_id));
        for face in faces {
            conn.execute(
                "INSERT INTO face_learning_event_faces (event_id, face_id, role, ordinal)
                 VALUES (?1, ?2, ?3, ?4)",
                params![event_id, face.face_id, face.role.as_str(), face.ordinal],
            )?;
        }
        event_ids.push(event_id);
    }
    conn.execute(
        "UPDATE face_learning_state
         SET generation = generation + 1,
             status = CASE WHEN status = 'training' THEN 'training' ELSE 'stale' END,
             last_error = CASE WHEN status = 'training' THEN last_error ELSE NULL END
         WHERE id = 1",
        [],
    )?;
    let generation = nonnegative_u64(
        conn.query_row(
            "SELECT generation FROM face_learning_state WHERE id = 1",
            [],
            |row| row.get(0),
        )?,
        "generation",
    )?;
    Ok(LearningBatchReceipt {
        generation,
        event_ids,
    })
}

/// Make all evidence tied to a removed identity ineligible and advance the
/// learning generation once. The caller owns the surrounding transaction and
/// calls this only when the visible person deletion changed face state.
/// Person-removal lifecycle: invalidate the identity's eligible evidence and
/// supersede its pending questions, advancing the generation exactly once
/// when either changed. Assumes the caller's transaction.
pub fn invalidate_identity_for_removal_in_transaction(
    conn: &Connection,
    identity: &str,
) -> Result<u64, LearningEventError> {
    let normalized = crate::person::normalize(identity).ok_or_else(|| {
        LearningEventError::InvalidEvent("identity to invalidate is empty".to_owned())
    })?;
    super::ensure_question_tables(conn)?;
    invalidate_exact_identity_in_transaction(conn, &normalized)
}

/// The exact-key form of person-removal invalidation: invalidates eligible
/// evidence and supersedes pending questions for the identity exactly as
/// stored, with one generation advance when either changed. The repair
/// migration uses this so legacy rows are matched by their stored text
/// rather than a re-normalization that might not round-trip.
pub fn invalidate_exact_identity_in_transaction(
    conn: &Connection,
    identity: &str,
) -> Result<u64, LearningEventError> {
    let invalidated = conn.execute(
        "UPDATE face_learning_events
         SET eligible = 0, invalidation_reason = 'person_removed'
         WHERE target_identity = ?1 AND eligible = 1",
        [identity],
    )?;
    let superseded = if crate::db::table_exists(conn, "face_learning_questions")? {
        conn.execute(
            "UPDATE face_learning_questions
             SET status = 'superseded', decided_at = datetime('now')
             WHERE target_identity = ?1 AND status = 'pending'",
            [identity],
        )?
    } else {
        0
    };
    if invalidated > 0 || superseded > 0 {
        conn.execute(
            "UPDATE face_learning_state
             SET generation = generation + 1,
                 status = CASE WHEN status = 'training' THEN 'training' ELSE 'stale' END,
                 last_error = CASE WHEN status = 'training' THEN last_error ELSE NULL END
             WHERE id = 1",
            [],
        )?;
    }
    nonnegative_u64(
        conn.query_row(
            "SELECT generation FROM face_learning_state WHERE id = 1",
            [],
            |row| row.get(0),
        )?,
        "generation",
    )
}

pub fn invalidate_identity_in_transaction(
    conn: &Connection,
    identity: &str,
) -> Result<u64, LearningEventError> {
    if conn.is_autocommit() {
        return Err(LearningEventError::TransactionRequired);
    }
    let identity = crate::person::normalize(identity).ok_or_else(|| {
        LearningEventError::InvalidEvent("identity to invalidate is empty".to_owned())
    })?;
    let changed = conn.execute(
        "UPDATE face_learning_events
         SET eligible = 0, invalidation_reason = 'person_removed'
         WHERE target_identity = ?1 AND eligible = 1",
        [&identity],
    )?;
    if changed > 0 {
        conn.execute(
            "UPDATE face_learning_state
             SET generation = generation + 1,
                 status = CASE WHEN status = 'training' THEN 'training' ELSE 'stale' END,
                 last_error = CASE WHEN status = 'training' THEN last_error ELSE NULL END
             WHERE id = 1",
            [],
        )?;
    }
    nonnegative_u64(
        conn.query_row(
            "SELECT generation FROM face_learning_state WHERE id = 1",
            [],
            |row| row.get(0),
        )?,
        "generation",
    )
}

struct RawLearningEvent {
    id: i64,
    action: String,
    decision_kind: String,
    outcome: String,
    embedding_model_id: String,
    feature_schema_version: i64,
    active_profile_id: Option<i64>,
    target_identity: Option<String>,
    feature_json: String,
    support_count: i64,
    scorer_confidence: Option<f64>,
    eligible: i64,
    invalidation_reason: Option<String>,
    created_at: String,
}

fn raw_event(row: &Row<'_>) -> rusqlite::Result<RawLearningEvent> {
    Ok(RawLearningEvent {
        id: row.get(0)?,
        action: row.get(1)?,
        decision_kind: row.get(2)?,
        outcome: row.get(3)?,
        embedding_model_id: row.get(4)?,
        feature_schema_version: row.get(5)?,
        active_profile_id: row.get(6)?,
        target_identity: row.get(7)?,
        feature_json: row.get(8)?,
        support_count: row.get(9)?,
        scorer_confidence: row.get(10)?,
        eligible: row.get(11)?,
        invalidation_reason: row.get(12)?,
        created_at: row.get(13)?,
    })
}

const EVENT_COLUMNS: &str = "id, action_kind, decision_kind, outcome, embedding_model_id,
     feature_schema_version, active_profile_id, target_identity,
     feature_snapshot_json, support_count, scorer_confidence, eligible,
     invalidation_reason, created_at";

fn nonnegative_u64(value: i64, name: &str) -> Result<u64, LearningEventError> {
    u64::try_from(value)
        .map_err(|_| LearningEventError::InvalidStoredValue(format!("{name} is negative: {value}")))
}

fn nonnegative_u32(value: i64, name: &str) -> Result<u32, LearningEventError> {
    u32::try_from(value).map_err(|_| {
        LearningEventError::InvalidStoredValue(format!("{name} is out of range: {value}"))
    })
}

fn load_event_faces(
    conn: &Connection,
    event_id: i64,
) -> Result<Vec<EventFaceRef>, LearningEventError> {
    let mut statement = conn.prepare(
        "SELECT face_id, role, ordinal
         FROM face_learning_event_faces
         WHERE event_id = ?1
         ORDER BY CASE role
             WHEN 'subject' THEN 0
             WHEN 'cluster_member' THEN 1
             WHEN 'target_support' THEN 2
             ELSE 3 END,
             ordinal, face_id",
    )?;
    let rows = statement
        .query_map([event_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    rows.into_iter()
        .map(|(face_id, role, ordinal)| {
            Ok(EventFaceRef {
                face_id,
                role: EventFaceRole::parse(&role)?,
                ordinal: nonnegative_u32(ordinal, "face ordinal")?,
            })
        })
        .collect()
}

fn parse_stored_features(
    json: &str,
    schema_version: i64,
) -> Result<FeatureVector, LearningEventError> {
    let schema_version = nonnegative_u32(schema_version, "feature schema version")?;
    let features: FeatureVector = serde_json::from_str(json).map_err(|error| {
        LearningEventError::InvalidStoredValue(format!("malformed feature JSON: {error}"))
    })?;
    if features.schema_version != schema_version {
        return Err(LearningEventError::InvalidStoredValue(format!(
            "feature JSON schema {} does not match column {schema_version}",
            features.schema_version
        )));
    }
    if features.values.is_empty() {
        return Err(LearningEventError::InvalidStoredValue(
            "feature vector is empty".to_owned(),
        ));
    }
    if let Some((name, _)) = features.values.iter().find(|(_, value)| !value.is_finite()) {
        return Err(LearningEventError::InvalidStoredValue(format!(
            "feature {name} is not finite"
        )));
    }
    if schema_version == FEATURE_SCHEMA_VERSION {
        features.validate().map_err(|error| {
            LearningEventError::InvalidStoredValue(format!("invalid feature vector: {error}"))
        })?;
    }
    Ok(features)
}

fn parse_raw_event(
    conn: &Connection,
    raw: RawLearningEvent,
) -> Result<StoredLearningEvent, LearningEventError> {
    if raw.embedding_model_id.trim().is_empty() {
        return Err(LearningEventError::InvalidStoredValue(
            "embedding model id is empty".to_owned(),
        ));
    }
    let eligible = match raw.eligible {
        0 => false,
        1 => true,
        other => {
            return Err(LearningEventError::InvalidStoredValue(format!(
                "eligible is not boolean: {other}"
            )))
        }
    };
    let invalidation_reason = raw
        .invalidation_reason
        .as_deref()
        .map(InvalidationReason::parse)
        .transpose()?;
    if eligible != invalidation_reason.is_none() {
        return Err(LearningEventError::InvalidStoredValue(
            "eligibility and invalidation reason disagree".to_owned(),
        ));
    }
    if raw
        .scorer_confidence
        .is_some_and(|value| !value.is_finite() || !(0.0..=1.0).contains(&value))
    {
        return Err(LearningEventError::InvalidStoredValue(
            "scorer confidence is not a probability".to_owned(),
        ));
    }
    let target_identity = raw.target_identity;
    if let Some(identity) = &target_identity {
        if crate::person::normalize(identity).as_deref() != Some(identity.as_str()) {
            return Err(LearningEventError::InvalidStoredValue(
                "target identity is not normalized".to_owned(),
            ));
        }
    }
    let features = parse_stored_features(&raw.feature_json, raw.feature_schema_version)?;
    let faces = load_event_faces(conn, raw.id)?;
    Ok(StoredLearningEvent {
        id: raw.id,
        action: LearningAction::parse(&raw.action)?,
        decision_kind: LearningDecisionKind::parse(&raw.decision_kind)?,
        outcome: LearningOutcome::parse(&raw.outcome)?,
        embedding_model_id: raw.embedding_model_id,
        active_profile_id: raw.active_profile_id,
        target_identity,
        features,
        support_count: nonnegative_u32(raw.support_count, "support count")?,
        scorer_confidence: raw.scorer_confidence,
        eligible,
        invalidation_reason,
        created_at: raw.created_at,
        faces,
    })
}

pub fn list_learning_events(
    conn: &Connection,
    limit: usize,
    before_id: Option<i64>,
) -> Result<Vec<StoredLearningEvent>, LearningEventError> {
    let sql = format!(
        "SELECT {EVENT_COLUMNS} FROM face_learning_events
         WHERE (?2 IS NULL OR id < ?2)
         ORDER BY id DESC LIMIT ?1"
    );
    let mut statement = conn.prepare(&sql)?;
    let limit = i64::try_from(limit).map_err(|_| {
        LearningEventError::InvalidStoredValue("event limit is out of range".to_owned())
    })?;
    let rows = statement
        .query_map(params![limit, before_id], raw_event)?
        .collect::<Result<Vec<_>, _>>()?;
    rows.into_iter()
        .map(|raw| parse_raw_event(conn, raw))
        .collect()
}

pub fn learning_event(
    conn: &Connection,
    event_id: i64,
) -> Result<Option<StoredLearningEvent>, LearningEventError> {
    let sql = format!("SELECT {EVENT_COLUMNS} FROM face_learning_events WHERE id = ?1");
    conn.query_row(&sql, [event_id], raw_event)
        .optional()?
        .map(|raw| parse_raw_event(conn, raw))
        .transpose()
}

pub fn eligible_events_for_training(
    conn: &Connection,
    embedding_model_id: &str,
    feature_schema_version: u32,
) -> Result<Vec<StoredLearningEvent>, LearningEventError> {
    let sql = format!(
        "SELECT {EVENT_COLUMNS} FROM face_learning_events
         WHERE eligible = 1
           AND embedding_model_id = ?1
           AND feature_schema_version = ?2
         ORDER BY id"
    );
    let mut statement = conn.prepare(&sql)?;
    let rows = statement
        .query_map(
            params![embedding_model_id, feature_schema_version],
            raw_event,
        )?
        .collect::<Result<Vec<_>, _>>()?;
    rows.into_iter()
        .map(|raw| parse_raw_event(conn, raw))
        .collect()
}

fn raw_learning_state(conn: &Connection) -> Result<LearningState, LearningEventError> {
    let raw = conn.query_row(
        "SELECT generation, trained_generation, status, training_generation,
                last_profile_id, last_error
         FROM face_learning_state WHERE id = 1",
        [],
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<i64>>(3)?,
                row.get::<_, Option<i64>>(4)?,
                row.get::<_, Option<String>>(5)?,
            ))
        },
    )?;
    let generation = nonnegative_u64(raw.0, "generation")?;
    let trained_generation = nonnegative_u64(raw.1, "trained generation")?;
    if trained_generation > generation {
        return Err(LearningEventError::InvalidStoredValue(
            "trained generation exceeds current generation".to_owned(),
        ));
    }
    let status = LearningStatus::parse(&raw.2)?;
    let training_generation = raw
        .3
        .map(|value| nonnegative_u64(value, "training generation"))
        .transpose()?;
    if (status == LearningStatus::Training) != training_generation.is_some() {
        return Err(LearningEventError::InvalidStoredValue(
            "training status and generation disagree".to_owned(),
        ));
    }
    if training_generation.is_some_and(|value| value > generation) {
        return Err(LearningEventError::InvalidStoredValue(
            "training generation exceeds current generation".to_owned(),
        ));
    }
    Ok(LearningState {
        generation,
        trained_generation,
        status,
        training_generation,
        last_profile_id: raw.4,
        last_error: raw.5,
    })
}

pub fn learning_state(conn: &Connection) -> Result<LearningState, LearningEventError> {
    raw_learning_state(conn)
}

pub fn mark_training_started(conn: &Connection) -> Result<LearningState, LearningEventError> {
    let changed = conn.execute(
        "UPDATE face_learning_state
         SET status = 'training', training_generation = generation, last_error = NULL
         WHERE id = 1
           AND ((generation > trained_generation AND status IN ('stale', 'failed'))
                OR status = 'waiting')",
        [],
    )?;
    if changed != 1 {
        return Err(LearningEventError::StateConflict(
            "no stale generation is available to train".to_owned(),
        ));
    }
    raw_learning_state(conn)
}

pub fn mark_training_failed(
    conn: &Connection,
    generation: u64,
    error: &str,
) -> Result<LearningState, LearningEventError> {
    if error.trim().is_empty() {
        return Err(LearningEventError::StateConflict(
            "training failure message is empty".to_owned(),
        ));
    }
    let generation = i64::try_from(generation).map_err(|_| {
        LearningEventError::StateConflict("training generation is out of range".to_owned())
    })?;
    let changed = conn.execute(
        "UPDATE face_learning_state
         SET status = 'failed', training_generation = NULL, last_error = ?1
         WHERE id = 1 AND status = 'training' AND training_generation = ?2",
        params![error, generation],
    )?;
    if changed != 1 {
        return Err(LearningEventError::StateConflict(format!(
            "generation {generation} is not training"
        )));
    }
    raw_learning_state(conn)
}

/// Record that `generation` could not be trained because the feedback so far
/// is too thin, with `needed` saying what would change that. Unlike a failure
/// the generation is marked evaluated: only new feedback, which advances the
/// generation, or the gallery's retry on start trains it again.
pub fn mark_training_waiting(
    conn: &Connection,
    generation: u64,
    needed: &str,
) -> Result<LearningState, LearningEventError> {
    if needed.trim().is_empty() {
        return Err(LearningEventError::StateConflict(
            "the feedback needed is empty".to_owned(),
        ));
    }
    let generation = i64::try_from(generation).map_err(|_| {
        LearningEventError::StateConflict("training generation is out of range".to_owned())
    })?;
    let changed = conn.execute(
        "UPDATE face_learning_state
         SET trained_generation = ?1,
             status = CASE WHEN generation = ?1 THEN 'waiting' ELSE 'stale' END,
             training_generation = NULL,
             last_error = CASE WHEN generation = ?1 THEN ?2 ELSE NULL END
         WHERE id = 1 AND status = 'training' AND training_generation = ?1",
        params![generation, needed],
    )?;
    if changed != 1 {
        return Err(LearningEventError::StateConflict(format!(
            "generation {generation} is not training"
        )));
    }
    raw_learning_state(conn)
}

pub fn mark_generation_trained(
    conn: &Connection,
    generation: u64,
    profile_id: Option<i64>,
) -> Result<LearningState, LearningEventError> {
    let generation = i64::try_from(generation).map_err(|_| {
        LearningEventError::StateConflict("trained generation is out of range".to_owned())
    })?;
    let changed = conn.execute(
        "UPDATE face_learning_state
         SET trained_generation = ?1,
             status = CASE WHEN generation = ?1 THEN 'current' ELSE 'stale' END,
             training_generation = NULL,
             last_profile_id = ?2,
             last_error = NULL
         WHERE id = 1 AND status = 'training' AND training_generation = ?1",
        params![generation, profile_id],
    )?;
    if changed != 1 {
        return Err(LearningEventError::StateConflict(format!(
            "generation {generation} is not training"
        )));
    }
    raw_learning_state(conn)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::face_learning::{
        extract_cluster_quality_features, extract_membership_features, DecisionStage,
        FaceObservation, FEATURE_SCHEMA_VERSION,
    };
    use rusqlite::{params, Connection};

    fn observation(face_id: i64, embedding: [f32; 2]) -> FaceObservation {
        FaceObservation {
            face_id,
            embedding: embedding.to_vec(),
            bbox_min_side: Some(80.0),
            blur: Some(160.0),
            det_score: Some(0.95),
            landmark_residual: Some(2.5),
            photo_hash: format!("photo-{face_id}"),
        }
    }

    fn event(action: LearningAction) -> NewLearningEvent {
        let subject = observation(10, [1.0, 0.0]);
        let target = observation(20, [0.8, 0.6]);
        NewLearningEvent {
            action,
            decision_kind: LearningDecisionKind::Membership,
            outcome: LearningOutcome::Positive,
            embedding_model_id: "buffalo_l/w600k_r50.onnx".to_owned(),
            active_profile_id: Some(7),
            target_identity: Some("alice".to_owned()),
            features: extract_membership_features(
                &[subject],
                &[target],
                DecisionStage::GallerySingleton,
            )
            .unwrap(),
            support_count: 1,
            scorer_confidence: Some(0.82),
            faces: vec![
                EventFaceRef {
                    face_id: 20,
                    role: EventFaceRole::TargetSupport,
                    ordinal: 0,
                },
                EventFaceRef {
                    face_id: 10,
                    role: EventFaceRole::Subject,
                    ordinal: 0,
                },
            ],
        }
    }

    fn append_committed(conn: &Connection, events: &[NewLearningEvent]) -> LearningBatchReceipt {
        conn.execute_batch("BEGIN IMMEDIATE").unwrap();
        let receipt = append_event_batch_in_transaction(conn, events).unwrap();
        conn.execute_batch("COMMIT").unwrap();
        receipt
    }

    #[test]
    fn event_round_trip_preserves_every_field_and_orders_provenance() {
        let conn = Connection::open_in_memory().unwrap();
        ensure_learning_tables(&conn).unwrap();
        let mut new = event(LearningAction::AssignCluster);
        new.decision_kind = LearningDecisionKind::ClusterQuality;
        new.outcome = LearningOutcome::Negative;
        new.features = extract_cluster_quality_features(
            &[
                observation(11, [1.0, 0.0]),
                observation(12, [0.8, 0.6]),
                observation(21, [0.6, 0.8]),
                observation(22, [0.0, 1.0]),
            ],
            DecisionStage::GalleryCluster,
        )
        .unwrap();
        new.faces = vec![
            EventFaceRef {
                face_id: 22,
                role: EventFaceRole::ClusterMember,
                ordinal: 3,
            },
            EventFaceRef {
                face_id: 11,
                role: EventFaceRole::ClusterMember,
                ordinal: 0,
            },
            EventFaceRef {
                face_id: 21,
                role: EventFaceRole::ClusterMember,
                ordinal: 2,
            },
            EventFaceRef {
                face_id: 12,
                role: EventFaceRole::ClusterMember,
                ordinal: 1,
            },
        ];
        new.support_count = 4;

        let receipt = append_committed(&conn, &[new.clone()]);
        assert_eq!(receipt.generation, 1);
        assert_eq!(receipt.event_ids.len(), 1);
        let stored = learning_event(&conn, receipt.event_ids[0])
            .unwrap()
            .unwrap();
        assert_eq!(stored.id, receipt.event_ids[0]);
        assert_eq!(stored.action, new.action);
        assert_eq!(stored.decision_kind, new.decision_kind);
        assert_eq!(stored.outcome, new.outcome);
        assert_eq!(stored.embedding_model_id, new.embedding_model_id);
        assert_eq!(stored.active_profile_id, new.active_profile_id);
        assert_eq!(stored.target_identity, new.target_identity);
        assert_eq!(stored.features, new.features);
        assert_eq!(stored.support_count, new.support_count);
        assert_eq!(stored.scorer_confidence, new.scorer_confidence);
        assert!(stored.eligible);
        assert_eq!(stored.invalidation_reason, None);
        assert!(!stored.created_at.is_empty());
        assert_eq!(
            stored.faces,
            vec![
                EventFaceRef {
                    face_id: 11,
                    role: EventFaceRole::ClusterMember,
                    ordinal: 0,
                },
                EventFaceRef {
                    face_id: 12,
                    role: EventFaceRole::ClusterMember,
                    ordinal: 1,
                },
                EventFaceRef {
                    face_id: 21,
                    role: EventFaceRole::ClusterMember,
                    ordinal: 2,
                },
                EventFaceRef {
                    face_id: 22,
                    role: EventFaceRole::ClusterMember,
                    ordinal: 3,
                },
            ]
        );
        assert_eq!(list_learning_events(&conn, 10, None).unwrap(), vec![stored]);
    }

    #[test]
    fn a_two_event_action_advances_generation_once() {
        let conn = Connection::open_in_memory().unwrap();
        ensure_learning_tables(&conn).unwrap();
        let receipt = append_committed(
            &conn,
            &[
                event(LearningAction::LabelCluster),
                event(LearningAction::AssignCluster),
            ],
        );

        assert_eq!(receipt.generation, 1);
        assert_eq!(receipt.event_ids.len(), 2);
        assert_eq!(learning_state(&conn).unwrap().generation, 1);
        assert_eq!(learning_state(&conn).unwrap().status, LearningStatus::Stale);
    }

    #[test]
    fn decision_features_roles_and_support_count_must_agree() {
        let conn = Connection::open_in_memory().unwrap();
        ensure_learning_tables(&conn).unwrap();

        let mut wrong_features = event(LearningAction::AssignFace);
        wrong_features.decision_kind = LearningDecisionKind::ClusterQuality;
        let mut wrong_count = event(LearningAction::AssignFace);
        wrong_count.support_count = 2;
        let mut wrong_roles = event(LearningAction::AssignFace);
        wrong_roles.faces[0].role = EventFaceRole::ClusterMember;

        for malformed in [wrong_features, wrong_count, wrong_roles] {
            conn.execute_batch("BEGIN IMMEDIATE").unwrap();
            assert!(matches!(
                append_event_batch_in_transaction(&conn, &[malformed]),
                Err(LearningEventError::InvalidEvent(_))
            ));
            conn.execute_batch("ROLLBACK").unwrap();
        }
        assert!(list_learning_events(&conn, 10, None).unwrap().is_empty());
        assert_eq!(learning_state(&conn).unwrap().generation, 0);
    }

    #[test]
    fn event_history_uses_a_stable_id_cursor() {
        let conn = Connection::open_in_memory().unwrap();
        ensure_learning_tables(&conn).unwrap();
        for action in [
            LearningAction::CreatePerson,
            LearningAction::AssignFace,
            LearningAction::RemoveFaceFromPerson,
        ] {
            append_committed(&conn, &[event(action)]);
        }

        let first = list_learning_events(&conn, 2, None).unwrap();
        assert_eq!(
            first.iter().map(|event| event.id).collect::<Vec<_>>(),
            vec![3, 2]
        );
        append_committed(&conn, &[event(LearningAction::QuestionNo)]);
        let second = list_learning_events(&conn, 2, Some(2)).unwrap();
        assert_eq!(
            second.iter().map(|event| event.id).collect::<Vec<_>>(),
            vec![1]
        );
    }

    #[test]
    fn validation_precedes_inserts_and_child_failure_rolls_back_with_the_caller() {
        let conn = Connection::open_in_memory().unwrap();
        ensure_learning_tables(&conn).unwrap();
        conn.execute_batch("BEGIN IMMEDIATE").unwrap();
        let mut invalid = event(LearningAction::QuestionNo);
        invalid.scorer_confidence = Some(f64::NAN);
        let error = append_event_batch_in_transaction(
            &conn,
            &[event(LearningAction::QuestionYes), invalid],
        )
        .unwrap_err();
        assert!(matches!(error, LearningEventError::InvalidEvent(_)));
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM face_learning_events", [], |row| row
                .get::<_, i64>(
                0
            ))
            .unwrap(),
            0
        );
        conn.execute_batch("ROLLBACK").unwrap();

        conn.execute_batch(
            "CREATE TRIGGER reject_learning_child
             BEFORE INSERT ON face_learning_event_faces
             WHEN NEW.face_id = 999
             BEGIN SELECT RAISE(ABORT, 'test child failure'); END;
             BEGIN IMMEDIATE;",
        )
        .unwrap();
        let mut second = event(LearningAction::RemoveFaceFromPerson);
        second.faces[0].face_id = 999;
        assert!(append_event_batch_in_transaction(
            &conn,
            &[event(LearningAction::AssignFace), second]
        )
        .is_err());
        conn.execute_batch("ROLLBACK").unwrap();
        assert_eq!(list_learning_events(&conn, 10, None).unwrap(), Vec::new());
        assert_eq!(learning_state(&conn).unwrap().generation, 0);
    }

    #[test]
    fn event_content_and_provenance_are_immutable() {
        let conn = Connection::open_in_memory().unwrap();
        ensure_learning_tables(&conn).unwrap();
        let receipt = append_committed(&conn, &[event(LearningAction::AssignFace)]);
        let id = receipt.event_ids[0];

        assert!(conn
            .execute(
                "UPDATE face_learning_events SET outcome = 'negative' WHERE id = ?1",
                [id]
            )
            .is_err());
        assert!(conn
            .execute("DELETE FROM face_learning_events WHERE id = ?1", [id])
            .is_err());
        assert!(conn
            .execute(
                "UPDATE face_learning_event_faces SET face_id = 77 WHERE event_id = ?1",
                [id]
            )
            .is_err());
        assert!(conn
            .execute(
                "DELETE FROM face_learning_event_faces WHERE event_id = ?1",
                [id]
            )
            .is_err());

        conn.execute(
            "UPDATE face_learning_events
             SET eligible = 0, invalidation_reason = 'person_removed'
             WHERE id = ?1",
            [id],
        )
        .unwrap();
        let stored = learning_event(&conn, id).unwrap().unwrap();
        assert!(!stored.eligible);
        assert_eq!(
            stored.invalidation_reason,
            Some(InvalidationReason::PersonRemoved)
        );
    }

    #[test]
    fn learning_state_handles_feedback_arriving_during_training() {
        let conn = Connection::open_in_memory().unwrap();
        ensure_learning_tables(&conn).unwrap();
        assert_eq!(
            learning_state(&conn).unwrap(),
            LearningState {
                generation: 0,
                trained_generation: 0,
                status: LearningStatus::Current,
                training_generation: None,
                last_profile_id: None,
                last_error: None,
            }
        );

        append_committed(&conn, &[event(LearningAction::CreatePerson)]);
        let state = mark_training_started(&conn).unwrap();
        assert_eq!(state.status, LearningStatus::Training);
        assert_eq!(state.training_generation, Some(1));

        append_committed(&conn, &[event(LearningAction::RemoveFaceFromCluster)]);
        let state = learning_state(&conn).unwrap();
        assert_eq!(state.generation, 2);
        assert_eq!(state.status, LearningStatus::Training);
        assert_eq!(state.training_generation, Some(1));

        let state = mark_generation_trained(&conn, 1, Some(42)).unwrap();
        assert_eq!(state.trained_generation, 1);
        assert_eq!(state.status, LearningStatus::Stale);
        assert_eq!(state.last_profile_id, Some(42));

        mark_training_started(&conn).unwrap();
        let failed = mark_training_failed(&conn, 2, "training stopped").unwrap();
        assert_eq!(failed.status, LearningStatus::Failed);
        assert_eq!(failed.last_error.as_deref(), Some("training stopped"));
        assert_eq!(failed.trained_generation, 1);
        assert_eq!(failed.last_profile_id, Some(42));

        let retried = mark_training_started(&conn).unwrap();
        assert_eq!(retried.training_generation, Some(2));
        assert_eq!(retried.last_error, None);
        let current = mark_generation_trained(&conn, 2, Some(43)).unwrap();
        assert_eq!(current.status, LearningStatus::Current);
        assert_eq!(current.trained_generation, 2);
        assert_eq!(current.last_profile_id, Some(43));
    }

    #[test]
    fn waiting_marks_the_generation_evaluated_and_new_feedback_retries() {
        let conn = Connection::open_in_memory().unwrap();
        ensure_learning_tables(&conn).unwrap();
        append_committed(&conn, &[event(LearningAction::AssignFace)]);
        mark_training_started(&conn).unwrap();
        assert!(mark_training_waiting(&conn, 1, " ").is_err());
        let waiting = mark_training_waiting(&conn, 1, "dissolve 2 more wrong clusters").unwrap();
        assert_eq!(waiting.status, LearningStatus::Waiting);
        assert_eq!(waiting.trained_generation, 1);
        assert_eq!(waiting.training_generation, None);
        assert_eq!(
            waiting.last_error.as_deref(),
            Some("dissolve 2 more wrong clusters")
        );
        assert!(mark_training_waiting(&conn, 2, "again").is_err());
        // A waiting generation may be tried again (the gallery does on start,
        // so a newer build's thresholds or wording apply) without new feedback.
        let retried = mark_training_started(&conn).unwrap();
        assert_eq!(retried.training_generation, Some(1));
        assert_eq!(retried.last_error, None);
        mark_training_waiting(&conn, 1, "dissolve 2 more wrong clusters").unwrap();

        append_committed(&conn, &[event(LearningAction::DissolveCluster)]);
        let stale = learning_state(&conn).unwrap();
        assert_eq!(stale.status, LearningStatus::Stale);
        assert_eq!(stale.last_error, None);
        assert_eq!(
            mark_training_started(&conn).unwrap().training_generation,
            Some(2)
        );

        // Feedback during the run: the evaluated generation is behind, so
        // the state is stale, not waiting, and carries no stale ask.
        append_committed(&conn, &[event(LearningAction::AssignFace)]);
        let behind = mark_training_waiting(&conn, 2, "dissolve 1 more wrong cluster").unwrap();
        assert_eq!(behind.status, LearningStatus::Stale);
        assert_eq!(behind.trained_generation, 2);
        assert_eq!(behind.last_error, None);
    }

    #[test]
    fn incompatible_events_are_visible_but_excluded_from_training() {
        let conn = Connection::open_in_memory().unwrap();
        ensure_learning_tables(&conn).unwrap();
        let mut wrong_model = event(LearningAction::AssignFace);
        wrong_model.embedding_model_id = "other/model.onnx".to_owned();
        let receipt = append_committed(&conn, &[event(LearningAction::CreatePerson), wrong_model]);
        let mut old_features = event(LearningAction::DissolveCluster).features;
        old_features.schema_version = FEATURE_SCHEMA_VERSION + 1;
        insert_raw_event(
            &conn,
            "dissolve_cluster",
            "buffalo_l/w600k_r50.onnx",
            FEATURE_SCHEMA_VERSION + 1,
            &serde_json::to_string(&old_features).unwrap(),
        );

        assert_eq!(list_learning_events(&conn, 10, None).unwrap().len(), 3);
        let eligible =
            eligible_events_for_training(&conn, "buffalo_l/w600k_r50.onnx", FEATURE_SCHEMA_VERSION)
                .unwrap();
        assert_eq!(eligible.len(), 1);
        assert_eq!(eligible[0].id, receipt.event_ids[0]);
    }

    fn insert_raw_event(
        conn: &Connection,
        action: &str,
        embedding_model_id: &str,
        feature_schema_version: u32,
        feature_json: &str,
    ) {
        insert_raw_event_values(
            conn,
            action,
            "membership",
            "positive",
            embedding_model_id,
            feature_schema_version,
            feature_json,
            1,
            None,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn insert_raw_event_values(
        conn: &Connection,
        action: &str,
        decision_kind: &str,
        outcome: &str,
        embedding_model_id: &str,
        feature_schema_version: u32,
        feature_json: &str,
        eligible: i64,
        invalidation_reason: Option<&str>,
    ) {
        conn.execute(
            "INSERT INTO face_learning_events (
                action_kind, decision_kind, outcome, embedding_model_id,
                feature_schema_version, active_profile_id, target_identity,
                feature_snapshot_json, support_count, scorer_confidence,
                eligible, invalidation_reason
             ) VALUES (?1, ?2, ?3, ?4, ?5, NULL, NULL,
                       ?6, 0, NULL, ?7, ?8)",
            params![
                action,
                decision_kind,
                outcome,
                embedding_model_id,
                feature_schema_version,
                feature_json,
                eligible,
                invalidation_reason,
            ],
        )
        .unwrap();
    }

    #[test]
    fn corrupt_enum_and_feature_values_fail_closed() {
        let conn = Connection::open_in_memory().unwrap();
        ensure_learning_tables(&conn).unwrap();
        let json = event(LearningAction::AssignFace)
            .features
            .to_canonical_json()
            .unwrap();
        insert_raw_event(&conn, "unknown_action", "model", 1, &json);
        assert!(matches!(
            list_learning_events(&conn, 10, None),
            Err(LearningEventError::InvalidStoredValue(_))
        ));

        for (decision_kind, outcome) in [
            ("unknown_decision", "positive"),
            ("membership", "unknown_outcome"),
        ] {
            let conn = Connection::open_in_memory().unwrap();
            ensure_learning_tables(&conn).unwrap();
            insert_raw_event_values(
                &conn,
                "assign_face",
                decision_kind,
                outcome,
                "model",
                1,
                &json,
                1,
                None,
            );
            assert!(matches!(
                list_learning_events(&conn, 10, None),
                Err(LearningEventError::InvalidStoredValue(_))
            ));
        }

        let conn = Connection::open_in_memory().unwrap();
        ensure_learning_tables(&conn).unwrap();
        insert_raw_event_values(
            &conn,
            "assign_face",
            "membership",
            "positive",
            "model",
            1,
            &json,
            0,
            Some("unknown_reason"),
        );
        assert!(matches!(
            list_learning_events(&conn, 10, None),
            Err(LearningEventError::InvalidStoredValue(_))
        ));

        let conn = Connection::open_in_memory().unwrap();
        ensure_learning_tables(&conn).unwrap();
        conn.execute(
            "UPDATE face_learning_state SET status = 'unknown_status' WHERE id = 1",
            [],
        )
        .unwrap();
        assert!(matches!(
            learning_state(&conn),
            Err(LearningEventError::InvalidStoredValue(_))
        ));

        let conn = Connection::open_in_memory().unwrap();
        ensure_learning_tables(&conn).unwrap();
        insert_raw_event(&conn, "assign_face", "model", 1, &json);
        let id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO face_learning_event_faces (event_id, face_id, role, ordinal)
             VALUES (?1, 1, 'unknown_role', 0)",
            [id],
        )
        .unwrap();
        assert!(matches!(
            list_learning_events(&conn, 10, None),
            Err(LearningEventError::InvalidStoredValue(_))
        ));

        let conn = Connection::open_in_memory().unwrap();
        ensure_learning_tables(&conn).unwrap();
        insert_raw_event(
            &conn,
            "assign_face",
            "model",
            1,
            r#"{"schema_version":1,"values":{"bad":1e999}}"#,
        );
        assert!(matches!(
            list_learning_events(&conn, 10, None),
            Err(LearningEventError::InvalidStoredValue(_))
        ));
    }

    #[test]
    fn appending_requires_the_callers_transaction() {
        let conn = Connection::open_in_memory().unwrap();
        ensure_learning_tables(&conn).unwrap();
        assert!(matches!(
            append_event_batch_in_transaction(&conn, &[event(LearningAction::AssignFace)]),
            Err(LearningEventError::TransactionRequired)
        ));
    }
}
