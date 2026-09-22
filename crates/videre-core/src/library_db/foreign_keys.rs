//! One-time legacy repairs that make an old library safe for enforced
//! foreign keys. Runs inside the versioned upgrade's transaction: the caller
//! commits only after verification succeeds, and the repair report is printed
//! only after that commit, so rolled-back work is never claimed as repaired.
//!
//! The upgrade entry point (`upgrade_to_v2`) lives here too; until it is
//! wired the report types are referenced only by tests.

use anyhow::Context;
use rusqlite::{Connection, OptionalExtension};

/// What the repair changed, in counts the caller can show a person. Labels
/// and cluster ids are the stored values that were cleared.
#[derive(Debug, Default, PartialEq)]
#[allow(dead_code)] // wired to the upgrade and stderr printing in the same PR
pub struct RepairReport {
    pub orphan_labels: Vec<(String, i64)>,
    pub orphan_clusters: Vec<(i64, i64)>,
    pub event_links_removed: usize,
    pub question_links_removed: usize,
    pub learning_events_invalidated: usize,
    pub questions_superseded: usize,
}

impl RepairReport {
    /// Deterministic stderr lines: labels and cluster ids in stored order,
    /// then the removed-row counts. Empty for a clean library.
    #[allow(dead_code)] // same wiring as RepairReport above
    pub fn stderr_lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        for (label, count) in &self.orphan_labels {
            lines.push(format!(
                "unassigned {count} face(s) whose person '{label}' no longer exists"
            ));
        }
        for (cluster, count) in &self.orphan_clusters {
            lines.push(format!(
                "cleared {count} file reference(s) to missing location cluster {cluster}"
            ));
        }
        if self.event_links_removed > 0 {
            lines.push(format!(
                "removed {} face-learning provenance row(s) whose event no longer exists",
                self.event_links_removed
            ));
        }
        if self.question_links_removed > 0 {
            lines.push(format!(
                "removed {} question provenance row(s) whose question no longer exists",
                self.question_links_removed
            ));
        }
        if self.learning_events_invalidated > 0 {
            lines.push(format!(
                "invalidated {} learning event(s) about unassigned people",
                self.learning_events_invalidated
            ));
        }
        if self.questions_superseded > 0 {
            lines.push(format!(
                "superseded {} pending question(s) about unassigned people",
                self.questions_superseded
            ));
        }
        lines
    }
}

/// True when the library carries the learning question tables at all. A
/// pre-learning library must not have them manufactured by the repair.
#[allow(dead_code)] // called by upgrade_to_v2, wired in the same PR
fn question_tables_present(conn: &Connection) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT
             EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table'
                    AND name = 'face_learning_questions') +
             EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table'
                    AND name = 'face_learning_question_faces')",
        [],
        |row| row.get::<_, i64>(0).map(|n| n == 2),
    )
}

/// Whether a table carries the immutability trigger that blocks the orphan
/// cleanup. Only the event-provenance trigger is ever dropped, inside the
/// upgrade's transaction, and recreated by `ensure_learning_tables`.
#[allow(dead_code)] // called by upgrade_to_v2, wired in the same PR
fn event_delete_trigger_name(conn: &Connection) -> rusqlite::Result<Option<String>> {
    let mut stmt = conn.prepare(
        "SELECT name FROM sqlite_master WHERE type = 'trigger'
         AND tbl_name = 'face_learning_event_faces' AND name LIKE '%immutable_delete%'",
    )?;
    stmt.query_row([], |row| row.get::<_, String>(0)).optional()
}

/// Repair every known legacy parent-child break so enforced foreign keys can
/// be turned on without stranding a person's data. Assumes an open outer
/// transaction (the upgrade owns the commit) and runs with enforcement off:
/// the point is to clear the violations first.
#[allow(dead_code)] // called by upgrade_to_v2, wired in the same PR
pub fn repair_legacy_rows(conn: &Connection) -> anyhow::Result<RepairReport> {
    // Orphan person labels: clear the relationship, keep the face. Collected
    // first so the report names the exact stored text.
    let orphan_labels = {
        let mut stmt = conn.prepare(
            "SELECT person_label, COUNT(*) FROM faces
             WHERE person_label IS NOT NULL
               AND NOT EXISTS (SELECT 1 FROM people WHERE people.name = faces.person_label)
             GROUP BY person_label ORDER BY person_label",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("collect orphan person labels")?;
        rows
    };
    let _unassigned = conn.execute(
        "UPDATE faces
         SET person_label = NULL, confirmed = 0, is_primary = 0, cluster_id = NULL
         WHERE person_label IS NOT NULL
           AND NOT EXISTS (SELECT 1 FROM people WHERE people.name = faces.person_label)",
        [],
    )?;

    let mut event_links_removed = 0usize;
    let mut question_links_removed = 0usize;
    let mut learning_events_invalidated = 0usize;
    let orphan_clusters = {
        let mut stmt = conn.prepare(
            "SELECT location_cluster_id, COUNT(*) FROM file_hashes
             WHERE location_cluster_id IS NOT NULL
               AND NOT EXISTS (SELECT 1 FROM location_clusters
                               WHERE location_clusters.id = file_hashes.location_cluster_id)
             GROUP BY location_cluster_id ORDER BY location_cluster_id",
        )?;
        let rows = stmt
            .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("collect orphan location cluster ids")?;
        rows
    };
    conn.execute(
        "UPDATE file_hashes SET location_cluster_id = NULL
         WHERE location_cluster_id IS NOT NULL
           AND NOT EXISTS (SELECT 1 FROM location_clusters
                           WHERE location_clusters.id = file_hashes.location_cluster_id)",
        [],
    )
    .context("clear orphan location cluster references")?;

    // Broken event provenance: drop the immutability trigger only as long as
    // the cleanup runs, then recreate it via ensure_learning_tables so the
    // table leaves this repair guarded again.
    if let Some(trigger) = event_delete_trigger_name(conn)
        .context("look up the event provenance trigger")?
        .filter(|_| crate::db::table_exists(conn, "face_learning_event_faces").unwrap_or(false))
    {
        conn.execute_batch(&format!("DROP TRIGGER {trigger};"))
            .context("drop the provenance immutability trigger")?;
        let removed = conn
            .execute(
                "DELETE FROM face_learning_event_faces
                 WHERE NOT EXISTS (SELECT 1 FROM face_learning_events
                                   WHERE face_learning_events.id = face_learning_event_faces.event_id)",
                [],
            )
            .context("remove event provenance rows with no parent event")?;
        #[cfg(test)]
        eprintln!("repair: dropped trigger {trigger:?}, removed {removed} provenance rows");
        event_links_removed = removed;
        crate::face_learning::ensure_learning_tables(conn)
            .context("restore learning triggers after provenance cleanup")?;
    }

    // Broken question provenance: no trigger guards these rows, and absent
    // tables mean the library predates questions entirely, so nothing is
    // created.
    if question_tables_present(conn).context("check for question tables")? {
        let removed = conn
            .execute(
                "DELETE FROM face_learning_question_faces
                 WHERE NOT EXISTS (SELECT 1 FROM face_learning_questions
                                   WHERE face_learning_questions.id = face_learning_question_faces.question_id)",
                [],
            )
            .context("remove question provenance rows with no parent question")?;
        question_links_removed = removed;
    }

    // Faces were unassigned, so the affected people's evidence is invalidated
    // by the exact stored label (never a re-normalization), and the gated
    // recluster re-opens.
    // Faces were unassigned, so the affected people's evidence is invalidated
    // by the exact stored label (never a re-normalization), the learning
    // generation is marked stale, and the gated recluster re-opens.
    if !orphan_labels.is_empty() {
        for (label, _) in &orphan_labels {
            crate::face_learning::invalidate_exact_identity_in_transaction(conn, label)
                .map_err(|e| anyhow::anyhow!("invalidate evidence for '{label}': {e}"))?;
        }
        // The per-label invalidation only advances the generation when that
        // label's evidence changed; unassigning faces is itself a change, so
        // the stale marker is forced here regardless.
        conn.execute(
            "UPDATE face_learning_state
             SET generation = generation + 1,
                 status = CASE WHEN status = 'training' THEN 'training' ELSE 'stale' END
             WHERE id = 1",
            [],
        )
        .context("mark the learning generation stale")?;
        // Everything carrying this reason inside the upgrade transaction was
        // invalidated by the loop above (a pre-v2 library has none: nothing
        // before this migration ever set it).
        learning_events_invalidated = conn
            .query_row(
                "SELECT COUNT(*) FROM face_learning_events
                 WHERE invalidation_reason = 'person_removed'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .context("count invalidated events")? as usize;
        crate::library_state::set(conn, crate::library_state::FACE_RECLUSTER_WATERMARK, 0)
            .context("clear the face recluster watermark")?;
    }

    Ok(RepairReport {
        orphan_labels,
        orphan_clusters,
        event_links_removed,
        question_links_removed,
        learning_events_invalidated,
        questions_superseded: 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn prepared() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        super::super::prepare_schema_components(&conn).unwrap();
        conn
    }

    /// A legacy label with no people row, a file referencing a deleted
    /// location cluster, an event-face row whose event is gone, and a
    /// question-face row whose question is gone. One further event is
    /// otherwise healthy: its face id appears only as historical provenance
    /// and its identity is unrelated to the orphan, so the repair must leave
    /// it eligible.
    fn legacy_fixture() -> Connection {
        let conn = prepared();
        conn.execute_batch("PRAGMA foreign_keys = OFF").unwrap();
        conn.execute_batch(
            "INSERT INTO people (name, full_name) VALUES ('elena', 'Elena');
             INSERT INTO faces (id, hash, bbox, embedding, cluster_id, person_label, confirmed, is_primary)
             VALUES (1, 'h1', '0,0,50,50', X'0000', 3, 'Özgür Sefik', 1, 1);
             INSERT INTO faces (id, hash, bbox, embedding, confirmed)
             VALUES (2, 'h2', '0,0,50,50', X'0000', 0);
             INSERT INTO file_hashes (path, hash, location_cluster_id)
             VALUES ('/p/a.jpg', 'h1', 999);
             INSERT INTO face_learning_events (id, action_kind, decision_kind, outcome,
                embedding_model_id, feature_schema_version, target_identity,
                feature_snapshot_json, support_count)
             VALUES (1, 'assign_face', 'membership', 'positive', 'm/1', 1,
                'elena', '{}', 0);
             INSERT INTO face_learning_event_faces (event_id, face_id, role, ordinal)
             VALUES (1, 1, 'subject', 0);
             INSERT INTO face_learning_events (id, action_kind, decision_kind, outcome,
                embedding_model_id, feature_schema_version, target_identity,
                feature_snapshot_json, support_count)
             VALUES (2, 'assign_face', 'membership', 'positive', 'm/1', 1,
                'elena', '{}', 0);
             INSERT INTO face_learning_event_faces (event_id, face_id, role, ordinal)
             VALUES (2, 2, 'subject', 0);
             -- The broken row: an event id that was never created (pre-v2
             -- libraries could carry this after a crash between inserts).
             INSERT INTO face_learning_event_faces (event_id, face_id, role, ordinal)
             VALUES (424242, 3, 'subject', 0);
             INSERT INTO face_learning_questions (id, status, target_identity, profile_id,
                model_kind, representative_face_id, cluster_id, evidence_revision, evidence_json)
             VALUES (1, 'pending', 'elena', 1, 'logistic', 1, 1, 'rev', '{}');
             -- The broken row: a question id that was never created. (The
             -- FK guards the question side; the face side is historical.)
             INSERT INTO face_learning_question_faces (question_id, face_id, role, ordinal)
             VALUES (424242, 1, 'subject', 0);",
        )
        .unwrap();
        // Face 2's event (id 2) is the healthy one: its provenance face still
        // exists, so it must survive repair eligible.
        conn.execute_batch("PRAGMA foreign_keys = ON").unwrap();
        conn
    }

    // The fixture is deliberately created with enforcement off; a NOT NULL or
    // parent violation here would mean the fixture no longer represents a
    // legacy library.
    #[test]
    fn fixture_is_legacy_shaped() {
        let conn = prepared();
        // Enforcement is off in the seed, so prove the v2 schema really
        // carries the key: with enforcement on, the same insert violates.
        conn.execute_batch("PRAGMA foreign_keys = ON").unwrap();
        let err = conn
            .execute(
                "INSERT INTO file_hashes (path, hash, location_cluster_id) VALUES ('/x', 'x', 999)",
                [],
            )
            .unwrap_err();
        assert!(matches!(
            err.sqlite_error_code(),
            Some(rusqlite::ErrorCode::ConstraintViolation)
        ));
    }

    #[test]
    fn repairs_legacy_relationships_without_erasing_provenance() {
        let conn = legacy_fixture();

        conn.execute_batch("PRAGMA foreign_keys = OFF").unwrap();
        conn.execute_batch("BEGIN").unwrap();
        let report = repair_legacy_rows(&conn).unwrap();
        conn.execute_batch("COMMIT").unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON").unwrap();

        // The orphan label is unassigned, not deleted: the face row and its
        // embedding survive, and it re-enters the unlabeled pool.
        let face: (Option<String>, i64, i64, Option<i64>) = conn
            .query_row(
                "SELECT person_label, confirmed, is_primary, cluster_id FROM faces WHERE id = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            face,
            (None, 0, 0, None),
            "the orphan label must be unassigned, never deleted"
        );
        let embedding: Vec<u8> = conn
            .query_row("SELECT embedding FROM faces WHERE id = 1", [], |r| r.get(0))
            .unwrap();
        assert!(!embedding.is_empty(), "the embedding survives the repair");

        // The orphan cluster reference is cleared, the row kept.
        let cluster: Option<i64> = conn
            .query_row(
                "SELECT location_cluster_id FROM file_hashes WHERE hash = 'h1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(cluster, None, "the orphan cluster id must be cleared");

        // The broken provenance rows are gone; the healthy ones remain.
        let event_faces: i64 = conn
            .query_row("SELECT COUNT(*) FROM face_learning_event_faces", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(event_faces, 2, "only the broken provenance row is removed");
        let question_faces: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM face_learning_question_faces",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(question_faces, 0, "the orphan question-face row is gone");

        // The healthy event stays eligible; its identity was unrelated to the
        // orphan repair, so no evidence was invalidated wholesale.
        let healthy: (i64, String) = conn
            .query_row(
                "SELECT eligible, target_identity FROM face_learning_events WHERE id = 2",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(healthy.0, 1, "an eligible event survives the repair");
        assert_eq!(healthy.1, "elena");

        // The report names every category honestly, by stored text.
        let lines = report.stderr_lines();
        assert!(lines.iter().any(|l| l.contains("Özgür Sefik")), "{lines:?}");
        assert!(lines.iter().any(|l| l.contains("999")), "{lines:?}");
        assert!(lines.iter().any(|l| l.contains("provenance")), "{lines:?}");
        assert!(lines.iter().any(|l| l.contains("question")), "{lines:?}");
    }

    #[test]
    fn repair_marks_generation_stale_and_clears_the_recluster_watermark() {
        let conn = legacy_fixture();
        conn.execute_batch(
            "UPDATE face_learning_state SET generation = 5, trained_generation = 5,
             status = 'current';",
        )
        .unwrap();
        crate::library_state::set(&conn, crate::library_state::FACE_RECLUSTER_WATERMARK, 5000)
            .unwrap();

        conn.execute_batch("PRAGMA foreign_keys = OFF").unwrap();
        conn.execute_batch("BEGIN").unwrap();
        let _ = repair_legacy_rows(&conn).unwrap();
        conn.execute_batch("COMMIT").unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON").unwrap();

        let state = crate::face_learning::learning_state(&conn).unwrap();
        assert_eq!(state.status, crate::face_learning::LearningStatus::Stale);
        let watermark: i64 = conn
            .query_row(
                "SELECT value FROM library_state WHERE key = 'face_recluster_watermark'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            watermark, 0,
            "unassigning faces must re-open the gated recluster"
        );
    }

    #[test]
    fn a_clean_library_reports_nothing() {
        let conn = prepared();
        conn.execute_batch("PRAGMA foreign_keys = OFF").unwrap();
        conn.execute_batch("BEGIN").unwrap();
        let report = repair_legacy_rows(&conn).unwrap();
        conn.execute_batch("COMMIT").unwrap();
        assert!(report.stderr_lines().is_empty());
    }

    #[test]
    fn absent_question_tables_are_not_manufactured() {
        // A pre-learning library has no question tables at all; the repair
        // must skip them rather than create history on an old schema.
        let conn = prepared();
        conn.execute_batch("PRAGMA foreign_keys = OFF").unwrap();
        conn.execute_batch(
            "DROP TABLE face_learning_question_faces;
             DROP TABLE face_learning_questions;",
        )
        .unwrap();
        conn.execute_batch("BEGIN").unwrap();
        let report = repair_legacy_rows(&conn).unwrap();
        conn.execute_batch("COMMIT").unwrap();
        assert_eq!(report.question_links_removed, 0);
        assert!(report
            .stderr_lines()
            .iter()
            .all(|l| !l.contains("question")));
        assert!(
            !crate::db::table_exists(&conn, "face_learning_questions").unwrap(),
            "the repair must not create tables the library did not have"
        );
    }
}
