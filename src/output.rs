use crate::types::{DuplicateGroup, FileRecord};
use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::{BufWriter, Write};
use std::path::Path;

// KEEP candidate sort key: exif_date wins; otherwise oldest of created_at / modified_at.
fn best_date(r: &FileRecord) -> &str {
    if let Some(d) = r.exif_date.as_deref() {
        return d;
    }
    match (r.created_at.as_deref(), r.modified_at.as_deref()) {
        (Some(c), Some(m)) => if c < m { c } else { m },
        (Some(c), None) => c,
        (None, Some(m)) => m,
        (None, None) => "",
    }
}

pub fn find_duplicate_groups(records: &[FileRecord]) -> Vec<DuplicateGroup> {
    let mut map: HashMap<String, Vec<FileRecord>> = HashMap::new();
    for record in records {
        map.entry(record.hash.clone()).or_default().push(record.clone());
    }
    let mut groups: Vec<DuplicateGroup> = map
        .into_iter()
        .filter(|(_, files)| files.len() > 1)
        .map(|(hash, mut files)| {
            // Oldest date first: exif_date wins; falls back to min(created_at, modified_at)
            files.sort_by(|a, b| best_date(a).cmp(best_date(b)));
            DuplicateGroup { hash, files }
        })
        .collect();
    groups.sort_by(|a, b| a.hash.cmp(&b.hash));
    groups
}

/// Prints REMOVE candidates to stdout, one path per line: ready for piping.
/// The first file in each group (oldest date = likely original) is kept; the rest are printed.
pub fn print_losers(groups: &[DuplicateGroup]) {
    for group in groups {
        for file in group.files.iter().skip(1) {
            println!("{}", file.path);
        }
    }
}

pub fn find_similar_groups(records: &[FileRecord], threshold: u32) -> Vec<DuplicateGroup> {
    let with_phash: Vec<&FileRecord> = records.iter().filter(|r| r.phash.is_some()).collect();
    let mut visited = vec![false; with_phash.len()];
    let mut groups: Vec<DuplicateGroup> = Vec::new();

    for i in 0..with_phash.len() {
        if visited[i] { continue; }
        let mut group = vec![with_phash[i].clone()];
        for j in (i + 1)..with_phash.len() {
            if visited[j] { continue; }
            let dist = crate::hasher::hamming(
                with_phash[i].phash.unwrap(),
                with_phash[j].phash.unwrap(),
            );
            if dist <= threshold {
                group.push(with_phash[j].clone());
                visited[j] = true;
            }
        }
        if group.len() > 1 {
            group.sort_by(|a, b| best_date(a).cmp(best_date(b)));
            groups.push(DuplicateGroup {
                hash: format!("phash:{:016x}", with_phash[i].phash.unwrap()),
                files: group,
            });
        }
    }
    groups
}

pub fn append_records(
    records: &[FileRecord],
    output_path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(output_path)?;
    let mut writer = BufWriter::new(file);
    for record in records {
        let line = serde_json::to_string(record)?;
        writeln!(writer, "{}", line)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::FileRecord;
    use std::fs;
    use tempfile::tempdir;

    fn make_record(path: &str, hash: &str) -> FileRecord {
        FileRecord {
            path: path.to_string(),
            hash: hash.to_string(),
            size_bytes: 100,
            created_at: None,
            modified_at: Some("2023-01-01T00:00:00+00:00".to_string()),
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
    fn append_records_writes_one_line_per_record() {
        let dir = tempdir().unwrap();
        let output = dir.path().join("hashes");

        let records = vec![
            make_record("/a.jpg", "hash1"),
            make_record("/b.jpg", "hash2"),
        ];
        append_records(&records, &output).unwrap();

        let content = fs::read_to_string(&output).unwrap();
        let lines: Vec<_> = content.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("hash1"));
        assert!(lines[1].contains("hash2"));
    }

    #[test]
    fn append_records_appends_on_second_call() {
        let dir = tempdir().unwrap();
        let output = dir.path().join("hashes");

        append_records(&[make_record("/a.jpg", "h1")], &output).unwrap();
        append_records(&[make_record("/b.jpg", "h2")], &output).unwrap();

        let content = fs::read_to_string(&output).unwrap();
        assert_eq!(content.lines().count(), 2);
    }

    #[test]
    fn find_duplicate_groups_groups_by_hash() {
        let records = vec![
            make_record("/a.jpg", "same"),
            make_record("/b.jpg", "same"),
            make_record("/c.jpg", "unique"),
        ];
        let groups = find_duplicate_groups(&records);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].files.len(), 2);
        assert_eq!(groups[0].hash, "same");
    }

    #[test]
    fn find_duplicate_groups_sorts_files_oldest_first() {
        let mut a = make_record("/a.jpg", "h");
        let mut b = make_record("/b.jpg", "h");
        a.modified_at = Some("2020-01-01T00:00:00+00:00".to_string());
        b.modified_at = Some("2024-01-01T00:00:00+00:00".to_string());

        let groups = find_duplicate_groups(&[b, a]);
        assert_eq!(groups[0].files[0].path, "/a.jpg"); // oldest first = keep
    }

    #[test]
    fn find_duplicate_groups_prefers_exif_date_for_sort() {
        let mut a = make_record("/a.jpg", "h");
        let mut b = make_record("/b.jpg", "h");
        // a has newer modified_at but older exif_date: exif wins, a should be KEEP
        a.modified_at = Some("2024-01-01T00:00:00+00:00".to_string());
        a.exif_date = Some("2019-06-01T10:00:00".to_string());
        b.modified_at = Some("2020-01-01T00:00:00+00:00".to_string());

        let groups = find_duplicate_groups(&[b, a]);
        assert_eq!(groups[0].files[0].path, "/a.jpg"); // exif_date older → keep
    }

    #[test]
    fn find_duplicate_groups_returns_empty_when_no_dupes() {
        let records = vec![
            make_record("/a.jpg", "h1"),
            make_record("/b.jpg", "h2"),
        ];
        assert!(find_duplicate_groups(&records).is_empty());
    }

    #[test]
    fn find_similar_groups_clusters_by_hamming_distance() {
        let mut a = make_record("/a.jpg", "unique_a");
        let mut b = make_record("/b.jpg", "unique_b");
        let mut c = make_record("/c.jpg", "unique_c");
        a.phash = Some(0b0000_0000u64);
        b.phash = Some(0b0000_0001u64);
        c.phash = Some(0xFFFF_FFFF_FFFF_FFFFu64);

        let groups = find_similar_groups(&[a, b, c], 10);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].files.len(), 2);
        let paths: Vec<&str> = groups[0].files.iter().map(|f| f.path.as_str()).collect();
        assert!(paths.contains(&"/a.jpg"));
        assert!(paths.contains(&"/b.jpg"));
        assert!(!paths.contains(&"/c.jpg"));
    }

    #[test]
    fn find_similar_groups_empty_when_all_unique() {
        let mut a = make_record("/a.jpg", "h1");
        let mut b = make_record("/b.jpg", "h2");
        a.phash = Some(0u64);
        b.phash = Some(u64::MAX);
        assert!(find_similar_groups(&[a, b], 10).is_empty());
    }
}
