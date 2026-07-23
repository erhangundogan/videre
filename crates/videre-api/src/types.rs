use serde::Serialize;

/// One labeled person: their confirmed faces plus a representative face id
/// (the primary, or lowest id) used as the card thumbnail.
#[derive(Serialize, Clone)]
pub struct PersonData {
    pub label: String,
    pub face_ids: Vec<i64>,
    pub representative_id: i64,
    pub hashes: Vec<String>,
}

/// One unassigned cluster (green section in the labeling UI).
#[derive(Serialize, Clone)]
pub struct ClusterData {
    pub cluster_id: i64,
    pub face_ids: Vec<i64>,
    pub hashes: Vec<String>,
}

/// One unclustered, unassigned face (orange section).
#[derive(Serialize, Clone)]
pub struct SingletonData {
    pub face_id: i64,
    pub hash: String,
}

/// Top-level payload for the labeling page.
#[derive(Serialize, Clone)]
pub struct FacesData {
    pub people: Vec<PersonData>,
    pub clusters: Vec<ClusterData>,
    pub singletons: Vec<SingletonData>,
}

/// One face row on a cluster detail page.
#[derive(Serialize, Clone)]
pub struct ClusterFaceData {
    pub face_id: i64,
    pub hash: String,
    pub path: String,
}

/// Cluster detail: every face in one unassigned cluster.
#[derive(Serialize, Clone)]
pub struct ClusterDetail {
    pub cluster_id: i64,
    pub faces: Vec<ClusterFaceData>,
}

/// One face row on a person detail page. `is_primary` marks the current
/// default photo (the person's thumbnail on the labeling page).
#[derive(Serialize, Clone)]
pub struct PersonFaceData {
    pub face_id: i64,
    pub hash: String,
    pub path: String,
    pub is_primary: bool,
}

/// Person detail: every confirmed face for one person.
#[derive(Serialize, Clone)]
pub struct PersonDetail {
    pub label: String,
    pub faces: Vec<PersonFaceData>,
}
