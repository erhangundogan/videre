use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileRecord {
    pub path: String,
    pub hash: String,
    /// BLAKE3 of the file's metadata alone (see `content_key`); `None` for a
    /// format that is not parsed. Records that the metadata changed while
    /// `hash`, the content key, stays the same.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meta_hash: Option<String>,
    pub size_bytes: u64,
    pub created_at: Option<String>,
    pub modified_at: Option<String>,
    pub ext: String,
    /// The type identified by the file's magic bytes, independent of its
    /// name. NULL for rows written before this column existed; a re-scan
    /// fills it. See `videre_core::mime_probe`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime: Option<String>,
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
    /// Video only: length in seconds, fractional. NULL for images and for rows
    /// written before 0.14.0.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_secs: Option<f64>,
    /// Video only: the container's format tag (`avc1`, `hvc1`), not a friendly
    /// codec name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub codec: Option<String>,
    /// Where `exif_date` came from at extraction: `exif`, `video` (the Apple
    /// key) or `mvhd`. The writer resolves the capture date from it, a
    /// Takeout sidecar and the file time (`videre_core::capture_date`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date_source: Option<String>,
    /// A video's `mvhd` creation time as the UTC instant it is, when its date
    /// came from there. Never serialised.
    #[serde(skip)]
    pub mvhd_unix: Option<i64>,
}

#[derive(Debug)]
pub struct DuplicateGroup {
    pub hash: String,
    pub files: Vec<FileRecord>,
}

/// Version of the --json output schema. Additive changes (new fields) do not
/// bump this; removals or renames would.
pub const SCHEMA_VERSION: u32 = 1;

/// The duplicates document's own version, apart from [`SCHEMA_VERSION`]:
/// version 2 lists groups by kind (`crate::duplicates`).
pub const DUPLICATES_SCHEMA_VERSION: u32 = 2;

/// One group in `dedupe --json` and MCP `find_duplicates`. A removable kind
/// splits into `keep` and `remove`; a review-only kind (`similar`) is a flat
/// `files` cluster, since no deletion is safe without judgment.
#[derive(Debug, Serialize)]
pub struct DuplicateGroupJson {
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keep: Option<FileRecord>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remove: Option<Vec<FileRecord>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub files: Option<Vec<FileRecord>>,
}

impl From<crate::duplicates::Group> for DuplicateGroupJson {
    fn from(group: crate::duplicates::Group) -> Self {
        let kind = group.kind.name();
        if group.kind.removable() {
            let mut files = group.files.into_iter();
            let keep = files.next();
            DuplicateGroupJson {
                kind,
                keep,
                remove: Some(files.collect()),
                files: None,
            }
        } else {
            DuplicateGroupJson {
                kind,
                keep: None,
                remove: None,
                files: Some(group.files),
            }
        }
    }
}

/// Top-level document for `dedupe --json` and the MCP `find_duplicates` tool.
/// Shared by both so they cannot silently diverge in shape. `unchecked`
/// counts resized candidates not yet compared (MCP never decodes).
#[derive(Debug, Serialize)]
pub struct FindDuplicatesJson {
    pub schema_version: u32,
    pub total_files: usize,
    pub kinds: Vec<&'static str>,
    pub unchecked: usize,
    pub groups: Vec<DuplicateGroupJson>,
}

/// Where `scan --json` wrote records: `"sqlite"` or `"jsonl"`, and the resolved path.
#[derive(Debug, Serialize)]
pub struct ScanOutputJson {
    pub kind: &'static str,
    pub path: String,
}

/// Top-level document for `scan --json`. Describes the scan itself (files
/// processed, where they were written), not duplicate data, that is
/// `dedupe`'s job, not scan's.
#[derive(Debug, Serialize)]
pub struct ScanJson {
    pub schema_version: u32,
    pub total_files: usize,
    pub output: ScanOutputJson,
}

/// Top-level document for `stats --json`. Reuses videre-core's/videre-api's
/// own serde types directly rather than re-declaring their fields here.
#[derive(Debug, Serialize)]
pub struct StatsJson {
    pub schema_version: u32,
    pub library: videre_core::library_stats::LibraryStats,
    /// Every file type, largest first; the text output shows the top 12.
    pub by_type: Vec<videre_core::library_stats::TypeBreakdown>,
    /// Conflicts between the filename extension and last scanned MIME.
    pub mismatches: videre_core::library_stats::MismatchReport,
    /// What videre stores for this library, largest first.
    pub disk_use: Vec<videre_core::disk::Usage>,
}

/// `videre status --json`: the shared status model, serialized as-is so the
/// CLI surface cannot drift from the core model it renders.
#[derive(Debug, Serialize)]
pub struct StatusJson {
    pub schema_version: u32,
    pub report: videre_core::status_report::StatusReport,
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
            error: ErrorBody {
                message: format!("{e:#}"),
            },
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
            meta_hash: None,
            size_bytes: 1024,
            created_at: Some("2023-01-01T00:00:00Z".to_string()),
            modified_at: Some("2024-01-01T00:00:00Z".to_string()),
            ext: "jpg".to_string(),
            mime: None,
            phash: None,
            exif_date: None,
            gps_lat: None,
            gps_lon: None,
            width: None,
            height: None,
            duration_secs: None,
            codec: None,
            date_source: None,
            mvhd_unix: None,
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
            meta_hash: None,
            size_bytes: 1024,
            created_at: None,
            modified_at: None,
            ext: "jpg".to_string(),
            mime: None,
            phash: None,
            exif_date: Some("2023-08-15T14:30:00".to_string()),
            gps_lat: Some(48.85),
            gps_lon: Some(2.35),
            width: Some(100),
            height: Some(80),
            duration_secs: None,
            codec: None,
            date_source: None,
            mvhd_unix: None,
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
            meta_hash: None,
            size_bytes: 0,
            created_at: None,
            modified_at: None,
            ext: "jpg".to_string(),
            mime: None,
            phash: None,
            exif_date: None,
            gps_lat: None,
            gps_lon: None,
            width: None,
            height: None,
            duration_secs: None,
            codec: None,
            date_source: None,
            mvhd_unix: None,
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
            meta_hash: None,
            size_bytes: 1,
            created_at: None,
            modified_at: None,
            ext: "jpg".to_string(),
            mime: None,
            phash: None,
            exif_date: None,
            gps_lat: None,
            gps_lon: None,
            width: None,
            height: None,
            duration_secs: None,
            codec: None,
            date_source: None,
            mvhd_unix: None,
        }
    }

    #[test]
    fn a_removable_group_splits_keep_and_remove() {
        let group = crate::duplicates::Group {
            kind: crate::duplicates::Kind::Resized,
            files: vec![rec("/keep.jpg", "a"), rec("/rm1.jpg", "b")],
        };
        let json = serde_json::to_value(DuplicateGroupJson::from(group)).unwrap();
        assert_eq!(json["kind"], "resized");
        assert_eq!(json["keep"]["path"], "/keep.jpg");
        assert_eq!(json["remove"][0]["path"], "/rm1.jpg");
        assert!(json.get("files").is_none());
    }

    #[test]
    fn a_similar_group_is_a_flat_cluster() {
        let group = crate::duplicates::Group {
            kind: crate::duplicates::Kind::Similar,
            files: vec![rec("/x.jpg", "111"), rec("/y.jpg", "222")],
        };
        let json = serde_json::to_value(DuplicateGroupJson::from(group)).unwrap();
        assert_eq!(json["files"].as_array().unwrap().len(), 2);
        assert!(json.get("keep").is_none() && json.get("remove").is_none());
    }

    #[test]
    fn the_duplicates_document_is_version_2() {
        let doc = FindDuplicatesJson {
            schema_version: DUPLICATES_SCHEMA_VERSION,
            total_files: 3,
            kinds: vec!["exact"],
            unchecked: 0,
            groups: vec![],
        };
        let json = serde_json::to_string(&doc).unwrap();
        assert!(json.starts_with("{\"schema_version\":2"));
    }

    #[test]
    fn scan_json_reports_output_kind_and_path() {
        let doc = ScanJson {
            schema_version: SCHEMA_VERSION,
            total_files: 5,
            output: ScanOutputJson {
                kind: "sqlite",
                path: "/tmp/hashes.db".to_string(),
            },
        };
        let json = serde_json::to_string(&doc).unwrap();
        assert!(json.starts_with("{\"schema_version\":1"));
        assert!(json.contains("\"total_files\":5"));
        assert!(json.contains("\"kind\":\"sqlite\""));
        assert!(json.contains("\"path\":\"/tmp/hashes.db\""));
    }

    #[test]
    fn error_json_contains_schema_version_and_message() {
        let err = anyhow::anyhow!("root cause").context("outer");
        let doc = ErrorJson::from_err(&err);
        let json = serde_json::to_string(&doc).unwrap();
        assert!(json.starts_with("{\"schema_version\":1"));
        assert!(json.contains("\"error\""));
        assert!(
            json.contains("outer"),
            "message must render the anyhow chain: {json}"
        );
        assert!(
            json.contains("root cause"),
            "chain rendered with {{e:#}}: {json}"
        );
    }
}
