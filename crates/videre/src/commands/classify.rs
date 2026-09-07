use crate::command_context::CommandContext;
use anyhow::Result;
use videre_core::library::LibraryContext;
use videre_core::{classify as classify_core, embeddings, vectors};
use videre_ml::{classify as classify_ml, device, model};

#[derive(clap::Args)]
pub struct ClassifyArgs {
    /// Which files to classify. No selection means every eligible file.
    #[command(flatten)]
    media: super::selection_args::MediaArgs,
    #[command(flatten)]
    dates: super::selection_args::DateArgs,
    #[command(flatten)]
    place: super::selection_args::PlaceArgs,
    #[command(flatten)]
    people: super::selection_args::PeopleArgs,
    #[command(flatten)]
    presence: super::selection_args::PresenceArgs,
    #[command(flatten)]
    paths: super::selection_args::PathArgs,

    /// Re-classify every embedded hash, including ones already classified
    #[arg(long)]
    reprocess: bool,

    /// Min similarity gap between the best and second-best category to
    /// accept a result; below this, stores "unknown" instead. Default 0.05.
    #[arg(long, default_value_t = 0.05)]
    margin: f32,

    /// Embedding model whose vectors to classify (default:
    /// 'videre config set model', else the built-in default). Classifications are
    /// stored per model, so two models classify independently.
    #[arg(long, value_parser = super::parse_model_id)]
    model: Option<String>,

    /// Suppress per-image progress output on stderr (errors always shown)
    #[arg(long)]
    silent: bool,
}

pub fn run(args: ClassifyArgs, ctx: &CommandContext) -> Result<()> {
    // Guard every --path against the selected root before any work; classify is
    // a reader of embeddings, so it never creates a model store.
    videre_core::library_guard::validate_paths(&ctx.library, &args.paths.path)?;

    let conn = videre_core::library_db::open_existing(&ctx.library)?;
    // Classify writes classification rows but coexists with readers and other
    // ordinary work; only exclusive maintenance locks it out.
    let _activity = videre_core::library_locks::try_activity(
        &ctx.library,
        videre_core::library_locks::ActivityMode::Shared,
    )?;
    let model_id = videre_core::embeddings::resolve_model_id_from(
        &ctx.library.settings,
        args.model.as_deref(),
    )?;
    videre_core::embeddings_db::attach_for_read_in(&conn, &ctx.library, &model_id)?;
    let guard = videre_core::library_locks::try_command(&ctx.library, "classify")?;

    videre_core::pipeline_runs::track_in(&conn, &ctx.library, &guard, "classify", || {
        run_classify(&args, &ctx.library, &conn, &model_id)
    })
}

/// The actual classification work, wrapped by `track_in()` above.
fn run_classify(
    args: &ClassifyArgs,
    library: &LibraryContext,
    conn: &rusqlite::Connection,
    model_id: &str,
) -> Result<()> {
    classify_core::ensure_classifications_table(conn)?;

    // Loaded once and looked up by hash below rather than holding the whole
    // corpus twice, hashes.len() can be in the tens of thousands.
    let all_embeddings: std::collections::HashMap<String, Vec<u8>> =
        embeddings::load_embeddings(conn, model_id)?
            .into_iter()
            .collect();

    let hashes: Vec<String> = if args.reprocess {
        let all: Vec<String> = all_embeddings.keys().cloned().collect();
        classify_core::exclude_video_hashes(conn, &all)?
    } else {
        classify_core::pending_hashes(conn, model_id)?
    };

    // Scope narrows the pending set; eligibility and staleness stay above.
    let selection = super::selection_args::row_selection(
        Some(&args.media),
        Some(&args.dates),
        Some(&args.place),
        Some(&args.people),
        Some(&args.presence),
        Some(&args.paths),
    )?;
    let work = videre_core::work::narrow_in(
        hashes,
        |h| h.as_str(),
        &selection,
        conn,
        &videre_core::selection::SelectionCtx {
            model_id: Some(model_id.to_string()),
        },
        library,
        videre_core::work::Words::new("classify", "Classifying"),
        args.silent,
    )?;

    // Everything below runs only when there is work, which is what keeps the
    // model load unreachable on an already-classified library. Reaching it with
    // nothing to do is what downloaded 778MB from inside a unit test.
    videre_core::work::with_work(work, args.silent, |work| {
        let hashes = work.items;
        let started = std::time::Instant::now();
        let embedder = model::Embedder::load(device::best_device(), model_id)?;

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

        classify_core::insert_classifications(conn, model_id, &rows)?;

        if !args.silent {
            eprintln!("{}", format_summary(rows.len(), started.elapsed()));
        }
        Ok(())
    })?;
    Ok(())
}

/// Assembles the single consolidated summary line printed after
/// classification finishes.
fn format_summary(done: usize, elapsed: std::time::Duration) -> String {
    format!(
        "{done} image(s) classified, done in {}",
        videre_core::progress::human_duration(elapsed)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory-local library with one embedded jpeg, nothing classified
    /// yet. Returns the temp dir (kept alive), its context and an open
    /// connection with the model store attached.
    fn library_with_one_pending_image() -> (tempfile::TempDir, LibraryContext, rusqlite::Connection)
    {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("lib");
        std::fs::create_dir(&root).unwrap();
        let ctx = LibraryContext::new(&root, &temp.path().join("cache")).unwrap();
        let conn = videre_core::library_db::initialize(&ctx).unwrap();
        let path = ctx.paths.root.join("a.jpg");
        conn.execute(
            "INSERT INTO file_hashes (path, hash, ext, mime)
               VALUES (?1, 'h_jpg', 'jpg', 'image/jpeg')",
            [path.to_string_lossy().as_ref()],
        )
        .unwrap();
        videre_core::embeddings_db::attach_in(
            &conn,
            &ctx,
            videre_core::embeddings::DEFAULT_MODEL_ID,
            true,
        )
        .unwrap();
        conn.execute(
            "INSERT INTO emb.embeddings (hash, model_id, embedding, embedded_at)
             VALUES ('h_jpg', ?1, X'0000', datetime('now'))",
            [videre_core::embeddings::DEFAULT_MODEL_ID],
        )
        .unwrap();
        (temp, ctx, conn)
    }

    fn parse(extra: &[&str]) -> ClassifyArgs {
        #[derive(clap::Parser)]
        struct Wrap {
            #[command(flatten)]
            args: ClassifyArgs,
        }
        let mut v = vec!["classify"];
        v.extend_from_slice(extra);
        <Wrap as clap::Parser>::parse_from(v).args
    }

    #[test]
    fn a_selection_matching_nothing_returns_before_loading_a_model() {
        // The model load sits after both early returns, so reaching one at all
        // proves no weights were touched. That matters: loading SigLIP is
        // ~0.8GB and minutes on a cold cache, and a scoped run that matches
        // nothing must not pay it.
        let (_temp, ctx, conn) = library_with_one_pending_image();
        let args = parse(&["--type", "video", "--silent"]);
        let r = run_classify(
            &args,
            &ctx,
            &conn,
            videre_core::embeddings::DEFAULT_MODEL_ID,
        );
        assert!(r.is_ok(), "a scope matching nothing is not an error: {r:?}");
        let classified: i64 = conn
            .query_row("SELECT COUNT(*) FROM classifications", [], |r| r.get(0))
            .unwrap();
        assert_eq!(classified, 0, "nothing may be written when nothing matched");
    }

    #[test]
    fn an_already_classified_library_also_returns_early() {
        let (_temp, ctx, conn) = library_with_one_pending_image();
        // `execute_batch`, not `execute`: the latter runs only the first
        // statement, so the INSERT silently never happened and `.ok()` hid the
        // error. The library was therefore *not* already classified, this test
        // did not take the early return it is named for, and it loaded SigLIP -
        // downloading 778MB on a cold cache from inside a unit test.
        //
        // On CI that landed in the cached weights, which woke
        // `cpu_batch_matches_single_image_baseline` in videre-ml: it skips when
        // weights are absent, and had done so since it was written. The Ubuntu
        // job went from ~3 minutes to nearly 40.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS classifications (
                hash TEXT NOT NULL, model_id TEXT NOT NULL, category TEXT NOT NULL,
                confidence REAL, classified_at TEXT, PRIMARY KEY (hash, model_id));
             INSERT INTO classifications (hash, model_id, category, confidence, classified_at)
               VALUES ('h_jpg', 'google/siglip-base-patch16-224', 'photo', 0.5, datetime('now'));",
        )
        .expect("seeding the classified row must succeed");

        let already: i64 = conn
            .query_row("SELECT COUNT(*) FROM classifications", [], |r| r.get(0))
            .unwrap();
        assert_eq!(already, 1, "the row this test depends on was not written");

        let args = parse(&["--silent"]);
        assert!(run_classify(
            &args,
            &ctx,
            &conn,
            videre_core::embeddings::DEFAULT_MODEL_ID
        )
        .is_ok());
    }

    #[test]
    fn classify_takes_every_filter_including_the_ones_embed_and_faces_refuse() {
        // classify has a model, so --category and --person resolve against real
        // data. embed and faces omit them because selecting their input by a
        // label they produce is circular.
        for ok in [
            vec!["classify", "--person", "Alice"],
            vec!["classify", "--category", "screenshot"],
            vec!["classify", "--location", "Berlin"],
            vec!["classify", "--date", "2024"],
            vec!["classify", "--type", "image"],
            vec!["classify", "--path", "/tmp"],
        ] {
            #[derive(clap::Parser)]
            struct Wrap {
                #[command(flatten)]
                args: ClassifyArgs,
            }
            assert!(
                <Wrap as clap::Parser>::try_parse_from(&ok).is_ok(),
                "classify must accept {:?}",
                ok[1]
            );
        }
    }

    #[test]
    fn an_unscoped_run_builds_an_empty_selection() {
        let a = parse(&[]);
        let sel = super::super::selection_args::row_selection(
            Some(&a.media),
            Some(&a.dates),
            Some(&a.place),
            Some(&a.people),
            Some(&a.presence),
            Some(&a.paths),
        )
        .unwrap();
        assert!(sel.is_empty(), "no flags must not narrow anything");
    }

    #[test]
    fn format_summary_reads_naturally() {
        assert_eq!(
            format_summary(42, std::time::Duration::from_secs(3)),
            // Tenths below 10s, because at 3s they are a tenth of the runtime.
            "42 image(s) classified, done in 3.0s"
        );
    }
}
