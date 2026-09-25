//! Background face-learning worker for the gallery.
//!
//! One coordinator task owns training for the whole server: teaching
//! mutations notify it after commit, it debounces bursts, then runs at most
//! one training cycle at a time with at most one queued follow-up. The cycle
//! loads the immutable training snapshot under the connection lock, releases
//! the lock for the CPU-bound fit, then relocks to persist, promote, and
//! refresh the pending questions. A failed cycle preserves the prior active
//! profile and marks the learning state failed for a later retry. A cycle
//! that finds too little feedback to train on is not a failure: it marks the
//! state waiting, with what the People page should ask for.

use super::server::poisoned;
use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use rusqlite::Connection;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;
use videre_core::face_learning::{
    ensure_learning_tables, learning_state, mark_generation_trained, mark_training_failed,
    mark_training_started, mark_training_waiting, QuestionSelectionConfig, TrainingConfig,
    TrainingError, TrainingRun, TrainingSnapshot,
};

use super::server::{api_status, AppState};

/// How long arriving notifications are coalesced before a cycle starts.
const DEBOUNCE: Duration = Duration::from_millis(1_500);

#[derive(Debug, PartialEq)]
enum CoordinatorMessage {
    Train,
    Shutdown,
}

/// Handle for request handlers and shutdown: notify never blocks and never
/// fails the caller; a lost notification only delays training until the next
/// mutation.
#[derive(Clone)]
pub struct LearningCoordinator {
    tx: mpsc::UnboundedSender<CoordinatorMessage>,
}

impl LearningCoordinator {
    pub fn notify(&self) {
        let _ = self.tx.send(CoordinatorMessage::Train);
    }

    pub fn shutdown(&self) {
        let _ = self.tx.send(CoordinatorMessage::Shutdown);
    }
}

/// Everything the coordinator loop needs. The trainer closure is injectable
/// so the mechanics (debounce, single-flight, recovery, lock release) are
/// testable without model work.
pub struct LearningDeps {
    pub conn: Arc<Mutex<Connection>>,
    pub embedding_model_id: String,
    pub config: TrainingConfig,
    pub gates: videre_core::face_learning::PromotionGates,
    pub questions: QuestionSelectionConfig,
    #[allow(clippy::type_complexity)]
    pub train: Arc<
        dyn Fn(
                &TrainingSnapshot,
                &TrainingConfig,
                &videre_core::face_learning::PromotionGates,
            ) -> Result<TrainingRun, TrainingError>
            + Send
            + Sync,
    >,
}

/// Start the coordinator. Also performs startup recovery: a state left in
/// training by a previous process, or a stale generation, trains right away.
pub fn spawn(deps: LearningDeps) -> LearningCoordinator {
    spawn_with(deps, DEBOUNCE)
}

pub fn spawn_with(deps: LearningDeps, debounce: Duration) -> LearningCoordinator {
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(run(deps, debounce, rx));
    LearningCoordinator { tx }
}

async fn run(
    deps: LearningDeps,
    debounce: Duration,
    mut rx: mpsc::UnboundedReceiver<CoordinatorMessage>,
) {
    // Startup recovery of stale work.
    if needs_training(&deps) {
        run_cycle(&deps).await;
    }
    while let Some(message) = rx.recv().await {
        if message == CoordinatorMessage::Shutdown {
            break;
        }
        tokio::time::sleep(debounce).await;
        while rx.try_recv().is_ok() {
            // Coalesced into the cycle below.
        }
        run_cycle(&deps).await;
    }
}

fn needs_training(deps: &LearningDeps) -> bool {
    let conn = deps.conn.lock().expect("learning connection poisoned");
    match learning_state(&conn) {
        Ok(state) => {
            state.status == videre_core::face_learning::LearningStatus::Training
                || state.generation > state.trained_generation
                || retry_on_new_build(&conn, &state)
        }
        Err(_) => false,
    }
}

/// Whether a waiting or failed library with no new feedback should be tried
/// again: only when this build has not completed an attempt yet. The same
/// build on the same evidence gives the same answer, so neither a restart nor
/// an action that records nothing rebuilds the snapshot; a newer build may
/// train it or word the ask differently. An attempt that never completed (the
/// gallery quit mid-run) records no build, so it is tried again.
fn retry_on_new_build(
    conn: &Connection,
    state: &videre_core::face_learning::LearningState,
) -> bool {
    use videre_core::face_learning::{LearningStatus, TRAINING_BUILD, TRAINING_BUILD_KEY};
    matches!(
        state.status,
        LearningStatus::Waiting | LearningStatus::Failed
    ) && videre_core::library_state::peek_string(conn, TRAINING_BUILD_KEY)
        .ok()
        .flatten()
        .as_deref()
        != Some(TRAINING_BUILD)
}

/// Record that this build completed a training attempt.
fn note_build(conn: &Connection) {
    use videre_core::face_learning::{TRAINING_BUILD, TRAINING_BUILD_KEY};
    if let Err(error) =
        videre_core::library_state::set_string(conn, TRAINING_BUILD_KEY, TRAINING_BUILD)
    {
        tracing::debug!("face learning: could not record the training build: {error}");
    }
}

struct PreparedCycle {
    generation: u64,
    snapshot: TrainingSnapshot,
}

/// Phase 1, under the lock: flip the state to training and load the immutable
/// snapshot for generation G. A legitimately empty library (no events beyond
/// generation 0) is skipped without touching state; real failures mark the
/// state failed while the lock is still held.
fn prepare_training(conn: &Connection, deps: &LearningDeps) -> Result<Option<PreparedCycle>, ()> {
    ensure_learning_tables(conn).map_err(|_| ())?;
    let state = learning_state(conn).map_err(|_| ())?;
    if state.status == videre_core::face_learning::LearningStatus::Training {
        // A previous process died mid-run; retire the stale run first.
        let interrupted = state.training_generation.unwrap_or(state.generation);
        mark_training_failed(conn, interrupted as u64, "training was interrupted")
            .map_err(|_| ())?;
    }
    let state = learning_state(conn).map_err(|_| ())?;
    if state.generation <= state.trained_generation && !retry_on_new_build(conn, &state) {
        return Ok(None);
    }
    let state = mark_training_started(conn).map_err(|error| {
        let _ = mark_training_failed(conn, state.generation, &error.to_string());
    })?;
    let generation = state.generation;
    let snapshot = load_snapshot(conn, deps, generation).map_err(|error| {
        record_failure(conn, generation, &error);
        note_build(conn);
    })?;
    Ok(Some(PreparedCycle {
        generation,
        snapshot,
    }))
}

fn load_snapshot(
    conn: &Connection,
    deps: &LearningDeps,
    generation: u64,
) -> Result<TrainingSnapshot, String> {
    videre_api::load_training_snapshot(conn, &deps.embedding_model_id, generation, &deps.config)
}

/// Persist a trained candidate: insert, promote through the shipped gates,
/// advance the state, then refresh the pending questions. The profile row
/// keeps the promotion verdict either way.
fn persist_training(
    conn: &Connection,
    deps: &LearningDeps,
    generation: u64,
    run: &TrainingRun,
) -> Result<i64, String> {
    let summary =
        videre_api::persist_trained_profile(conn, &deps.embedding_model_id, run, &deps.gates)
            .map_err(|e| e.to_string())?;
    // The pending question page was built against the still-active profile,
    // so it is refreshed only when this run actually promoted. Rebuilding it
    // after a rejection would supersede those questions with identical ones
    // and leave the page empty.
    if summary.promoted {
        videre_api::refresh_identity_questions(conn, &deps.questions)
            .map_err(|e| format!("question refresh failed: {e}"))?;
    }
    // Marked trained only after the refresh succeeded, so a refresh failure
    // leaves the state failed and the next notification retrains the
    // generation instead of stranding stale questions behind a current
    // status. A retry inserts a fresh candidate row; promotion history stays
    // per row.
    mark_generation_trained(conn, generation, Some(summary.profile_id))
        .map_err(|e| e.to_string())?;
    Ok(summary.profile_id)
}

/// One training cycle. The connection lock is held only in phases 1 and 3;
/// the fit itself runs on the snapshot without it.
async fn run_cycle(deps: &LearningDeps) {
    let prepared = {
        let conn = deps.conn.lock().expect("learning connection poisoned");
        match prepare_training(&conn, deps) {
            Ok(prepared) => prepared,
            Err(()) => return,
        }
    };
    let Some(prepared) = prepared else {
        return;
    };
    let generation = prepared.generation;
    let snapshot = prepared.snapshot;
    let train = deps.train.clone();
    let config = deps.config.clone();
    let gates = deps.gates.clone();
    let trained = tokio::task::spawn_blocking(move || train(&snapshot, &config, &gates)).await;
    let conn = deps.conn.lock().expect("learning connection poisoned");
    match trained {
        Ok(Ok(run)) => match persist_training(&conn, deps, generation, &run) {
            Ok(profile_id) => {
                tracing::debug!(generation, profile_id, "face learning: trained");
            }
            Err(error) => record_failure(&conn, generation, &error),
        },
        Ok(Err(error)) => match error.feedback_needed(&deps.config) {
            // Too little feedback yet is the normal state of a young library,
            // not a failure: the page asks for what is missing.
            Some(needed) => {
                tracing::debug!(generation, %error, "face learning: waiting for feedback: {needed}");
                if let Err(write) = mark_training_waiting(&conn, generation, &needed) {
                    // The state stays training until the next start retires
                    // it; say why here, where the cause is known.
                    tracing::warn!(
                        "face learning: could not record waiting for generation {generation}: {write}"
                    );
                }
            }
            None => record_failure(&conn, generation, &error.to_string()),
        },
        Err(join_error) => record_failure(&conn, generation, &join_error.to_string()),
    }
    note_build(&conn);
}

/// A training run that really failed: kept in the state for the page, and
/// logged, since the previous profile silently stays in use.
fn record_failure(conn: &Connection, generation: u64, error: &str) {
    tracing::warn!("face learning: training generation {generation} failed: {error}");
    let _ = mark_training_failed(conn, generation, error);
}

/// GET /api/face-learning/status
pub(crate) async fn handle_learning_status(
    State(state): State<Arc<AppState>>,
) -> Result<Json<videre_api::FaceLearningStatus>, StatusCode> {
    let conn = state.conn.lock().map_err(poisoned)?;
    videre_api::face_learning_status(&conn)
        .map(Json)
        .map_err(api_status)
}

/// GET /api/face-learning/questions?limit=N
pub(crate) async fn handle_learning_questions(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Result<Json<Vec<videre_core::face_learning::StoredQuestion>>, StatusCode> {
    let limit = params
        .get("limit")
        .and_then(|limit| limit.parse::<usize>().ok())
        .unwrap_or(8)
        .clamp(1, 50);
    let conn = state.conn.lock().map_err(poisoned)?;
    videre_api::pending_identity_questions(&conn, limit)
        .map(Json)
        .map_err(api_status)
}

#[derive(serde::Deserialize)]
pub struct QuestionAnswerBody {
    pub answer: String,
}

/// POST /api/face-learning/questions/{id}/answer with `{"answer":"yes"}`.
/// The worker is notified only after the answer transaction has committed.
pub(crate) async fn handle_learning_answer(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<i64>,
    Json(body): Json<QuestionAnswerBody>,
) -> Result<Json<videre_api::QuestionAnswerOutcome>, StatusCode> {
    let answer = match body.answer.as_str() {
        "yes" | "Yes" => videre_core::face_learning::QuestionAnswer::Yes,
        "no" | "No" => videre_core::face_learning::QuestionAnswer::No,
        "skip" | "Skip" => videre_core::face_learning::QuestionAnswer::Skip,
        _ => return Err(StatusCode::BAD_REQUEST),
    };
    let outcome = {
        let conn = state.conn.lock().map_err(poisoned)?;
        let context = super::server::teaching_context(&conn, &state);
        videre_api::answer_question_with_learning(&conn, id, answer, &context)
            .map_err(api_status)?
    };
    if let Some(learning) = &state.learning {
        learning.notify();
    }
    Ok(Json(outcome))
}

/// GET /api/face-learning/events?limit=N&before=ID
pub(crate) async fn handle_learning_events(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Result<Json<Vec<videre_api::FaceLearningEventProof>>, StatusCode> {
    let limit = params
        .get("limit")
        .and_then(|limit| limit.parse::<usize>().ok())
        .unwrap_or(50)
        .clamp(1, 200);
    let before = params
        .get("before")
        .and_then(|before| before.parse::<i64>().ok());
    let conn = state.conn.lock().map_err(poisoned)?;
    videre_api::face_learning_events(&conn, limit, before, Some(&state.model_id))
        .map(Json)
        .map_err(api_status)
}

/// GET /api/face-learning/events/{id}
pub(crate) async fn handle_learning_event_detail(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<i64>,
) -> Result<Json<videre_api::FaceLearningEventProof>, StatusCode> {
    let conn = state.conn.lock().map_err(poisoned)?;
    match videre_api::face_learning_event(&conn, id, Some(&state.model_id)) {
        Ok(Some(event)) => Ok(Json(event)),
        Ok(None) => Err(StatusCode::NOT_FOUND),
        Err(error) => Err(api_status(error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

    use videre_core::face_learning::{
        CalibrationModel, LogisticModel, LogisticScorer, MEMBERSHIP_FEATURE_NAMES,
    };

    fn embedding_blob() -> Vec<u8> {
        // f16 LE of (1.0, 0.0), the same shape the feature fixture uses.
        vec![0x00, 0x3C, 0x00, 0x00]
    }

    /// Faces 10 and 11 sit in cluster 1; faces 12 and 13 confirm alice, so a
    /// snapshot can always be built and the injected trainer decides whether
    /// training succeeds.
    fn library() -> Arc<Mutex<Connection>> {
        let conn = Connection::open_in_memory().unwrap();
        // Foreign keys on, and labels referencing people, as the library
        // schema will enforce.
        conn.execute_batch(
            "PRAGMA foreign_keys = ON;
             CREATE TABLE people (name TEXT PRIMARY KEY, full_name TEXT NOT NULL);
             CREATE TABLE faces (id INTEGER PRIMARY KEY, hash TEXT NOT NULL,
             bbox TEXT NOT NULL, landmark TEXT, embedding BLOB NOT NULL,
             cluster_id INTEGER,
             person_label TEXT REFERENCES people(name) ON DELETE RESTRICT ON UPDATE RESTRICT,
             confirmed INTEGER DEFAULT 0,
             is_primary INTEGER DEFAULT 0, det_score REAL, blur REAL, oriented INTEGER);
             INSERT INTO people VALUES ('alice','Alice');",
        )
        .unwrap();
        ensure_learning_tables(&conn).unwrap();
        videre_core::face_learning::ensure_profile_table(&conn).unwrap();
        videre_core::face_learning::ensure_question_tables(&conn).unwrap();
        for (id, cluster, label, confirmed) in [
            (10, Some(1), None, 0),
            (11, Some(1), None, 0),
            (12, None, Some("alice"), 1),
            (13, None, Some("alice"), 1),
        ] {
            conn.execute(
                "INSERT INTO faces (id, hash, bbox, embedding, cluster_id, person_label, confirmed, det_score, blur)
                 VALUES (?1, 'h' || ?1, '0,0,80,80', ?2, ?3, ?4, ?5, 0.9, 600.0)",
                rusqlite::params![id, embedding_blob(), cluster, label, confirmed],
            )
            .unwrap();
        }
        Arc::new(Mutex::new(conn))
    }

    fn make_deps(
        conn: Arc<Mutex<Connection>>,
        train: impl Fn(
                &TrainingSnapshot,
                &TrainingConfig,
                &videre_core::face_learning::PromotionGates,
            ) -> Result<TrainingRun, TrainingError>
            + Send
            + Sync
            + 'static,
    ) -> LearningDeps {
        LearningDeps {
            conn,
            embedding_model_id: "arcface/test".into(),
            config: TrainingConfig::default(),
            gates: videre_core::face_learning::PromotionGates::shipped(),
            questions: QuestionSelectionConfig::default(),
            train: Arc::new(train),
        }
    }

    /// Force a stale generation so the next cycle has something to train.
    /// This mirrors what an event append does: generation advances and the
    /// state becomes stale.
    fn make_generation_pending(conn: &Connection) {
        conn.execute(
            "UPDATE face_learning_state SET generation = 1, status = 'stale'",
            [],
        )
        .unwrap();
    }

    fn stub_report() -> videre_core::face_learning::ValidationReport {
        use videre_core::face_learning::{
            ClusteringMetrics, DatasetValidation as Report, SuggestionMetrics, ValidationReport,
        };
        let dataset = |key: &str| Report {
            dataset_key: key.into(),
            clustering: ClusteringMetrics {
                labeled_faces: 100,
                labeled_identities: 10,
                predicted_clusters: 10,
                true_positive_pairs: 720,
                false_positive_pairs: 0,
                false_negative_pairs: 280,
                pair_precision: Some(1.0),
                pair_recall: Some(0.72),
                pair_f1: Some(2.0 * 0.72 / 1.72),
                mixed_clusters: 1,
                fragmented_identities: 3,
                unassigned_labeled_faces: 10,
                unassigned_rate: 0.10,
            },
            suggestions: Some(SuggestionMetrics {
                correct: 100,
                incorrect: 0,
                not_suggested: 0,
                precision: Some(1.0),
                coverage: 1.0,
            }),
            hard_rule_violations: 0,
            invalid_explanations: 0,
            wall_time_ms: 100.0,
            peak_memory_mib: 50.0,
        };
        ValidationReport {
            protocol_version: 1,
            evidence_schema_version: 1,
            feature_schema_version: 1,
            datasets: vec![dataset("library-a"), dataset("library-b")],
        }
    }

    /// A scorer over the real membership schema, so question selection can
    /// score real feature vectors against the promoted profile.
    fn membership_scorer() -> LogisticScorer {
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
            .map(|name| if name == "similarity_mean" { 2.0 } else { 0.0 })
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

    fn stub_run() -> TrainingRun {
        use videre_core::face_learning::{
            CandidateComparison, CandidateKind, TrainingEvidenceCounts, MODEL_ARTIFACT_VERSION,
        };
        let scorer = membership_scorer();
        TrainingRun {
            selected: videre_core::face_learning::ModelBundle::Logistic {
                artifact_version: MODEL_ARTIFACT_VERSION,
                embedding_model_id: "arcface/test".into(),
                feature_schema_version: 1,
                membership: scorer.clone(),
                cluster_quality: scorer,
            },
            logistic_validation: stub_report(),
            additive_validation: stub_report(),
            comparison: CandidateComparison {
                selected: CandidateKind::Logistic,
                logistic_passes: true,
                additive_passes: false,
                additive_gain: 0.0,
                folds_agree: true,
                additive_regressions: Vec::new(),
            },
            evidence_counts: TrainingEvidenceCounts {
                positive_pairs: 20,
                negative_pairs: 20,
                explicit_negative_pairs: 0,
            },
        }
    }

    fn rejected_run() -> TrainingRun {
        let mut run = stub_run();
        run.logistic_validation.datasets.truncate(1);
        run.additive_validation.datasets.truncate(1);
        run.comparison.logistic_passes = false;
        run
    }

    /// Promote a profile before the coordinator runs and capture its pending
    /// question page, so tests can assert what later cycles do to it.
    fn seed_active_profile_and_questions(conn: &Connection) -> i64 {
        use videre_core::face_learning::{
            replace_pending_questions, select_questions, ModelBundle, MODEL_ARTIFACT_VERSION,
        };
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
        let weights: Vec<f64> = names.iter().map(|_| 0.0).collect();
        let scorer = LogisticScorer {
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
        };
        let parameters = serde_json::to_vec(&ModelBundle::Logistic {
            artifact_version: MODEL_ARTIFACT_VERSION,
            embedding_model_id: "arcface/test".into(),
            feature_schema_version: 1,
            membership: scorer.clone(),
            cluster_quality: scorer,
        })
        .unwrap();
        let evidence = serde_json::to_string(&videre_core::face_learning::TrainingEvidenceCounts {
            positive_pairs: 20,
            negative_pairs: 20,
            explicit_negative_pairs: 0,
        })
        .unwrap();
        let report = serde_json::to_string(&stub_report()).unwrap();
        conn.execute(
            "INSERT INTO face_learning_profiles (
                artifact_version, embedding_model_id, feature_schema_version, model_kind,
                parameters, training_evidence_json, validation_report_json, stage, status
             ) VALUES (1, 'arcface/test', 1, 'logistic', ?1, ?2, ?3, 'suggestion', 'active')",
            rusqlite::params![parameters, evidence, report],
        )
        .unwrap();
        let profile_id = conn.last_insert_rowid();
        let candidates = select_questions(conn, &QuestionSelectionConfig::default()).unwrap();
        assert_eq!(candidates.len(), 1, "fixture must produce one question");
        let stored = replace_pending_questions(conn, &candidates).unwrap();
        assert_eq!(stored.len(), 1);
        profile_id
    }

    fn learning_status(conn: &Connection) -> (u64, u64, String) {
        let state = learning_state(conn).unwrap();
        (
            state.generation,
            state.trained_generation,
            format!("{:?}", state.status).to_lowercase(),
        )
    }

    #[tokio::test]
    async fn a_notification_trains_one_cycle_and_marks_the_state_current() {
        let conn = library();
        make_generation_pending(&conn.lock().unwrap());
        let trained = Arc::new(AtomicUsize::new(0));
        let counter = trained.clone();
        let coordinator = spawn_with(
            make_deps(conn.clone(), move |_, _, _| {
                counter.fetch_add(1, AtomicOrdering::SeqCst);
                Ok(stub_run())
            }),
            Duration::from_millis(20),
        );
        coordinator.notify();
        // Wait for the settled state, not for the trainer call: the
        // generation is marked trained only after the trainer returns and
        // the candidate is persisted, which a slow runner can observe apart.
        for _ in 0..2000 {
            if learning_status(&conn.lock().unwrap()).2 == "current" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(trained.load(AtomicOrdering::SeqCst), 1);
        let conn = conn.lock().unwrap();
        let (generation, trained_generation, status) = learning_status(&conn);
        assert_eq!(generation, 1);
        assert_eq!(trained_generation, 1);
        assert_eq!(status, "current");
        let active: i64 = conn
            .query_row(
                "SELECT count(*) FROM face_learning_profiles WHERE status = 'active'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(active, 1, "the promoted candidate must be active");
    }

    #[tokio::test]
    async fn bursts_coalesce_into_a_single_cycle() {
        let conn = library();
        make_generation_pending(&conn.lock().unwrap());
        let trained = Arc::new(AtomicUsize::new(0));
        let counter = trained.clone();
        let coordinator = spawn_with(
            make_deps(conn.clone(), move |_, _, _| {
                counter.fetch_add(1, AtomicOrdering::SeqCst);
                Ok(stub_run())
            }),
            Duration::from_millis(80),
        );
        for _ in 0..5 {
            coordinator.notify();
        }
        // Wait for the one cycle, then keep waiting well past the debounce
        // window to prove no second cycle fires.
        for _ in 0..400 {
            if trained.load(AtomicOrdering::SeqCst) == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(trained.load(AtomicOrdering::SeqCst), 1);
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(
            trained.load(AtomicOrdering::SeqCst),
            1,
            "a burst within the debounce window is one cycle"
        );
        coordinator.shutdown();
    }

    #[tokio::test]
    async fn failure_marks_failed_and_a_retry_recovers() {
        let conn = library();
        make_generation_pending(&conn.lock().unwrap());
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let coordinator = spawn_with(
            make_deps(conn.clone(), move |_, _, _| {
                if counter.fetch_add(1, AtomicOrdering::SeqCst) == 0 {
                    Err(TrainingError::NonConvergence)
                } else {
                    Ok(stub_run())
                }
            }),
            Duration::from_millis(20),
        );
        coordinator.notify();
        for _ in 0..2000 {
            {
                let conn = conn.lock().unwrap();
                if learning_status(&conn).2 == "failed" {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        {
            let conn = conn.lock().unwrap();
            assert_eq!(learning_status(&conn).2, "failed");
            let profiles: i64 = conn
                .query_row("SELECT count(*) FROM face_learning_profiles", [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(profiles, 0, "a failed run must not store a candidate");
            let faces: i64 = conn
                .query_row("SELECT count(*) FROM faces", [], |row| row.get(0))
                .unwrap();
            let confirmed: i64 = conn
                .query_row(
                    "SELECT count(*) FROM faces WHERE confirmed = 1",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(faces, 4, "a failed run touches no face rows");
            assert_eq!(confirmed, 2, "labels survive a failed run");
        }

        coordinator.notify();
        for _ in 0..2000 {
            {
                let conn = conn.lock().unwrap();
                if learning_status(&conn).2 == "current" {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let conn = conn.lock().unwrap();
        assert_eq!(learning_status(&conn).2, "current", "retry recovers");
        let active: i64 = conn
            .query_row(
                "SELECT count(*) FROM face_learning_profiles WHERE status = 'active'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(active, 1);
    }

    async fn wait_for_status(conn: &Arc<Mutex<Connection>>, wanted: &str) {
        for _ in 0..2000 {
            if learning_status(&conn.lock().unwrap()).2 == wanted {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!(
            "status never became {wanted}: {:?}",
            learning_status(&conn.lock().unwrap())
        );
    }

    #[tokio::test]
    async fn too_little_feedback_waits_instead_of_failing() {
        let conn = library();
        make_generation_pending(&conn.lock().unwrap());
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let trainer = move |_: &TrainingSnapshot,
                            _: &TrainingConfig,
                            _: &videre_core::face_learning::PromotionGates| {
            if counter.fetch_add(1, AtomicOrdering::SeqCst) < 3 {
                // Only confirmed labels so far: no dissolved cluster yet.
                Err(TrainingError::InsufficientEvidence {
                    decision_kind: videre_core::face_learning::LearningDecisionKind::ClusterQuality,
                    positive_identities: 14,
                    negative_identities: 0,
                })
            } else {
                Ok(stub_run())
            }
        };
        let trainer = Arc::new(trainer);
        let first = trainer.clone();
        let coordinator = spawn_with(
            make_deps(conn.clone(), move |s, c, g| first(s, c, g)),
            Duration::from_millis(20),
        );
        coordinator.notify();
        wait_for_status(&conn, "waiting").await;
        {
            let conn = conn.lock().unwrap();
            let (generation, trained_generation, status) = learning_status(&conn);
            assert_eq!(status, "waiting", "missing feedback is not a failure");
            assert_eq!(
                trained_generation, generation,
                "the generation was fully evaluated, so a restart must not retrain it"
            );
            let reported = videre_api::face_learning_status(&conn).unwrap();
            assert_eq!(reported.status, "waiting");
            assert_eq!(
                reported.feedback_needed.as_deref(),
                Some("dissolve 2 more wrong clusters")
            );
            assert_eq!(reported.last_error, None);
        }
        coordinator.shutdown();

        // The same build restarting, and actions that record no evidence,
        // do not rebuild the snapshot: nothing that decides the outcome
        // changed.
        let spawn_again = || {
            let again = trainer.clone();
            spawn_with(
                make_deps(conn.clone(), move |s, c, g| again(s, c, g)),
                Duration::from_millis(20),
            )
        };
        let same_build = spawn_again();
        same_build.notify();
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(calls.load(AtomicOrdering::SeqCst), 1);
        same_build.shutdown();

        async fn wait_for_calls(calls: &AtomicUsize, wanted: usize) {
            for _ in 0..2000 {
                if calls.load(AtomicOrdering::SeqCst) >= wanted {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            panic!("the trainer never ran {wanted} time(s)");
        }

        // A newer build tries once (its thresholds or wording may differ),
        // and still waits.
        let forget_build = |conn: &Arc<Mutex<Connection>>| {
            conn.lock()
                .unwrap()
                .execute(
                    "UPDATE library_state SET value = 0 WHERE key = 'face_learning_build'",
                    [],
                )
                .unwrap();
        };
        forget_build(&conn);
        let upgraded = spawn_again();
        wait_for_calls(&calls, 2).await;
        wait_for_status(&conn, "waiting").await;
        upgraded.shutdown();

        // That retry was interrupted (the gallery quit mid-run): the next
        // start tries again instead of leaving a lasting failure.
        forget_build(&conn);
        conn.lock()
            .unwrap()
            .execute(
                "UPDATE face_learning_state SET status = 'training', training_generation = 1",
                [],
            )
            .unwrap();
        let restarted = spawn_again();
        wait_for_calls(&calls, 3).await;
        wait_for_status(&conn, "waiting").await;
        assert_eq!(
            learning_status(&conn.lock().unwrap()),
            (1, 1, "waiting".to_string())
        );

        // New feedback trains again.
        conn.lock()
            .unwrap()
            .execute(
                "UPDATE face_learning_state SET generation = 2, status = 'stale', last_error = NULL",
                [],
            )
            .unwrap();
        restarted.notify();
        wait_for_status(&conn, "current").await;
        let conn = conn.lock().unwrap();
        assert_eq!(learning_status(&conn), (2, 2, "current".to_string()));
        assert_eq!(
            videre_api::face_learning_status(&conn)
                .unwrap()
                .feedback_needed,
            None
        );
    }

    #[tokio::test]
    async fn the_connection_lock_is_free_while_the_trainer_runs() {
        let conn = library();
        make_generation_pending(&conn.lock().unwrap());
        let (started_tx, started_rx) = std::sync::mpsc::channel::<()>();
        let release = Arc::new(std::sync::Mutex::new(false));
        let release_for_trainer = release.clone();
        let _coordinator = spawn_with(
            make_deps(conn.clone(), move |_, _, _| {
                started_tx.send(()).unwrap();
                while !*release_for_trainer.lock().unwrap() {
                    std::thread::sleep(Duration::from_millis(2));
                }
                Ok(stub_run())
            }),
            Duration::from_millis(500),
        );
        // Startup recovery alone triggers the cycle: the state is stale.
        // Poll without blocking so the current-thread runtime can drive the
        // coordinator task.
        let mut started = false;
        for _ in 0..2000 {
            if started_rx.try_recv().is_ok() {
                started = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(started, "trainer must start");
        let probe = conn.try_lock();
        assert!(
            probe.is_ok(),
            "the snapshot lock must be released during the fit"
        );
        *release.lock().unwrap() = true;
        drop(probe);
        for _ in 0..2000 {
            let done = {
                let conn = conn.lock().unwrap();
                learning_status(&conn).2 == "current"
            };
            if done {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    #[tokio::test]
    async fn startup_recovers_a_run_interrupted_by_a_previous_process() {
        let conn = library();
        {
            let conn = conn.lock().unwrap();
            conn.execute(
                "UPDATE face_learning_state SET generation = 1, status = 'training',
                 training_generation = 1",
                [],
            )
            .unwrap();
        }
        let trained = Arc::new(AtomicUsize::new(0));
        let counter = trained.clone();
        let _coordinator = spawn_with(
            make_deps(conn.clone(), move |_, _, _| {
                counter.fetch_add(1, AtomicOrdering::SeqCst);
                Ok(stub_run())
            }),
            Duration::from_millis(500),
        );
        for _ in 0..2000 {
            {
                let conn = conn.lock().unwrap();
                if learning_status(&conn).2 == "current" {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        {
            let conn = conn.lock().unwrap();
            assert_eq!(learning_status(&conn).2, "current");
            assert_eq!(trained.load(AtomicOrdering::SeqCst), 1);
        }
    }

    #[tokio::test]
    async fn a_rejected_candidate_keeps_the_active_profiles_questions() {
        let conn = library();
        make_generation_pending(&conn.lock().unwrap());
        seed_active_profile_and_questions(&conn.lock().unwrap());
        // The stub run here carries too few datasets for the shipped gates:
        // training succeeds, promotion rejects.
        let coordinator = spawn_with(
            make_deps(conn.clone(), |_, _, _| Ok(rejected_run())),
            Duration::from_millis(10),
        );
        coordinator.notify();
        for _ in 0..2000 {
            {
                let conn = conn.lock().unwrap();
                if learning_status(&conn).2 == "current" {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let conn = conn.lock().unwrap();
        assert_eq!(learning_status(&conn).2, "current");
        let rejected: i64 = conn
            .query_row(
                "SELECT count(*) FROM face_learning_profiles WHERE status = 'rejected'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(rejected, 1, "the failing candidate is stored and rejected");
        let pending: i64 = conn
            .query_row(
                "SELECT count(*) FROM face_learning_questions WHERE status = 'pending'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            pending, 1,
            "a rejection must not supersede the active profile's questions"
        );
        let status = videre_api::face_learning_status(&conn).unwrap();
        assert_eq!(
            status.last_candidate.as_deref(),
            Some("rejected"),
            "a rejection must be distinguishable from a promotion"
        );
    }

    #[tokio::test]
    async fn a_failed_question_refresh_fails_the_cycle_and_recovers() {
        let conn = library();
        make_generation_pending(&conn.lock().unwrap());
        seed_active_profile_and_questions(&conn.lock().unwrap());
        // Any supersede of a pending question aborts, so the refresh step of
        // a promoted run fails.
        conn.lock()
            .unwrap()
            .execute_batch(
                "CREATE TRIGGER abort_question_refresh
                 BEFORE UPDATE OF status ON face_learning_questions
                 WHEN NEW.status = 'superseded'
                 BEGIN SELECT RAISE(ABORT, 'injected refresh failure'); END;",
            )
            .unwrap();
        let trained = Arc::new(AtomicUsize::new(0));
        let counter = trained.clone();
        let coordinator = spawn_with(
            make_deps(conn.clone(), move |_, _, _| {
                counter.fetch_add(1, AtomicOrdering::SeqCst);
                Ok(stub_run())
            }),
            Duration::from_millis(10),
        );
        coordinator.notify();
        for _ in 0..2000 {
            {
                let conn = conn.lock().unwrap();
                if learning_status(&conn).2 == "failed" {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        {
            let conn = conn.lock().unwrap();
            let (generation, trained_generation, status) = learning_status(&conn);
            assert_eq!(status, "failed", "the refresh failure must fail the cycle");
            assert_ne!(generation, trained_generation, "nothing is marked trained");
            let state = learning_state(&conn).unwrap();
            assert!(
                state
                    .last_error
                    .as_deref()
                    .unwrap_or("")
                    .contains("question refresh"),
                "the error must name the refresh: {:?}",
                state.last_error
            );
            let active: i64 = conn
                .query_row(
                    "SELECT count(*) FROM face_learning_profiles WHERE status = 'active'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(active, 1, "the promoted profile stays active");
        }

        // Removing the fault and notifying again retrains the generation.
        conn.lock()
            .unwrap()
            .execute_batch("DROP TRIGGER abort_question_refresh;")
            .unwrap();
        coordinator.notify();
        for _ in 0..2000 {
            {
                let conn = conn.lock().unwrap();
                if learning_status(&conn).2 == "current" {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let conn = conn.lock().unwrap();
        let (generation, trained_generation, status) = learning_status(&conn);
        assert_eq!(status, "current", "the retry recovers");
        assert_eq!(generation, trained_generation);
        let pending: i64 = conn
            .query_row(
                "SELECT count(*) FROM face_learning_questions WHERE status = 'pending'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(pending, 1, "the promoted profile gets a fresh page");
        let status = videre_api::face_learning_status(&conn).unwrap();
        assert_eq!(status.last_candidate.as_deref(), Some("promoted"));
    }

    /// Every face column a learning run could conceivably touch.
    fn face_snapshot(conn: &Connection) -> Vec<(i64, Option<i64>, Option<String>, i64, i64)> {
        let mut statement = conn
            .prepare(
                "SELECT id, cluster_id, person_label, confirmed, is_primary
                 FROM faces ORDER BY id",
            )
            .unwrap();
        statement
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    }

    /// The central guarantee, pinned for each way a background run can end:
    /// training that fails, a candidate the gates reject, and a candidate
    /// that is promoted (which also refreshes questions). None of them may
    /// change a face row; only explicit user mutations do.
    #[tokio::test]
    async fn no_background_outcome_changes_face_rows() {
        type Trainer = fn(
            &TrainingSnapshot,
            &TrainingConfig,
            &videre_core::face_learning::PromotionGates,
        ) -> Result<TrainingRun, TrainingError>;
        let outcomes: [(&str, Trainer, &str, Option<&str>); 3] = [
            (
                "failed",
                |_, _, _| Err(TrainingError::NonConvergence),
                "failed",
                None,
            ),
            (
                "rejected",
                |_, _, _| Ok(rejected_run()),
                "current",
                Some("rejected"),
            ),
            (
                "promoted",
                |_, _, _| Ok(stub_run()),
                "current",
                Some("promoted"),
            ),
        ];
        for (name, trainer, settled, candidate) in outcomes {
            let conn = library();
            make_generation_pending(&conn.lock().unwrap());
            seed_active_profile_and_questions(&conn.lock().unwrap());
            let before = face_snapshot(&conn.lock().unwrap());
            let coordinator =
                spawn_with(make_deps(conn.clone(), trainer), Duration::from_millis(10));
            coordinator.notify();
            for _ in 0..2000 {
                if learning_status(&conn.lock().unwrap()).2 == settled {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            let conn = conn.lock().unwrap();
            assert_eq!(
                learning_status(&conn).2,
                settled,
                "{name} run did not settle"
            );
            let status = videre_api::face_learning_status(&conn).unwrap();
            if candidate.is_some() {
                assert_eq!(status.last_candidate.as_deref(), candidate, "{name}");
            }
            assert_eq!(
                face_snapshot(&conn),
                before,
                "a {name} run changed face rows"
            );
        }
    }
}
