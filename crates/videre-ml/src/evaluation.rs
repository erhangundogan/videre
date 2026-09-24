use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::time::Instant;
use videre_core::face_learning::{
    evaluate_candidate, CandidatePredictions, ClusterPrediction, DatasetEvaluation,
    EvaluationError, EVALUATION_PROTOCOL_VERSION,
};

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClusteringParameters {
    pub eps: f32,
    pub min_cluster_size: usize,
    pub merge_sim: f32,
    pub min_face_size: f32,
    pub max_generic_sim: f32,
    pub max_landmark_error: f32,
    pub min_blur: f32,
    pub attach_sim: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BaselineEvaluation {
    pub protocol_version: u32,
    pub parameters: ClusteringParameters,
    pub total_faces: usize,
    pub labeled_faces: usize,
    pub clustering_time_ms: u64,
    pub evaluation: DatasetEvaluation,
}

#[derive(Debug)]
pub enum ReplayError {
    Database(rusqlite::Error),
    Evaluation(EvaluationError),
    InvalidParameter(&'static str),
}

impl fmt::Display for ReplayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Database(error) => write!(f, "could not read face data: {error}"),
            Self::Evaluation(error) => error.fmt(f),
            Self::InvalidParameter(name) => write!(f, "invalid clustering parameter {name}"),
        }
    }
}

impl std::error::Error for ReplayError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Database(error) => Some(error),
            Self::Evaluation(error) => Some(error),
            Self::InvalidParameter(_) => None,
        }
    }
}

type ClusteringFace = (i64, Vec<f32>, f32, Option<String>, Option<f32>);

fn ordered_faces_for_evaluation(mut faces: Vec<ClusteringFace>) -> Vec<ClusteringFace> {
    faces.sort_by_key(|face| face.0);
    faces
}

fn validate_parameters(parameters: &ClusteringParameters) -> Result<(), ReplayError> {
    fn in_range(value: f32, range: std::ops::RangeInclusive<f32>) -> bool {
        value.is_finite() && range.contains(&value)
    }
    if !in_range(parameters.eps, 0.0..=2.0) {
        return Err(ReplayError::InvalidParameter("eps"));
    }
    if parameters.min_cluster_size == 0 {
        return Err(ReplayError::InvalidParameter("min_cluster_size"));
    }
    for (value, name) in [
        (parameters.merge_sim, "merge_sim"),
        (parameters.max_generic_sim, "max_generic_sim"),
        (parameters.attach_sim, "attach_sim"),
    ] {
        if !in_range(value, -1.0..=1.0) {
            return Err(ReplayError::InvalidParameter(name));
        }
    }
    for (value, name) in [
        (parameters.min_face_size, "min_face_size"),
        (parameters.max_landmark_error, "max_landmark_error"),
        (parameters.min_blur, "min_blur"),
    ] {
        if !value.is_finite() || value < 0.0 {
            return Err(ReplayError::InvalidParameter(name));
        }
    }
    Ok(())
}

impl From<rusqlite::Error> for ReplayError {
    fn from(value: rusqlite::Error) -> Self {
        Self::Database(value)
    }
}

impl From<EvaluationError> for ReplayError {
    fn from(value: EvaluationError) -> Self {
        Self::Evaluation(value)
    }
}

pub fn evaluate_current_clustering(
    conn: &Connection,
    parameters: &ClusteringParameters,
) -> Result<BaselineEvaluation, ReplayError> {
    validate_parameters(parameters)?;
    let faces =
        ordered_faces_for_evaluation(videre_core::face_db::load_faces_for_clustering(conn)?);
    let labels = videre_core::face_db::load_confirmed_face_labels(conn)?;
    if labels.is_empty() {
        return Err(EvaluationError::NoLabeledFaces.into());
    }

    let started = Instant::now();
    let assignments = crate::pipeline::cluster_with_quality_gate(
        &faces,
        parameters.eps,
        parameters.min_cluster_size,
        parameters.merge_sim,
        parameters.min_face_size,
        parameters.max_generic_sim,
        parameters.max_landmark_error,
        parameters.min_blur,
        parameters.attach_sim,
        true,
    );
    let elapsed = started.elapsed().as_millis().min(u64::MAX as u128) as u64;

    let labeled_ids: std::collections::BTreeSet<i64> =
        labels.iter().map(|label| label.face_id).collect();
    let predictions = CandidatePredictions {
        clusters: assignments
            .into_iter()
            .filter(|(face_id, _)| labeled_ids.contains(face_id))
            .map(|(face_id, cluster_id)| ClusterPrediction::new(face_id, cluster_id))
            .collect(),
        suggestions: Vec::new(),
    };
    let evaluation = evaluate_candidate(&labels, &predictions)?;

    Ok(BaselineEvaluation {
        protocol_version: EVALUATION_PROTOCOL_VERSION,
        parameters: *parameters,
        total_faces: faces.len(),
        labeled_faces: labels.len(),
        clustering_time_ms: elapsed,
        evaluation,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use half::f16;
    use rusqlite::{params, Connection};
    use videre_core::face_learning::{EvaluationError, EVALUATION_PROTOCOL_VERSION};

    fn parameters() -> ClusteringParameters {
        ClusteringParameters {
            eps: 0.10,
            min_cluster_size: 2,
            merge_sim: 1.0,
            min_face_size: 0.0,
            max_generic_sim: 1.0,
            max_landmark_error: f32::MAX,
            min_blur: 0.0,
            attach_sim: 1.0,
        }
    }

    fn embedding(angle: f32) -> Vec<u8> {
        let radians = angle.to_radians();
        [radians.cos(), radians.sin()]
            .into_iter()
            .flat_map(|value| f16::from_f32(value).to_le_bytes())
            .collect()
    }

    fn connection() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        videre_core::face_db::create_faces_table(&conn).unwrap();
        // Every face's photo exists: these tests are not about missing
        //  photos, and the readers list, group and offer only faces some file still has.
        conn.execute_batch(
            "CREATE VIEW file_hashes AS SELECT DISTINCT hash, '/p/' || hash AS path FROM faces;",
        )
        .unwrap();
        conn
    }

    fn insert_face(
        conn: &Connection,
        id: i64,
        angle: f32,
        label: Option<&str>,
        cluster_id: Option<i64>,
    ) {
        // A labeled face needs its people parent under enforced foreign
        // keys; unlabeled faces insert without one.
        if let Some(name) = label {
            conn.execute(
                "INSERT INTO people (name, full_name) VALUES (?1, ?1)
                 ON CONFLICT(name) DO NOTHING",
                params![name],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO faces (
                id, hash, bbox, embedding, cluster_id, person_label, confirmed, blur
             ) VALUES (?1, ?2, '0,0,100,100', ?3, ?4, ?5, ?6, 100.0)",
            params![
                id,
                format!("hash-{id}"),
                embedding(angle),
                cluster_id,
                label,
                i64::from(label.is_some()),
            ],
        )
        .unwrap();
    }

    fn snapshot(conn: &Connection) -> Vec<(i64, Option<i64>, Option<String>, i64)> {
        conn.prepare("SELECT id, cluster_id, person_label, confirmed FROM faces ORDER BY id")
            .unwrap()
            .query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    #[test]
    fn replays_all_labeled_faces_deterministically_without_writing() {
        let conn = connection();
        insert_face(&conn, 1, 0.0, Some("alpha"), Some(91));
        insert_face(&conn, 2, 3.0, Some("alpha"), None);
        insert_face(&conn, 3, 90.0, Some("beta"), Some(92));
        insert_face(&conn, 4, 93.0, Some("beta"), None);
        let before = snapshot(&conn);

        let first = evaluate_current_clustering(&conn, &parameters()).unwrap();
        let second = evaluate_current_clustering(&conn, &parameters()).unwrap();

        assert_eq!(first.protocol_version, EVALUATION_PROTOCOL_VERSION);
        assert_eq!(first.parameters, parameters());
        assert_eq!(first.total_faces, 4);
        assert_eq!(first.labeled_faces, 4);
        assert_eq!(first.evaluation.clustering.true_positive_pairs, 2);
        assert_eq!(first.evaluation.clustering.false_positive_pairs, 0);
        assert_eq!(first.evaluation.clustering.false_negative_pairs, 0);
        assert_eq!(first.evaluation, second.evaluation);
        assert_eq!(snapshot(&conn), before);
    }

    #[test]
    fn no_labels_returns_the_public_error() {
        let conn = connection();
        insert_face(&conn, 1, 0.0, None, None);
        assert!(matches!(
            evaluate_current_clustering(&conn, &parameters()),
            Err(ReplayError::Evaluation(EvaluationError::NoLabeledFaces))
        ));
    }

    #[test]
    fn one_identity_is_valid_and_keeps_undefined_pair_metrics_explicit() {
        let conn = connection();
        insert_face(&conn, 1, 0.0, Some("alpha"), None);
        let report = evaluate_current_clustering(&conn, &parameters()).unwrap();
        assert_eq!(report.evaluation.clustering.pair_precision, None);
        assert_eq!(report.evaluation.clustering.pair_recall, None);
        assert_eq!(report.evaluation.suggestions, None);
    }

    #[test]
    fn evaluation_orders_faces_by_id_before_clustering() {
        let faces = vec![
            (3, vec![0.0], 1.0, None, None),
            (1, vec![0.0], 1.0, None, None),
            (2, vec![0.0], 1.0, None, None),
        ];
        assert_eq!(
            ordered_faces_for_evaluation(faces)
                .into_iter()
                .map(|face| face.0)
                .collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
    }

    #[test]
    fn invalid_parameters_fail_before_replay() {
        let conn = connection();
        insert_face(&conn, 1, 0.0, Some("alpha"), None);
        let mut invalid = parameters();
        invalid.eps = f32::NAN;
        assert!(matches!(
            evaluate_current_clustering(&conn, &invalid),
            Err(ReplayError::InvalidParameter("eps"))
        ));

        let mut invalid = parameters();
        invalid.min_cluster_size = 0;
        assert!(matches!(
            evaluate_current_clustering(&conn, &invalid),
            Err(ReplayError::InvalidParameter("min_cluster_size"))
        ));
    }
}
