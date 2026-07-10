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
        let rows: Vec<_> = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .filter_map(|r| r.ok())
            .collect();
        rows
    };

    let skip_hashes: std::collections::HashSet<String> = if args.reprocess {
        std::collections::HashSet::new()
    } else {
        face_db::hashes_with_faces(&conn)?.into_iter().collect()
    };

    let to_process: Vec<(String, String)> = all_paths.into_iter()
        .filter(|(_, hash)| !skip_hashes.contains(hash))
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

    // 3. Detect + align + embed
    for (path, hash) in &to_process {
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

        let embeddings = match embedder.embed_batch(&crops) {
            Ok(e) => e,
            Err(e) => { eprintln!("embed failed {path}: {e}"); continue; }
        };

        let rows: Vec<face_db::FaceRow> = detections.iter().zip(embeddings.iter()).map(|(det, emb)| {
            let [x1, y1, x2, y2] = det.bbox;
            let bbox = format!("{},{},{},{}", x1 as i32, y1 as i32, (x2 - x1) as i32, (y2 - y1) as i32);
            let lm_str: String = det.landmarks.iter()
                .flat_map(|[x, y]| [x.to_string(), y.to_string()])
                .collect::<Vec<_>>().join(",");
            let embedding: Vec<u8> = emb.iter()
                .flat_map(|&v| f16::from_f32(v).to_le_bytes())
                .collect();
            face_db::FaceRow {
                hash: hash.clone(), bbox, landmark: Some(lm_str),
                embedding, cluster_id: None, person_label: None, confirmed: 0,
            }
        }).collect();

        if !args.silent { println!("[faces] {path}: {} face(s)", rows.len()); }
        total_faces += rows.len();
        if !args.dry_run {
            face_db::replace_faces_for_hash(&conn, hash, &rows)?;
        }
    }

    // 4. Re-cluster all embeddings in DB
    if !args.dry_run && total_faces > 0 {
        let all_embs = face_db::load_face_embeddings(&conn)?;
        let assignments = face_cluster::dbscan_cosine(&all_embs, 0.4, 2);
        face_db::update_cluster_assignments(&conn, &assignments)?;
        if !args.silent { eprintln!("Clustering complete: {} faces in DB.", all_embs.len()); }
    }

    if !args.silent { eprintln!("Done: {} new face(s) detected.", total_faces); }
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
