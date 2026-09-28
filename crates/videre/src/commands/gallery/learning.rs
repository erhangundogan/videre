//! Background face-learning worker for the gallery.
//!
//! One coordinator owns training for the whole server, on its own OS thread
//! (`videre-learning`) and its own database connection. It never touches the
//! gallery's shared connection: a training run can take a minute of CPU on a
//! large library, and holding the shared connection for that long stalled
//! every request. WAL lets this connection read and write beside the
//! gallery's and beside `videre watch`.
//!
//! Teaching mutations notify it after commit. It waits until no notification
//! has arrived for a quiet window, then runs at most one cycle at a time with
//! at most one queued follow-up. A cycle marks the state training, reads its
//! inputs in one short read transaction, builds the snapshot and fits with
//! nothing held, then persists, promotes, and refreshes the pending questions
//! in short write transactions. A failed cycle preserves the prior active
//! profile and marks the learning state failed for a later retry. A cycle that
//! finds too little feedback to train on is not a failure: it marks the state
//! waiting, with what the People page should ask for. A busy database is not a
//! failure either: the cycle is retried after the next quiet window.
//!
//! Learning is off unless the library's `gallery.json` sets `faces.learning`;
//! the setting is read before every cycle, so off means no cycle runs at all.

use super::server::poisoned;
use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use rusqlite::Connection;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::Arc;
use std::time::Duration;
use videre_core::face_learning::{
    ensure_learning_tables, learning_state, mark_generation_trained, mark_training_failed,
    mark_training_started, mark_training_waiting, QuestionSelectionConfig, TrainingConfig,
    TrainingError, TrainingRun, TrainingSnapshot,
};

use super::server::{api_error, ApiError, AppState};

/// How long the coordinator waits after the last notification before a
/// cycle starts. A cycle costs about a minute of one core on a large library
/// however little changed, and naming actions come seconds apart.
const QUIET: Duration = Duration::from_secs(10);

/// How long a connection waits on a lock the other connection holds. Set on
/// both the gallery's and the engine's, since they now share the database.
pub(crate) const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

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
    tx: mpsc::Sender<CoordinatorMessage>,
}

impl LearningCoordinator {
    pub fn notify(&self) {
        let _ = self.tx.send(CoordinatorMessage::Train);
    }

    pub fn shutdown(&self) {
        let _ = self.tx.send(CoordinatorMessage::Shutdown);
    }
}

/// Everything the coordinator needs. The trainer closure and the enabled
/// check are injectable so the mechanics (quiet window, single-flight,
/// recovery, busy retry) are testable without model work or a settings file.
pub struct LearningDeps {
    /// The engine's own connection, never the gallery's.
    pub conn: Connection,
    pub embedding_model_id: String,
    pub config: TrainingConfig,
    pub gates: videre_core::face_learning::PromotionGates,
    pub questions: QuestionSelectionConfig,
    /// Whether learning is on for this library, asked before every cycle.
    pub enabled: Arc<dyn Fn() -> bool + Send + Sync>,
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
pub fn spawn(deps: LearningDeps) -> std::io::Result<LearningCoordinator> {
    spawn_with(deps, QUIET)
}

pub fn spawn_with(deps: LearningDeps, quiet: Duration) -> std::io::Result<LearningCoordinator> {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("videre-learning".into())
        .spawn(move || run(deps, quiet, rx))?;
    Ok(LearningCoordinator { tx })
}

#[derive(Debug, PartialEq)]
enum Cycle {
    Done,
    /// The database was busy; nothing was recorded as failed, try again.
    Busy,
}

fn run(deps: LearningDeps, quiet: Duration, rx: mpsc::Receiver<CoordinatorMessage>) {
    // Startup recovery of stale work.
    let mut retry = (deps.enabled)() && needs_training(&deps) && run_cycle(&deps) == Cycle::Busy;
    loop {
        if !retry {
            match rx.recv() {
                Ok(CoordinatorMessage::Train) => {}
                Ok(CoordinatorMessage::Shutdown) | Err(_) => return,
            }
        }
        // Wait until no notification has arrived for `quiet`; everything
        // that arrives meanwhile is coalesced into the one cycle below.
        loop {
            match rx.recv_timeout(quiet) {
                Ok(CoordinatorMessage::Train) => continue,
                Ok(CoordinatorMessage::Shutdown) | Err(RecvTimeoutError::Disconnected) => return,
                Err(RecvTimeoutError::Timeout) => break,
            }
        }
        retry = (deps.enabled)() && run_cycle(&deps) == Cycle::Busy;
    }
}

/// Whether an error is SQLite reporting a lock held elsewhere past the busy
/// timeout. The learning store turns errors into text, so the text is what is
/// left to tell a busy database from a real failure.
fn is_busy(error: &str) -> bool {
    error.contains("database is locked") || error.contains("database table is locked")
}

fn needs_training(deps: &LearningDeps) -> bool {
    match learning_state(&deps.conn) {
        Ok(state) => {
            state.status == videre_core::face_learning::LearningStatus::Training
                || state.generation > state.trained_generation
                || retry_on_new_build(&deps.conn, &state)
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
    inputs: videre_api::TrainingInputs,
}

/// A failed step of phase 1: busy asks for a retry, anything else ends the
/// cycle (the caller has already recorded it where that is possible).
fn busy_or_done(error: impl std::fmt::Display) -> Cycle {
    if is_busy(&error.to_string()) {
        Cycle::Busy
    } else {
        Cycle::Done
    }
}

/// Phase 1, short: flip the state to training and read the inputs for
/// generation G in one read transaction. A legitimately empty library (no
/// events beyond generation 0) is skipped without touching state; real
/// failures mark the state failed, a busy database only asks for a retry.
fn prepare_training(deps: &LearningDeps) -> Result<Option<PreparedCycle>, Cycle> {
    let conn = &deps.conn;
    ensure_learning_tables(conn).map_err(busy_or_done)?;
    let state = learning_state(conn).map_err(busy_or_done)?;
    if state.status == videre_core::face_learning::LearningStatus::Training {
        // A previous process died mid-run, or a run here was cut short by a
        // busy database; retire the stale run first.
        let interrupted = state.training_generation.unwrap_or(state.generation);
        mark_training_failed(conn, interrupted as u64, "training was interrupted")
            .map_err(busy_or_done)?;
    }
    let state = learning_state(conn).map_err(busy_or_done)?;
    if state.generation <= state.trained_generation && !retry_on_new_build(conn, &state) {
        return Ok(None);
    }
    let state = mark_training_started(conn).map_err(|error| {
        let cycle = busy_or_done(&error);
        if cycle == Cycle::Done {
            let _ = mark_training_failed(conn, state.generation, &error.to_string());
        }
        cycle
    })?;
    let generation = state.generation;
    let inputs =
        videre_api::load_training_inputs(conn, &deps.embedding_model_id).map_err(|error| {
            let cycle = busy_or_done(&error);
            if cycle == Cycle::Done {
                record_failure(conn, generation, &error.to_string());
                note_build(conn);
            }
            cycle
        })?;
    Ok(Some(PreparedCycle { generation, inputs }))
}

/// Persist a trained candidate: insert, promote through the shipped gates,
/// advance the state, then refresh the pending questions. The profile row
/// keeps the promotion verdict either way.
fn persist_training(
    conn: &Connection,
    deps: &LearningDeps,
    generation: u64,
    run: &TrainingRun,
) -> Result<videre_api::TrainedProfileSummary, videre_api::Error> {
    let summary = videre_api::persist_trained_profile(
        conn,
        &deps.embedding_model_id,
        run,
        &deps.gates,
        generation,
    )?;
    // The pending question page was built against the still-active profile,
    // so it is refreshed only when this run actually promoted. Rebuilding it
    // after a rejection would supersede those questions with identical ones
    // and leave the page empty.
    if summary.promoted {
        videre_api::refresh_identity_questions(conn, &deps.questions).map_err(|error| {
            videre_api::Error::Other(format!("question refresh failed: {error}"))
        })?;
    }
    // Marked trained only after the refresh succeeded, so a refresh failure
    // leaves the state failed and the next notification retrains the
    // generation instead of stranding stale questions behind a current
    // status. A retry inserts a fresh candidate row; promotion history stays
    // per row.
    mark_generation_trained(conn, generation, Some(summary.profile_id)).map_err(|error| {
        // The same fence as persistence: a cleared training marker means prune
        // withdrew evidence meanwhile. A newer generation alone is new
        // feedback, which mark_generation_trained handles by going stale.
        if learning_state(conn).is_ok_and(|state| {
            state.status != videre_core::face_learning::LearningStatus::Training
                || state.training_generation != Some(generation)
        }) {
            videre_api::Error::Conflict
        } else {
            error.into()
        }
    })?;
    Ok(summary)
}

/// One training cycle: a short read, the slow build and fit with nothing
/// held, then short writes.
fn run_cycle(deps: &LearningDeps) -> Cycle {
    let prepared = match prepare_training(deps) {
        Ok(Some(prepared)) => prepared,
        Ok(None) => return Cycle::Done,
        Err(cycle) => return cycle,
    };
    let conn = &deps.conn;
    let generation = prepared.generation;
    tracing::debug!("face learning: training generation {generation}");
    let trained = videre_api::build_training_inputs(
        &prepared.inputs,
        generation,
        &deps.embedding_model_id,
        &deps.config,
    )
    .map(|snapshot| (deps.train)(&snapshot, &deps.config, &deps.gates));
    match trained {
        Ok(Ok(run)) => match persist_training(conn, deps, generation, &run) {
            Ok(summary) if summary.promoted => {
                tracing::info!(
                    "face learning: generation {generation} promoted, profile {} in use",
                    summary.profile_id
                );
            }
            Ok(summary) => {
                let reason = videre_api::rejection_reason(conn, summary.profile_id)
                    .ok()
                    .flatten()
                    .map(|r| format!(" ({r})"))
                    .unwrap_or_default();
                tracing::info!(
                    "face learning: generation {generation} did not pass the quality checks{reason}; previous profile kept"
                );
            }
            Err(videre_api::Error::Conflict) => return Cycle::Busy,
            Err(error) if is_busy(&error.to_string()) => return Cycle::Busy,
            Err(error) => record_failure(conn, generation, &error.to_string()),
        },
        Ok(Err(error)) => match error.feedback_needed(&deps.config) {
            // Too little feedback yet is the normal state of a young library,
            // not a failure: the page asks for what is missing.
            Some(needed) => {
                tracing::info!("face learning: waiting for feedback: {needed}");
                if let Err(write) = mark_training_waiting(conn, generation, &needed) {
                    if is_busy(&write.to_string()) {
                        return Cycle::Busy;
                    }
                    // The state stays training until the next start retires
                    // it; say why here, where the cause is known.
                    tracing::warn!(
                        "face learning: could not record waiting for generation {generation}: {write}"
                    );
                }
            }
            // A fit that does not converge is an expected outcome on little or
            // lopsided feedback, handled by keeping the previous profile: worth
            // a line, not a warning.
            None if matches!(error, TrainingError::NonConvergence) => {
                tracing::info!(
                    "face learning: generation {generation} did not converge; previous profile kept, retries after new feedback"
                );
                let _ = mark_training_failed(conn, generation, &error.to_string());
            }
            None => record_failure(conn, generation, &error.to_string()),
        },
        Err(error) => record_failure(conn, generation, &error),
    }
    note_build(conn);
    Cycle::Done
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
) -> Result<Json<videre_api::FaceLearningStatus>, ApiError> {
    let enabled = state.learning_enabled();
    let conn = state.conn.lock().map_err(poisoned)?;
    videre_api::face_learning_status(&conn)
        .map(|status| if enabled { status } else { status.turned_off() })
        .map(Json)
        .map_err(api_error)
}

/// GET /api/face-learning/questions?limit=N
pub(crate) async fn handle_learning_questions(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Result<Json<Vec<videre_core::face_learning::StoredQuestion>>, ApiError> {
    let limit = params
        .get("limit")
        .and_then(|limit| limit.parse::<usize>().ok())
        .unwrap_or(8)
        .clamp(1, 50);
    // Off, nothing is asked: the stored page stays for when it is on again.
    if !state.learning_enabled() {
        return Ok(Json(Vec::new()));
    }
    let conn = state.conn.lock().map_err(poisoned)?;
    videre_api::pending_identity_questions(&conn, limit)
        .map(Json)
        .map_err(api_error)
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
) -> Result<Json<videre_api::QuestionAnswerOutcome>, ApiError> {
    let answer = match body.answer.as_str() {
        "yes" | "Yes" => videre_core::face_learning::QuestionAnswer::Yes,
        "no" | "No" => videre_core::face_learning::QuestionAnswer::No,
        "skip" | "Skip" => videre_core::face_learning::QuestionAnswer::Skip,
        _ => return Err(StatusCode::BAD_REQUEST.into()),
    };
    if !state.learning_enabled() {
        return Err(StatusCode::SERVICE_UNAVAILABLE.into());
    }
    let outcome = {
        let conn = state.conn.lock().map_err(poisoned)?;
        let context = super::server::teaching_context(&conn, &state);
        videre_api::answer_question_with_learning(&conn, id, answer, &context).map_err(api_error)?
    };
    state.notify_learning();
    Ok(Json(outcome))
}

/// GET /api/face-learning/events?limit=N&before=ID
pub(crate) async fn handle_learning_events(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Result<Json<Vec<videre_api::FaceLearningEventProof>>, ApiError> {
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
        .map_err(api_error)
}

/// GET /api/face-learning/events/{id}
pub(crate) async fn handle_learning_event_detail(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<i64>,
) -> Result<Json<videre_api::FaceLearningEventProof>, ApiError> {
    let conn = state.conn.lock().map_err(poisoned)?;
    match videre_api::face_learning_event(&conn, id, Some(&state.model_id)) {
        Ok(Some(event)) => Ok(Json(event)),
        Ok(None) => Err(StatusCode::NOT_FOUND.into()),
        Err(error) => Err(api_error(error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
    use std::sync::Mutex;

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
    ///
    /// File-backed in WAL mode, as a library is: the engine opens its own
    /// connection to it, and the returned one plays the gallery's.
    fn library() -> (tempfile::TempDir, Arc<Mutex<Connection>>) {
        let dir = tempfile::tempdir().unwrap();
        let conn = Connection::open(dir.path().join("hashes.db")).unwrap();
        conn.pragma_update(None, "journal_mode", "WAL").unwrap();
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
        (dir, Arc::new(Mutex::new(conn)))
    }

    /// The engine's own connection to the same database file.
    fn engine_connection(conn: &Arc<Mutex<Connection>>, busy: Duration) -> Connection {
        let path = conn.lock().unwrap().path().unwrap().to_owned();
        let engine = Connection::open(path).unwrap();
        engine.busy_timeout(busy).unwrap();
        engine
    }

    fn start(deps: LearningDeps, quiet: Duration) -> LearningCoordinator {
        spawn_with(deps, quiet).unwrap()
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
            conn: engine_connection(&conn, BUSY_TIMEOUT),
            enabled: Arc::new(|| true),
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

    #[test]
    fn prune_during_fit_retries_without_reactivating_old_evidence() {
        let (_dir, gallery) = library();
        {
            let conn = gallery.lock().unwrap();
            make_generation_pending(&conn);
            let features = videre_core::face_learning::FeatureVector {
                schema_version: videre_core::face_learning::FEATURE_SCHEMA_VERSION,
                values: MEMBERSHIP_FEATURE_NAMES
                    .iter()
                    .map(|name| ((*name).to_owned(), 0.0))
                    .collect(),
            }
            .to_canonical_json()
            .unwrap();
            conn.execute(
                "INSERT INTO face_learning_events
                 (id,action_kind,decision_kind,outcome,embedding_model_id,feature_schema_version,feature_snapshot_json,support_count)
                 VALUES(1,'assign_face','membership','positive','arcface/test',1,?1,0)",
                [features],
            ).unwrap();
            conn.execute(
                "INSERT INTO face_learning_event_faces(event_id,face_id,role,ordinal)
                 VALUES(1,10,'subject',0)",
                [],
            )
            .unwrap();
        }
        let during_fit = gallery.clone();
        let deps = make_deps(gallery.clone(), move |snapshot, _, _| {
            assert_eq!(snapshot.generation, 1);
            let conn = during_fit.lock().unwrap();
            conn.execute_batch("BEGIN IMMEDIATE; DELETE FROM faces WHERE id=10;")
                .unwrap();
            videre_core::face_learning::reconcile_missing_face_sources_in_transaction(&conn)
                .unwrap();
            conn.execute_batch("COMMIT").unwrap();
            Ok(stub_run())
        });
        assert_eq!(run_cycle(&deps), Cycle::Busy);
        {
            let conn = gallery.lock().unwrap();
            assert_eq!(
                conn.query_row("SELECT count(*) FROM face_learning_profiles", [], |r| r
                    .get::<_, i64>(0))
                    .unwrap(),
                0
            );
            assert_eq!(learning_state(&conn).unwrap().generation, 2);
        }
        assert_eq!(prepare_training(&deps).unwrap().unwrap().generation, 2);
    }

    /// Deleting a person during a fit withdraws their evidence, like prune: the
    /// fit trained on it, so it must not be promoted.
    #[test]
    fn person_removal_during_fit_retries_without_its_evidence() {
        let (_dir, gallery) = library();
        {
            let conn = gallery.lock().unwrap();
            make_generation_pending(&conn);
            let features = videre_core::face_learning::FeatureVector {
                schema_version: videre_core::face_learning::FEATURE_SCHEMA_VERSION,
                values: MEMBERSHIP_FEATURE_NAMES
                    .iter()
                    .map(|name| ((*name).to_owned(), 0.0))
                    .collect(),
            }
            .to_canonical_json()
            .unwrap();
            conn.execute(
                "INSERT INTO face_learning_events
                 (id,action_kind,decision_kind,outcome,embedding_model_id,feature_schema_version,feature_snapshot_json,support_count,target_identity)
                 VALUES(1,'assign_face','membership','positive','arcface/test',1,?1,0,'alice')",
                [features],
            )
            .unwrap();
        }
        let during_fit = gallery.clone();
        let deps = make_deps(gallery.clone(), move |_, _, _| {
            let conn = during_fit.lock().unwrap();
            conn.execute_batch("BEGIN IMMEDIATE").unwrap();
            videre_core::face_learning::invalidate_identity_for_removal_in_transaction(
                &conn, "alice",
            )
            .unwrap();
            conn.execute_batch("COMMIT").unwrap();
            Ok(stub_run())
        });
        assert_eq!(run_cycle(&deps), Cycle::Busy);
        let conn = gallery.lock().unwrap();
        assert_eq!(
            conn.query_row("SELECT count(*) FROM face_learning_profiles", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0,
            "the fit on withdrawn evidence is not stored"
        );
        let state = learning_state(&conn).unwrap();
        assert_eq!(state.generation, 2);
        assert_eq!(state.training_generation, None);
    }

    /// Naming during a fit advances the generation but withdraws nothing, so
    /// the finished run is kept (and the state goes stale for a follow-up),
    /// unlike a prune during the fit. On a large library a run takes a
    /// minute; discarding it on every naming action would starve promotion.
    #[test]
    fn a_teaching_action_during_fit_keeps_the_run() {
        let (_dir, gallery) = library();
        make_generation_pending(&gallery.lock().unwrap());
        let during_fit = gallery.clone();
        let deps = make_deps(gallery.clone(), move |_, _, _| {
            // What `append_event_batch_in_transaction` does to the state.
            during_fit
                .lock()
                .unwrap()
                .execute(
                    "UPDATE face_learning_state SET generation = generation + 1 WHERE id = 1",
                    [],
                )
                .unwrap();
            Ok(stub_run())
        });
        assert_eq!(run_cycle(&deps), Cycle::Done);
        let conn = gallery.lock().unwrap();
        assert_eq!(
            conn.query_row(
                "SELECT count(*) FROM face_learning_profiles WHERE status = 'active'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1,
            "the run is promoted"
        );
        let (generation, trained_generation, status) = learning_status(&conn);
        assert_eq!((generation, trained_generation), (2, 1));
        assert_eq!(status, "stale", "the new feedback trains next");
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
        let (_dir, conn) = library();
        make_generation_pending(&conn.lock().unwrap());
        let trained = Arc::new(AtomicUsize::new(0));
        let counter = trained.clone();
        let coordinator = start(
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
        let (_dir, conn) = library();
        make_generation_pending(&conn.lock().unwrap());
        let trained = Arc::new(AtomicUsize::new(0));
        let counter = trained.clone();
        let coordinator = start(
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
        let (_dir, conn) = library();
        make_generation_pending(&conn.lock().unwrap());
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let coordinator = start(
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
        let (_dir, conn) = library();
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
        let coordinator = start(
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
            start(
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

    /// The freeze this module exists to prevent: while a run is in progress
    /// the gallery's connection writes and reads the state at once, and the
    /// state says training.
    #[tokio::test]
    async fn the_gallery_connection_is_free_while_the_trainer_runs() {
        let (_dir, conn) = library();
        make_generation_pending(&conn.lock().unwrap());
        let (started_tx, started_rx) = std::sync::mpsc::channel::<()>();
        let release = Arc::new(std::sync::Mutex::new(false));
        let release_for_trainer = release.clone();
        let _coordinator = start(
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
        started_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("trainer must start");
        {
            let gallery = conn.lock().unwrap();
            gallery.busy_timeout(Duration::from_millis(100)).unwrap();
            let began = std::time::Instant::now();
            gallery
                .execute_batch(
                    "BEGIN IMMEDIATE;
                     UPDATE faces SET cluster_id = 2 WHERE id = 10;
                     COMMIT;",
                )
                .expect("a naming write must not wait for training");
            let status = videre_api::face_learning_status(&gallery).unwrap();
            assert!(
                began.elapsed() < Duration::from_millis(100),
                "{:?}",
                began.elapsed()
            );
            assert_eq!(status.status, "training");
        }
        *release.lock().unwrap() = true;
        wait_for_status(&conn, "current").await;
    }

    #[tokio::test]
    async fn a_cycle_starts_only_after_notifications_stop() {
        let (_dir, conn) = library();
        let trained = Arc::new(std::sync::Mutex::new(None::<std::time::Instant>));
        let at = trained.clone();
        let coordinator = start(
            make_deps(conn.clone(), move |_, _, _| {
                *at.lock().unwrap() = Some(std::time::Instant::now());
                Ok(stub_run())
            }),
            Duration::from_millis(150),
        );
        // Past startup recovery, which found nothing to train.
        tokio::time::sleep(Duration::from_millis(100)).await;
        make_generation_pending(&conn.lock().unwrap());
        let mut last = std::time::Instant::now();
        for _ in 0..4 {
            coordinator.notify();
            last = std::time::Instant::now();
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        wait_for_status(&conn, "current").await;
        let ran = trained.lock().unwrap().expect("one cycle ran");
        assert!(
            ran >= last + Duration::from_millis(150),
            "the cycle must wait for the quiet window after the last notification"
        );
    }

    #[tokio::test]
    async fn with_learning_off_nothing_runs_or_changes() {
        let (_dir, conn) = library();
        make_generation_pending(&conn.lock().unwrap());
        let before = learning_status(&conn.lock().unwrap());
        let trained = Arc::new(AtomicUsize::new(0));
        let counter = trained.clone();
        let mut deps = make_deps(conn.clone(), move |_, _, _| {
            counter.fetch_add(1, AtomicOrdering::SeqCst);
            Ok(stub_run())
        });
        deps.enabled = Arc::new(|| false);
        let coordinator = start(deps, Duration::from_millis(10));
        coordinator.notify();
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(trained.load(AtomicOrdering::SeqCst), 0);
        assert_eq!(learning_status(&conn.lock().unwrap()), before);
    }

    /// A write the gallery holds past the engine's busy timeout is not a
    /// failure: nothing is recorded as failed, and the cycle runs again.
    #[tokio::test]
    async fn a_busy_database_is_retried_not_failed() {
        let (_dir, conn) = library();
        make_generation_pending(&conn.lock().unwrap());
        conn.lock()
            .unwrap()
            .execute_batch("BEGIN IMMEDIATE;")
            .unwrap();
        let mut deps = make_deps(conn.clone(), |_, _, _| Ok(stub_run()));
        deps.conn = engine_connection(&conn, Duration::from_millis(50));
        let _coordinator = start(deps, Duration::from_millis(100));
        // Startup recovery runs into the held write.
        tokio::time::sleep(Duration::from_millis(300)).await;
        {
            let gallery = conn.lock().unwrap();
            let state = learning_state(&gallery).unwrap();
            assert_eq!(
                state.last_error, None,
                "busy must not be recorded as a failure"
            );
            gallery.execute_batch("COMMIT;").unwrap();
        }
        wait_for_status(&conn, "current").await;
    }

    #[tokio::test]
    async fn startup_recovers_a_run_interrupted_by_a_previous_process() {
        let (_dir, conn) = library();
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
        let _coordinator = start(
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
        let (_dir, conn) = library();
        make_generation_pending(&conn.lock().unwrap());
        seed_active_profile_and_questions(&conn.lock().unwrap());
        // The stub run here carries too few datasets for the shipped gates:
        // training succeeds, promotion rejects.
        let coordinator = start(
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
        let (_dir, conn) = library();
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
        let coordinator = start(
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
            let (_dir, conn) = library();
            make_generation_pending(&conn.lock().unwrap());
            seed_active_profile_and_questions(&conn.lock().unwrap());
            let before = face_snapshot(&conn.lock().unwrap());
            let coordinator = start(make_deps(conn.clone(), trainer), Duration::from_millis(10));
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
