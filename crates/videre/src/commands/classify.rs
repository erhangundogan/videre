use anyhow::{Context, Result};
use videre_core::{classify as classify_core, embeddings, vectors};
use videre_ml::{classify as classify_ml, device, model};
use std::path::PathBuf;

#[derive(clap::Args)]
pub struct ClassifyArgs {
    /// SQLite database (default: resolved from ~/.videre; see 'videre config')
    #[arg(long)]
    db: Option<PathBuf>,

    /// Re-classify every embedded hash, including ones already classified
    #[arg(long)]
    reprocess: bool,

    /// Min similarity gap between the best and second-best category to
    /// accept a result; below this, stores "unknown" instead. Default 0.05.
    #[arg(long, default_value_t = 0.05)]
    margin: f32,

    /// Suppress per-image progress output on stderr (errors always shown)
    #[arg(long)]
    silent: bool,
}

pub fn run(args: ClassifyArgs) -> Result<()> {
    let db = super::resolve_reader_db(args.db.clone())?;
    let conn = videre_core::db::open_wal(&db)
        .with_context(|| format!("open {}", db.display()))?;

    videre_core::pipeline_runs::track(&conn, &db, "classify", || run_classify(&args, &conn))
}

/// The actual classification work, wrapped by `track()` above.
fn run_classify(args: &ClassifyArgs, conn: &rusqlite::Connection) -> Result<()> {
    classify_core::ensure_classifications_table(conn)?;

    // Loaded once and looked up by hash below rather than holding the whole
    // corpus twice - hashes.len() can be in the tens of thousands.
    let all_embeddings: std::collections::HashMap<String, Vec<u8>> =
        embeddings::load_embeddings(conn, model::MODEL_ID)?.into_iter().collect();

    let hashes: Vec<String> = if args.reprocess {
        let all: Vec<String> = all_embeddings.keys().cloned().collect();
        classify_core::exclude_video_hashes(conn, &all)?
    } else {
        classify_core::pending_hashes(conn, model::MODEL_ID)?
    };

    if hashes.is_empty() {
        if !args.silent {
            eprintln!("Nothing to classify: all embedded hashes already classified.");
        }
        return Ok(());
    }

    let started = std::time::Instant::now();
    let embedder = model::Embedder::load(device::best_device())?;

    // Embed each category prompt once; reused for every image below.
    let prompt_vecs: Vec<(&'static str, Vec<f32>)> = classify_ml::CATEGORY_PROMPTS
        .iter()
        .map(|(name, prompt)| Ok((*name, embedder.embed_text(prompt)?)))
        .collect::<Result<_>>()?;

    let progress = videre_core::progress::Progress::new(hashes.len() as u64, args.silent);
    let mut rows: Vec<(String, &str, f32)> = Vec::with_capacity(hashes.len());
    for hash in &hashes {
        let Some(blob) = all_embeddings.get(hash) else {
            progress.println(&format!("skipping {hash}: embedding vanished mid-run"));
            progress.tick();
            continue;
        };
        let vec = vectors::from_f16_bytes(blob);
        let scores: Vec<(&'static str, f32)> = prompt_vecs
            .iter()
            .map(|(name, prompt_vec)| {
                let dot: f32 = vec.iter().zip(prompt_vec.iter()).map(|(a, b)| a * b).sum();
                (*name, dot)
            })
            .collect();
        let (category, confidence) = classify_ml::classify_from_scores(&scores, args.margin);
        rows.push((hash.clone(), category, confidence));
        progress.tick();
    }
    progress.finish();

    classify_core::insert_classifications(conn, &rows)?;

    if !args.silent {
        eprintln!("{}", format_summary(rows.len(), started.elapsed()));
    }
    Ok(())
}

/// Assembles the single consolidated summary line printed after
/// classification finishes.
fn format_summary(done: usize, elapsed: std::time::Duration) -> String {
    format!("{done} image(s) classified, done in {}s", elapsed.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_summary_reads_naturally() {
        assert_eq!(
            format_summary(42, std::time::Duration::from_secs(3)),
            "42 image(s) classified, done in 3s"
        );
    }
}
