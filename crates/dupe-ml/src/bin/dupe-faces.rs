use anyhow::Result;
use clap::Parser;
use dupe_core::{face_cluster, face_db};
use dupe_ml::{face_align, face_detect, face_embed, face_models};
use half::f16;
use image::DynamicImage;
use rusqlite::Connection;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "dupe-faces", about = "Detect, embed, and cluster faces in a dupe SQLite database.")]
struct Args {
    db: PathBuf,
    #[arg(long)] reprocess: bool,
    #[arg(long, default_value = "8")] batch: usize,
    #[arg(long)] dry_run: bool,
    #[arg(long)] silent: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();
    if !args.db.exists() {
        anyhow::bail!("{:?} does not exist", args.db);
    }
    let conn = Connection::open(&args.db)?;
    face_db::create_faces_table(&conn)?;

    // 1. Determine which hashes to process
    let all_paths: Vec<(String, String)> = {
        let mut stmt = conn.prepare(
            "SELECT path, hash FROM file_hashes WHERE ext IN ('jpg','jpeg','png','gif','webp','bmp','tiff','heic')"
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };

    let skip_hashes: std::collections::HashSet<String> = if args.reprocess {
        std::collections::HashSet::new()
    } else {
        face_db::hashes_with_faces(&conn)?.into_iter().collect()
    };

    // Gap 1: Deduplicate by hash - one representative path per hash
    let mut seen_hashes = std::collections::HashSet::new();
    let to_process: Vec<(String, String)> = all_paths.into_iter()
        .filter(|(_, hash)| !skip_hashes.contains(hash) && seen_hashes.insert(hash.clone()))
        .collect();

    if to_process.is_empty() {
        if !args.silent { eprintln!("All hashes already processed."); }
        return Ok(());
    }

    if !args.silent { eprintln!("Processing {} images...", to_process.len()); }

    // 2. Download models
    let (det_path, rec_path) = face_models::buffalo_l_paths()?;
    let mut detector = face_detect::FaceDetector::new(&det_path)?;
    let mut embedder = face_embed::FaceEmbedder::new(&rec_path)?;

    let mut total_faces = 0usize;
    let mut write_errors = 0usize;

    // Gap 2: Batch embedding across images using --batch as the chunk size.
    // Detection runs per image (SCRFD requires fixed-size single-image input),
    // but embed_batch is called once per chunk of args.batch images.
    for chunk in to_process.chunks(args.batch) {
        // Phase 1: detect + align all images in this chunk
        struct ChunkEntry {
            path: String,
            hash: String,
            detections: Vec<face_detect::Detection>,
            n_crops: usize,
        }
        let mut chunk_entries: Vec<ChunkEntry> = Vec::new();
        let mut chunk_crops: Vec<image::RgbImage> = Vec::new();

        for (path, hash) in chunk {
            let img = match load_image(path) {
                Some(i) => i,
                None => continue,
            };
            let detections = match detector.detect(&img) {
                Ok(d) => d,
                Err(e) => { eprintln!("detect failed {path}: {e}"); continue; }
            };
            if detections.is_empty() { continue; }

            let crops: Vec<image::RgbImage> = detections.iter()
                .map(|d| face_align::align_face(&img, &d.landmarks))
                .collect();

            if !args.silent { eprintln!("[faces] {path}: {} face(s)", detections.len()); }
            let n_crops = crops.len();
            chunk_crops.extend(crops);
            chunk_entries.push(ChunkEntry {
                path: path.clone(),
                hash: hash.clone(),
                detections,
                n_crops,
            });
        }

        if chunk_crops.is_empty() { continue; }

        // Phase 2: embed all crops from this chunk in one call
        let all_embeddings = match embedder.embed_batch(&chunk_crops) {
            Ok(e) => e,
            Err(e) => { eprintln!("embed_batch failed: {e}"); continue; }
        };

        // Phase 3: distribute embeddings back to each image and write rows
        let mut emb_offset = 0;
        for entry in &chunk_entries {
            let n = entry.n_crops;
            let embs = &all_embeddings[emb_offset..emb_offset + n];
            emb_offset += n;

            let rows: Vec<face_db::FaceRow> = entry.detections.iter().zip(embs.iter()).map(|(det, emb)| {
                let [x1, y1, x2, y2] = det.bbox;
                let bbox = format!("{},{},{},{}", x1 as i32, y1 as i32, (x2 - x1) as i32, (y2 - y1) as i32);
                let lm_str: String = det.landmarks.iter()
                    .flat_map(|[x, y]| [x.to_string(), y.to_string()])
                    .collect::<Vec<_>>().join(",");
                let embedding: Vec<u8> = emb.iter()
                    .flat_map(|&v| f16::from_f32(v).to_le_bytes())
                    .collect();
                face_db::FaceRow {
                    hash: entry.hash.clone(), bbox, landmark: Some(lm_str),
                    embedding, cluster_id: None, person_label: None, confirmed: 0, is_primary: 0,
                }
            }).collect();

            total_faces += rows.len();
            if !args.dry_run {
                if let Err(e) = face_db::replace_faces_for_hash(&conn, &entry.hash, &rows) {
                    eprintln!("write failed {}: {e}", entry.path);
                    write_errors += 1;
                }
            }
        }
    }

    // Gap 3: Cluster whenever there are faces in the DB, not only when new faces were found
    if !args.dry_run {
        let all_embs = face_db::load_face_embeddings(&conn)?;
        if !all_embs.is_empty() {
            let assignments = face_cluster::dbscan_cosine(&all_embs, 0.4, 2);
            face_db::update_cluster_assignments(&conn, &assignments)?;
            if !args.silent { eprintln!("Clustering complete: {} faces in DB.", all_embs.len()); }
        }
    }

    if !args.silent { eprintln!("Done: {} new face(s) detected.", total_faces); }
    if write_errors > 0 {
        std::process::exit(1);
    }
    Ok(())
}

fn load_image(path: &str) -> Option<DynamicImage> {
    if path.to_lowercase().ends_with(".heic") {
        #[cfg(target_os = "macos")]
        {
            let tmp = std::env::temp_dir().join("dupe_faces_heic.jpg");
            let ok = std::process::Command::new("sips")
                .args(["-s", "format", "jpeg", path, "--out", tmp.to_str()?])
                .status().ok()?.success();
            if ok { return image::open(&tmp).ok(); }
        }
        return None;
    }
    image::open(path).ok()
}
