//! Background face-learning worker for the gallery.
//!
//! One coordinator task owns training for the whole server: teaching
//! mutations notify it after commit, it debounces bursts, then runs at most
//! one training cycle at a time with at most one queued follow-up. The cycle
//! loads the immutable training snapshot under the connection lock, releases
//! the lock for the CPU-bound fit, then relocks to persist, promote, and
//! refresh the pending questions. A failed cycle preserves the prior active
//! profile and marks the learning state failed for a later retry.

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use rusqlite::Connection;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;
use videre_core::face_learning::{
    ensure_learning_tables, learning_state, mark_generation_trained, mark_training_failed,
    mark_training_started, QuestionSelectionConfig, TrainingConfig, TrainingError, TrainingRun,
    TrainingSnapshot,
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
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(run(deps, DEBOUNCE, rx));
    LearningCoordinator { tx }
}

#[cfg_attr(not(test), allow(dead_code))]
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
        }
        Err(_) => false,
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
    if state.generation <= state.trained_generation {
        return Ok(None);
    }
    let state = mark_training_started(conn).map_err(|error| {
        let _ = mark_training_failed(conn, state.generation, &error.to_string());
    })?;
    let generation = state.generation;
    let snapshot = load_snapshot(conn, deps, generation).map_err(|error| {
        let _ = mark_training_failed(conn, generation, &error);
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
    mark_generation_trained(conn, generation, Some(summary.profile_id))
        .map_err(|e| e.to_string())?;
    let _ = videre_api::refresh_identity_questions(conn, &deps.questions);
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
            Ok(_profile_id) => {}
            Err(error) => {
                let _ = mark_training_failed(&conn, generation, &error);
            }
        },
        Ok(Err(error)) => {
            let _ = mark_training_failed(&conn, generation, &error.to_string());
        }
        Err(join_error) => {
            let _ = mark_training_failed(&conn, generation, &join_error.to_string());
        }
    }
}

/// GET /api/face-learning/status
pub(crate) async fn handle_learning_status(
    State(state): State<Arc<AppState>>,
) -> Result<Json<videre_api::FaceLearningStatus>, StatusCode> {
    let conn = state
        .conn
        .lock()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
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
    let conn = state
        .conn
        .lock()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
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
        let conn = state
            .conn
            .lock()
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
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
) -> Result<Json<Vec<videre_core::face_learning::StoredLearningEvent>>, StatusCode> {
    let limit = params
        .get("limit")
        .and_then(|limit| limit.parse::<usize>().ok())
        .unwrap_or(50)
        .clamp(1, 200);
    let before = params
        .get("before")
        .and_then(|before| before.parse::<i64>().ok());
    let conn = state
        .conn
        .lock()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    videre_api::face_learning_events(&conn, limit, before)
        .map(Json)
        .map_err(api_status)
}

/// GET /api/face-learning/events/{id}
pub(crate) async fn handle_learning_event_detail(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<i64>,
) -> Result<Json<videre_core::face_learning::StoredLearningEvent>, StatusCode> {
    let conn = state
        .conn
        .lock()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    match videre_api::face_learning_event(&conn, id) {
        Ok(Some(event)) => Ok(Json(event)),
        Ok(None) => Err(StatusCode::NOT_FOUND),
        Err(error) => Err(api_status(error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

    fn embedding_blob() -> Vec<u8> {
        // f16 LE of (1.0, 0.0), the same shape the feature fixture uses.
        vec![0x00, 0x3C, 0x00, 0x00]
    }

    /// Faces 10 and 11 sit in cluster 1; faces 12 and 13 confirm alice, so a
    /// snapshot can always be built and the injected trainer decides whether
    /// training succeeds.
    fn library() -> Arc<Mutex<Connection>> {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE faces (id INTEGER PRIMARY KEY, hash TEXT NOT NULL,
             bbox TEXT NOT NULL, landmark TEXT, embedding BLOB NOT NULL,
             cluster_id INTEGER, person_label TEXT, confirmed INTEGER DEFAULT 0,
             is_primary INTEGER DEFAULT 0, det_score REAL, blur REAL, oriented INTEGER);
             CREATE TABLE people (name TEXT PRIMARY KEY, full_name TEXT NOT NULL);
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

    fn stub_run() -> TrainingRun {
        use videre_core::face_learning::{
            CalibrationModel, CandidateComparison, CandidateKind, ClusteringMetrics,
            DatasetValidation as Report, LogisticModel, LogisticScorer, SuggestionMetrics,
            TrainingEvidenceCounts, ValidationReport, MODEL_ARTIFACT_VERSION,
        };
        fn report() -> ValidationReport {
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
        let scorer = LogisticScorer {
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
        };
        TrainingRun {
            selected: videre_core::face_learning::ModelBundle::Logistic {
                artifact_version: MODEL_ARTIFACT_VERSION,
                embedding_model_id: "arcface/test".into(),
                feature_schema_version: 1,
                membership: scorer.clone(),
                cluster_quality: scorer,
            },
            logistic_validation: report(),
            additive_validation: report(),
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
        for _ in 0..200 {
            if trained.load(AtomicOrdering::SeqCst) == 1 {
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
        for _ in 0..200 {
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
        }

        coordinator.notify();
        for _ in 0..200 {
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
        for _ in 0..200 {
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
        for _ in 0..200 {
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
        for _ in 0..200 {
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
}
