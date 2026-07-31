//! Facade over videre's faces-labeling operations. Plain functions over an
//! open `rusqlite::Connection`, returning serde types and a shared `Error`.
//! Called by both the axum `--faces` server and the Tauri desktop app.

mod error;
mod faces;
mod images;
mod label;
mod pipeline_status;
mod stats;
mod types;

pub use error::{Error, Result};
pub use faces::{
    assign, cluster_detail, delete_person, dissolve_cluster, faces_list, new_person, person_detail,
    remove_face, rename_person, search_person, set_primary,
};
pub use images::{
    face_bytes_from_lookup, face_image_bytes, face_lookup, mime_for_ext, original_bytes_from_lookup,
    original_image_bytes, original_lookup, FaceLookup, OriginalLookup,
};
pub use label::sanitize_person_label;
pub use pipeline_status::{pipeline_status, PipelineRunStatus};
pub use stats::{library_stats, LibraryStats};
pub use types::{
    ClusterData, ClusterDetail, ClusterFaceData, FacesData, PersonData, PersonDetail,
    PersonFaceData, SingletonData,
};
