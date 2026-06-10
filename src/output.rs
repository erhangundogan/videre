use crate::types::{DuplicateGroup, FileRecord};
use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::{BufWriter, Write};
use std::path::Path;

pub fn find_duplicate_groups(records: &[FileRecord]) -> Vec<DuplicateGroup> {
    let mut map: HashMap<String, Vec<FileRecord>> = HashMap::new();
    for record in records {
        map.entry(record.hash.clone()).or_default().push(record.clone());
    }
    let mut groups: Vec<DuplicateGroup> = map
        .into_iter()
        .filter(|(_, files)| files.len() > 1)
        .map(|(hash, mut files)| {
            files.sort_by(|a, b| a.modified_at.cmp(&b.modified_at));
            DuplicateGroup { hash, files }
        })
        .collect();
    groups.sort_by(|a, b| a.hash.cmp(&b.hash));
    groups
}

pub fn print_duplicate_groups(groups: &[DuplicateGroup]) {
    println!("\nFound {} duplicate group(s):\n", groups.len());
    for (i, group) in groups.iter().enumerate() {
        println!(
            "Group {} (hash: {}...):",
            i + 1,
            &group.hash[..group.hash.len().min(8)]
        );
        for (j, file) in group.files.iter().enumerate() {
            let label = if j == 0 { "[ORIGINAL?]" } else { "           " };
            println!(
                "  {} {}  modified: {}",
                label,
                file.path,
                file.modified_at.as_deref().unwrap_or("unknown")
            );
        }
        println!();
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
            group.sort_by(|a, b| a.modified_at.cmp(&b.modified_at));
            groups.push(DuplicateGroup {
                hash: format!("phash:{:016x}", with_phash[i].phash.unwrap()),
                files: group,
            });
        }
    }
    groups
}

pub fn print_similar_groups(groups: &[DuplicateGroup]) {
    println!("\nFound {} visually similar group(s):\n", groups.len());
    for (i, group) in groups.iter().enumerate() {
        println!("Similar Group {}:", i + 1);
        for (j, file) in group.files.iter().enumerate() {
            let label = if j == 0 { "[ORIGINAL?]" } else { "           " };
            println!(
                "  {} {}  modified: {}",
                label,
                file.path,
                file.modified_at.as_deref().unwrap_or("unknown")
            );
        }
        println!();
    }
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

        let groups = find_duplicate_groups(&[b, a]); // intentionally reversed
        assert_eq!(groups[0].files[0].path, "/a.jpg"); // oldest first
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
        // hashes differ by 1 bit → similar
        a.phash = Some(0b0000_0000u64);
        b.phash = Some(0b0000_0001u64);
        c.phash = Some(0xFFFF_FFFF_FFFF_FFFFu64); // far away (64 bits differ from a)

        let groups = find_similar_groups(&[a, b, c], 10);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].files.len(), 2);
        let paths: Vec<&str> = groups[0].files.iter().map(|f| f.path.as_str()).collect();
        assert!(paths.contains(&"/a.jpg"), "expected /a.jpg in group");
        assert!(paths.contains(&"/b.jpg"), "expected /b.jpg in group");
        assert!(!paths.contains(&"/c.jpg"), "/c.jpg should not be in group");
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
