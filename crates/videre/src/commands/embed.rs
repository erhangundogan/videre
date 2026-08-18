use anyhow::{Context, Result};
use rayon::prelude::*;
use std::path::PathBuf;
use videre_core::{embeddings, vectors};
use videre_ml::{device, model, preprocess};

#[derive(clap::Args)]
pub struct EmbedArgs {
    /// SQLite database (default: resolved from ~/.videre; see 'videre config')
    #[arg(long)]
    db: Option<PathBuf>,

    /// Embedding model to use (default: 'videre config set model', else the
    /// built-in default). Each model gets its own database under
    /// ~/.videre/embeddings/, so models never overwrite each other.
    #[arg(long, value_parser = super::parse_model_id)]
    model: Option<String>,

    /// Which files to embed. No selection means every pending file, as before.
    ///
    /// `--person`/`--category` are deliberately absent: selecting by person for
    /// a run that produces the very vectors person search needs is circular,
    /// and category is model-scoped in a way that would need the model resolved
    /// before the selection.
    #[command(flatten)]
    media: super::selection_args::MediaArgs,
    #[command(flatten)]
    dates: super::selection_args::DateArgs,
    #[command(flatten)]
    place: super::selection_args::PlaceArgs,
    #[command(flatten)]
    paths: super::selection_args::PathArgs,

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
    let conn = videre_core::db::open_wal(&db).with_context(|| format!("open {}", db.display()))?;

    let model_id = videre_core::embeddings::resolve_model_id(args.model.as_deref())?;
    // create: true here and nowhere else. embed is the only command allowed
    // to bring a model database into existence; every reader errors instead,
    // so a typo in --model never silently produces an empty library.
    videre_core::embeddings_db::attach(&conn, &db, &model_id, true)?;

    videre_core::pipeline_runs::track(&conn, &db, "embed", || run_embed(&args, &conn, &model_id))
}

/// The actual embedding work, wrapped by `track()` above.
fn run_embed(args: &EmbedArgs, conn: &rusqlite::Connection, model_id: &str) -> Result<()> {
    embeddings::ensure_embeddings_index(conn)?;

    let pending = embeddings::pending_images(conn, model_id)?;

    // Scope intersects with the pending set; it never replaces the eligibility
    // and backfill rules above, which know things this layer does not (the DNG
    // veto, what is already embedded under this model).
    let selection = super::selection_args::row_selection(
        Some(&args.media),
        Some(&args.dates),
        Some(&args.place),
        None,
        Some(&args.paths),
    )?;
    let work = videre_core::work::narrow(
        pending,
        |p| p.hash.as_str(),
        &selection,
        conn,
        &videre_core::selection::SelectionCtx {
            model_id: Some(model_id.to_string()),
        },
        videre_core::work::Words::new("embed", "Embedding"),
        args.silent,
    )?;

    // Everything below runs only when there is work, which is what keeps the
    // model load unreachable on an up-to-date library.
    videre_core::work::with_work(work, args.silent, |work| {
        let pending = work.items;
        let batch = model::clamp_batch(args.batch, Some(model::MAX_SAFE_BATCH));
        // `slice::chunks` panics on 0, so a bare `--chunk 0` would abort the run.
        let chunk_size = args.chunk.max(1);

        let started = std::time::Instant::now();
        let dev = device::best_device();
        let embedder = model::Embedder::load(dev.clone(), model_id)?;

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
                        model::image_size_for(model_id),
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
    })?;
    Ok(())
}

/// Assembles the single consolidated summary line printed after embedding
/// finishes. Not `pub(crate)` (unlike `videre faces`'s equivalent
/// `format_summary`): nothing outside this file calls it, `videre embed`
/// has no `videre watch` stage equivalent that shares this logic.
fn format_summary(done: usize, failed: usize, elapsed: std::time::Duration) -> String {
    if failed > 0 {
        format!(
            "{done} image(s) embedded, {failed} skipped, done in {}s",
            elapsed.as_secs()
        )
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
    fn safe_batch_maximum_stays_below_the_measured_corruption_threshold() {
        // 120 measured clean, 127 measured corrupt, so anything at 121 or
        // above is unproven at best. This guards against someone raising
        // MAX_SAFE_BATCH for speed without re-running the baseline comparison in
        // docs/superpowers/2026-08-04-embed-batch-corruption-investigation.md.
        // The corruption is silent, so a bad value there would not surface as a
        // failure anywhere else in the suite.
        let max = model::MAX_SAFE_BATCH;
        assert!(
            max <= 120,
            "MAX_SAFE_BATCH ({max}) is at or above the batch size measured to silently corrupt \
             embeddings; do not raise it without re-measuring against a small-batch baseline"
        );
    }
}
