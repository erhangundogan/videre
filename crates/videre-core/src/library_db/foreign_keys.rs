//! One-time legacy repairs that make an old library safe for enforced
//! foreign keys. Runs inside the versioned upgrade's transaction: the caller
//! commits only after verification succeeds, and the repair report is printed
//! only after that commit, so rolled-back work is never claimed as repaired.
//!

use anyhow::Context;
use rusqlite::{Connection, OptionalExtension};

/// What the repair changed, in counts the caller can show a person. Labels
/// and cluster ids are the stored values that were cleared.
#[derive(Debug, Default, PartialEq)]
pub struct RepairReport {
    pub orphan_labels: Vec<(String, i64)>,
    pub orphan_clusters: Vec<(i64, i64)>,
    pub orphan_event_ids: Vec<(i64, i64)>,
    pub orphan_question_ids: Vec<(i64, i64)>,
    pub event_links_removed: usize,
    pub question_links_removed: usize,
    pub learning_events_invalidated: usize,
    pub questions_superseded: usize,
}

impl RepairReport {
    /// Deterministic stderr lines: labels and cluster ids in stored order,
    /// then the removed-row counts. Empty for a clean library.
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
        for (event_id, count) in &self.orphan_event_ids {
            lines.push(format!(
                "removed {count} face-learning provenance row(s) whose event {event_id} no longer exists"
            ));
        }
        for (question_id, count) in &self.orphan_question_ids {
            lines.push(format!(
                "removed {count} question provenance row(s) whose question {question_id} no longer exists"
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

fn orphan_parent_counts(conn: &Connection, sql: &str) -> rusqlite::Result<Vec<(i64, i64)>> {
    let mut stmt = conn.prepare(sql)?;
    let rows: Vec<(i64, i64)> = stmt
        .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// True when the library carries the learning question tables at all. A
/// pre-learning library must not have them manufactured by the repair.
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

/// Whether the event-provenance immutability trigger blocks orphan cleanup.
/// Only this known trigger is dropped inside the upgrade transaction.
fn event_delete_trigger_name(conn: &Connection) -> rusqlite::Result<Option<String>> {
    let mut stmt = conn.prepare(
        "SELECT name FROM sqlite_master WHERE type = 'trigger'
         AND tbl_name = 'face_learning_event_faces'
         AND name = 'face_learning_event_faces_immutable_delete'",
    )?;
    stmt.query_row([], |row| row.get::<_, String>(0)).optional()
}

/// Repair every known legacy parent-child break so enforced foreign keys can
/// be turned on without stranding a person's data. Assumes an open outer
/// transaction (the upgrade owns the commit) and runs with enforcement off:
/// the point is to clear the violations first.
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
    let mut orphan_event_ids = Vec::new();
    let mut orphan_question_ids = Vec::new();
    let mut learning_events_invalidated = 0usize;
    let mut questions_superseded = 0usize;
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
    if crate::db::table_exists(conn, "face_learning_event_faces")
        .context("check for event provenance table")?
    {
        orphan_event_ids = orphan_parent_counts(
            conn,
            "SELECT event_id, COUNT(*) FROM face_learning_event_faces
             WHERE NOT EXISTS (SELECT 1 FROM face_learning_events
                               WHERE face_learning_events.id = face_learning_event_faces.event_id)
             GROUP BY event_id ORDER BY event_id",
        )
        .context("collect missing event ids")?;
        if let Some(trigger) =
            event_delete_trigger_name(conn).context("look up the event provenance trigger")?
        {
            conn.execute_batch(&format!("DROP TRIGGER {trigger};"))
                .context("drop the provenance immutability trigger")?;
        }
        let removed = conn
            .execute(
                "DELETE FROM face_learning_event_faces
                 WHERE NOT EXISTS (SELECT 1 FROM face_learning_events
                                   WHERE face_learning_events.id = face_learning_event_faces.event_id)",
                [],
            )
            .context("remove event provenance rows with no parent event")?;
        event_links_removed = removed;
        crate::face_learning::ensure_learning_tables(conn)
            .context("restore learning triggers after provenance cleanup")?;
    }

    // Broken question provenance: no trigger guards these rows, and absent
    // tables mean the library predates questions entirely, so nothing is
    // created.
    if question_tables_present(conn).context("check for question tables")? {
        orphan_question_ids = orphan_parent_counts(
            conn,
            "SELECT question_id, COUNT(*) FROM face_learning_question_faces
             WHERE NOT EXISTS (SELECT 1 FROM face_learning_questions
                               WHERE face_learning_questions.id = face_learning_question_faces.question_id)
             GROUP BY question_id ORDER BY question_id",
        )
        .context("collect missing question ids")?;
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
    // by the exact stored label (never a re-normalization), the learning
    // generation is marked stale, and the gated recluster re-opens.
    if !orphan_labels.is_empty() {
        let has_questions = crate::db::table_exists(conn, "face_learning_questions")
            .context("check for pending-question table")?;
        for (label, _) in &orphan_labels {
            learning_events_invalidated += conn.query_row(
                "SELECT COUNT(*) FROM face_learning_events
                 WHERE target_identity = ?1 AND eligible = 1",
                [label],
                |row| row.get::<_, i64>(0),
            )? as usize;
            if has_questions {
                questions_superseded += conn.query_row(
                    "SELECT COUNT(*) FROM face_learning_questions
                     WHERE target_identity = ?1 AND status = 'pending'",
                    [label],
                    |row| row.get::<_, i64>(0),
                )? as usize;
            }
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
        crate::library_state::set(conn, crate::library_state::FACE_RECLUSTER_WATERMARK, 0)
            .context("clear the face recluster watermark")?;
    }

    Ok(RepairReport {
        orphan_labels,
        orphan_clusters,
        orphan_event_ids,
        orphan_question_ids,
        event_links_removed,
        question_links_removed,
        learning_events_invalidated,
        questions_superseded,
    })
}

/// The saved non-auto schema objects (indexes, triggers) that point at a
/// table being rebuilt, restored after the new table takes the old name.
#[derive(Debug)]
struct SavedSchemaObject {
    kind: String,
    name: String,
    sql: String,
}

fn save_objects_on(conn: &Connection, table: &str) -> anyhow::Result<Vec<SavedSchemaObject>> {
    let mut stmt = conn.prepare(
        "SELECT type, name, sql FROM sqlite_master
         WHERE tbl_name = ?1 AND sql IS NOT NULL
           AND type IN ('index', 'trigger')
           AND name NOT LIKE 'sqlite_autoindex%'",
    )?;
    let rows = stmt
        .query_map([table], |row| {
            Ok(SavedSchemaObject {
                kind: row.get(0)?,
                name: row.get(1)?,
                sql: row.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

#[derive(Debug)]
struct LegacyColumn {
    name: String,
    declared_type: String,
    not_null: bool,
    default_value: Option<String>,
    primary_key: bool,
    hidden: i64,
}

fn legacy_columns(conn: &Connection, table: &str) -> anyhow::Result<Vec<LegacyColumn>> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_xinfo({table})"))?;
    let columns = stmt
        .query_map([], |row| {
            Ok(LegacyColumn {
                name: row.get(1)?,
                declared_type: row.get(2)?,
                not_null: row.get::<_, i64>(3)? != 0,
                default_value: row.get(4)?,
                primary_key: row.get::<_, i64>(5)? != 0,
                hidden: row.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(columns)
}

fn quoted_identifier(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn free_stage_name(conn: &Connection, table: &str) -> anyhow::Result<String> {
    for suffix in 0..=1000 {
        let candidate = if suffix == 0 {
            format!("{table}__v2_stage")
        } else {
            format!("{table}__v2_stage_{suffix}")
        };
        let occupied: i64 = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name = ?1)
                    OR EXISTS(SELECT 1 FROM sqlite_temp_master WHERE name = ?1)",
            [&candidate],
            |row| row.get(0),
        )?;
        if occupied == 0 {
            return Ok(candidate);
        }
    }
    anyhow::bail!("no free staging name for {table}")
}

/// Rebuild one table into its canonical version-2 shape inside the upgrade
/// transaction: stage a fresh table under a temporary name, copy every
/// column by explicit name, drop the old table, rename the stage into place,
/// then restore the saved indexes and triggers. No foreign key points to
/// these tables; dependent views and other triggers retain the original name.
fn rebuild_table(
    conn: &Connection,
    table: &str,
    canonical_ddl: fn(&str, bool) -> String,
    canonical_columns: &[(&str, &str)],
) -> anyhow::Result<()> {
    let saved = save_objects_on(conn, table)?;
    let columns = legacy_columns(conn, table)?;
    let stage = free_stage_name(conn, table)?;
    let mut ddl = canonical_ddl(&stage, false);
    let mut extra_declarations = Vec::new();
    for column in &columns {
        anyhow::ensure!(
            column.hidden == 0,
            "cannot rebuild {table} with generated column {}",
            column.name
        );
        if canonical_columns
            .iter()
            .any(|(name, _)| *name == column.name)
        {
            continue;
        }
        anyhow::ensure!(
            !column.primary_key,
            "unexpected primary-key column {} on {table}",
            column.name
        );
        anyhow::ensure!(
            column
                .declared_type
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, ' ' | '_' | '(' | ')' | ',')),
            "unsupported declared type for {table}.{}",
            column.name
        );
        let mut declaration = quoted_identifier(&column.name);
        if !column.declared_type.is_empty() {
            declaration.push(' ');
            declaration.push_str(&column.declared_type);
        }
        if column.not_null {
            declaration.push_str(" NOT NULL");
        }
        if let Some(default) = &column.default_value {
            declaration.push_str(" DEFAULT ");
            declaration.push_str(default);
        }
        extra_declarations.push(declaration);
    }
    if !extra_declarations.is_empty() {
        let constraint = ddl
            .find("FOREIGN KEY")
            .context("canonical FK declaration missing")?;
        ddl.insert_str(
            constraint,
            &format!("{},\n    ", extra_declarations.join(",\n    ")),
        );
    }
    conn.execute_batch(&ddl)
        .with_context(|| format!("create the v2 staging table for {table}"))?;
    let column_list = columns
        .iter()
        .map(|column| quoted_identifier(&column.name))
        .collect::<Vec<_>>()
        .join(", ");
    conn.execute_batch(&format!(
        "INSERT INTO {stage} ({column_list}) SELECT {column_list} FROM {table};"
    ))
    .with_context(|| format!("copy rows into the v2 {table}"))?;
    // Between DROP and RENAME, views and triggers that reference the old name
    // are temporarily unresolved. SQLite's normal ALTER validation rejects
    // that intermediate schema; legacy_alter_table permits the rename without
    // retargeting those dependencies to the stage. Restore the prior setting
    // even when the replacement fails (the outer transaction then rolls back).
    let prior_legacy_alter: i64 =
        conn.query_row("PRAGMA legacy_alter_table", [], |row| row.get(0))?;
    conn.pragma_update(None, "legacy_alter_table", "ON")?;
    let replace_result = conn.execute_batch(&format!(
        "DROP TABLE {table}; ALTER TABLE {stage} RENAME TO {table};"
    ));
    let restore_result = conn.pragma_update(None, "legacy_alter_table", prior_legacy_alter);
    replace_result.with_context(|| format!("replace the old {table} with its v2 table"))?;
    restore_result.context("restore SQLite ALTER TABLE behavior")?;
    for object in &saved {
        conn.execute_batch(&object.sql)
            .with_context(|| format!("restore {} {}", object.kind, object.name))?;
    }
    Ok(())
}

fn rebuild_file_hashes(conn: &Connection) -> anyhow::Result<()> {
    rebuild_table(
        conn,
        "file_hashes",
        crate::library_db::file_hashes_ddl_for,
        crate::library_db::FILE_HASHES_COLUMNS,
    )
}

fn rebuild_faces(conn: &Connection) -> anyhow::Result<()> {
    rebuild_table(
        conn,
        "faces",
        crate::face_db::faces_ddl_for,
        crate::library_db::FACES_COLUMNS,
    )
}

/// Upgrade a library database to schema version 2 in one transaction:
/// prepare every required table, repair the known legacy parent-child
/// breaks, rebuild `file_hashes` and `faces` into their canonical
/// foreign-keyed shapes, verify, then stamp version 2 and commit. Any
/// failure rolls the whole thing back and returns the error; the caller
/// prints the report only after this returns Ok.
pub fn upgrade_to_v2(conn: &Connection) -> anyhow::Result<RepairReport> {
    let tx = conn.unchecked_transaction()?;
    crate::library_db::prepare_schema_components(&tx)?;
    let report = repair_legacy_rows(&tx)?;
    rebuild_file_hashes(&tx)?;
    rebuild_faces(&tx)?;
    let violations: (String, i64, String, i64) = {
        let mut stmt = tx.prepare("PRAGMA foreign_key_check")?;
        let mut rows = stmt.query([])?;
        match rows.next()? {
            Some(row) => (row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?),
            None => Default::default(),
        }
    };
    if !violations.0.is_empty() {
        // The repair plus rebuild must have caught everything; a violation
        // here means the library carries a shape this migration does not
        // know, and committing it under enforced keys would be wrong.
        anyhow::bail!(
            "foreign-key violations remain after repair: {} rowid {} references {} (fk {}); the upgrade was rolled back",
            violations.0, violations.1, violations.2, violations.3
        );
    }
    crate::library_db::verify_foreign_keys(&tx)
        .context("verify declared foreign keys before stamping schema version 2")?;
    tx.pragma_update(None, "user_version", 2)?;
    tx.commit()?;
    Ok(report)
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
        assert!(
            lines.iter().any(|l| l.contains("face-learning provenance")
                && l.contains("event 424242")
                && l.contains("removed 1")),
            "{lines:?}"
        );
        assert!(
            lines.iter().any(|l| l.contains("question provenance")
                && l.contains("question 424242")
                && l.contains("removed 1")),
            "{lines:?}"
        );
    }

    #[test]
    fn report_counts_only_evidence_changed_by_this_repair() {
        let conn = legacy_fixture();
        conn.execute_batch(
            "INSERT INTO face_learning_events (id, action_kind, decision_kind, outcome,
                embedding_model_id, feature_schema_version, target_identity,
                feature_snapshot_json, support_count, eligible, invalidation_reason)
             VALUES (3, 'assign_face', 'membership', 'positive', 'm/1', 1,
                'someone_else', '{}', 0, 0, 'person_removed');
             INSERT INTO face_learning_events (id, action_kind, decision_kind, outcome,
                embedding_model_id, feature_schema_version, target_identity,
                feature_snapshot_json, support_count)
             VALUES (4, 'assign_face', 'membership', 'positive', 'm/1', 1,
                'Özgür Sefik', '{}', 0);
             INSERT INTO face_learning_questions (id, status, target_identity, profile_id,
                model_kind, representative_face_id, cluster_id, evidence_revision, evidence_json)
             VALUES (2, 'pending', 'Özgür Sefik', 1, 'logistic', 1, 1, 'rev', '{}');",
        )
        .unwrap();

        conn.execute_batch("PRAGMA foreign_keys = OFF; BEGIN")
            .unwrap();
        let report = repair_legacy_rows(&conn).unwrap();
        conn.execute_batch("COMMIT; PRAGMA foreign_keys = ON")
            .unwrap();

        assert_eq!(report.learning_events_invalidated, 1);
        assert_eq!(report.questions_superseded, 1);
        assert_eq!(
            conn.query_row(
                "SELECT status FROM face_learning_questions WHERE id = 2",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "superseded"
        );
    }

    #[test]
    fn repairs_orphan_event_links_even_when_immutability_trigger_is_missing() {
        let conn = legacy_fixture();
        conn.execute_batch(
            "PRAGMA foreign_keys = OFF;
             DROP TRIGGER face_learning_event_faces_immutable_delete;
             BEGIN;",
        )
        .unwrap();
        let report = repair_legacy_rows(&conn).unwrap();
        conn.execute_batch("COMMIT; PRAGMA foreign_keys = ON")
            .unwrap();

        assert_eq!(report.event_links_removed, 1);
        let violations: i64 = conn
            .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(violations, 0);
        let trigger: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'trigger'
                 AND name = 'face_learning_event_faces_immutable_delete'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(trigger, 1);
    }

    #[test]
    fn report_groups_broken_provenance_by_missing_parent_id() {
        let conn = legacy_fixture();
        conn.execute_batch(
            "PRAGMA foreign_keys = OFF;
             INSERT INTO face_learning_event_faces (event_id, face_id, role, ordinal)
             VALUES (424243, 5, 'subject', 0), (424243, 6, 'support', 0);
             INSERT INTO face_learning_question_faces (question_id, face_id, role, ordinal)
             VALUES (424243, 5, 'subject', 0), (424243, 6, 'support', 0);
             BEGIN;",
        )
        .unwrap();
        let report = repair_legacy_rows(&conn).unwrap();
        conn.execute_batch("COMMIT").unwrap();

        assert_eq!(report.orphan_event_ids, vec![(424242, 1), (424243, 2)]);
        assert_eq!(report.orphan_question_ids, vec![(424242, 1), (424243, 2)]);
        assert_eq!(report.event_links_removed, 3);
        assert_eq!(report.question_links_removed, 3);
        let lines = report.stderr_lines();
        assert!(lines
            .iter()
            .any(|line| line.contains("removed 2") && line.contains("event 424243")));
        assert!(lines
            .iter()
            .any(|line| line.contains("removed 2") && line.contains("question 424243")));
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

    #[test]
    fn orphan_label_repair_skips_absent_question_tables() {
        let conn = prepared();
        conn.execute_batch(
            "PRAGMA foreign_keys = OFF;
             DROP TABLE face_learning_question_faces;
             DROP TABLE face_learning_questions;
             INSERT INTO faces (hash, bbox, embedding, person_label, confirmed)
             VALUES ('orphan', '0,0,1,1', X'00', 'lost_label', 1);
             BEGIN;",
        )
        .unwrap();
        let report = repair_legacy_rows(&conn).unwrap();
        conn.execute_batch("COMMIT").unwrap();

        assert_eq!(report.orphan_labels, vec![("lost_label".to_owned(), 1)]);
        assert_eq!(report.questions_superseded, 0);
        assert!(!crate::db::table_exists(&conn, "face_learning_questions").unwrap());
    }
}

#[cfg(test)]
mod upgrade_tests {
    use super::*;
    use rusqlite::Connection;

    /// A version-1 library: old no-FK DDL for the two main tables, a custom
    /// index and a harmless custom trigger that a rebuild must preserve, one
    /// legacy orphan (an unlabeled person reference is absent here; the
    /// orphan-label path is covered by the repair tests), and a derived row
    /// whose hash is absent from `file_hashes` (historical provenance).
    fn legacy_v1_library() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "user_version", 1).unwrap();
        conn.execute_batch(&crate::library_db::legacy_v1_fixture_ddl())
            .unwrap();
        conn.execute_batch(
            "INSERT INTO file_hashes (path, hash, ext) VALUES ('/p/a.jpg', 'aaaa', 'jpg'),
                ('/p/b.jpg', 'bbbb', 'jpg');
             INSERT INTO faces (id, hash, bbox, embedding, cluster_id, person_label, confirmed)
             VALUES (1, 'aaaa', '0,0,50,50', X'0000', NULL, 'missing_person', 1);
             CREATE INDEX idx_custom_hash ON file_hashes(hash);
             CREATE TRIGGER trg_custom_faces AFTER INSERT ON faces
             BEGIN SELECT 1; END;",
        )
        .unwrap();
        conn
    }

    fn user_version(conn: &Connection) -> i64 {
        conn.query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap()
    }

    fn foreign_key_violations(conn: &Connection) -> i64 {
        conn.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |r| {
            r.get(0)
        })
        .unwrap()
    }

    #[test]
    fn upgrades_legacy_schema_atomically() {
        let conn = legacy_v1_library();
        let report = upgrade_to_v2(&conn).expect("the v1 upgrade must succeed");

        assert_eq!(user_version(&conn), 2);
        assert_eq!(foreign_key_violations(&conn), 0);

        // All four declared parent-child relationships exist.
        super::super::verify_foreign_keys(&conn).unwrap();

        // Rows and their columns survive the rebuild.
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM file_hashes", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 2);
        let oriented: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('faces') WHERE name = 'oriented'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(oriented, 1, "the faces rebuild carries the full column set");
        let label: Option<String> = conn
            .query_row("SELECT person_label FROM faces WHERE id = 1", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(label, None, "the orphan label is unassigned by the repair");

        // Custom schema objects survive the rebuild.
        let index: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index'
                 AND name = 'idx_custom_hash'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(index, 1, "a user index on a rebuilt table must survive");
        let trigger: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'trigger'
                 AND name = 'trg_custom_faces'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(trigger, 1, "a user trigger on a rebuilt table must survive");

        // Required learning tables exist after the upgrade, and the report
        // names the repaired orphan label.
        for table in [
            "face_learning_events",
            "face_learning_event_faces",
            "face_learning_questions",
            "face_learning_question_faces",
        ] {
            assert!(crate::db::table_exists(&conn, table).unwrap(), "{table}");
        }
        assert!(report
            .stderr_lines()
            .iter()
            .any(|l| l.contains("missing_person")));

        // Idempotent: a second upgrade on the same connection changes nothing.
        let before = stub_snapshot(&conn);
        upgrade_to_v2(&conn).expect("second upgrade must succeed");
        assert_eq!(stub_snapshot(&conn), before);
    }

    #[test]
    fn upgrade_preserves_views_of_rebuilt_tables() {
        let conn = legacy_v1_library();
        conn.execute_batch(
            "CREATE VIEW visible_faces AS SELECT id, person_label FROM faces;
             CREATE VIEW visible_files AS SELECT path, hash FROM file_hashes;
             CREATE TABLE custom_probe (id INTEGER);
             CREATE TABLE audit_log (file_count INTEGER);
             CREATE TRIGGER probe_files AFTER INSERT ON custom_probe
             BEGIN INSERT INTO audit_log (file_count) SELECT COUNT(*) FROM file_hashes; END;",
        )
        .unwrap();

        upgrade_to_v2(&conn).unwrap();
        let faces: i64 = conn
            .query_row("SELECT COUNT(*) FROM visible_faces", [], |r| r.get(0))
            .unwrap();
        let files: i64 = conn
            .query_row("SELECT COUNT(*) FROM visible_files", [], |r| r.get(0))
            .unwrap();
        assert_eq!(faces, 1);
        assert_eq!(files, 2);
        conn.execute("INSERT INTO custom_probe (id) VALUES (1)", [])
            .unwrap();
        let trigger_count: i64 = conn
            .query_row("SELECT file_count FROM audit_log", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            trigger_count, 2,
            "cross-table triggers keep the original table name"
        );
    }

    #[test]
    fn upgrade_preserves_extra_legacy_columns_and_values() {
        let conn = legacy_v1_library();
        conn.execute_batch(
            "ALTER TABLE file_hashes ADD COLUMN custom_rating INTEGER DEFAULT 0;
             UPDATE file_hashes SET custom_rating = 7 WHERE hash = 'aaaa';
             ALTER TABLE faces ADD COLUMN source_note TEXT;
             UPDATE faces SET source_note = 'old import' WHERE id = 1;",
        )
        .unwrap();

        upgrade_to_v2(&conn).unwrap();
        let rating: i64 = conn
            .query_row(
                "SELECT custom_rating FROM file_hashes WHERE hash = 'aaaa'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let note: String = conn
            .query_row("SELECT source_note FROM faces WHERE id = 1", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(rating, 7);
        assert_eq!(note, "old import");
    }

    #[test]
    fn upgrade_never_drops_a_preexisting_stage_named_table() {
        let conn = legacy_v1_library();
        conn.execute_batch(
            "CREATE TABLE faces__v2_stage (note TEXT NOT NULL);
             INSERT INTO faces__v2_stage (note) VALUES ('keep me');",
        )
        .unwrap();

        upgrade_to_v2(&conn).unwrap();
        let note: String = conn
            .query_row("SELECT note FROM faces__v2_stage", [], |r| r.get(0))
            .unwrap();
        assert_eq!(note, "keep me");
    }

    #[test]
    fn noncanonical_learning_key_rolls_back_before_version_stamp() {
        let conn = legacy_v1_library();
        crate::library_db::prepare_schema_components(&conn).unwrap();
        conn.execute_batch(
            "DROP TABLE face_learning_event_faces;
             CREATE TABLE face_learning_event_faces (
                 event_id INTEGER NOT NULL REFERENCES face_learning_events(id) ON DELETE CASCADE,
                 face_id INTEGER NOT NULL,
                 role TEXT NOT NULL,
                 ordinal INTEGER NOT NULL CHECK(ordinal >= 0),
                 PRIMARY KEY(event_id, role, ordinal),
                 UNIQUE(event_id, face_id)
             );",
        )
        .unwrap();

        assert!(upgrade_to_v2(&conn).is_err());
        assert_eq!(
            user_version(&conn),
            1,
            "an invalid key must not be stamped v2"
        );
    }

    fn stub_snapshot(conn: &Connection) -> String {
        let mut out = String::new();
        out.push_str(&format!("v{}\n", user_version(conn)));
        let mut stmt = conn
            .prepare("SELECT type, name FROM sqlite_master ORDER BY name")
            .unwrap();
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .unwrap();
        for row in rows {
            let (t, n) = row.unwrap();
            out.push_str(&format!("{t}:{n}\n"));
        }
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM file_hashes", [], |r| r.get(0))
            .unwrap();
        out.push_str(&format!("files {n}\n"));
        out
    }

    #[test]
    fn a_library_without_learning_tables_gains_them_and_upgrades() {
        let conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "user_version", 1).unwrap();
        conn.execute_batch(&crate::library_db::legacy_v1_fixture_ddl())
            .unwrap();
        conn.execute_batch(
            "INSERT INTO file_hashes (path, hash, ext) VALUES ('/p/a.jpg', 'aaaa', 'jpg');",
        )
        .unwrap();
        assert!(!crate::db::table_exists(&conn, "face_learning_events").unwrap());

        upgrade_to_v2(&conn).expect("upgrade with no learning tables");

        assert_eq!(user_version(&conn), 2);
        for table in [
            "face_learning_events",
            "face_learning_event_faces",
            "face_learning_questions",
            "face_learning_question_faces",
        ] {
            assert!(crate::db::table_exists(&conn, table).unwrap(), "{table}");
        }
    }

    #[test]
    fn user_version_zero_upgrades_the_same_way() {
        let conn = legacy_v1_library();
        conn.pragma_update(None, "user_version", 0).unwrap();
        upgrade_to_v2(&conn).expect("version 0 upgrade");
        assert_eq!(user_version(&conn), 2);
    }

    #[test]
    fn a_failed_upgrade_rolls_back_schema_rows_and_report() {
        let conn = legacy_v1_library();
        // Abort the orphan-label repair's UPDATE: the whole upgrade must roll
        // back, leaving the v1 schema, rows, and custom objects intact.
        conn.execute_batch(
            "CREATE TRIGGER abort_orphan_repair BEFORE UPDATE ON faces
             WHEN OLD.person_label IS NOT NULL
             BEGIN SELECT RAISE(ABORT, 'injected repair failure'); END;",
        )
        .unwrap();

        let result = upgrade_to_v2(&conn);
        assert!(
            result.is_err(),
            "the injected failure must fail the upgrade"
        );

        assert_eq!(user_version(&conn), 1, "the version must not advance");
        let ddl: String = conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'file_hashes'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            !ddl.contains("FOREIGN KEY"),
            "the old no-FK DDL must remain after a rollback: {ddl}"
        );
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM file_hashes", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 2, "rows survive the rollback");
        // No report may be emitted for rolled-back work: the caller only sees
        // an Err, and stderr_lines belong to a returned report. Assert via the
        // returned type that nothing was produced.
        assert!(result.err().unwrap().to_string().contains("injected"));
    }
}
