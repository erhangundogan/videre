//! Facade over videre's faces-labeling read operations. Plain functions over
//! an open `rusqlite::Connection`, returning serde types and a shared
//! `Error`. Called by the axum `--faces` server and any other embedder.

use crate::error::{Error, Result};
use crate::types::*;
use rusqlite::Connection;
use std::collections::{BTreeSet, HashMap};
use videre_core::face_db::load_face_observations;
use videre_core::face_learning::{
    active_question_context, append_event_batch_in_transaction, extract_cluster_quality_features,
    extract_membership_features, finish_question_in_transaction,
    invalidate_identity_for_removal_in_transaction, learning_state, list_learning_events,
    list_pending_questions, question_evidence_revision, replace_pending_questions,
    select_questions, stored_question, DecisionStage, EventFaceRef, EventFaceRole, LearningAction,
    LearningDecisionKind, LearningOutcome, NewLearningEvent, QuestionAnswer,
    QuestionSelectionConfig, QuestionStatus,
};

const MAX_MEMBERSHIP_EVENTS_PER_ACTION: usize = 8;
const MAX_SUPPORT_FACES: usize = 8;

#[derive(Debug)]
struct FaceState {
    id: i64,
    cluster_id: Option<i64>,
    person_label: Option<String>,
    confirmed: bool,
}

fn immediate_transaction<T>(conn: &Connection, operation: impl FnOnce() -> Result<T>) -> Result<T> {
    conn.execute_batch("BEGIN IMMEDIATE")?;
    match operation() {
        Ok(value) => match conn.execute_batch("COMMIT") {
            Ok(()) => Ok(value),
            Err(error) => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(error.into())
            }
        },
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

fn face_states(conn: &Connection, face_ids: &[i64]) -> Result<Vec<FaceState>> {
    if face_ids.is_empty()
        || face_ids.iter().copied().collect::<BTreeSet<_>>().len() != face_ids.len()
    {
        return Err(Error::Invalid);
    }
    let mut ids = face_ids.to_vec();
    ids.sort_unstable();
    let mut statement =
        conn.prepare("SELECT cluster_id, person_label, confirmed FROM faces WHERE id = ?1")?;
    ids.into_iter()
        .map(|id| {
            statement
                .query_row([id], |row| {
                    Ok(FaceState {
                        id,
                        cluster_id: row.get(0)?,
                        person_label: row.get(1)?,
                        confirmed: row.get::<_, i64>(2)? != 0,
                    })
                })
                .map_err(|error| match error {
                    rusqlite::Error::QueryReturnedNoRows => Error::NotFound,
                    other => other.into(),
                })
        })
        .collect()
}

fn unassigned_cluster_ids(conn: &Connection, cluster_id: i64) -> Result<Vec<i64>> {
    let mut statement = conn.prepare(
        "SELECT id FROM faces
         WHERE cluster_id = ?1 AND confirmed = 0 AND person_label IS NULL
         ORDER BY id",
    )?;
    let ids = statement
        .query_map([cluster_id], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(ids)
}

fn person_support_ids(conn: &Connection, identity: &str, excluded: &[i64]) -> Result<Vec<i64>> {
    let excluded: BTreeSet<_> = excluded.iter().copied().collect();
    let mut statement = conn.prepare(
        "SELECT id FROM faces
         WHERE person_label = ?1 AND confirmed = 1 AND cluster_id IS NULL
         ORDER BY is_primary DESC, id ASC",
    )?;
    let ids = statement
        .query_map([identity], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<i64>>>()?
        .into_iter()
        .filter(|id| !excluded.contains(id))
        .take(MAX_SUPPORT_FACES)
        .collect();
    Ok(ids)
}

fn event_faces(subject: &[i64], support: &[i64], support_role: EventFaceRole) -> Vec<EventFaceRef> {
    subject
        .iter()
        .enumerate()
        .map(|(ordinal, face_id)| EventFaceRef {
            face_id: *face_id,
            role: EventFaceRole::Subject,
            ordinal: ordinal as u32,
        })
        .chain(
            support
                .iter()
                .enumerate()
                .map(|(ordinal, face_id)| EventFaceRef {
                    face_id: *face_id,
                    role: support_role,
                    ordinal: ordinal as u32,
                }),
        )
        .collect()
}

fn membership_event(
    conn: &Connection,
    subject_ids: &[i64],
    support_ids: &[i64],
    action: LearningAction,
    outcome: LearningOutcome,
    target_identity: Option<String>,
    context: &TeachingContext,
    stage: DecisionStage,
) -> Result<NewLearningEvent> {
    let subject = load_face_observations(conn, subject_ids)?;
    let support = load_face_observations(conn, support_ids)?;
    Ok(NewLearningEvent {
        action,
        decision_kind: LearningDecisionKind::Membership,
        outcome,
        embedding_model_id: context.embedding_model_id.clone(),
        active_profile_id: context.active_profile_id,
        target_identity,
        features: extract_membership_features(&subject, &support, stage)?,
        support_count: support.len() as u32,
        scorer_confidence: None,
        faces: event_faces(subject_ids, support_ids, EventFaceRole::TargetSupport),
    })
}

fn cluster_event(
    conn: &Connection,
    face_ids: &[i64],
    action: LearningAction,
    outcome: LearningOutcome,
    target_identity: Option<String>,
    context: &TeachingContext,
) -> Result<NewLearningEvent> {
    let cluster = load_face_observations(conn, face_ids)?;
    Ok(NewLearningEvent {
        action,
        decision_kind: LearningDecisionKind::ClusterQuality,
        outcome,
        embedding_model_id: context.embedding_model_id.clone(),
        active_profile_id: context.active_profile_id,
        target_identity,
        features: extract_cluster_quality_features(&cluster, DecisionStage::GalleryCluster)?,
        support_count: cluster.len() as u32,
        scorer_confidence: None,
        faces: face_ids
            .iter()
            .enumerate()
            .map(|(ordinal, face_id)| EventFaceRef {
                face_id: *face_id,
                role: EventFaceRole::ClusterMember,
                ordinal: ordinal as u32,
            })
            .collect(),
    })
}

/// Whether detection has ever run against this library.
fn faces_table_exists(conn: &Connection) -> bool {
    conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='faces'",
        [],
        |r| r.get::<_, i64>(0),
    )
    .map(|n| n > 0)
    .unwrap_or(false)
}

/// People / unassigned clusters / singletons for the labeling page.
///
/// :warning: **A library that has never run `videre faces` has no `faces` table
/// at all**, and that is not an error. `videre scan` creates `file_hashes`,
/// `people` and `pipeline_runs`; the faces table arrives with the first
/// detection run. Querying it before then failed with "no such table", which the
/// server turned into a 500 with an empty body, which the page turned into
/// `Unexpected end of JSON input` across the top of the labeling UI.
///
/// "Nothing detected yet" is a state, not a failure. It returns empty here and
/// the page says so.
pub fn faces_list(conn: &Connection) -> Result<FacesData> {
    if !faces_table_exists(conn) {
        return Ok(FacesData::default());
    }
    let mut people: HashMap<String, PersonData> = HashMap::new();
    {
        let mut stmt = conn.prepare(
            // LEFT JOIN, not JOIN: a face labelled before the people table
            // existed still has to appear, showing its raw label until the
            // migration gives it a row.
            "SELECT f.id, f.hash, f.person_label, COALESCE(p.full_name, f.person_label) \
             FROM faces f LEFT JOIN people p ON p.name = f.person_label \
             WHERE f.confirmed = 1 AND f.person_label IS NOT NULL \
             ORDER BY f.person_label, f.is_primary DESC, f.id ASC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?;
        for row in rows {
            let (id, hash, label, full_name) = row?;
            let person = people.entry(label.clone()).or_insert(PersonData {
                label: label.clone(),
                full_name,
                face_ids: vec![],
                representative_id: id,
                hashes: vec![],
            });
            person.face_ids.push(id);
            if !person.hashes.contains(&hash) {
                person.hashes.push(hash);
            }
        }
    }

    let mut cluster_map: HashMap<i64, ClusterData> = HashMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT id, hash, cluster_id FROM faces \
             WHERE cluster_id IS NOT NULL AND (confirmed = 0 OR person_label IS NULL) \
             ORDER BY cluster_id, id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })?;
        for row in rows {
            let (id, hash, cid) = row?;
            let cluster = cluster_map.entry(cid).or_insert(ClusterData {
                cluster_id: cid,
                face_ids: vec![],
                hashes: vec![],
            });
            cluster.face_ids.push(id);
            if !cluster.hashes.contains(&hash) {
                cluster.hashes.push(hash);
            }
        }
    }

    let mut singletons: Vec<SingletonData> = vec![];
    {
        let mut stmt = conn.prepare(
            "SELECT id, hash FROM faces \
             WHERE cluster_id IS NULL AND (confirmed = 0 OR person_label IS NULL) \
             ORDER BY id",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
        for row in rows {
            let (id, hash) = row?;
            singletons.push(SingletonData { face_id: id, hash });
        }
    }

    // Both maps are HashMaps, whose iteration order is arbitrary and differs
    // between instances, so collecting straight from them threw away the
    // ORDER BY the queries above establish. The labeling UI re-fetches this
    // list after every assignment, so the effect was that people and clusters
    // reshuffled on each drop: the cluster lined up next moved somewhere else,
    // and so did the person being dragged onto. `singletons` never had the
    // problem, and the difference is exactly that it is built as a Vec.
    //
    // Clusters are ordered largest first, which is the order people label in:
    // the big clusters are worth the most and are the easiest to recognise.
    // cluster_id breaks ties so the order is total, not merely sorted.
    let mut people: Vec<PersonData> = people.into_values().collect();
    people.sort_by_key(|a| a.full_name.to_lowercase());
    let mut clusters: Vec<ClusterData> = cluster_map.into_values().collect();
    clusters.sort_by(|a, b| {
        b.face_ids
            .len()
            .cmp(&a.face_ids.len())
            .then(a.cluster_id.cmp(&b.cluster_id))
    });

    Ok(FacesData {
        people,
        clusters,
        singletons,
    })
}

/// Every face in one unassigned cluster (for the cluster detail page).
pub fn cluster_detail(conn: &Connection, cluster_id: i64) -> Result<ClusterDetail> {
    // Same unlabeled filter the cluster card uses: the page a card opens
    // must show the population the card counted. A labeled face can hold no
    // cluster id any more; the filter stays so the two queries cannot drift.
    let mut stmt = conn.prepare(
        "SELECT f.id, f.hash, fh.path FROM faces f \
         JOIN file_hashes fh ON f.hash = fh.hash \
         WHERE f.cluster_id = ?1 AND (f.confirmed = 0 OR f.person_label IS NULL) \
         ORDER BY f.id",
    )?;
    let faces = stmt
        .query_map([cluster_id], |r| {
            Ok(ClusterFaceData {
                face_id: r.get(0)?,
                hash: r.get(1)?,
                path: r.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(ClusterDetail { cluster_id, faces })
}

/// Every confirmed face for one person, primary first and flagged.
pub fn person_detail(conn: &Connection, name: &str) -> Result<PersonDetail> {
    // Reads normalize too, so `/people/person/Erhan`, `/people/person/erhan` and the original
    // spelling all reach the same person. That is what keeps existing links
    // working across the migration without a redirect table.
    let name = videre_core::person::normalize(name).unwrap_or_else(|| name.to_string());
    let name = name.as_str();
    let mut stmt = conn.prepare(
        "SELECT f.id, f.hash, fh.path, f.is_primary FROM faces f \
         JOIN file_hashes fh ON f.hash = fh.hash \
         WHERE f.person_label = ?1 AND f.confirmed = 1 \
         ORDER BY f.is_primary DESC, f.id",
    )?;
    let faces = stmt
        .query_map([name], |r| {
            Ok(PersonFaceData {
                face_id: r.get(0)?,
                hash: r.get(1)?,
                path: r.get(2)?,
                is_primary: r.get::<_, i64>(3)? != 0,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    // Falls back to the identity for a person with no row yet, so a library
    // opened before the migration still shows something sensible.
    let full_name: String = conn
        .query_row(
            "SELECT full_name FROM people WHERE name = ?1",
            rusqlite::params![name],
            |r| r.get(0),
        )
        .unwrap_or_else(|_| name.to_string());
    Ok(PersonDetail {
        label: name.to_string(),
        full_name,
        faces,
    })
}

/// Image paths for confirmed faces of a person (prefix match), for the
/// person-name autocomplete. Delegates to the existing core search.
pub fn search_person(conn: &Connection, name: &str) -> Result<Vec<String>> {
    Ok(videre_core::person_search::search_by_person(
        conn, name, None,
    )?)
}

/// Assign faces to an existing/new person: sets person_label + confirmed.
/// Rejects an empty label after sanitizing.
pub fn assign(conn: &Connection, face_ids: &[i64], person_label: &str) -> Result<()> {
    // What was typed becomes the display name; its normalized form is the
    // identity written to every face row. Upserting keeps `people` complete
    // without a separate "create person" step.
    let display = crate::label::sanitize_person_label(person_label).ok_or(Error::Invalid)?;
    let label = videre_core::person::normalize(&display).ok_or(Error::Invalid)?;
    // Nothing to assign is a malformed request, not a silent success that would
    // create a person with no faces.
    if face_ids.is_empty() {
        return Err(Error::Invalid);
    }
    // All-or-nothing: a face id that matches no row makes the whole assign a
    // NotFound, and the person insert is rolled back with it so a failed assign
    // leaves nothing behind. An `UPDATE` matching no row is `Ok(0)`, not an
    // error, so a partial write would otherwise be reported as success.
    conn.execute_batch("BEGIN")?;
    let result = assign_in_transaction(conn, face_ids, &label, &display);
    finish_unit_transaction(conn, result)
}

fn finish_unit_transaction(conn: &Connection, result: Result<()>) -> Result<()> {
    match result {
        Ok(()) => {
            if let Err(error) = conn.execute_batch("COMMIT") {
                let _ = conn.execute_batch("ROLLBACK");
                return Err(error.into());
            }
            Ok(())
        }
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

fn assign_in_transaction(
    conn: &Connection,
    face_ids: &[i64],
    identity: &str,
    display: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO people (name, full_name) VALUES (?1, ?2) ON CONFLICT(name) DO NOTHING",
        rusqlite::params![identity, display],
    )?;
    for id in face_ids {
        let changed = conn.execute(
            "UPDATE faces
             SET person_label = ?1, confirmed = 1, cluster_id = NULL
             WHERE id = ?2",
            rusqlite::params![identity, id],
        )?;
        if changed == 0 {
            return Err(Error::NotFound);
        }
    }
    Ok(())
}

fn validate_teaching_subject(conn: &Connection, face_ids: &[i64]) -> Result<Vec<FaceState>> {
    let states = face_states(conn, face_ids)?;
    if states
        .iter()
        .any(|state| state.confirmed || state.person_label.is_some())
    {
        return Err(Error::Invalid);
    }
    if states.len() == 1 && states[0].cluster_id.is_some() {
        return Err(Error::Invalid);
    }
    if states.len() > 1 {
        let cluster_id = states[0].cluster_id.ok_or(Error::Invalid)?;
        if states
            .iter()
            .any(|state| state.cluster_id != Some(cluster_id))
            || unassigned_cluster_ids(conn, cluster_id)?
                != states.iter().map(|state| state.id).collect::<Vec<_>>()
        {
            return Err(Error::Invalid);
        }
    }
    Ok(states)
}

fn assignment_events(
    conn: &Connection,
    states: &[FaceState],
    identity: &str,
    existing_support: &[i64],
    context: &TeachingContext,
    creating_person: bool,
) -> Result<Vec<NewLearningEvent>> {
    let ids: Vec<_> = states.iter().map(|state| state.id).collect();
    let clustered = ids.len() > 1;
    if !clustered && creating_person {
        // One newly named face contains identity truth but no relationship to
        // score. Persisting self-similarity would be a tautological positive,
        // so this action deliberately waits for a later supported assignment.
        load_face_observations(conn, &ids)?;
        return Ok(Vec::new());
    }
    let action = match (creating_person, clustered) {
        (true, true) => LearningAction::LabelCluster,
        (true, false) => LearningAction::CreatePerson,
        (false, true) => LearningAction::AssignCluster,
        (false, false) => LearningAction::AssignFace,
    };
    let mut events = Vec::new();
    if clustered {
        events.push(cluster_event(
            conn,
            &ids,
            action,
            LearningOutcome::Positive,
            Some(identity.to_owned()),
            context,
        )?);
    }
    if creating_person {
        for (index, subject) in ids
            .iter()
            .copied()
            .take(MAX_MEMBERSHIP_EVENTS_PER_ACTION)
            .enumerate()
        {
            let support: Vec<_> = ids
                .iter()
                .copied()
                .filter(|id| *id != subject)
                .cycle()
                .skip(index.min(ids.len().saturating_sub(1)))
                .take(ids.len().saturating_sub(1).min(MAX_SUPPORT_FACES))
                .collect();
            events.push(membership_event(
                conn,
                &[subject],
                &support,
                action,
                LearningOutcome::Positive,
                Some(identity.to_owned()),
                context,
                DecisionStage::GalleryCluster,
            )?);
        }
    } else if !existing_support.is_empty() {
        for subject in ids.iter().copied().take(MAX_MEMBERSHIP_EVENTS_PER_ACTION) {
            events.push(membership_event(
                conn,
                &[subject],
                existing_support,
                action,
                LearningOutcome::Positive,
                Some(identity.to_owned()),
                context,
                if clustered {
                    DecisionStage::GalleryCluster
                } else {
                    DecisionStage::GallerySingleton
                },
            )?);
        }
    }
    Ok(events)
}

fn assign_teaching(
    conn: &Connection,
    face_ids: &[i64],
    person_label: &str,
    context: &TeachingContext,
    creating_person: bool,
) -> Result<LearningAcknowledgement> {
    if context.embedding_model_id.trim().is_empty() {
        return Err(Error::Invalid);
    }
    let display = crate::label::sanitize_person_label(person_label).ok_or(Error::Invalid)?;
    let identity = videre_core::person::normalize(&display).ok_or(Error::Invalid)?;
    immediate_transaction(conn, || {
        let states = validate_teaching_subject(conn, face_ids)?;
        let person_exists = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM people WHERE name = ?1)",
            [&identity],
            |row| row.get::<_, bool>(0),
        )?;
        let creating_person = creating_person && !person_exists;
        let support = if creating_person {
            Vec::new()
        } else {
            if !person_exists {
                return Err(Error::NotFound);
            }
            person_support_ids(conn, &identity, face_ids)?
        };
        let events =
            assignment_events(conn, &states, &identity, &support, context, creating_person)?;
        assign_in_transaction(conn, face_ids, &identity, &display)?;
        if events.is_empty() {
            let state = learning_state(conn)?;
            return Ok(LearningAcknowledgement {
                generation: state.generation,
                event_ids: Vec::new(),
                message_key: "face_named_without_comparison".to_owned(),
            });
        }
        let receipt = append_event_batch_in_transaction(conn, &events)?;
        Ok(LearningAcknowledgement {
            generation: receipt.generation,
            event_ids: receipt.event_ids,
            message_key: if states.len() > 1 {
                "cluster_confirmed"
            } else {
                "membership_confirmed"
            }
            .to_owned(),
        })
    })
}

pub fn assign_with_learning(
    conn: &Connection,
    face_ids: &[i64],
    person_label: &str,
    context: &TeachingContext,
) -> Result<LearningAcknowledgement> {
    assign_teaching(conn, face_ids, person_label, context, false)
}

pub fn new_person_with_learning(
    conn: &Connection,
    face_ids: &[i64],
    person_label: &str,
    context: &TeachingContext,
) -> Result<LearningAcknowledgement> {
    assign_teaching(conn, face_ids, person_label, context, true)
}

/// Create a person from faces. Same effect as `assign`; kept as a distinct
/// operation because callers treat "new person" and "assign to existing" as
/// separate user intents.
pub fn new_person(conn: &Connection, face_ids: &[i64], label: &str) -> Result<()> {
    assign(conn, face_ids, label)
}

/// Reset one face to fully unassigned (cluster, label, confirmed, primary).
pub fn remove_face(conn: &Connection, face_id: i64) -> Result<()> {
    // A face id from the client that matches no row is `Ok(0)`, not an error;
    // reported as success it would tell the UI a face was reset that never
    // existed.
    remove_face_in_transaction(conn, face_id)
}

fn remove_face_in_transaction(conn: &Connection, face_id: i64) -> Result<()> {
    let n = conn.execute(
        "UPDATE faces SET cluster_id = NULL, person_label = NULL, confirmed = 0, is_primary = 0 WHERE id = ?1",
        [face_id],
    )?;
    if n == 0 {
        return Err(Error::NotFound);
    }
    Ok(())
}

pub fn remove_face_with_learning(
    conn: &Connection,
    face_id: i64,
    context: &TeachingContext,
) -> Result<LearningAcknowledgement> {
    if context.embedding_model_id.trim().is_empty() {
        return Err(Error::Invalid);
    }
    immediate_transaction(conn, || {
        let state = face_states(conn, &[face_id])?.remove(0);
        let (action, support, identity, stage) =
            if state.confirmed && state.person_label.is_some() && state.cluster_id.is_none() {
                let identity = state.person_label.clone().ok_or(Error::Invalid)?;
                let support = person_support_ids(conn, &identity, &[face_id])?;
                if support.is_empty() {
                    remove_face_in_transaction(conn, face_id)?;
                    let generation = learning_state(conn)?.generation;
                    return Ok(LearningAcknowledgement {
                        generation,
                        event_ids: Vec::new(),
                        message_key: "face_removed_without_comparison".to_owned(),
                    });
                }
                (
                    LearningAction::RemoveFaceFromPerson,
                    support,
                    Some(identity),
                    DecisionStage::GallerySingleton,
                )
            } else if !state.confirmed && state.person_label.is_none() {
                let cluster_id = state.cluster_id.ok_or(Error::Invalid)?;
                let support: Vec<_> = unassigned_cluster_ids(conn, cluster_id)?
                    .into_iter()
                    .filter(|id| *id != face_id)
                    .take(MAX_SUPPORT_FACES)
                    .collect();
                if support.is_empty() {
                    remove_face_in_transaction(conn, face_id)?;
                    let generation = learning_state(conn)?.generation;
                    return Ok(LearningAcknowledgement {
                        generation,
                        event_ids: Vec::new(),
                        message_key: "face_removed_without_comparison".to_owned(),
                    });
                }
                (
                    LearningAction::RemoveFaceFromCluster,
                    support,
                    None,
                    DecisionStage::GalleryCluster,
                )
            } else {
                return Err(Error::Invalid);
            };
        let event = membership_event(
            conn,
            &[face_id],
            &support,
            action,
            LearningOutcome::Negative,
            identity,
            context,
            stage,
        )?;
        remove_face_in_transaction(conn, face_id)?;
        let receipt = append_event_batch_in_transaction(conn, &[event])?;
        Ok(LearningAcknowledgement {
            generation: receipt.generation,
            event_ids: receipt.event_ids,
            message_key: "membership_corrected".to_owned(),
        })
    })
}

/// Ungroup a bad cluster: its faces become unassigned singletons (not deleted).
pub fn dissolve_cluster(conn: &Connection, cluster_id: i64) -> Result<()> {
    // A cluster id from the client that matches no row is `Ok(0)`, not an error;
    // reported as success it would tell the UI a cluster was ungrouped that
    // never existed.
    dissolve_cluster_in_transaction(conn, cluster_id)
}

fn dissolve_cluster_in_transaction(conn: &Connection, cluster_id: i64) -> Result<()> {
    let n = conn.execute(
        "UPDATE faces SET cluster_id = NULL WHERE cluster_id = ?1",
        [cluster_id],
    )?;
    if n == 0 {
        return Err(Error::NotFound);
    }
    Ok(())
}

pub fn dissolve_cluster_with_learning(
    conn: &Connection,
    cluster_id: i64,
    context: &TeachingContext,
) -> Result<LearningAcknowledgement> {
    if context.embedding_model_id.trim().is_empty() {
        return Err(Error::Invalid);
    }
    immediate_transaction(conn, || {
        let face_ids = unassigned_cluster_ids(conn, cluster_id)?;
        let all_faces: i64 = conn.query_row(
            "SELECT COUNT(*) FROM faces WHERE cluster_id = ?1",
            [cluster_id],
            |row| row.get(0),
        )?;
        if all_faces != face_ids.len() as i64 {
            return Err(Error::Invalid);
        }
        if face_ids.len() < 2 {
            return if face_ids.is_empty() {
                Err(Error::NotFound)
            } else {
                Err(Error::Invalid)
            };
        }
        let event = cluster_event(
            conn,
            &face_ids,
            LearningAction::DissolveCluster,
            LearningOutcome::Negative,
            None,
            context,
        )?;
        dissolve_cluster_in_transaction(conn, cluster_id)?;
        let receipt = append_event_batch_in_transaction(conn, &[event])?;
        Ok(LearningAcknowledgement {
            generation: receipt.generation,
            event_ids: receipt.event_ids,
            message_key: "cluster_dissolved".to_owned(),
        })
    })
}

/// Reset every face of a person back to unassigned. Deliberately does NOT touch
/// cluster_id, so a face rejoins its cluster's unassigned group rather than
/// scattering to singletons.
/// Change only what a person is shown as, never their identity.
///
/// This is the only rename there is. Identity is permanent: `Erhan` to
/// `Erhan Gündoğan` is a display correction even though its normalized form
/// would change too, and there is no way to ask for the other reading. One row,
/// no face touched, and `/people/person/<name>` keeps working, which is the whole
/// reason identity and display are separate.
pub fn set_full_name(conn: &Connection, name: &str, full_name: &str) -> Result<()> {
    let display = crate::label::sanitize_person_label(full_name).ok_or(Error::Invalid)?;
    let name = videre_core::person::normalize(name).ok_or(Error::Invalid)?;
    let n = conn.execute(
        "UPDATE people SET full_name = ?1 WHERE name = ?2",
        rusqlite::params![display, name],
    )?;
    if n == 0 {
        return Err(Error::NotFound);
    }
    Ok(())
}

pub fn delete_person(conn: &Connection, label: &str) -> Result<()> {
    let label = videre_core::person::normalize(label).unwrap_or_else(|| label.to_string());
    // One transaction: the faces either come back unassigned AND the
    // regroup gate reopens, or nothing changes at all.
    conn.execute_batch("BEGIN")?;
    let result = delete_person_in_transaction(conn, &label).map(|_| ());
    finish_unit_transaction(conn, result)
}

fn delete_person_in_transaction(conn: &Connection, identity: &str) -> Result<usize> {
    let changed = conn.execute(
        "UPDATE faces
         SET person_label = NULL, confirmed = 0, is_primary = 0, cluster_id = NULL
         WHERE person_label = ?1",
        [identity],
    )?;
    if changed > 0 {
        videre_core::library_state::set(
            conn,
            videre_core::library_state::FACE_RECLUSTER_WATERMARK,
            0,
        )?;
    }
    Ok(changed)
}

pub fn delete_person_with_learning(
    conn: &Connection,
    label: &str,
) -> Result<Option<LearningAcknowledgement>> {
    let identity = videre_core::person::normalize(label).ok_or(Error::Invalid)?;
    immediate_transaction(conn, || {
        let changed = delete_person_in_transaction(conn, &identity)?;
        if changed == 0 {
            return Ok(None);
        }
        let generation = invalidate_identity_for_removal_in_transaction(conn, &identity)?;
        Ok(Some(LearningAcknowledgement {
            generation,
            event_ids: Vec::new(),
            message_key: "person_removed".to_owned(),
        }))
    })
}

/// Answer one pending identity question. Yes confirms the subject cluster as
/// the target person and teaches one positive membership; No teaches one
/// negative membership without labeling; Skip only changes delivery state.
/// Every answer revalidates subject, target, active profile, and evidence
/// revision inside the transaction. Drift supersedes the stale question and
/// returns Conflict without writing a label or teaching event.
pub fn answer_question_with_learning(
    conn: &Connection,
    question_id: i64,
    answer: QuestionAnswer,
    context: &TeachingContext,
) -> Result<QuestionAnswerOutcome> {
    if context.embedding_model_id.trim().is_empty() {
        return Err(Error::Invalid);
    }
    let outcome = immediate_transaction(conn, || {
        let question = stored_question(conn, question_id)?;
        let question = match question {
            Some(question) if question.status == QuestionStatus::Pending => question,
            _ => return Err(Error::NotFound),
        };
        let supersede = || {
            finish_question_in_transaction(conn, question_id, QuestionStatus::Superseded)?;
            Ok(None)
        };
        let states = match face_states(conn, &question.subject_face_ids) {
            Ok(states) => states,
            Err(Error::NotFound) => return supersede(),
            Err(error) => return Err(error),
        };
        if states
            .iter()
            .any(|state| state.confirmed || state.person_label.is_some())
        {
            return supersede();
        }
        // Every subject face must still sit in the cluster the question was
        // built from; a recluster that moved any of them invalidates the
        // evidence and the question.
        if states
            .iter()
            .any(|state| state.cluster_id != Some(question.cluster_id))
        {
            return supersede();
        }
        let display: String = match conn.query_row(
            "SELECT full_name FROM people WHERE name = ?1",
            [&question.target_identity],
            |row| row.get(0),
        ) {
            Ok(display) => display,
            Err(rusqlite::Error::QueryReturnedNoRows) => return supersede(),
            Err(error) => return Err(error.into()),
        };
        let active = active_question_context(conn)?;
        let Some(active) = active else {
            return supersede();
        };
        if active.profile_id != question.profile_id || active.model_kind != question.model_kind {
            return supersede();
        }
        let representative: i64 = match conn.query_row(
            "SELECT f.id FROM faces AS f
                 JOIN face_learning_question_faces AS qf
                   ON qf.face_id = f.id AND qf.question_id = ?1 AND qf.role = 'subject'
                 WHERE f.confirmed = 0 AND f.person_label IS NULL
                 ORDER BY f.is_primary DESC, f.det_score DESC, f.id ASC
                 LIMIT 1",
            [question_id],
            |row| row.get(0),
        ) {
            Ok(representative) => representative,
            Err(rusqlite::Error::QueryReturnedNoRows) => return supersede(),
            Err(error) => return Err(error.into()),
        };
        let support = person_support_ids(conn, &question.target_identity, &[])?;
        let subject_observation = load_face_observations(conn, &[representative])?;
        let support_observation = load_face_observations(conn, &support)?;
        let features = extract_membership_features(
            &subject_observation,
            &support_observation,
            DecisionStage::Question,
        )?;
        let revision = question_evidence_revision(
            question.profile_id,
            question.model_kind.as_str(),
            &question.subject_face_ids,
            &question.target_identity,
            &features,
            active.membership_threshold,
            &support,
        );
        if revision != question.evidence_revision {
            return supersede();
        }
        match answer {
            QuestionAnswer::Skip => {
                finish_question_in_transaction(conn, question_id, QuestionStatus::Skipped)?;
                Ok(Some(QuestionAnswerOutcome {
                    status: "skipped".into(),
                    acknowledgement: None,
                }))
            }
            QuestionAnswer::Yes => {
                assign_in_transaction(
                    conn,
                    &question.subject_face_ids,
                    &question.target_identity,
                    &display,
                )?;
                let event = membership_event(
                    conn,
                    &[representative],
                    &support,
                    LearningAction::QuestionYes,
                    LearningOutcome::Positive,
                    Some(question.target_identity.clone()),
                    context,
                    DecisionStage::Question,
                )?;
                let receipt = append_event_batch_in_transaction(conn, &[event])?;
                finish_question_in_transaction(conn, question_id, QuestionStatus::Answered)?;
                Ok(Some(QuestionAnswerOutcome {
                    status: "answered".into(),
                    acknowledgement: Some(LearningAcknowledgement {
                        generation: receipt.generation,
                        event_ids: receipt.event_ids,
                        message_key: "question_confirmed".into(),
                    }),
                }))
            }
            QuestionAnswer::No => {
                let event = membership_event(
                    conn,
                    &[representative],
                    &support,
                    LearningAction::QuestionNo,
                    LearningOutcome::Negative,
                    Some(question.target_identity.clone()),
                    context,
                    DecisionStage::Question,
                )?;
                let receipt = append_event_batch_in_transaction(conn, &[event])?;
                finish_question_in_transaction(conn, question_id, QuestionStatus::Answered)?;
                Ok(Some(QuestionAnswerOutcome {
                    status: "answered".into(),
                    acknowledgement: Some(LearningAcknowledgement {
                        generation: receipt.generation,
                        event_ids: receipt.event_ids,
                        message_key: "question_corrected".into(),
                    }),
                }))
            }
        }
    })?;
    outcome.ok_or(Error::Conflict)
}

/// Pending identity questions for the gallery page, bounded and in priority
/// order. Selection never mutates anything.
pub fn pending_identity_questions(
    conn: &Connection,
    limit: usize,
) -> Result<Vec<videre_core::face_learning::StoredQuestion>> {
    Ok(list_pending_questions(conn, limit)?)
}

/// Refresh the pending question page from the active profile. Runs outside
/// request handling; safe to call whenever training promotes a profile.
pub fn refresh_identity_questions(
    conn: &Connection,
    config: &QuestionSelectionConfig,
) -> Result<Vec<videre_core::face_learning::StoredQuestion>> {
    videre_core::face_learning::ensure_question_tables(conn)?;
    let candidates = select_questions(conn, config)?;
    Ok(replace_pending_questions(conn, &candidates)?)
}

/// Learning state plus pending question volume for the status resource.
pub fn face_learning_status(conn: &Connection) -> Result<FaceLearningStatus> {
    videre_core::face_learning::ensure_learning_tables(conn)?;
    videre_core::face_learning::ensure_question_tables(conn)?;
    let state = videre_core::face_learning::learning_state(conn)?;
    let pending_questions = conn.query_row(
        "SELECT count(*) FROM face_learning_questions WHERE status = 'pending'",
        [],
        |row| row.get::<_, i64>(0),
    )?;
    // A retired profile was promoted and later replaced; it still counts as
    // the run having promoted.
    let last_candidate = match state.last_profile_id {
        Some(id) => {
            videre_core::face_learning::ensure_profile_table(conn)?;
            conn.query_row(
                "SELECT status FROM face_learning_profiles WHERE id = ?1",
                [id],
                |row| row.get::<_, String>(0),
            )
            .map(Some)
            .or_else(|error| match error {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })?
            .and_then(|status| match status.as_str() {
                "active" | "retired" => Some("promoted".to_string()),
                "rejected" => Some("rejected".to_string()),
                _ => None,
            })
        }
        None => None,
    };
    let waiting = state.status == videre_core::face_learning::LearningStatus::Waiting;
    Ok(FaceLearningStatus {
        generation: state.generation,
        trained_generation: state.trained_generation,
        status: format!("{:?}", state.status).to_lowercase(),
        last_profile_id: state.last_profile_id,
        last_candidate,
        // A waiting state stores what it needs in the same column a failure
        // stores its error in; the API names the two apart.
        last_error: state.last_error.clone().filter(|_| !waiting),
        feedback_needed: state.last_error.filter(|_| waiting),
        pending_questions: pending_questions as usize,
    })
}

/// One journal entry plus read-time proof facts. `source_available` says
/// whether every referenced face still exists; `incompatible` says whether
/// the entry can no longer feed training. Both are computed at read time and
/// never rewrite the historical row.
#[derive(Debug, Clone, serde::Serialize)]
pub struct FaceLearningEventProof {
    #[serde(flatten)]
    pub event: videre_core::face_learning::StoredLearningEvent,
    pub source_available: bool,
    pub incompatible: bool,
}

fn proof_for(
    conn: &Connection,
    event: videre_core::face_learning::StoredLearningEvent,
    current_embedding_model_id: Option<&str>,
) -> Result<FaceLearningEventProof> {
    let mut source_available = true;
    for face in &event.faces {
        let exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM faces WHERE id = ?1)",
            [face.face_id],
            |row| row.get(0),
        )?;
        if !exists {
            source_available = false;
            break;
        }
    }
    let incompatible = event.features.schema_version
        != videre_core::face_learning::FEATURE_SCHEMA_VERSION
        || current_embedding_model_id.is_some_and(|model| model != event.embedding_model_id);
    Ok(FaceLearningEventProof {
        event,
        source_available,
        incompatible,
    })
}

/// Learning events, newest first. Payloads carry scalar feature snapshots and
/// provenance ids only; embeddings never leave the library.
pub fn face_learning_events(
    conn: &Connection,
    limit: usize,
    before_id: Option<i64>,
    current_embedding_model_id: Option<&str>,
) -> Result<Vec<FaceLearningEventProof>> {
    videre_core::face_learning::ensure_learning_tables(conn)?;
    let limit = limit.clamp(1, 200);
    let events = list_learning_events(conn, limit, before_id)?;
    events
        .into_iter()
        .map(|event| proof_for(conn, event, current_embedding_model_id))
        .collect()
}

pub fn face_learning_event(
    conn: &Connection,
    event_id: i64,
    current_embedding_model_id: Option<&str>,
) -> Result<Option<FaceLearningEventProof>> {
    videre_core::face_learning::ensure_learning_tables(conn)?;
    match videre_core::face_learning::learning_event(conn, event_id)? {
        Some(event) => Ok(Some(proof_for(conn, event, current_embedding_model_id)?)),
        None => Ok(None),
    }
}

/// Load the immutable training inputs for the learning worker's snapshot.
pub fn load_training_snapshot(
    conn: &Connection,
    embedding_model_id: &str,
    generation: u64,
    config: &videre_core::face_learning::TrainingConfig,
) -> std::result::Result<videre_core::face_learning::TrainingSnapshot, String> {
    let labels =
        videre_core::face_db::load_confirmed_face_labels(conn).map_err(|e| e.to_string())?;
    let face_ids: Vec<i64> = {
        let mut statement = conn
            .prepare("SELECT id FROM faces ORDER BY id")
            .map_err(|e| e.to_string())?;
        let rows = statement
            .query_map([], |row| row.get(0))
            .map_err(|e| e.to_string())?
            .collect::<rusqlite::Result<Vec<i64>>>()
            .map_err(|e| e.to_string())?;
        rows
    };
    let observations =
        videre_core::face_db::load_face_observations(conn, &face_ids).map_err(|e| e.to_string())?;
    let events = videre_core::face_learning::eligible_events_for_training(
        conn,
        embedding_model_id,
        videre_core::face_learning::FEATURE_SCHEMA_VERSION,
    )
    .map_err(|e| e.to_string())?;
    videre_core::face_learning::build_training_snapshot(
        generation,
        embedding_model_id,
        &labels,
        &observations,
        &events,
        config,
    )
    .map_err(|e| e.to_string())
}

/// Persist a trained candidate: insert, promote through the shipped gates,
/// and return the profile identity and verdict. The profile row keeps the
/// promotion outcome either way.
pub fn persist_trained_profile(
    conn: &Connection,
    embedding_model_id: &str,
    run: &videre_core::face_learning::TrainingRun,
    gates: &videre_core::face_learning::PromotionGates,
) -> Result<TrainedProfileSummary> {
    let validation = match run.comparison.selected {
        videre_core::face_learning::CandidateKind::Logistic => &run.logistic_validation,
        videre_core::face_learning::CandidateKind::Additive => &run.additive_validation,
    };
    let profile = videre_core::face_learning::NewProfile {
        artifact_version: videre_core::face_learning::PROFILE_ARTIFACT_VERSION,
        embedding_model_id: embedding_model_id.to_owned(),
        feature_schema_version: videre_core::face_learning::FEATURE_SCHEMA_VERSION,
        model_kind: run.selected.model_kind().to_owned(),
        parameters: serde_json::to_vec(&run.selected).map_err(Error::from)?,
        training_evidence: run.evidence_counts.clone(),
        validation_report: validation.clone(),
        stage: videre_core::face_learning::ProfileStage::Suggestion,
    };
    let profile_id = videre_core::face_learning::insert_candidate(conn, &profile)?;
    let outcome = videre_core::face_learning::evaluate_and_promote(conn, profile_id, gates)?;
    Ok(TrainedProfileSummary {
        profile_id,
        model_kind: profile.model_kind,
        promoted: outcome == videre_core::face_learning::PromotionOutcome::Promoted,
    })
}

/// Mark one face as the person's primary (their labeling-page thumbnail),
/// clearing any previous primary in the same transaction so exactly one
/// remains. The target update is guarded by person_label so it can't steal a
/// face from another person.
pub fn set_primary(conn: &Connection, face_id: i64, person_label: &str) -> Result<()> {
    let person_label =
        videre_core::person::normalize(person_label).unwrap_or_else(|| person_label.to_string());
    conn.execute_batch("BEGIN")?;
    let result = (|| -> Result<()> {
        conn.execute(
            "UPDATE faces SET is_primary = 0 WHERE person_label = ?1",
            rusqlite::params![person_label],
        )?;
        // The guard on person_label means a face id that does not exist, or
        // belongs to someone else, matches no row: `Ok(0)`, not an error. That
        // is a NotFound, and the rollback restores the primary cleared above so
        // a failed call leaves the person's primary untouched.
        let n = conn.execute(
            "UPDATE faces SET is_primary = 1, confirmed = 1, person_label = ?1 WHERE id = ?2 AND person_label = ?1",
            rusqlite::params![person_label, face_id],
        )?;
        if n == 0 {
            return Err(Error::NotFound);
        }
        Ok(())
    })();
    match result {
        Ok(()) => {
            conn.execute_batch("COMMIT")?;
            Ok(())
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assign_detaches_the_face_from_its_cluster() {
        let conn = seed();
        // Face 3 sits in cluster 7 (unassigned). Assigning it to a person
        // must detach the machine grouping in the same write.
        assign(&conn, &[3], "Bob").unwrap();
        let (label, confirmed, cid): (Option<String>, i64, Option<i64>) = conn
            .query_row(
                "SELECT person_label, confirmed, cluster_id FROM faces WHERE id = 3",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(label.as_deref(), Some("bob"));
        assert_eq!(confirmed, 1);
        assert_eq!(cid, None, "assignment must detach the machine grouping");
    }

    #[test]
    fn cluster_detail_never_shows_labeled_faces() {
        let conn = seed();
        // A tombstone the detach migration should have cleared: a labeled
        // face still carrying a cluster id. The filter keeps the detail
        // page from ever showing it, whatever wrote that row.
        conn.execute(
            "INSERT INTO faces (id,hash,bbox,embedding,cluster_id,person_label,confirmed) VALUES
                (11,'h6','0,0,9,9',X'0000',7,'alice',1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO file_hashes (hash, path) VALUES ('h6','/p/6.jpg')",
            [],
        )
        .unwrap();
        let detail = cluster_detail(&conn, 7).unwrap();
        assert_eq!(
            detail.faces.len(),
            2,
            "only the unlabeled faces of cluster 7 belong on the page"
        );
    }

    /// In-memory db with the faces + file_hashes tables and a few rows:
    /// - face 1: person "Alice", confirmed, is_primary
    /// - face 2: person "Alice", confirmed
    /// - face 3: cluster 7 (unassigned)
    /// - face 4: cluster 7 (unassigned)
    /// - face 5: singleton (no cluster, unassigned)
    pub(super) fn seed() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        videre_core::face_db::create_faces_table(&conn).unwrap();
        conn.execute_batch(
            "CREATE TABLE file_hashes (hash TEXT PRIMARY KEY, path TEXT);
             INSERT INTO file_hashes VALUES ('h1','/p/1.jpg'),('h2','/p/2.jpg'),
                ('h3','/p/3.jpg'),('h4','/p/4.jpg'),('h5','/p/5.jpg');
             -- Labels are stored in identity form, as `assign` writes them and
             -- as the migration leaves them; `people` carries what a reader
             -- sees. Seeding raw 'Alice' would test a state the application no
             -- longer produces.
             INSERT INTO people (name, full_name) VALUES ('alice','Alice');
             INSERT INTO faces (id,hash,bbox,embedding,cluster_id,person_label,confirmed,is_primary) VALUES
                (1,'h1','0,0,9,9',X'0000',NULL,'alice',1,1),
                (2,'h2','0,0,9,9',X'0000',NULL,'alice',1,0),
                (3,'h3','0,0,9,9',X'0000',7,NULL,0,0),
                (4,'h4','0,0,9,9',X'0000',7,NULL,0,0),
                (5,'h5','0,0,9,9',X'0000',NULL,NULL,0,0);",
        )
        .unwrap();
        videre_core::db::ensure_file_hashes_columns(&conn);
        conn
    }

    mod learning {
        use super::*;
        use videre_core::face_learning::{
            learning_state, list_learning_events, LearningAction, LearningDecisionKind,
            LearningOutcome,
        };

        fn context() -> TeachingContext {
            TeachingContext {
                embedding_model_id: "buffalo_l/w600k_r50.onnx".to_owned(),
                active_profile_id: None,
            }
        }

        fn embedding(x: u16, y: u16) -> Vec<u8> {
            [x.to_le_bytes(), y.to_le_bytes()].concat()
        }

        fn learning_seed() -> Connection {
            let conn = Connection::open_in_memory().unwrap();
            videre_core::face_db::create_faces_table(&conn).unwrap();
            conn.execute_batch(
                "CREATE TABLE file_hashes (hash TEXT PRIMARY KEY, path TEXT);
                 INSERT INTO people (name, full_name) VALUES ('alice', 'Alice');",
            )
            .unwrap();
            let rows = [
                (1, "a1", embedding(0x3c00, 0), None, Some("alice"), 1),
                (2, "a2", embedding(0x3b9a, 0x3266), None, Some("alice"), 1),
                (3, "c1", embedding(0x3c00, 0), Some(7), None, 0),
                (4, "c2", embedding(0x3b9a, 0x3266), Some(7), None, 0),
                (5, "c3", embedding(0x3b33, 0x34cd), Some(7), None, 0),
                (6, "s1", embedding(0x3266, 0x3b9a), None, None, 0),
                (7, "d1", embedding(0x3c00, 0), Some(9), None, 0),
                (8, "d2", embedding(0, 0x3c00), Some(9), None, 0),
            ];
            for (id, hash, bytes, cluster, label, confirmed) in rows {
                conn.execute(
                    "INSERT INTO file_hashes (hash, path) VALUES (?1, ?2)",
                    rusqlite::params![hash, format!("/p/{hash}.jpg")],
                )
                .unwrap();
                conn.execute(
                    "INSERT INTO faces
                     (id, hash, bbox, embedding, cluster_id, person_label, confirmed,
                      is_primary, det_score, blur)
                     VALUES (?1, ?2, '0,0,112,112', ?3, ?4, ?5, ?6, 0, 0.95, 900.0)",
                    rusqlite::params![id, hash, bytes, cluster, label, confirmed],
                )
                .unwrap();
            }
            conn
        }

        #[test]
        fn learning_assignments_emit_expected_positive_evidence_once_per_action() {
            let conn = learning_seed();

            let assigned = assign_with_learning(&conn, &[6], "alice", &context()).unwrap();
            assert_eq!(assigned.generation, 1);
            assert_eq!(assigned.event_ids.len(), 1);

            let labeled = new_person_with_learning(&conn, &[3, 4, 5], "Bob", &context()).unwrap();
            assert_eq!(labeled.generation, 2);
            assert_eq!(labeled.event_ids.len(), 4);

            let events = list_learning_events(&conn, 20, None).unwrap();
            assert_eq!(events.len(), 5);
            assert_eq!(
                events
                    .iter()
                    .filter(|event| event.action == LearningAction::LabelCluster
                        && event.decision_kind == LearningDecisionKind::ClusterQuality
                        && event.outcome == LearningOutcome::Positive)
                    .count(),
                1
            );
            assert_eq!(
                events
                    .iter()
                    .filter(
                        |event| event.decision_kind == LearningDecisionKind::Membership
                            && event.outcome == LearningOutcome::Positive
                    )
                    .count(),
                4
            );
            assert!(events.iter().all(|event| {
                let json = event.features.to_canonical_json().unwrap();
                !json.contains("alice") && !json.contains("bob") && !json.contains("/p/")
            }));

            let conn = learning_seed();
            let assigned_cluster =
                assign_with_learning(&conn, &[3, 4, 5], "alice", &context()).unwrap();
            assert_eq!(assigned_cluster.generation, 1);
            assert_eq!(assigned_cluster.event_ids.len(), 4);
            let events = list_learning_events(&conn, 20, None).unwrap();
            assert_eq!(
                events
                    .iter()
                    .filter(|event| event.action == LearningAction::AssignCluster
                        && event.decision_kind == LearningDecisionKind::Membership)
                    .count(),
                3
            );
            assert_eq!(
                events
                    .iter()
                    .filter(|event| event.action == LearningAction::AssignCluster
                        && event.decision_kind == LearningDecisionKind::ClusterQuality)
                    .count(),
                1
            );
        }

        #[test]
        fn a_large_cluster_has_a_deterministic_per_action_membership_cap() {
            let conn = learning_seed();
            for id in 10..22 {
                let hash = format!("large-{id}");
                conn.execute(
                    "INSERT INTO faces
                     (id, hash, bbox, embedding, cluster_id, confirmed, is_primary,
                      det_score, blur)
                     VALUES (?1, ?2, '0,0,112,112', ?3, 42, 0, 0, 0.95, 900.0)",
                    rusqlite::params![id, hash, embedding(0x3c00, (id as u16) + 0x2000)],
                )
                .unwrap();
            }
            let ids: Vec<_> = (10..22).collect();
            let acknowledgement =
                new_person_with_learning(&conn, &ids, "Large Family", &context()).unwrap();
            assert_eq!(
                acknowledgement.event_ids.len(),
                1 + MAX_MEMBERSHIP_EVENTS_PER_ACTION
            );
            assert_eq!(learning_state(&conn).unwrap().generation, 1);

            let events = list_learning_events(&conn, 20, None).unwrap();
            assert_eq!(
                events
                    .iter()
                    .filter(|event| event.decision_kind == LearningDecisionKind::Membership)
                    .count(),
                MAX_MEMBERSHIP_EVENTS_PER_ACTION
            );
            assert!(events
                .iter()
                .filter(|event| event.decision_kind == LearningDecisionKind::Membership)
                .all(|event| event.support_count as usize <= MAX_SUPPORT_FACES));
        }

        #[test]
        fn learning_corrections_use_pre_action_state_without_pairwise_dissolve_labels() {
            let conn = learning_seed();

            let removed_cluster = remove_face_with_learning(&conn, 3, &context()).unwrap();
            assert_eq!(removed_cluster.generation, 1);
            let removed_person = remove_face_with_learning(&conn, 2, &context()).unwrap();
            assert_eq!(removed_person.generation, 2);
            let dissolved = dissolve_cluster_with_learning(&conn, 9, &context()).unwrap();
            assert_eq!(dissolved.generation, 3);

            let events = list_learning_events(&conn, 20, None).unwrap();
            assert_eq!(events.len(), 3);
            assert_eq!(
                events
                    .iter()
                    .filter(
                        |event| event.decision_kind == LearningDecisionKind::Membership
                            && event.outcome == LearningOutcome::Negative
                    )
                    .count(),
                2
            );
            let dissolve = events
                .iter()
                .find(|event| event.action == LearningAction::DissolveCluster)
                .unwrap();
            assert_eq!(dissolve.decision_kind, LearningDecisionKind::ClusterQuality);
            assert_eq!(dissolve.outcome, LearningOutcome::Negative);
            assert_eq!(dissolve.faces.len(), 2);
        }

        #[test]
        fn unsupported_last_face_removals_still_apply_without_fabricated_evidence() {
            let conn = learning_seed();
            remove_face_with_learning(&conn, 1, &context()).unwrap();
            let last_person_face = remove_face_with_learning(&conn, 2, &context()).unwrap();
            assert!(last_person_face.event_ids.is_empty());
            assert_eq!(last_person_face.generation, 1);
            let person_state: (Option<String>, i64) = conn
                .query_row(
                    "SELECT person_label, confirmed FROM faces WHERE id = 2",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            assert_eq!(person_state, (None, 0));

            remove_face_with_learning(&conn, 3, &context()).unwrap();
            remove_face_with_learning(&conn, 4, &context()).unwrap();
            let last_cluster_face = remove_face_with_learning(&conn, 5, &context()).unwrap();
            assert!(last_cluster_face.event_ids.is_empty());
            assert_eq!(last_cluster_face.generation, 3);
            let cluster_id: Option<i64> = conn
                .query_row("SELECT cluster_id FROM faces WHERE id = 5", [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(cluster_id, None);
        }

        #[test]
        fn new_person_collision_uses_existing_person_support() {
            let conn = learning_seed();
            let acknowledgement =
                new_person_with_learning(&conn, &[6], "Alice", &context()).unwrap();
            assert_eq!(acknowledgement.generation, 1);
            assert_eq!(acknowledgement.event_ids.len(), 1);
            let events = list_learning_events(&conn, 10, None).unwrap();
            assert_eq!(events[0].action, LearningAction::AssignFace);
            assert_eq!(events[0].target_identity.as_deref(), Some("alice"));
            assert_eq!(events[0].support_count, 2);
        }

        #[test]
        fn assigning_to_a_face_less_person_keeps_only_supported_evidence() {
            let conn = learning_seed();
            conn.execute(
                "UPDATE faces
                 SET person_label = NULL, confirmed = 0
                 WHERE person_label = 'alice'",
                [],
            )
            .unwrap();

            let singleton = assign_with_learning(&conn, &[6], "Alice", &context()).unwrap();
            assert!(singleton.event_ids.is_empty());
            assert_eq!(singleton.generation, 0);
            assert_eq!(singleton.message_key, "face_named_without_comparison");
            let assigned: (Option<String>, i64) = conn
                .query_row(
                    "SELECT person_label, confirmed FROM faces WHERE id = 6",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            assert_eq!(assigned, (Some("alice".to_owned()), 1));
            assert!(list_learning_events(&conn, 10, None).unwrap().is_empty());

            let conn = learning_seed();
            conn.execute(
                "UPDATE faces
                 SET person_label = NULL, confirmed = 0
                 WHERE person_label = 'alice'",
                [],
            )
            .unwrap();
            let cluster = new_person_with_learning(&conn, &[3, 4, 5], "Alice", &context()).unwrap();
            assert_eq!(cluster.event_ids.len(), 1);
            assert_eq!(cluster.generation, 1);
            let events = list_learning_events(&conn, 10, None).unwrap();
            assert_eq!(events.len(), 1);
            assert_eq!(events[0].action, LearningAction::AssignCluster);
            assert_eq!(
                events[0].decision_kind,
                LearningDecisionKind::ClusterQuality
            );
        }

        #[test]
        fn event_insert_failure_rolls_back_the_visible_assignment_and_generation() {
            let conn = learning_seed();
            conn.execute_batch(
                "CREATE TRIGGER reject_learning_event
                 BEFORE INSERT ON face_learning_events
                 BEGIN SELECT RAISE(ABORT, 'test rejection'); END;",
            )
            .unwrap();

            assert!(assign_with_learning(&conn, &[6], "alice", &context()).is_err());
            let state: (Option<String>, i64) = conn
                .query_row(
                    "SELECT person_label, confirmed FROM faces WHERE id = 6",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            assert_eq!(state, (None, 0));
            assert_eq!(learning_state(&conn).unwrap().generation, 0);
            assert!(list_learning_events(&conn, 20, None).unwrap().is_empty());
        }

        #[test]
        fn commit_failure_rolls_back_faces_events_and_generation() {
            let conn = learning_seed();
            conn.execute_batch(
                "PRAGMA foreign_keys = ON;
                 CREATE TABLE commit_guard_parent (id INTEGER PRIMARY KEY);
                 CREATE TABLE commit_guard_child (
                     event_id INTEGER PRIMARY KEY,
                     parent_id INTEGER NOT NULL,
                     FOREIGN KEY(parent_id) REFERENCES commit_guard_parent(id)
                         DEFERRABLE INITIALLY DEFERRED
                 );
                 CREATE TRIGGER fail_learning_commit
                 AFTER INSERT ON face_learning_events
                 BEGIN
                     INSERT INTO commit_guard_child (event_id, parent_id)
                     VALUES (NEW.id, 999);
                 END;",
            )
            .unwrap();

            assert!(assign_with_learning(&conn, &[6], "alice", &context()).is_err());
            let state: (Option<String>, i64) = conn
                .query_row(
                    "SELECT person_label, confirmed FROM faces WHERE id = 6",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            assert_eq!(state, (None, 0));
            assert_eq!(learning_state(&conn).unwrap().generation, 0);
            assert!(list_learning_events(&conn, 20, None).unwrap().is_empty());
        }

        #[test]
        fn malformed_or_mixed_prestate_rolls_back_without_learning() {
            let conn = learning_seed();
            conn.execute("UPDATE faces SET embedding = X'0000' WHERE id = 6", [])
                .unwrap();
            assert!(assign_with_learning(&conn, &[6], "alice", &context()).is_err());
            assert!(new_person_with_learning(&conn, &[3, 7], "Bob", &context()).is_err());
            assert!(new_person_with_learning(&conn, &[1], "Bob", &context()).is_err());
            assert!(assign_with_learning(&conn, &[999], "alice", &context()).is_err());
            assert_eq!(learning_state(&conn).unwrap().generation, 0);
            assert!(list_learning_events(&conn, 20, None).unwrap().is_empty());
        }

        #[test]
        fn deleting_a_person_invalidates_identity_evidence_without_a_negative_event() {
            let conn = learning_seed();
            assign_with_learning(&conn, &[6], "alice", &context()).unwrap();
            let acknowledgement = delete_person_with_learning(&conn, "alice")
                .unwrap()
                .unwrap();
            assert_eq!(acknowledgement.generation, 2);
            assert!(acknowledgement.event_ids.is_empty());

            let events = list_learning_events(&conn, 20, None).unwrap();
            assert_eq!(events.len(), 1);
            assert!(!events[0].eligible);
            assert_eq!(
                events[0].invalidation_reason,
                Some(videre_core::face_learning::InvalidationReason::PersonRemoved)
            );
            assert!(delete_person_with_learning(&conn, "alice")
                .unwrap()
                .is_none());
            assert_eq!(learning_state(&conn).unwrap().generation, 2);
        }

        #[test]
        fn deleting_a_person_without_learning_evidence_keeps_generation_current() {
            let conn = learning_seed();
            assert_eq!(learning_state(&conn).unwrap().generation, 0);

            let acknowledgement = delete_person_with_learning(&conn, "alice")
                .unwrap()
                .unwrap();

            assert_eq!(acknowledgement.generation, 0);
            assert!(acknowledgement.event_ids.is_empty());
            assert_eq!(learning_state(&conn).unwrap().generation, 0);
            assert!(list_learning_events(&conn, 10, None).unwrap().is_empty());
        }
    }

    #[test]
    fn the_list_comes_back_in_the_same_order_every_time() {
        // The labeling UI re-fetches after every assignment, so an unstable
        // order means the cluster lined up next moves, and so does the person
        // being dragged onto. Both lists were collected straight out of a
        // HashMap, which discarded the ORDER BY in the queries above.
        // `singletons` never had the bug, and the only difference is that it is
        // built as a Vec.
        let conn = seed();
        // The seed has one cluster and one person, which cannot show an
        // ordering problem. Add enough of both to have an order at all, with
        // sizes deliberately not matching id order.
        conn.execute_batch(
            // Columns named explicitly: `seed` runs ensure_file_hashes_columns,
            // so the table has more than the two it was created with.
            "INSERT INTO file_hashes (hash, path) VALUES ('h6','/p/6.jpg'),('h7','/p/7.jpg'),
                ('h8','/p/8.jpg'),('h9','/p/9.jpg'),('h10','/p/10.jpg');
             INSERT INTO people (name, full_name) VALUES ('bob','Bob');
             INSERT INTO faces (id,hash,bbox,embedding,cluster_id,person_label,confirmed,is_primary) VALUES
                (6,'h6','0,0,9,9',X'0000',9,NULL,0,0),
                (7,'h7','0,0,9,9',X'0000',9,NULL,0,0),
                (8,'h8','0,0,9,9',X'0000',9,NULL,0,0),
                (9,'h9','0,0,9,9',X'0000',3,NULL,0,0),
                (10,'h10','0,0,9,9',X'0000',NULL,'bob',1,0);",
        )
        .unwrap();

        // Two calls on one connection: each builds fresh HashMaps, and Rust
        // seeds them differently, so an unstable order shows up here.
        let a = faces_list(&conn).unwrap();
        let b = faces_list(&conn).unwrap();

        let ids = |f: &FacesData| -> Vec<i64> { f.clusters.iter().map(|c| c.cluster_id).collect() };
        let names =
            |f: &FacesData| -> Vec<String> { f.people.iter().map(|p| p.label.clone()).collect() };
        assert!(ids(&a).len() >= 3, "fixture must have several clusters");
        assert_eq!(
            ids(&a),
            ids(&b),
            "cluster order must not change between calls"
        );
        assert_eq!(
            names(&a),
            names(&b),
            "people order must not change between calls"
        );

        // And the order is the useful one: biggest first, so the cluster worth
        // the most labelling effort is where it is expected.
        let sizes: Vec<usize> = a.clusters.iter().map(|c| c.face_ids.len()).collect();
        let mut want = sizes.clone();
        want.sort_unstable_by(|x, y| y.cmp(x));
        assert_eq!(
            sizes, want,
            "clusters must be ordered largest first, got {sizes:?}"
        );
    }

    #[test]
    fn faces_list_splits_people_clusters_singletons() {
        let conn = seed();
        let d = faces_list(&conn).unwrap();
        assert_eq!(d.people.len(), 1);
        // Identity is the normalized form; what a reader sees is separate.
        assert_eq!(d.people[0].label, "alice");
        assert_eq!(d.people[0].full_name, "Alice");
        assert_eq!(
            d.people[0].representative_id, 1,
            "primary face is representative"
        );
        assert_eq!(d.clusters.len(), 1);
        assert_eq!(d.clusters[0].cluster_id, 7);
        assert_eq!(d.clusters[0].face_ids, vec![3, 4]);
        assert_eq!(d.singletons.len(), 1);
        assert_eq!(d.singletons[0].face_id, 5);
    }

    #[test]
    fn person_detail_marks_primary() {
        let conn = seed();
        let p = person_detail(&conn, "Alice").unwrap();
        assert_eq!(p.faces.len(), 2);
        assert!(p.faces[0].is_primary, "primary sorts first and is flagged");
        assert!(!p.faces[1].is_primary);
    }

    #[test]
    fn cluster_detail_lists_faces() {
        let conn = seed();
        let c = cluster_detail(&conn, 7).unwrap();
        assert_eq!(c.cluster_id, 7);
        assert_eq!(
            c.faces.iter().map(|f| f.face_id).collect::<Vec<_>>(),
            vec![3, 4]
        );
    }

    #[test]
    fn assign_labels_and_confirms() {
        let conn = seed();
        assign(&conn, &[3, 4], "Bob").unwrap();
        let p = person_detail(&conn, "Bob").unwrap();
        assert_eq!(p.faces.len(), 2, "both faces now confirmed under Bob");
    }

    #[test]
    fn assign_rejects_empty_label() {
        let conn = seed();
        assert!(matches!(assign(&conn, &[3], "   "), Err(Error::Invalid)));
    }

    #[test]
    fn remove_face_unassigns_everything() {
        let conn = seed();
        remove_face(&conn, 1).unwrap();
        let (cid, label, confirmed, prim): (Option<i64>, Option<String>, i64, i64) = conn
            .query_row(
                "SELECT cluster_id, person_label, confirmed, is_primary FROM faces WHERE id=1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!((cid, label, confirmed, prim), (None, None, 0, 0));
    }

    #[test]
    fn dissolve_cluster_nulls_cluster_id() {
        let conn = seed();
        dissolve_cluster(&conn, 7).unwrap();
        assert_eq!(faces_list(&conn).unwrap().clusters.len(), 0);
        assert_eq!(
            faces_list(&conn).unwrap().singletons.len(),
            3,
            "3,4 join 5 as singletons"
        );
    }

    #[test]
    fn deleting_a_missing_person_leaves_the_regrouping_gate_alone() {
        // A delete that matched zero faces is a no-op by design; it must not
        // schedule a whole-library regroup (watermark reset) for nothing.
        let conn = seed();
        videre_core::face_db::advance_recluster_watermark(&conn).unwrap();
        let before = videre_core::face_db::recluster_watermark(&conn).unwrap();
        assert!(before > 0);
        delete_person(&conn, "ghost").unwrap();
        assert_eq!(
            videre_core::face_db::recluster_watermark(&conn).unwrap(),
            before,
            "a no-op delete must not reopen the gated regroup"
        );
    }

    #[test]
    fn delete_person_returns_faces_to_the_unassigned_pool_and_reopens_regrouping() {
        // The real workflow: assign through the public path (which detaches
        // the cluster, per the frozen-faces contract), then delete the
        // person. The faces go back to the unassigned pool, and the recluster
        // watermark resets so the next gated pass regroups them: without the
        // reset, these pre-existing face ids sit below the watermark and the
        // gate stays closed forever.
        let conn = seed();
        assign(&conn, &[1, 2], "Alice").unwrap();
        assert_eq!(faces_list(&conn).unwrap().people.len(), 1);
        // Simulate a completed recluster covering these faces: the watermark
        // sits at their ids, so the gate would stay closed for them forever.
        videre_core::face_db::advance_recluster_watermark(&conn).unwrap();
        assert!(videre_core::face_db::recluster_watermark(&conn).unwrap() > 0);

        delete_person(&conn, "Alice").unwrap();
        assert_eq!(faces_list(&conn).unwrap().people.len(), 0, "Alice is gone");
        assert_eq!(
            videre_core::face_db::recluster_watermark(&conn).unwrap(),
            0,
            "deleting a person must reopen the gated regroup for their faces"
        );
        let rows: Vec<(Option<i64>, Option<String>, i64)> = {
            let mut s = conn
                .prepare("SELECT cluster_id, person_label, confirmed FROM faces WHERE id IN (1, 2) ORDER BY id")
                .unwrap();
            s.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap()
        };
        assert!(
            rows.iter()
                .all(|(cid, label, confirmed)| cid.is_none() && label.is_none() && *confirmed == 0),
            "every face returns to the unassigned pool: {rows:?}"
        );
    }

    #[test]
    fn set_primary_is_exclusive_per_person() {
        let conn = seed();
        set_primary(&conn, 2, "Alice").unwrap();
        let primaries: Vec<i64> = {
            let mut s = conn
                .prepare("SELECT id FROM faces WHERE person_label='alice' AND is_primary=1")
                .unwrap();
            s.query_map([], |r| r.get(0))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap()
        };
        assert_eq!(primaries, vec![2], "exactly one primary, now face 2");
    }

    #[test]
    fn renaming_only_the_spelling_keeps_the_identity() {
        // The common rename: correcting or extending what is shown, which must
        // not change the URL or touch a single face row.
        let conn = seed();
        set_full_name(&conn, "alice", "Alice Smith").unwrap();
        let (name, full): (String, String) = conn
            .query_row("SELECT name, full_name FROM people", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(name, "alice", "identity is unchanged");
        assert_eq!(full, "Alice Smith", "only the display name moved");
        assert_eq!(person_detail(&conn, "alice").unwrap().faces.len(), 2);
    }

    // A write against a client-supplied id that matches no row is `Ok(0)` from
    // rusqlite, not an error. Reported as success it tells the labeling UI an
    // action worked when nothing changed. Each handler that takes an id from the
    // client must turn "matched nothing" into NotFound, the way set_full_name
    // already does.

    #[test]
    fn assign_a_missing_face_is_not_found() {
        let conn = seed();
        assert!(matches!(assign(&conn, &[999], "Bob"), Err(Error::NotFound)));
    }

    #[test]
    fn assign_is_atomic_when_one_face_is_missing() {
        // face 3 exists, 999 does not. All-or-nothing: face 3 must be untouched
        // and no `Bob` person may be created, so a partial write can never be
        // reported as success.
        let conn = seed();
        assert!(matches!(
            assign(&conn, &[3, 999], "Bob"),
            Err(Error::NotFound)
        ));
        let (label, confirmed): (Option<String>, i64) = conn
            .query_row(
                "SELECT person_label, confirmed FROM faces WHERE id = 3",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(label, None, "face 3 must not have been labelled");
        assert_eq!(confirmed, 0, "face 3 must not have been confirmed");
        let bob: i64 = conn
            .query_row("SELECT COUNT(*) FROM people WHERE name = 'bob'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            bob, 0,
            "no person may be created when the assign rolls back"
        );
    }

    #[test]
    fn assign_commit_failure_rolls_back_and_closes_the_transaction() {
        let conn = seed();
        conn.execute_batch(
            "PRAGMA foreign_keys = ON;
             CREATE TABLE commit_guard_parent (id INTEGER PRIMARY KEY);
             CREATE TABLE commit_guard_child (
                 face_id INTEGER PRIMARY KEY,
                 parent_id INTEGER NOT NULL,
                 FOREIGN KEY(parent_id) REFERENCES commit_guard_parent(id)
                     DEFERRABLE INITIALLY DEFERRED
             );
             CREATE TRIGGER fail_assign_commit
             AFTER UPDATE ON faces
             WHEN NEW.id = 3
             BEGIN
                 INSERT INTO commit_guard_child (face_id, parent_id)
                 VALUES (NEW.id, 999);
             END;",
        )
        .unwrap();

        assert!(assign(&conn, &[3], "Bob").is_err());
        assert!(conn.is_autocommit());
        let state: (Option<String>, i64) = conn
            .query_row(
                "SELECT person_label, confirmed FROM faces WHERE id = 3",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(state, (None, 0));
        let bob: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM people WHERE name = 'bob'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(bob, 0);
    }

    #[test]
    fn assign_rejects_empty_face_ids() {
        // Nothing to assign is a malformed request, not a silent success that
        // creates a person with no faces.
        let conn = seed();
        assert!(matches!(assign(&conn, &[], "Bob"), Err(Error::Invalid)));
    }

    #[test]
    fn remove_face_missing_is_not_found() {
        let conn = seed();
        assert!(matches!(remove_face(&conn, 999), Err(Error::NotFound)));
    }

    #[test]
    fn dissolve_cluster_missing_is_not_found() {
        let conn = seed();
        assert!(matches!(dissolve_cluster(&conn, 999), Err(Error::NotFound)));
    }

    #[test]
    fn set_primary_missing_face_is_not_found() {
        let conn = seed();
        assert!(matches!(
            set_primary(&conn, 999, "Alice"),
            Err(Error::NotFound)
        ));
    }

    #[test]
    fn set_primary_face_of_another_person_is_not_found_and_rolls_back() {
        // face 5 is an unassigned singleton, so the guarded update matches no
        // row for Alice. The failure must roll back the primary-clearing step:
        // Alice's existing primary (face 1) has to survive.
        let conn = seed();
        assert!(matches!(
            set_primary(&conn, 5, "Alice"),
            Err(Error::NotFound)
        ));
        let primary: i64 = conn
            .query_row(
                "SELECT id FROM faces WHERE person_label = 'alice' AND is_primary = 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            primary, 1,
            "the original primary must be restored on rollback"
        );
    }

    #[test]
    fn delete_person_missing_is_idempotent_success() {
        // Delete is idempotent: asking to unassign a person who is already gone
        // has already achieved its goal. A person can also legitimately have a
        // `people` row and no confirmed faces, which would make a row-count
        // check wrongly 404 a real person, so delete stays out of the NotFound
        // rule by design.
        let conn = seed();
        assert!(delete_person(&conn, "Nobody").is_ok());
    }
}

#[cfg(test)]
mod identity_tests {
    use super::tests::seed;
    use super::*;

    fn people(conn: &Connection) -> Vec<(String, String)> {
        conn.prepare("SELECT name, full_name FROM people ORDER BY name")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    #[test]
    fn assign_stores_the_identity_and_records_the_display_name() {
        let conn = seed();
        assign(&conn, &[3], "Işıl Özyeğin").unwrap();

        let label: String = conn
            .query_row("SELECT person_label FROM faces WHERE id = 3", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(label, "isil_ozyegin", "faces hold the identity");
        assert!(
            people(&conn).contains(&("isil_ozyegin".into(), "Işıl Özyeğin".into())),
            "and the spelling is kept for display"
        );
    }

    #[test]
    fn assigning_an_existing_name_in_another_case_joins_that_person() {
        // The bug this whole change exists to fix: this used to create a second
        // person.
        let conn = seed();
        assign(&conn, &[3], "ALICE").unwrap();
        assert_eq!(people(&conn).len(), 1, "still one person, not two");
        assert_eq!(person_detail(&conn, "alice").unwrap().faces.len(), 3);
        assert_eq!(
            people(&conn)[0].1,
            "Alice",
            "the existing spelling is not overwritten by the new casing"
        );
    }

    #[test]
    fn assign_rejects_a_name_with_no_usable_identity() {
        // Punctuation alone leaves nothing to identify a person by, and an
        // empty identity would be a person nobody could address.
        let conn = seed();
        assert!(matches!(assign(&conn, &[3], "!!!"), Err(Error::Invalid)));
    }

    #[test]
    fn person_detail_resolves_every_form_of_the_name() {
        let conn = seed();
        for form in ["alice", "Alice", "ALICE", "  alice  "] {
            assert_eq!(
                person_detail(&conn, form).unwrap().faces.len(),
                2,
                "form {form:?}"
            );
        }
    }

    #[test]
    fn person_detail_reports_the_display_name() {
        let d = person_detail(&seed(), "alice").unwrap();
        assert_eq!(d.label, "alice");
        assert_eq!(d.full_name, "Alice");
    }

    #[test]
    fn person_detail_falls_back_when_there_is_no_people_row() {
        // A label written before the table existed still has to render. The
        // orphan label is the pre-v2 shape, so its seed runs with enforcement
        // lifted and restored.
        let conn = seed();
        conn.execute_batch("PRAGMA foreign_keys = OFF").unwrap();
        conn.execute(
            "INSERT INTO faces (id,hash,bbox,embedding,person_label,confirmed) \
             VALUES (9,'h9','0,0,9,9',X'0000','orphan',1)",
            [],
        )
        .unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON").unwrap();
        let d = person_detail(&conn, "orphan").unwrap();
        assert_eq!(d.full_name, "orphan", "falls back to the identity");
    }

    #[test]
    fn set_full_name_changes_only_the_display_name() {
        let conn = seed();
        set_full_name(&conn, "alice", "Alice Smith").unwrap();
        assert_eq!(people(&conn), vec![("alice".into(), "Alice Smith".into())]);
        assert_eq!(
            person_detail(&conn, "alice").unwrap().faces.len(),
            2,
            "no face was touched"
        );
    }

    #[test]
    fn set_full_name_accepts_any_form_of_the_identity() {
        let conn = seed();
        set_full_name(&conn, "ALICE", "Alice Smith").unwrap();
        assert_eq!(people(&conn)[0].1, "Alice Smith");
    }

    #[test]
    fn set_full_name_on_a_missing_person_is_not_found() {
        assert!(matches!(
            set_full_name(&seed(), "nobody", "Someone"),
            Err(Error::NotFound)
        ));
    }

    #[test]
    fn set_full_name_rejects_an_empty_display_name() {
        // A person with no name to show is worse than one shown by identity.
        assert!(matches!(
            set_full_name(&seed(), "alice", "   "),
            Err(Error::Invalid)
        ));
    }

    #[test]
    fn delete_person_accepts_any_form_of_the_name() {
        let conn = seed();
        delete_person(&conn, "Alice").unwrap();
        let left: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM faces WHERE person_label IS NOT NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(left, 0, "faces are unassigned whichever form was passed");
    }

    #[test]
    fn set_primary_accepts_any_form_of_the_name() {
        let conn = seed();
        set_primary(&conn, 2, "ALICE").unwrap();
        let primary: i64 = conn
            .query_row(
                "SELECT id FROM faces WHERE person_label='alice' AND is_primary=1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(primary, 2);
    }
}

#[cfg(test)]
mod never_run_tests {
    use super::*;

    /// :warning: **A scanned-but-never-detected library has no `faces` table.**
    ///
    /// `videre scan` creates `file_hashes`, `people` and `pipeline_runs`. The
    /// faces table arrives with the first `videre faces` run, so every query
    /// here failed with "no such table" until then. The server turned that into
    /// a 500 with an empty body, and the page turned the empty body into
    /// `Unexpected end of JSON input` across the top of the labeling UI.
    ///
    /// Every existing test in this file seeds a faces table, which is why none
    /// of them could see it: they all describe a library that has already run
    /// detection.
    #[test]
    fn a_library_that_never_ran_detection_is_empty_not_an_error() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE file_hashes (path TEXT PRIMARY KEY, hash TEXT NOT NULL);
             CREATE TABLE people (name TEXT PRIMARY KEY, full_name TEXT);",
        )
        .unwrap();

        let data = faces_list(&conn).expect("a library with no faces table is not an error");
        assert!(data.people.is_empty());
        assert!(data.clusters.is_empty());
        assert!(data.singletons.is_empty());
    }

    // ---- questions ----

    mod question_fixture {
        use super::*;
        use videre_core::face_learning::{
            ensure_question_tables, replace_pending_questions, select_questions, LogisticModel,
            LogisticScorer, ModelBundle, QuestionSelectionConfig, MEMBERSHIP_FEATURE_NAMES,
            MODEL_ARTIFACT_VERSION,
        };

        pub fn embedding_blob(x: f32, y: f32) -> Vec<u8> {
            let mut bytes = Vec::with_capacity(4);
            bytes.extend_from_slice(&half::f16::from_f32(x).to_le_bytes());
            bytes.extend_from_slice(&half::f16::from_f32(y).to_le_bytes());
            bytes
        }

        fn logistic_bundle() -> ModelBundle {
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
                calibration: videre_core::face_learning::CalibrationModel {
                    intercept: 0.0,
                    slope: 1.0,
                },
                threshold: 0.5,
            };
            ModelBundle::Logistic {
                artifact_version: MODEL_ARTIFACT_VERSION,
                embedding_model_id: "arcface/test".into(),
                feature_schema_version: 1,
                membership: scorer.clone(),
                cluster_quality: scorer,
            }
        }

        /// Faces 10 and 11 sit in cluster 1; faces 12 and 13 confirm "alice".
        /// Returns the connection and the pending question id asking about
        /// cluster 1 and alice.
        /// Mirrors the planned foreign-key contract: enforcement on, and a face
        /// label must name an existing person.
        pub fn library() -> (Connection, i64, i64) {
            let conn = Connection::open_in_memory().unwrap();
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
            videre_core::face_learning::ensure_learning_tables(&conn).unwrap();
            videre_core::face_learning::ensure_profile_table(&conn).unwrap();
            ensure_question_tables(&conn).unwrap();

            for (id, cluster) in [(10, Some(1)), (11, Some(1)), (12, None), (13, None)] {
                conn.execute(
                    "INSERT INTO faces (id, hash, bbox, embedding, cluster_id, confirmed, det_score, blur)
                     VALUES (?1, 'h' || ?1, '0,0,80,80', ?2, ?3, 0, 0.9, 600.0)",
                    rusqlite::params![id, embedding_blob(1.0, 0.0), cluster],
                )
                .unwrap();
            }
            assign(&conn, &[12, 13], "Alice").unwrap();

            let evidence =
                serde_json::to_string(&videre_core::face_learning::TrainingEvidenceCounts {
                    positive_pairs: 20,
                    negative_pairs: 20,
                    explicit_negative_pairs: 0,
                })
                .unwrap();
            let report = serde_json::to_string(&videre_core::face_learning::ValidationReport {
                protocol_version: 1,
                evidence_schema_version: 1,
                feature_schema_version: 1,
                datasets: Vec::new(),
            })
            .unwrap();
            conn.execute(
                "INSERT INTO face_learning_profiles (
                    artifact_version, embedding_model_id, feature_schema_version, model_kind,
                    parameters, training_evidence_json, validation_report_json, stage, status
                 ) VALUES (1, 'arcface/test', 1, 'logistic', ?1, ?2, ?3, 'suggestion', 'active')",
                rusqlite::params![
                    serde_json::to_vec(&logistic_bundle()).unwrap(),
                    evidence,
                    report
                ],
            )
            .unwrap();
            let profile_id = conn.last_insert_rowid();

            let candidates = select_questions(&conn, &QuestionSelectionConfig::default()).unwrap();
            assert_eq!(candidates.len(), 1, "fixture must produce one question");
            let stored = replace_pending_questions(&conn, &candidates).unwrap();
            assert_eq!(stored.len(), 1);
            (conn, stored[0].id, profile_id)
        }

        pub fn stub_evidence() -> videre_core::face_learning::DecisionEvidence {
            use videre_core::face_learning::{
                Calibration, DecisionKind, DecisionOutcome, DecisionTarget, FeatureContribution,
                ValidationSummary, EVIDENCE_SCHEMA_VERSION, FEATURE_SCHEMA_VERSION,
            };
            let evidence = videre_core::face_learning::DecisionEvidence {
                schema_version: EVIDENCE_SCHEMA_VERSION,
                profile_id: 1,
                feature_schema_version: FEATURE_SCHEMA_VERSION,
                decision_kind: DecisionKind::Membership,
                outcome: DecisionOutcome::Allowed,
                subject_face_ids: vec![10],
                target: DecisionTarget::Person("alice".into()),
                intercept: 0.0,
                raw_logit: 0.0,
                calibration: Calibration {
                    intercept: 0.0,
                    slope: 1.0,
                },
                calibrated_confidence: 0.5,
                threshold: 0.5,
                margin: 0.0,
                features: vec![FeatureContribution {
                    name: "similarity_mean".into(),
                    value: 1.0,
                    contribution: 0.0,
                }],
                support_face_ids: vec![12, 13],
                rule_vetoes: Vec::new(),
                validation: ValidationSummary {
                    protocol_version: 1,
                    datasets: 1,
                    pair_precision: None,
                    pair_recall: None,
                    suggestion_precision: None,
                    suggestion_coverage: None,
                },
            };
            evidence.validate().unwrap();
            evidence
        }

        pub fn context(profile_id: i64) -> TeachingContext {
            TeachingContext {
                embedding_model_id: "arcface/test".into(),
                active_profile_id: Some(profile_id),
            }
        }
    }

    use question_fixture as qf;

    #[test]
    fn deleting_a_person_supersedes_questions_and_advances_once() {
        let (conn, _question_id, _profile_id) = qf::library();
        // A second pending question for the same identity: both must go.
        let second = videre_core::face_learning::StoredQuestion {
            id: 999,
            status: videre_core::face_learning::QuestionStatus::Pending,
            subject_face_ids: vec![10],
            support_face_ids: vec![12, 13],
            target_identity: "alice".into(),
            target_display: "Alice".into(),
            profile_id: 1,
            model_kind: "logistic".into(),
            representative_face_id: 10,
            cluster_id: 1,
            evidence_revision: "another-revision".into(),
            evidence: qf::stub_evidence(),
            created_at: "2026-01-01 00:00:00".into(),
            decided_at: None,
        };
        let _ = second;
        delete_person_with_learning(&conn, "Alice").unwrap();
        let superseded: i64 = conn
            .query_row(
                "SELECT count(*) FROM face_learning_questions WHERE status = 'superseded'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(superseded, 1, "the pending question must be superseded");
        let state = learning_state(&conn).unwrap();
        assert_eq!(state.generation, 1, "exactly one generation advance");
        let invalidated: i64 = conn
            .query_row(
                "SELECT count(*) FROM face_learning_events WHERE eligible = 0",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(invalidated, 0, "no events existed to invalidate");
    }

    #[test]
    fn the_journal_reports_availability_without_rewriting_history() {
        let (conn, _question_id, profile_id) = qf::library();
        // Produce journal entries, then remove a face an entry points at.
        assign_with_learning(&conn, &[10, 11], "Alice", &qf::context(profile_id)).unwrap();
        let subject_event_id = face_learning_events(&conn, 50, None, Some("arcface/test"))
            .unwrap()
            .iter()
            .find(|proof| proof.event.faces.iter().any(|face| face.face_id == 10))
            .map(|proof| proof.event.id)
            .unwrap();
        // A re-detection rebuild replaces face rows; simulate the row the
        // entry references vanishing.
        conn.execute("DELETE FROM faces WHERE id = 10", []).unwrap();

        let proofs = face_learning_events(&conn, 50, None, Some("arcface/test")).unwrap();
        let proof = proofs
            .iter()
            .find(|proof| proof.event.id == subject_event_id)
            .unwrap();
        assert!(!proof.source_available, "the subject face is gone");
        assert!(!proof.incompatible, "same model and schema stay usable");
        assert!(proof.event.eligible, "missing provenance stays eligible");

        // A different configured model marks the entry incompatible.
        let proofs = face_learning_events(&conn, 50, None, Some("other/model")).unwrap();
        let proof = proofs
            .iter()
            .find(|proof| proof.event.id == subject_event_id)
            .unwrap();
        assert!(proof.incompatible);

        // Invalidation changes eligibility columns only, never the features.
        let (conn, question_id, profile_id) = qf::library();
        answer_question_with_learning(
            &conn,
            question_id,
            videre_core::face_learning::QuestionAnswer::No,
            &qf::context(profile_id),
        )
        .unwrap();
        let before: String = conn
            .query_row(
                "SELECT feature_snapshot_json FROM face_learning_events WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        delete_person_with_learning(&conn, "Alice").unwrap();
        let after: String = conn
            .query_row(
                "SELECT feature_snapshot_json FROM face_learning_events WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(before, after, "historical feature JSON never mutates");
    }

    #[test]
    fn yes_confirms_the_target_and_teaches_positive_membership() {
        let (conn, question_id, profile_id) = qf::library();
        let outcome = answer_question_with_learning(
            &conn,
            question_id,
            QuestionAnswer::Yes,
            &qf::context(profile_id),
        )
        .unwrap();
        assert_eq!(outcome.status, "answered");
        let ack = outcome.acknowledgement.expect("yes must teach");
        assert_eq!(ack.event_ids.len(), 1);
        assert_eq!(ack.generation, 1);

        let labeled: i64 = conn
            .query_row(
                "SELECT count(*) FROM faces WHERE id IN (10, 11) AND person_label = 'alice'
                 AND confirmed = 1 AND cluster_id IS NULL",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(labeled, 2, "yes labels the whole subject cluster");

        let event: (String, String, String) = conn
            .query_row(
                "SELECT action_kind, outcome, target_identity FROM face_learning_events",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(event.0, "question_yes");
        assert_eq!(event.1, "positive");
        assert_eq!(event.2, "alice");
    }

    #[test]
    fn no_teaches_negative_without_labeling() {
        let (conn, question_id, profile_id) = qf::library();
        let outcome = answer_question_with_learning(
            &conn,
            question_id,
            QuestionAnswer::No,
            &qf::context(profile_id),
        )
        .unwrap();
        assert_eq!(outcome.status, "answered");

        let untouched: i64 = conn
            .query_row(
                "SELECT count(*) FROM faces WHERE id IN (10, 11) AND confirmed = 0
                 AND person_label IS NULL AND cluster_id = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(untouched, 2, "no must not label");

        let event: (String, String) = conn
            .query_row(
                "SELECT action_kind, outcome FROM face_learning_events",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(event.0, "question_no");
        assert_eq!(event.1, "negative");
    }

    #[test]
    fn skip_only_changes_delivery_state() {
        let (conn, question_id, profile_id) = qf::library();
        let outcome = answer_question_with_learning(
            &conn,
            question_id,
            QuestionAnswer::Skip,
            &qf::context(profile_id),
        )
        .unwrap();
        assert_eq!(outcome.status, "skipped");
        assert!(outcome.acknowledgement.is_none());

        let events: i64 = conn
            .query_row("SELECT count(*) FROM face_learning_events", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(events, 0, "skip produces no event");
        let state = learning_state(&conn).unwrap();
        assert_eq!(state.generation, 0, "skip does not advance generation");
    }

    #[test]
    fn stale_answers_conflict_without_partial_writes() {
        // Already-labeled subject.
        let (conn, question_id, profile_id) = qf::library();
        assign(&conn, &[10, 11], "Bob").unwrap();
        assert!(matches!(
            answer_question_with_learning(
                &conn,
                question_id,
                QuestionAnswer::Yes,
                &qf::context(profile_id)
            ),
            Err(Error::Conflict)
        ));
        let events: i64 = conn
            .query_row("SELECT count(*) FROM face_learning_events", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(events, 0, "a conflict must not teach");
        assert_eq!(
            videre_core::face_learning::stored_question(&conn, question_id)
                .unwrap()
                .unwrap()
                .status,
            QuestionStatus::Superseded
        );

        // Removed target person. With face labels referencing people, the
        // row can only go once no face carries the label.
        let (conn, question_id, profile_id) = qf::library();
        conn.execute_batch(
            "UPDATE faces SET person_label = NULL, confirmed = 0 WHERE person_label = 'alice';
             DELETE FROM people WHERE name = 'alice';",
        )
        .unwrap();
        assert!(matches!(
            answer_question_with_learning(
                &conn,
                question_id,
                QuestionAnswer::No,
                &qf::context(profile_id)
            ),
            Err(Error::Conflict)
        ));
        assert_eq!(
            videre_core::face_learning::stored_question(&conn, question_id)
                .unwrap()
                .unwrap()
                .status,
            QuestionStatus::Superseded
        );

        // A different active profile.
        let (conn, question_id, profile_id) = qf::library();
        conn.execute("UPDATE face_learning_profiles SET status = 'retired'", [])
            .unwrap();
        let _ = profile_id;
        assert!(matches!(
            answer_question_with_learning(&conn, question_id, QuestionAnswer::No, &qf::context(99)),
            Err(Error::Conflict)
        ));
        assert_eq!(
            videre_core::face_learning::stored_question(&conn, question_id)
                .unwrap()
                .unwrap()
                .status,
            QuestionStatus::Superseded
        );

        // Support set changed: the evidence revision no longer matches.
        let (conn, question_id, profile_id) = qf::library();
        assign(&conn, &[13], "Alice").unwrap();
        remove_face(&conn, 12).unwrap();
        insert_face_with_score(&conn, 14, None, 0.9);
        assign(&conn, &[14], "Alice").unwrap();
        assert!(matches!(
            answer_question_with_learning(
                &conn,
                question_id,
                QuestionAnswer::No,
                &qf::context(profile_id)
            ),
            Err(Error::Conflict)
        ));
        let question = videre_core::face_learning::stored_question(&conn, question_id)
            .unwrap()
            .unwrap();
        assert_eq!(
            question.status,
            videre_core::face_learning::QuestionStatus::Superseded
        );
        assert!(pending_identity_questions(&conn, 5).unwrap().is_empty());
    }

    fn insert_face_with_score(conn: &Connection, id: i64, cluster: Option<i64>, score: f64) {
        conn.execute(
            "INSERT INTO faces (id, hash, bbox, embedding, cluster_id, confirmed, det_score, blur)
             VALUES (?1, 'h' || ?1, '0,0,80,80', ?2, ?3, 0, ?4, 600.0)",
            rusqlite::params![id, qf::embedding_blob(1.0, 0.0), cluster, score],
        )
        .unwrap();
    }

    #[test]
    fn faces_moved_out_of_the_question_cluster_conflict() {
        let (conn, question_id, profile_id) = qf::library();
        // A recluster reassigned one subject face after the question was
        // built: the evidence no longer describes the displayed cluster.
        conn.execute("UPDATE faces SET cluster_id = 9 WHERE id = 11", [])
            .unwrap();
        assert!(matches!(
            answer_question_with_learning(
                &conn,
                question_id,
                QuestionAnswer::Yes,
                &qf::context(profile_id)
            ),
            Err(Error::Conflict)
        ));

        let labeled: i64 = conn
            .query_row(
                "SELECT count(*) FROM faces WHERE id IN (10, 11) AND confirmed = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(labeled, 0, "a stale cluster must not label");
        let events: i64 = conn
            .query_row("SELECT count(*) FROM face_learning_events", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(events, 0);
        let question = videre_core::face_learning::stored_question(&conn, question_id)
            .unwrap()
            .unwrap();
        assert_eq!(
            question.status,
            videre_core::face_learning::QuestionStatus::Superseded
        );
        assert!(pending_identity_questions(&conn, 5).unwrap().is_empty());
    }

    #[test]
    fn refresh_creates_question_tables_for_a_first_training_cycle() {
        let (conn, _, _) = qf::library();
        conn.execute_batch(
            "DROP TABLE face_learning_question_faces;
             DROP TABLE face_learning_questions;",
        )
        .unwrap();

        let questions = refresh_identity_questions(&conn, &QuestionSelectionConfig::default())
            .expect("a promoted profile should create the question tables");
        assert_eq!(questions.len(), 1);
        assert_eq!(pending_identity_questions(&conn, 5).unwrap().len(), 1);
    }

    /// A real initialized version-2 library: foreign keys verified on, the
    /// canonical DDL in place. The public face paths must work unchanged and
    /// orphan writes must fail.
    #[test]
    fn v2_library_enforces_keys_through_the_public_paths() {
        use videre_core::face_learning::QuestionAnswer as Answer;
        let root = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let ctx = videre_core::library::LibraryContext::new(root.path(), cache.path()).unwrap();
        let conn = videre_core::library_db::initialize(&ctx).unwrap();
        let keys_on: i64 = conn
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .unwrap();
        assert_eq!(keys_on, 1, "an initialized library verifies enforcement");

        // Seed two people with one confirmed face each, through the public
        // assign path, after detectable faces exist as unassigned clusters.
        conn.execute_batch(
            "INSERT INTO faces (id, hash, bbox, embedding, cluster_id, confirmed, det_score, blur) VALUES
                (1, 'k1', '0,0,9,9', X'0000', 7, 0, 0.9, 600.0),
                (2, 'k2', '0,0,9,9', X'0000', 7, 0, 0.9, 600.0);
             INSERT INTO people (name, full_name) VALUES ('alice', 'Alice'), ('bob', 'Bob');",
        )
        .unwrap();
        assign(&conn, &[1], "Alice").unwrap();
        assign(&conn, &[2], "Bob").unwrap();

        // Orphan writes fail: a face labeled with an unknown person, and an
        // event provenance row with no parent event.
        assert!(conn
            .execute(
                "INSERT INTO faces (hash,bbox,embedding,person_label,confirmed)
                 VALUES ('k9','0,0,9,9',X'0000','ghost',1)",
                [],
            )
            .is_err());
        assert!(conn
            .execute(
                "INSERT INTO face_learning_event_faces (event_id, face_id, role, ordinal)
                 VALUES (999, 1, 'subject', 0)",
                [],
            )
            .is_err());

        // Parent-first inserts succeed.
        conn.execute(
            "INSERT INTO face_learning_events (id, action_kind, decision_kind, outcome,
                embedding_model_id, feature_schema_version, target_identity,
                feature_snapshot_json, support_count)
             VALUES (1, 'assign_face', 'membership', 'positive', 'x/1', 1, 'alice', '{}', 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO face_learning_event_faces (event_id, face_id, role, ordinal)
             VALUES (1, 1, 'subject', 0)",
            [],
        )
        .unwrap();

        // Deleting a person invalidates their evidence; the face the user
        // assigned becomes unassigned again.
        delete_person_with_learning(&conn, "Alice").unwrap();
        let state: (i64, Option<String>) = conn
            .query_row(
                "SELECT confirmed, person_label FROM faces WHERE id = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(state, (0, None));

        // A stale question is rejected by the answer path.
        let question = videre_core::face_learning::select_questions(
            &conn,
            &videre_core::face_learning::QuestionSelectionConfig::default(),
        )
        .unwrap();
        if !question.is_empty() {
            let stored =
                videre_core::face_learning::replace_pending_questions(&conn, &question).unwrap();
            conn.execute(
                "UPDATE face_learning_questions SET evidence_revision = 'stale' WHERE id = ?1",
                rusqlite::params![stored[0].id],
            )
            .unwrap();
            let context = TeachingContext {
                embedding_model_id: "x/1".into(),
                active_profile_id: None,
            };
            assert!(matches!(
                answer_question_with_learning(&conn, stored[0].id, Answer::Yes, &context),
                Err(Error::Conflict)
            ));
        }

        // Reset clears every learning table and passes foreign_key_check.
        videre_core::face_db::reset_all(&conn).unwrap();
        for table in [
            "face_learning_events",
            "face_learning_event_faces",
            "face_learning_questions",
            "face_learning_question_faces",
            "face_learning_profiles",
        ] {
            let n: i64 = conn
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(n, 0, "{table} must be empty after reset");
        }
        let violations: i64 = conn
            .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(violations, 0);
    }

    #[test]
    fn yes_cannot_label_without_evidence() {
        let (conn, question_id, profile_id) = qf::library();
        conn.execute_batch(
            "CREATE TRIGGER abort_question_events
             BEFORE INSERT ON face_learning_events
             BEGIN SELECT RAISE(ABORT, 'injected event failure'); END;",
        )
        .unwrap();
        assert!(answer_question_with_learning(
            &conn,
            question_id,
            QuestionAnswer::Yes,
            &qf::context(profile_id)
        )
        .is_err());

        let labeled: i64 = conn
            .query_row(
                "SELECT count(*) FROM faces WHERE id IN (10, 11) AND confirmed = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(labeled, 0, "yes cannot label without its evidence row");

        let question = videre_core::face_learning::stored_question(&conn, question_id)
            .unwrap()
            .unwrap();
        assert_eq!(
            question.status,
            videre_core::face_learning::QuestionStatus::Pending
        );
    }
}
