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
}

#[derive(Debug)]
pub struct DuplicateGroup {
    pub hash: String,
    pub files: Vec<FileRecord>,
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
        };
        let json = serde_json::to_string(&record).unwrap();
        assert!(json.contains("\"path\":\"/photos/img.jpg\""));
        assert!(json.contains("\"hash\":\"abc123\""));
        assert!(!json.contains("phash")); // None fields skipped
    }

    #[test]
    fn file_record_deserializes_from_json() {
        let json = r#"{"path":"/a.jpg","hash":"x","size_bytes":100,"created_at":null,"modified_at":null,"ext":"jpg"}"#;
        let record: FileRecord = serde_json::from_str(json).unwrap();
        assert_eq!(record.path, "/a.jpg");
        assert_eq!(record.ext, "jpg");
    }
}
