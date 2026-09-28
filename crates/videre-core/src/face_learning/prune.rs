use rusqlite::Connection;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PrunedLearning {
    pub events_invalidated: usize,
    pub questions_superseded: usize,
    pub profile_retired: bool,
}

/// Reconcile evidence after a caller has deleted orphan faces in its own
/// transaction. Provenance remains immutable; only eligibility and pending
/// question state change.
pub fn reconcile_missing_face_sources_in_transaction(
    conn: &Connection,
) -> anyhow::Result<PrunedLearning> {
    anyhow::ensure!(
        !conn.is_autocommit(),
        "face learning reconciliation requires a transaction"
    );
    let events_invalidated = conn.execute(
        "UPDATE face_learning_events
         SET eligible = 0, invalidation_reason = 'source_face_pruned'
         WHERE eligible = 1 AND EXISTS (
             SELECT 1 FROM face_learning_event_faces AS source
             WHERE source.event_id = face_learning_events.id
               AND NOT EXISTS (SELECT 1 FROM faces WHERE faces.id = source.face_id)
         )",
        [],
    )?;
    let mut questions_superseded = conn.execute(
        "UPDATE face_learning_questions
         SET status = 'superseded', decided_at = datetime('now')
         WHERE status = 'pending' AND (
             NOT EXISTS (SELECT 1 FROM faces WHERE faces.id = representative_face_id)
             OR EXISTS (
                 SELECT 1 FROM face_learning_question_faces AS source
                 WHERE source.question_id = face_learning_questions.id
                   AND NOT EXISTS (SELECT 1 FROM faces WHERE faces.id = source.face_id)
             )
         )",
        [],
    )?;
    let mut profile_retired = false;
    if events_invalidated > 0 {
        questions_superseded += conn.execute(
            "UPDATE face_learning_questions
             SET status = 'superseded', decided_at = datetime('now')
             WHERE status = 'pending'
               AND profile_id IN (SELECT id FROM face_learning_profiles WHERE status = 'active')",
            [],
        )?;
        profile_retired = conn.execute(
            "UPDATE face_learning_profiles SET status = 'retired' WHERE status = 'active'",
            [],
        )? > 0;
        let changed = conn.execute(
            "UPDATE face_learning_state
             SET generation = generation + 1, status = 'stale',
                 training_generation = NULL, feedback_needed = NULL, last_error = NULL
             WHERE id = 1",
            [],
        )?;
        anyhow::ensure!(changed == 1, "face learning state row is missing");
    }
    Ok(PrunedLearning {
        events_invalidated,
        questions_superseded,
        profile_retired,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::enable_foreign_keys(&conn).unwrap();
        crate::face_db::ensure_faces_schema(&conn).unwrap();
        super::super::ensure_learning_tables(&conn).unwrap();
        super::super::ensure_question_tables(&conn).unwrap();
        super::super::ensure_profile_table(&conn).unwrap();
        conn.execute_batch(
            "INSERT INTO faces(id,hash,bbox,embedding) VALUES
                 (1,'one','0,0,1,1',X'00'),(2,'two','0,0,1,1',X'00');",
        )
        .unwrap();
        let features = super::super::FeatureVector {
            schema_version: super::super::FEATURE_SCHEMA_VERSION,
            values: super::super::MEMBERSHIP_FEATURE_NAMES
                .iter()
                .map(|name| ((*name).to_owned(), 0.0))
                .collect(),
        }
        .to_canonical_json()
        .unwrap();
        for id in [1, 2] {
            conn.execute(
                "INSERT INTO face_learning_events
                 (id,action_kind,decision_kind,outcome,embedding_model_id,feature_schema_version,feature_snapshot_json,support_count)
                 VALUES(?1,'assign_face','membership','positive','test',1,?2,0)",
                rusqlite::params![id, features],
            ).unwrap();
        }
        conn.execute_batch(
            "INSERT INTO face_learning_event_faces(event_id,face_id,role,ordinal)
            VALUES(1,1,'subject',0),(2,2,'subject',0);",
        )
        .unwrap();
        conn
    }

    fn reconcile(conn: &Connection) -> PrunedLearning {
        conn.execute_batch("BEGIN IMMEDIATE").unwrap();
        let result = reconcile_missing_face_sources_in_transaction(conn).unwrap();
        conn.execute_batch("COMMIT").unwrap();
        result
    }

    #[test]
    fn missing_subject_or_support_invalidates_event_once() {
        let conn = setup();
        conn.execute_batch(
            "INSERT INTO face_learning_event_faces(event_id,face_id,role,ordinal)
            VALUES(1,3,'target_support',0);",
        )
        .unwrap();
        let before: String = conn
            .query_row(
                "SELECT feature_snapshot_json FROM face_learning_events WHERE id=1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let result = reconcile(&conn);
        assert_eq!(result.events_invalidated, 1);
        assert_eq!(
            conn.query_row(
                "SELECT invalidation_reason FROM face_learning_events WHERE id=1",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "source_face_pruned"
        );
        assert_eq!(
            conn.query_row(
                "SELECT eligible FROM face_learning_events WHERE id=2",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
        assert_eq!(
            conn.query_row(
                "SELECT feature_snapshot_json FROM face_learning_events WHERE id=1",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            before
        );
        assert_eq!(
            conn.query_row(
                "SELECT count(*) FROM face_learning_event_faces WHERE event_id=1",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            2
        );
        assert_eq!(
            super::super::eligible_events_for_training(&conn, "test", 1)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn preexisting_missing_source_is_reconciled() {
        let conn = setup();
        conn.execute_batch("DELETE FROM faces WHERE id=1").unwrap();
        assert_eq!(reconcile(&conn).events_invalidated, 1);
    }

    #[test]
    fn questions_and_profile_follow_invalidation() {
        let conn = setup();
        conn.execute_batch(
            "INSERT INTO face_learning_profiles(id,artifact_version,embedding_model_id,feature_schema_version,model_kind,parameters,training_evidence_json,validation_report_json,stage,status)
                 VALUES(5,1,'test',1,'logistic',X'00','{}','{}','suggestion','active');
             INSERT INTO face_learning_questions(id,status,target_identity,profile_id,model_kind,representative_face_id,cluster_id,evidence_revision,evidence_json)
                 VALUES(1,'pending','one',5,'logistic',1,1,'a','{}'),
                       (2,'pending','two',5,'logistic',2,1,'b','{}');
             DELETE FROM faces WHERE id=1;",
        ).unwrap();
        let result = reconcile(&conn);
        assert_eq!(result.events_invalidated, 1);
        assert_eq!(result.questions_superseded, 2);
        assert!(result.profile_retired);
        assert_eq!(
            conn.query_row(
                "SELECT status FROM face_learning_profiles WHERE id=5",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "retired"
        );
        assert_eq!(
            conn.query_row("SELECT generation FROM face_learning_state", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            conn.query_row(
                "SELECT count(*) FROM face_learning_questions WHERE status='pending'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
    }

    #[test]
    fn question_only_loss_keeps_unrelated_profile() {
        let conn = setup();
        conn.execute_batch(
            "INSERT INTO face_learning_profiles(id,artifact_version,embedding_model_id,feature_schema_version,model_kind,parameters,training_evidence_json,validation_report_json,stage,status)
                 VALUES(5,1,'test',1,'logistic',X'00','{}','{}','suggestion','active');
             INSERT INTO face_learning_questions(id,status,target_identity,profile_id,model_kind,representative_face_id,cluster_id,evidence_revision,evidence_json)
                 VALUES(1,'pending','one',5,'logistic',99,1,'a','{}');",
        ).unwrap();
        let result = reconcile(&conn);
        assert_eq!(result.events_invalidated, 0);
        assert_eq!(result.questions_superseded, 1);
        assert!(!result.profile_retired);
        assert_eq!(
            conn.query_row("SELECT generation FROM face_learning_state", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }

    #[test]
    fn idempotent_second_reconciliation() {
        let conn = setup();
        conn.execute_batch("DELETE FROM faces WHERE id=1").unwrap();
        assert_eq!(reconcile(&conn).events_invalidated, 1);
        let second = reconcile(&conn);
        assert_eq!(second.events_invalidated, 0);
        assert_eq!(second.questions_superseded, 0);
        assert!(!second.profile_retired);
    }
}
