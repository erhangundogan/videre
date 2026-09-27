use crate::command_context::CommandContext;
use anyhow::Result;
use rayon::prelude::*;
use videre_core::{decode_failures, embeddings, vectors};
use videre_ml::{device, model, preprocess};

#[derive(clap::Args)]
pub struct EmbedArgs {
    /// Embedding model to use (default: 'videre config set model', else the
    /// built-in default). Each model gets its own database under
    /// <library>/.videre/embeddings/, so models never overwrite each other.
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
    presence: super::selection_args::PresenceArgs,
    #[command(flatten)]
    paths: super::selection_args::PathArgs,
    #[command(flatten)]
    marks: super::selection_args::MarkArgs,
    #[command(flatten)]
    tags: super::selection_args::TagFilterArgs,

    /// Re-embed every eligible image, including ones already embedded under
    /// this model. Use after a fix that changed what the model sees, such as
    /// the orientation-correct decode, so pre-fix embeddings are rebuilt.
    #[arg(long)]
    reprocess: bool,

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

impl EmbedArgs {
    /// Pipeline-stage defaults: no selection, clap defaults for every knob,
    /// silence controlled by the pipeline. Built by letting clap parse an
    /// empty argv so defaults never drift from the flag definitions.
    pub(crate) fn for_pipeline(silent: bool) -> Self {
        #[derive(clap::Parser)]
        struct P {
            #[command(flatten)]
            a: EmbedArgs,
        }
        let argv: &[&str] = if silent {
            &["embed", "--silent"]
        } else {
            &["embed"]
        };
        <P as clap::Parser>::parse_from(argv).a
    }
}

pub fn run(args: EmbedArgs, ctx: &CommandContext) -> Result<()> {
    // Guard every --path against the selected root before any state changes, so
    // an out-of-root filter is rejected before the model store below is created.
    videre_core::library_guard::validate_paths(&ctx.library, &args.paths.path)?;

    let conn = videre_core::library_db::open_existing(&ctx.library)?;
    // Embedding is an ordinary writer: it coexists with readers and other
    // ordinary work in this library, but exclusive maintenance (prune) locks
    // it out. Held for the whole run, released when this returns.
    let _activity = videre_core::library_locks::try_activity(
        &ctx.library,
        videre_core::library_locks::ActivityMode::Shared,
    )?;
    let model_id = videre_core::embeddings::resolve_model_id_from(
        &ctx.library.settings,
        args.model.as_deref(),
    )?;
    let guard = videre_core::library_locks::try_command(&ctx.library, "embed")?;
    // embed is the only command allowed to bring a model database into
    // existence, and it does so only once a model has loaded and there is
    // work to write (inside run_embed, after Embedder::load). A failed load
    // and a zero-work run both leave the embeddings directory untouched, so
    // a typo in --model cannot leave an empty model behind for stats to list
    // forever. The attach therefore lives inside track_in, where a failed
    // create is recorded as a failed pipeline run, which it now genuinely is.
    let db_existed = videre_core::embeddings_db::db_path_in(&ctx.library, &model_id)?.exists();
    if db_existed {
        // Today's ensure-schema-and-attach; on a missing file it must not run.
        videre_core::embeddings_db::attach_in(&conn, &ctx.library, &model_id, true)?;
    }

    videre_core::pipeline_runs::track_in(&conn, &ctx.library, &guard, "embed", || {
        run_embed(&args, ctx, &conn, &model_id, db_existed)
    })
}

/// The actual embedding work, wrapped by `track_in()` above.
fn run_embed(
    args: &EmbedArgs,
    ctx: &CommandContext,
    conn: &rusqlite::Connection,
    model_id: &str,
    db_existed: bool,
) -> Result<()> {
    embeddings::ensure_embeddings_index(conn)?;
    decode_failures::ensure_table(conn)?;

    let pending = if args.reprocess {
        // --reprocess is the retry hatch: clear this stage's recorded failures so
        // a file that was skipped as undecodable gets tried again (a videre fix
        // may have made it decodable).
        decode_failures::clear_stage(conn, decode_failures::STAGE_EMBED)?;
        embeddings::embeddable_images(conn, model_id)?
    } else {
        // Drop hashes this stage has already failed to decode enough times: they
        // would only re-pay the same multi-second timeout for the same
        // guaranteed failure on every run. The filter applies whether or not a
        // store exists: a never-embedded library must not re-try a hash that
        // already failed to threshold either.
        let mut pending = if db_existed {
            embeddings::pending_images(conn, model_id)?
        } else {
            // No store means nothing is embedded under this model, so the
            // no-exclusion query is the same set without needing the emb
            // attach the embedded-hash anti-join would require.
            embeddings::embeddable_images(conn, model_id)?
        };
        let failed = decode_failures::failed_hashes(
            conn,
            decode_failures::STAGE_EMBED,
            decode_failures::FAILURE_THRESHOLD,
        )?;
        pending.retain(|p| !failed.contains(&p.hash));
        pending
    };

    // Scope intersects with the pending set; it never replaces the eligibility
    // and backfill rules above, which know things this layer does not (the DNG
    // veto, what is already embedded under this model).
    let selection = super::selection_args::row_selection(
        Some(&args.media),
        Some(&args.dates),
        Some(&args.place),
        None,
        Some(&args.presence),
        Some(&args.paths),
        Some(&args.marks),
        Some(&args.tags),
    )?;
    let work = videre_core::work::narrow_in(
        pending,
        |p| p.hash.as_str(),
        &selection,
        conn,
        &videre_core::selection::SelectionCtx {
            model_id: Some(model_id.to_string()),
        },
        &ctx.library,
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
        // A run that only fills missing fingerprints never needs the model,
        // which costs seconds and memory to load. Such a run always has a
        // store: an item is `embedded` only because the store holds its row.
        let embedder = if pending.iter().any(|p| !p.embedded) {
            let embedder = model::Embedder::load(dev.clone(), model_id)?;
            if !db_existed {
                // First creation point: the model loaded, so rows will be written.
                videre_core::embeddings_db::attach_in(conn, &ctx.library, model_id, true)?;
            }
            Some(embedder)
        } else {
            None
        };
        let size = model::image_size_for(model_id);

        let progress = videre_core::progress::Progress::new(pending.len() as u64, args.silent);

        let mut done = 0usize;
        let mut fingerprinted = 0usize;
        let mut failed = 0usize;
        for chunk in pending.chunks(chunk_size) {
            // Decode in parallel; a failure carries (hash, error) back so it can
            // be recorded serially below (the connection is not Sync, so no DB
            // write can happen inside the rayon closure).
            // Each success also carries the near-duplicate fingerprint, taken
            // from the decoded image before the model's resize.
            type Decoded = std::result::Result<
                (String, Option<candle_core::Tensor>, u64),
                (String, String),
            >;
            let outcomes: Vec<Decoded> = chunk
                .par_iter()
                .map(|p| {
                    let decoded = preprocess::decode(std::path::Path::new(&p.path), size)
                        .and_then(|img| {
                            let phash = videre_core::image_decode::dhash(&img);
                            // Already embedded: only the fingerprint was missing.
                            if p.embedded {
                                return Ok((None, phash));
                            }
                            // Decode on CPU, move to the device in batch.
                            let t = preprocess::to_tensor(&img, size, &candle_core::Device::Cpu)?;
                            Ok((Some(t), phash))
                        });
                    match decoded {
                        Ok((t, phash)) => Ok((p.hash.clone(), t, phash)),
                        Err(e) => {
                            let reason = format!("{e:#}");
                            // The kind was attached where the cause was known.
                            progress.skip(&p.path, e);
                            Err((p.hash.clone(), reason))
                        }
                    }
                })
                .collect();
            let mut decoded: Vec<(String, candle_core::Tensor)> =
                Vec::with_capacity(outcomes.len());
            let mut phashes: Vec<(String, u64)> = Vec::with_capacity(outcomes.len());
            for outcome in outcomes {
                match outcome {
                    Ok((hash, tensor, phash)) => {
                        if let Some(t) = tensor {
                            decoded.push((hash.clone(), t));
                        }
                        phashes.push((hash, phash));
                    }
                    // Record the failure so a later run can skip it once it has
                    // failed FAILURE_THRESHOLD times.
                    Err((hash, err)) => {
                        let _ = decode_failures::record(
                            conn,
                            &hash,
                            decode_failures::STAGE_EMBED,
                            &err,
                        );
                    }
                }
            }
            failed += chunk.len() - phashes.len();
            fingerprinted += phashes.len() - decoded.len();

            let mut rows: Vec<(String, Vec<u8>)> = Vec::with_capacity(decoded.len());
            for group in decoded.chunks(batch) {
                let tensors: Vec<candle_core::Tensor> = group
                    .iter()
                    .map(|(_, t)| t.to_device(&dev))
                    .collect::<candle_core::Result<_>>()?;
                let vecs = embedder
                    .as_ref()
                    .expect("the model loads whenever an item needs embedding")
                    .embed_images(&tensors)?;
                for ((hash, _), v) in group.iter().zip(vecs) {
                    rows.push((hash.clone(), vectors::to_f16_bytes(&v)));
                }
            }

            embeddings::insert_embeddings(conn, model_id, &rows)?;
            // Its own transaction: a crash between the two leaves `phash` NULL,
            // which the next run's pending query picks up again.
            embeddings::set_phashes(conn, &phashes)?;
            // A file that decoded is not failing: drop any earlier strike so a
            // transient timeout never lingers toward the threshold.
            for (hash, _) in &phashes {
                let _ = decode_failures::clear(conn, hash, decode_failures::STAGE_EMBED);
            }
            done += rows.len();
            progress.tick_by(chunk.len() as u64);
        }

        progress.finish();

        if !args.silent {
            tracing::info!(
                "{}",
                format_summary(done, fingerprinted, failed, started.elapsed())
            );
        }
        Ok(())
    })?;
    Ok(())
}

/// Assembles the single consolidated summary line printed after embedding
/// finishes. Not `pub(crate)` (unlike `videre faces`'s equivalent
/// `format_summary`): nothing outside this file calls it, `videre embed`
/// has no `videre watch` stage equivalent that shares this logic.
fn format_summary(
    done: usize,
    fingerprinted: usize,
    failed: usize,
    elapsed: std::time::Duration,
) -> String {
    let added = if fingerprinted > 0 {
        format!(", {fingerprinted} fingerprint(s) added")
    } else {
        String::new()
    };
    if failed > 0 {
        format!(
            "{done} image(s) embedded{added}, {failed} skipped, done in {}s",
            elapsed.as_secs()
        )
    } else {
        format!(
            "{done} image(s) embedded{added}, done in {}",
            videre_core::progress::human_duration(elapsed)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn for_pipeline_applies_clap_defaults() {
        let a = EmbedArgs::for_pipeline(false);
        assert_eq!(a.batch, 32, "clap default_value_t for batch");
        assert_eq!(a.chunk, 500, "clap default_value_t for chunk");
        assert!(!a.silent);
        let s = EmbedArgs::for_pipeline(true);
        assert!(s.silent);
    }

    #[test]
    fn embed_accepts_mark_and_tag_filters_but_refuses_derived_selectors() {
        use clap::Parser;
        #[derive(Parser)]
        struct Wrap {
            #[command(flatten)]
            a: EmbedArgs,
        }
        let a = Wrap::try_parse_from([
            "embed", "--label", "Green", "--tag", "t", "--rating", "5", "--like",
        ])
        .expect("mark/tag filters must parse on embed")
        .a;
        assert_eq!(a.marks.label.as_deref(), Some("Green"));
        assert_eq!(a.tags.tags, vec!["t".to_string()]);
        assert!(a.marks.like);
        // The derived-data gap holds: person/category are not part of embed's vocabulary.
        assert!(Wrap::try_parse_from(["embed", "--person", "Ada"]).is_err());
        assert!(Wrap::try_parse_from(["embed", "--category", "photo"]).is_err());
    }

    #[test]
    fn format_summary_no_skips() {
        let summary = format_summary(234, 0, 0, std::time::Duration::from_secs(41));
        assert_eq!(summary, "234 image(s) embedded, done in 41s");
    }

    #[test]
    fn format_summary_reports_fingerprint_only_work() {
        let summary = format_summary(0, 12, 0, std::time::Duration::from_secs(41));
        assert_eq!(
            summary,
            "0 image(s) embedded, 12 fingerprint(s) added, done in 41s"
        );
    }

    #[test]
    fn format_summary_with_skips() {
        let summary = format_summary(230, 0, 4, std::time::Duration::from_secs(41));
        assert_eq!(summary, "230 image(s) embedded, 4 skipped, done in 41s");
    }

    #[test]
    fn safe_batch_maximum_stays_below_the_measured_corruption_threshold() {
        // 120 measured clean, 127 measured corrupt, so anything at 121 or
        // above is unproven at best. This guards against someone raising
        // MAX_SAFE_BATCH for speed without re-running the comparison against a
        // one-image-at-a-time baseline.
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
