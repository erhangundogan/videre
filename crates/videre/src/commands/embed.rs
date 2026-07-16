use anyhow::{Context, Result};
use videre_core::{embeddings, vectors};
use videre_ml::{device, model, preprocess};
use rayon::prelude::*;
use std::path::PathBuf;

#[derive(clap::Args)]
pub struct EmbedArgs {
    /// SQLite database (default: resolved from ~/.videre; see 'videre config')
    #[arg(long)]
    db: Option<PathBuf>,

    /// Inference batch size
    #[arg(long, default_value_t = 32)]
    batch: usize,

    /// Rows written per transaction (resume granularity)
    #[arg(long, default_value_t = 500)]
    chunk: usize,

    /// Suppress progress output on stderr (errors always shown)
    #[arg(long)]
    silent: bool,
}

pub fn run(args: EmbedArgs) -> Result<()> {
    let db = super::resolve_reader_db(args.db.clone())?;
    let conn = videre_core::db::open_wal(&db)
        .with_context(|| format!("open {}", db.display()))?;
    embeddings::ensure_embeddings_table(&conn)?;

    let pending = embeddings::pending_images(&conn, model::MODEL_ID)?;
    if pending.is_empty() {
        if !args.silent {
            eprintln!("Nothing to embed: all hashes already have embeddings.");
        }
        return Ok(());
    }

    let started = std::time::Instant::now();
    let dev = device::best_device();
    let embedder = model::Embedder::load(dev.clone())?;

    let progress = videre_core::progress::Progress::new(pending.len() as u64, args.silent);

    let mut done = 0usize;
    let mut failed = 0usize;
    for chunk in pending.chunks(args.chunk) {
        // Decode in parallel; None = unreadable, logged and skipped.
        let decoded: Vec<Option<(String, candle_core::Tensor)>> = chunk
            .par_iter()
            .map(|p| {
                match preprocess::image_to_tensor(
                    std::path::Path::new(&p.path),
                    model::IMAGE_SIZE,
                    &candle_core::Device::Cpu, // decode on CPU, move to device in batch
                ) {
                    Ok(t) => Some((p.hash.clone(), t)),
                    Err(e) => {
                        progress.println(&format!("skip {}: {e:#}", p.path));
                        None
                    }
                }
            })
            .collect();
        let decoded: Vec<(String, candle_core::Tensor)> =
            decoded.into_iter().flatten().collect();
        failed += chunk.len() - decoded.len();

        let mut rows: Vec<(String, Vec<u8>)> = Vec::with_capacity(decoded.len());
        for batch in decoded.chunks(args.batch) {
            let tensors: Vec<candle_core::Tensor> = batch
                .iter()
                .map(|(_, t)| t.to_device(&dev))
                .collect::<candle_core::Result<_>>()?;
            let vecs = embedder.embed_images(&tensors)?;
            for ((hash, _), v) in batch.iter().zip(vecs) {
                rows.push((hash.clone(), vectors::to_f16_bytes(&v)));
            }
        }

        embeddings::insert_embeddings(&conn, model::MODEL_ID, &rows)?;
        done += rows.len();
        progress.tick_by(chunk.len() as u64);
    }

    progress.finish();

    if !args.silent {
        eprintln!("{}", format_summary(done, failed, started.elapsed()));
    }
    Ok(())
}

/// Assembles the single consolidated summary line printed after embedding
/// finishes. Not `pub(crate)` (unlike `videre faces`'s equivalent
/// `format_summary`): nothing outside this file calls it - `videre embed`
/// has no `videre watch` stage equivalent that shares this logic.
fn format_summary(done: usize, failed: usize, elapsed: std::time::Duration) -> String {
    if failed > 0 {
        format!("{done} image(s) embedded, {failed} skipped, done in {}s", elapsed.as_secs())
    } else {
        format!("{done} image(s) embedded, done in {}s", elapsed.as_secs())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_summary_no_skips() {
        let summary = format_summary(234, 0, std::time::Duration::from_secs(41));
        assert_eq!(summary, "234 image(s) embedded, done in 41s");
    }

    #[test]
    fn format_summary_with_skips() {
        let summary = format_summary(230, 4, std::time::Duration::from_secs(41));
        assert_eq!(summary, "230 image(s) embedded, 4 skipped, done in 41s");
    }
}
