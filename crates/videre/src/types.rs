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
}
