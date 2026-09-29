//! What `videre watch` did for each file of a batch, read back from the
//! database after the batch rather than taken from what the stages claimed,
//! so the line a user reads is the state every other command will see.

/// One stage's outcome for one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Step {
    Done(String),
    /// Not done, with the reason when one is known.
    Missing(String),
}

/// Everything a batch did for one file, in stage order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FileFacts {
    pub path: String,
    pub steps: Vec<Step>,
}

impl FileFacts {
    fn complete(&self) -> bool {
        self.steps.iter().all(|s| matches!(s, Step::Done(_)))
    }
}

/// Up to this many files, one line each; above it, totals plus the files that
/// were not completed.
pub(crate) const PER_FILE_LINES: usize = 20;

/// The report lines for a batch.
pub(crate) fn render(files: &[FileFacts]) -> Vec<String> {
    if files.len() <= PER_FILE_LINES {
        return files.iter().map(line).collect();
    }
    let complete = files.iter().filter(|f| f.complete()).count();
    let mut out = vec![format!(
        "{} files: {complete} complete, {} not yet",
        files.len(),
        files.len() - complete
    )];
    let incomplete: Vec<&FileFacts> = files.iter().filter(|f| !f.complete()).collect();
    for f in incomplete.iter().take(PER_FILE_LINES) {
        out.push(line(f));
    }
    if incomplete.len() > PER_FILE_LINES {
        out.push(format!(
            "...and {} more not yet complete",
            incomplete.len() - PER_FILE_LINES
        ));
    }
    out
}

fn line(f: &FileFacts) -> String {
    let parts: Vec<&str> = f
        .steps
        .iter()
        .map(|s| match s {
            Step::Done(t) | Step::Missing(t) => t.as_str(),
        })
        .collect();
    format!(
        "{}: {}",
        crate::display_path::escape_controls(&f.path),
        parts.join(", ")
    )
}

/// Which stages the batch ran, and what the drain knows about why one did not.
pub(crate) struct Stages {
    pub faces: bool,
    pub embed: bool,
    pub location: bool,
    /// Set when the embed stage could not run, with the reason.
    pub embed_skipped: Option<String>,
}

const FACE_EXTS: &[&str] = &["jpg", "jpeg", "png", "gif", "webp", "bmp", "tiff", "heic"];

/// Read what the batch left for each `(path, hash)` row. Paths are shown
/// relative to `root`. `model_id` is the embedding model the library uses.
pub(crate) fn gather(
    conn: &rusqlite::Connection,
    root: &std::path::Path,
    rows: &[(String, String)],
    stages: &Stages,
    model_id: &str,
    embeddable: &std::collections::HashSet<String>,
) -> anyhow::Result<Vec<FileFacts>> {
    use rusqlite::OptionalExtension;
    let has = |table: &str| videre_core::db::table_exists(conn, table).unwrap_or(false);
    let (faces_ok, places_ok) = (
        has("faces") && has("faces_scanned"),
        has("location_clusters"),
    );
    let classes_ok = has("classifications");
    let emb_ok = conn
        .query_row(
            "SELECT 1 FROM pragma_database_list WHERE name = 'emb'",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    let mut out = Vec::with_capacity(rows.len());
    for (path, hash) in rows {
        let shown = std::path::Path::new(path)
            .strip_prefix(root)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| path.clone());
        let mut steps = vec![Step::Done("scanned".into())];
        let (ext, gps): (Option<String>, bool) = conn.query_row(
            "SELECT lower(COALESCE(ext, '')), gps_lat IS NOT NULL FROM file_hashes WHERE path = ?1",
            [path],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let ext = ext.unwrap_or_default();

        if stages.faces && FACE_EXTS.contains(&ext.as_str()) {
            let scanned = faces_ok
                && conn
                    .query_row(
                        "SELECT 1 FROM faces_scanned WHERE hash = ?1",
                        [hash],
                        |_| Ok(()),
                    )
                    .optional()?
                    .is_some();
            if !scanned {
                steps.push(Step::Missing("faces not detected yet".into()));
            } else {
                let (n, grouped): (i64, i64) = conn.query_row(
                    "SELECT COUNT(*), COUNT(cluster_id) + \
                     SUM(CASE WHEN confirmed = 1 AND person_label IS NOT NULL THEN 1 ELSE 0 END) \
                     FROM faces WHERE hash = ?1",
                    [hash],
                    |r| Ok((r.get(0)?, r.get::<_, Option<i64>>(1)?.unwrap_or(0))),
                )?;
                steps.push(Step::Done(match n {
                    0 => "no faces".into(),
                    1 => format!("1 face ({grouped} grouped)"),
                    n => format!("{n} faces ({grouped} grouped)"),
                }));
            }
        }

        if stages.embed && embeddable.contains(hash) {
            let embedded = emb_ok
                && conn
                    .query_row(
                        "SELECT 1 FROM emb.embeddings WHERE hash = ?1 AND model_id = ?2",
                        [hash, model_id],
                        |_| Ok(()),
                    )
                    .optional()?
                    .is_some();
            if !embedded {
                steps.push(Step::Missing(match &stages.embed_skipped {
                    Some(reason) => format!("embed skipped: {reason}"),
                    None => "not embedded yet".into(),
                }));
            } else {
                let category: Option<String> = if classes_ok {
                    conn.query_row(
                        "SELECT category FROM classifications WHERE hash = ?1 AND model_id = ?2",
                        [hash, model_id],
                        |r| r.get(0),
                    )
                    .optional()?
                } else {
                    None
                };
                steps.push(Step::Done(match category {
                    Some(c) => format!("embedded ({c})"),
                    None => "embedded".into(),
                }));
            }
        }

        if stages.location && gps {
            let place: Option<Option<String>> = if places_ok {
                conn.query_row(
                    "SELECT c.name FROM file_hashes f JOIN location_clusters c \
                     ON c.id = f.location_cluster_id WHERE f.path = ?1",
                    [path],
                    |r| r.get(0),
                )
                .optional()?
            } else {
                None
            };
            steps.push(match place {
                Some(Some(name)) => Step::Done(format!("placed in {name}")),
                Some(None) => Step::Done("placed".into()),
                None => Step::Missing("not placed yet".into()),
            });
        }
        out.push(FileFacts { path: shown, steps });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(path: &str, steps: &[Step]) -> FileFacts {
        FileFacts {
            path: path.into(),
            steps: steps.to_vec(),
        }
    }

    #[test]
    fn a_small_batch_gets_one_line_per_file() {
        let lines = render(&[facts(
            "çiçek.heic",
            &[
                Step::Done("scanned".into()),
                Step::Done("2 faces (1 grouped)".into()),
                Step::Done("embedded (photo)".into()),
                Step::Done("placed in Altunizade".into()),
            ],
        )]);
        assert_eq!(
            lines,
            vec![
                "çiçek.heic: scanned, 2 faces (1 grouped), embedded (photo), placed in Altunizade"
            ]
        );
    }

    #[test]
    fn a_filename_cannot_forge_lines_or_drive_the_terminal() {
        let lines = render(&[facts(
            "a\nfake: done\u{1b}[31m\u{202e}gnp.jpg",
            &[Step::Done("scanned".into())],
        )]);
        assert_eq!(lines.len(), 1);
        assert_eq!(
            lines[0],
            "a\\nfake: done\\u{1b}[31m\\u{202e}gnp.jpg: scanned"
        );
    }

    #[test]
    fn a_missing_stage_says_why() {
        let lines = render(&[facts(
            "deniz.jpg",
            &[
                Step::Done("scanned".into()),
                Step::Missing("embed skipped: model not downloaded".into()),
            ],
        )]);
        assert_eq!(
            lines,
            vec!["deniz.jpg: scanned, embed skipped: model not downloaded"]
        );
    }

    #[test]
    fn a_large_batch_gets_totals_and_only_its_incomplete_files() {
        let mut files: Vec<FileFacts> = (0..21)
            .map(|i| facts(&format!("f{i}.jpg"), &[Step::Done("scanned".into())]))
            .collect();
        files[7].steps.push(Step::Missing("not placed yet".into()));
        let lines = render(&files);
        assert_eq!(lines[0], "21 files: 20 complete, 1 not yet");
        assert_eq!(lines[1..], ["f7.jpg: scanned, not placed yet".to_string()]);
    }

    #[test]
    fn gather_reads_faces_and_places_back_from_the_database() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        videre_core::face_db::create_faces_table(&conn).unwrap();
        videre_core::location_cluster::ensure_location_clusters_table(&conn).unwrap();
        conn.execute_batch(
            "CREATE TABLE file_hashes (path TEXT PRIMARY KEY, hash TEXT, ext TEXT,
                gps_lat REAL, gps_lon REAL, location_cluster_id INTEGER);
             INSERT INTO location_clusters (id, centroid_lat, centroid_lon, name, photo_count,
                radius_km, created_at) VALUES (4, 40.99, 29.02, 'Kadıköy', 1, 15, 'now');
             INSERT INTO file_hashes VALUES ('/k/çiçek.jpg', 'h1', 'jpg', 40.99, 29.02, 4);
             INSERT INTO file_hashes VALUES ('/k/deniz.jpg', 'h2', 'jpg', 40.99, 29.02, NULL);
             INSERT INTO faces_scanned (hash) VALUES ('h1');
             INSERT INTO faces (hash, bbox, embedding, cluster_id) VALUES
               ('h1', '0,0,9,9', X'00', 3), ('h1', '0,0,9,9', X'00', NULL);",
        )
        .unwrap();
        let stages = Stages {
            faces: true,
            embed: false,
            location: true,
            embed_skipped: None,
        };
        let rows = vec![
            ("/k/çiçek.jpg".to_string(), "h1".to_string()),
            ("/k/deniz.jpg".to_string(), "h2".to_string()),
        ];
        let got = gather(
            &conn,
            std::path::Path::new("/k"),
            &rows,
            &stages,
            "m",
            &Default::default(),
        )
        .unwrap();
        assert_eq!(
            render(&got),
            vec![
                "çiçek.jpg: scanned, 2 faces (1 grouped), placed in Kadıköy",
                "deniz.jpg: scanned, faces not detected yet, not placed yet",
            ]
        );
    }
}
