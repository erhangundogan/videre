//! Facade over videre's faces-labeling operations. Plain functions over an
//! open `rusqlite::Connection`, returning serde types and a shared `Error`.
//! Called by both the axum `--faces` server and the Tauri desktop app.

mod error;
mod faces;
mod label;
mod types;

pub use error::{Error, Result};
pub use faces::{cluster_detail, faces_list, person_detail, search_person};
pub use label::sanitize_person_label;
pub use types::{
    ClusterData, ClusterDetail, ClusterFaceData, FacesData, PersonData, PersonDetail,
    PersonFaceData, SingletonData,
};
