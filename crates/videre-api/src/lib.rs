//! Facade over videre's faces-labeling operations. Plain functions over an
//! open `rusqlite::Connection`, returning serde types and a shared `Error`.
//! Transport-agnostic on purpose: the axum `--faces` server is the in-repo
//! caller, but nothing here depends on it.

mod error;
mod faces;
mod images;
mod label;
mod pipeline_status;
mod stats;
mod types;

pub use error::{Error, Result};
pub use faces::{
    answer_question_with_learning, assign, assign_with_learning, cluster_detail, delete_person,
    delete_person_with_learning, dissolve_cluster, dissolve_cluster_with_learning,
    face_learning_event, face_learning_events, face_learning_status, faces_list,
    load_training_snapshot, new_person, new_person_with_learning, pending_identity_questions,
    persist_trained_profile, person_detail, refresh_identity_questions, remove_face,
    remove_face_with_learning, search_person, set_full_name, set_primary,
};
pub use images::{
    face_bytes_from_lookup, face_image_bytes, face_lookup, make_face_thumb, mime_for_ext,
    original_bytes_from_lookup, original_image_bytes, original_lookup, FaceLookup, OriginalLookup,
};
pub use label::sanitize_person_label;
pub use pipeline_status::{pipeline_status, PipelineRunStatus};
pub use stats::{library_stats, LibraryStats};
pub use types::{
    ClusterData, ClusterDetail, ClusterFaceData, FaceLearningStatus, FacesData,
    LearningAcknowledgement, PersonData, PersonDetail, PersonFaceData, QuestionAnswerOutcome,
    SingletonData, TeachingContext, TrainedProfileSummary,
};
