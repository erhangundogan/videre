use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileRecord {
    pub path: String,
    pub hash: String,
    pub size_bytes: u64,
    pub created_at: Option<String>,
    pub modified_at: Option<String>,
    pub ext: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phash: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exif_date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gps_lat: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gps_lon: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
}

#[derive(Debug)]
pub struct DuplicateGroup {
    pub hash: String,
    pub files: Vec<FileRecord>,
}

/// Version of the --json output schema. Additive changes (new fields) do not
/// bump this; removals or renames would.
pub const SCHEMA_VERSION: u32 = 1;

/// One exact-duplicate group in `dedupe --json`: byte-identical files split
/// into the one to keep (oldest by the KEEP rule) and the rest to remove.
#[derive(Debug, Serialize)]
pub struct DupGroupJson {
    pub hash: String,
    pub keep: FileRecord,
    pub remove: Vec<FileRecord>,
}

impl From<DuplicateGroup> for DupGroupJson {
    fn from(group: DuplicateGroup) -> Self {
        let mut files = group.files.into_iter();
        let keep = files.next().expect("duplicate groups always have >= 2 files");
        DupGroupJson { hash: group.hash, keep, remove: files.collect() }
    }
}

/// One perceptual-hash near-duplicate group in `dedupe --json --similar`.
/// Deliberately a flat review cluster with no keep/remove split: these files
/// are NOT byte-identical, so no deletion is safe without human/agent judgment.
#[derive(Debug, Serialize)]
pub struct SimilarGroupJson {
    pub hash: String,
    pub files: Vec<FileRecord>,
}

impl From<DuplicateGroup> for SimilarGroupJson {
    fn from(group: DuplicateGroup) -> Self {
        SimilarGroupJson { hash: group.hash, files: group.files }
    }
}

/// Top-level document for `dedupe --json`.
#[derive(Debug, Serialize)]
pub struct DedupeJson {
    pub schema_version: u32,
    pub scanned: usize,
    pub duplicate_groups: Vec<DupGroupJson>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub similar_groups: Option<Vec<SimilarGroupJson>>,
}

/// Error document: in --json mode stdout always carries exactly one valid JSON
/// object, so runtime failures are emitted as this instead of leaving stdout empty.
#[derive(Debug, Serialize)]
pub struct ErrorJson {
    pub schema_version: u32,
    pub error: ErrorBody,
}

#[derive(Debug, Serialize)]
pub struct ErrorBody {
    pub message: String,
}

impl ErrorJson {
    pub fn from_err(e: &anyhow::Error) -> Self {
        ErrorJson {
            schema_version: SCHEMA_VERSION,
            error: ErrorBody { message: format!("{e:#}") },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_record_serializes_to_json() {
        let record = FileRecord {
            path: "/photos/img.jpg".to_string(),
            hash: "abc123".to_string(),
            size_bytes: 1024,
            created_at: Some("2023-01-01T00:00:00Z".to_string()),
            modified_at: Some("2024-01-01T00:00:00Z".to_string()),
            ext: "jpg".to_string(),
            phash: None,
            exif_date: None,
            gps_lat: None,
            gps_lon: None,
            width: None,
            height: None,
        };
        let json = serde_json::to_string(&record).unwrap();
        assert!(json.contains("\"path\":\"/photos/img.jpg\""));
        assert!(json.contains("\"hash\":\"abc123\""));
        assert!(!json.contains("phash")); // None fields skipped
    }

    #[test]
    fn file_record_deserializes_from_json() {
        let json = r#"{"path":"/a.jpg","hash":"x","size_bytes":100,"created_at":null,"modified_at":null,"ext":"jpg","phash":null,"exif_date":null,"gps_lat":null,"gps_lon":null,"width":null,"height":null}"#;
        let record: FileRecord = serde_json::from_str(json).unwrap();
        assert_eq!(record.path, "/a.jpg");
        assert_eq!(record.ext, "jpg");
    }

    #[test]
    fn file_record_exif_fields_serialize_when_present() {
        let record = FileRecord {
            path: "/photos/img.jpg".to_string(),
            hash: "abc123".to_string(),
            size_bytes: 1024,
            created_at: None,
            modified_at: None,
            ext: "jpg".to_string(),
            phash: None,
            exif_date: Some("2023-08-15T14:30:00".to_string()),
            gps_lat: Some(48.85),
            gps_lon: Some(2.35),
            width: Some(100),
            height: Some(80),
        };
        let json = serde_json::to_string(&record).unwrap();
        assert!(json.contains("\"exif_date\":\"2023-08-15T14:30:00\""));
        assert!(json.contains("\"gps_lat\":48.85"));
        assert!(json.contains("\"gps_lon\":2.35"));
        assert!(json.contains("\"width\":100"));
        assert!(json.contains("\"height\":80"));
    }

    #[test]
    fn file_record_exif_fields_absent_when_none() {
        let record = FileRecord {
            path: "/a.jpg".to_string(),
            hash: "x".to_string(),
            size_bytes: 0,
            created_at: None,
            modified_at: None,
            ext: "jpg".to_string(),
            phash: None,
            exif_date: None,
            gps_lat: None,
            gps_lon: None,
            width: None,
            height: None,
        };
        let json = serde_json::to_string(&record).unwrap();
        assert!(!json.contains("exif_date"));
        assert!(!json.contains("gps_lat"));
        assert!(!json.contains("gps_lon"));
        assert!(!json.contains("width"));
        assert!(!json.contains("height"));
    }

    fn rec(path: &str, hash: &str) -> FileRecord {
        FileRecord {
            path: path.to_string(),
            hash: hash.to_string(),
            size_bytes: 1,
            created_at: None,
            modified_at: None,
            ext: "jpg".to_string(),
            phash: None,
            exif_date: None,
            gps_lat: None,
            gps_lon: None,
            width: None,
            height: None,
        }
    }

    #[test]
    fn dup_group_json_splits_keep_and_remove() {
        let group = DuplicateGroup {
            hash: "h".to_string(),
            files: vec![rec("/keep.jpg", "h"), rec("/rm1.jpg", "h"), rec("/rm2.jpg", "h")],
        };
        let json_group = DupGroupJson::from(group);
        assert_eq!(json_group.keep.path, "/keep.jpg");
        assert_eq!(json_group.remove.len(), 2);
        assert_eq!(json_group.remove[0].path, "/rm1.jpg");
    }

    #[test]
    fn dedupe_json_omits_similar_groups_when_none() {
        let doc = DedupeJson {
            schema_version: SCHEMA_VERSION,
            scanned: 3,
            duplicate_groups: vec![],
            similar_groups: None,
        };
        let json = serde_json::to_string(&doc).unwrap();
        assert!(json.starts_with("{\"schema_version\":1"));
        assert!(!json.contains("similar_groups"));
    }

    #[test]
    fn dedupe_json_includes_similar_groups_when_some() {
        let doc = DedupeJson {
            schema_version: SCHEMA_VERSION,
            scanned: 2,
            duplicate_groups: vec![],
            similar_groups: Some(vec![SimilarGroupJson {
                hash: "phash:00000000000000ff".to_string(),
                files: vec![rec("/x.jpg", "111"), rec("/y.jpg", "222")],
            }]),
        };
        let json = serde_json::to_string(&doc).unwrap();
        assert!(json.contains("\"similar_groups\""));
        assert!(json.contains("\"files\""));
        assert!(!json.contains("\"keep\""), "similar groups are flat clusters, not keep/remove");
    }

    #[test]
    fn error_json_contains_schema_version_and_message() {
        let err = anyhow::anyhow!("root cause").context("outer");
        let doc = ErrorJson::from_err(&err);
        let json = serde_json::to_string(&doc).unwrap();
        assert!(json.starts_with("{\"schema_version\":1"));
        assert!(json.contains("\"error\""));
        assert!(json.contains("outer"), "message must render the anyhow chain: {json}");
        assert!(json.contains("root cause"), "chain rendered with {{e:#}}: {json}");
    }
}
