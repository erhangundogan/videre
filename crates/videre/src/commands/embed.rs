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

    /// Inference batch size (clamped to videre_ml::model::MAX_SAFE_BATCH)
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

    videre_core::pipeline_runs::track(&conn, &db, "embed", || run_embed(&args, &conn))
}

/// Clamps `--batch` into the range known to produce correct embeddings, and
/// rejects the degenerate 0 (`slice::chunks(0)` panics).
///
/// Warns unconditionally rather than honoring `--silent`: this guards against
/// silently corrupting the embeddings table, which is closer to an error than
/// to progress output.
pub(crate) fn clamp_batch(requested: usize) -> usize {
    if requested == 0 {
        eprintln!("warning: --batch 0 is not valid; using 1");
        return 1;
    }
    let max = model::MAX_SAFE_BATCH;
    if requested > max {
        eprintln!(
            "warning: --batch {requested} exceeds the safe maximum of {max}; using {max} instead. \
             Larger batches silently produce incorrect embeddings on this inference path (no error \
             is raised, so this cap is the only thing preventing a corrupt embeddings table)."
        );
        return max;
    }
    requested
}

/// The actual embedding work, wrapped by `track()` above.
fn run_embed(args: &EmbedArgs, conn: &rusqlite::Connection) -> Result<()> {
    embeddings::ensure_embeddings_table(conn)?;

    let model_id = model::configured_model_id();

    // A model change silently invalidates every stored embedding (rows are
    // tagged with the model id, and `pending_images` filters on it), so an
    // upgrade would otherwise look like "re-embedding my entire finished
    // library for no reason". Say so explicitly before doing hours of work.
    let (stale, other_models) = embeddings::embeddings_from_other_models(conn, model_id)?;
    if stale > 0 {
        eprintln!(
            "note: {stale} existing embedding(s) were made with {} and cannot be compared against \
             {model_id}, so they will be recomputed and replaced. Embeddings from different \
             models are not interchangeable; this is a one-time cost per model change.",
            other_models.join(", ")
        );
    }

    let pending = embeddings::pending_images(conn, model_id)?;
    if pending.is_empty() {
        if !args.silent {
            eprintln!("Nothing to embed: all hashes already have embeddings.");
        }
        return Ok(());
    }

    let batch = clamp_batch(args.batch);
    // `slice::chunks` panics on 0, so a bare `--chunk 0` would abort the run.
    let chunk_size = args.chunk.max(1);

    let started = std::time::Instant::now();
    let dev = device::best_device();
    let embedder = model::Embedder::load(dev.clone())?;

    let progress = videre_core::progress::Progress::new(pending.len() as u64, args.silent);

    let mut done = 0usize;
    let mut failed = 0usize;
    for chunk in pending.chunks(chunk_size) {
        // Decode in parallel; None = unreadable, logged and skipped.
        let decoded: Vec<Option<(String, candle_core::Tensor)>> = chunk
            .par_iter()
            .map(|p| {
                match preprocess::image_to_tensor(
                    std::path::Path::new(&p.path),
                    model::configured_image_size(),
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
        for group in decoded.chunks(batch) {
            let tensors: Vec<candle_core::Tensor> = group
                .iter()
                .map(|(_, t)| t.to_device(&dev))
                .collect::<candle_core::Result<_>>()?;
            let vecs = embedder.embed_images(&tensors)?;
            for ((hash, _), v) in group.iter().zip(vecs) {
                rows.push((hash.clone(), vectors::to_f16_bytes(&v)));
            }
        }

        embeddings::insert_embeddings(conn, model_id, &rows)?;
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

    #[test]
    fn clamp_batch_leaves_safe_values_alone() {
        assert_eq!(clamp_batch(1), 1);
        assert_eq!(clamp_batch(32), 32, "the default must never be altered");
        assert_eq!(clamp_batch(model::MAX_SAFE_BATCH), model::MAX_SAFE_BATCH);
    }

    #[test]
    fn clamp_batch_caps_values_above_the_safe_maximum() {
        assert_eq!(clamp_batch(model::MAX_SAFE_BATCH + 1), model::MAX_SAFE_BATCH);
        assert_eq!(clamp_batch(128), model::MAX_SAFE_BATCH);
        assert_eq!(clamp_batch(256), model::MAX_SAFE_BATCH);
        assert_eq!(clamp_batch(usize::MAX), model::MAX_SAFE_BATCH);
    }

    #[test]
    fn clamp_batch_rejects_zero_which_would_panic_slice_chunks() {
        // `slice::chunks(0)` panics, so 0 has to become something usable
        // rather than reaching the loop.
        assert_eq!(clamp_batch(0), 1);
    }

    #[test]
    fn safe_batch_maximum_stays_below_the_measured_corruption_threshold() {
        // 120 measured clean, 127 measured corrupt, so anything at 121 or
        // above is unproven at best. This guards against someone raising
        // MAX_SAFE_BATCH for speed without re-running the baseline comparison in
        // docs/superpowers/2026-08-04-embed-batch-corruption-investigation.md -
        // the corruption is silent, so a bad value there would not surface as a
        // failure anywhere else in the suite.
        let max = model::MAX_SAFE_BATCH;
        assert!(
            max <= 120,
            "MAX_SAFE_BATCH ({max}) is at or above the batch size measured to silently corrupt \
             embeddings; do not raise it without re-measuring against a small-batch baseline"
        );
    }
}
