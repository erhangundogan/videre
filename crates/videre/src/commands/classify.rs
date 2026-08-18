use anyhow::{Context, Result};
use std::path::PathBuf;
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
    paths: super::selection_args::PathArgs,

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

    /// Embedding model whose vectors to classify (default:
    /// 'videre config set model', else the built-in default). Classifications are
    /// stored per model, so two models classify independently.
    #[arg(long, value_parser = super::parse_model_id)]
    model: Option<String>,

    /// Suppress per-image progress output on stderr (errors always shown)
    #[arg(long)]
    silent: bool,
}

pub fn run(args: ClassifyArgs) -> Result<()> {
    let db = super::resolve_reader_db(args.db.clone())?;
    let conn = videre_core::db::open_wal(&db).with_context(|| format!("open {}", db.display()))?;

    let model_id = videre_core::embeddings::resolve_model_id(args.model.as_deref())?;
    videre_core::embeddings_db::attach_for_read(&conn, &db, &model_id)?;

    videre_core::pipeline_runs::track(&conn, &db, "classify", || {
        run_classify(&args, &conn, &model_id)
    })
}

/// The actual classification work, wrapped by `track()` above.
fn run_classify(args: &ClassifyArgs, conn: &rusqlite::Connection, model_id: &str) -> Result<()> {
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

    if hashes.is_empty() {
        if !args.silent {
            eprintln!("Nothing to classify: all embedded hashes already classified.");
        }
        return Ok(());
    }

    // Scope narrows the pending set; eligibility and staleness stay above.
    let selection = super::selection_args::row_selection(
        Some(&args.media),
        Some(&args.dates),
        Some(&args.place),
        Some(&args.people),
        Some(&args.paths),
    )?;
    let eligible = hashes.len();
    let hashes: Vec<String> = if selection.is_empty() {
        hashes
    } else {
        let sel = selection.resolve(
            conn,
            &videre_core::selection::SelectionCtx {
                model_id: Some(model_id.to_string()),
            },
        )?;
        match sel.hashes {
            None => hashes,
            Some(h) => hashes.into_iter().filter(|x| h.contains(x)).collect(),
        }
    };
    if !selection.is_empty() && !args.silent {
        eprintln!(
            "Classifying {} of {} pending hash(es) ({})",
            hashes.len(),
            eligible,
            selection.describe()
        );
    }
    if hashes.is_empty() {
        if !args.silent {
            eprintln!("Nothing to classify: the selection matched no pending hashes.");
        }
        return Ok(());
    }

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
}

/// Assembles the single consolidated summary line printed after
/// classification finishes.
fn format_summary(done: usize, elapsed: std::time::Duration) -> String {
    format!("{done} image(s) classified, done in {}s", elapsed.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `VIDERE_HOME` is set once per test binary, and every test here calls this
    /// before deriving any path from it. Setting it per test races every
    /// concurrent getenv; deriving a path on both sides of the one flip is how
    /// the report tests failed intermittently for days.
    fn test_home() -> &'static std::path::Path {
        static HOME: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
        HOME.get_or_init(|| {
            let dir = std::env::temp_dir().join(format!("videre-classify-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            unsafe { std::env::set_var("VIDERE_HOME", &dir) };
            dir
        })
    }

    /// A library with one embedded jpeg, nothing classified yet.
    fn library_with_one_pending_image() -> rusqlite::Connection {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let home = test_home();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE file_hashes (
                path TEXT PRIMARY KEY, hash TEXT NOT NULL, size_bytes INTEGER,
                created_at TEXT, modified_at TEXT, ext TEXT, mime TEXT, phash INTEGER,
                exif_date TEXT, gps_lat REAL, gps_lon REAL, width INTEGER, height INTEGER);
             INSERT INTO file_hashes (path, hash, ext, mime)
               VALUES ('/lib/a.jpg', 'h_jpg', 'jpg', 'image/jpeg');",
        )
        .unwrap();
        let i = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let lib = home.join(format!("classify-{i}.db"));
        std::fs::write(&lib, b"").unwrap();
        videre_core::embeddings_db::attach(
            &conn,
            &lib,
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
        conn
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
        let conn = library_with_one_pending_image();
        let args = parse(&["--type", "video", "--silent"]);
        let r = run_classify(&args, &conn, videre_core::embeddings::DEFAULT_MODEL_ID);
        assert!(r.is_ok(), "a scope matching nothing is not an error: {r:?}");
        let classified: i64 = conn
            .query_row("SELECT COUNT(*) FROM classifications", [], |r| r.get(0))
            .unwrap();
        assert_eq!(classified, 0, "nothing may be written when nothing matched");
    }

    #[test]
    fn an_already_classified_library_also_returns_early() {
        let conn = library_with_one_pending_image();
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
        assert!(run_classify(&args, &conn, videre_core::embeddings::DEFAULT_MODEL_ID).is_ok());
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
            Some(&a.paths),
        )
        .unwrap();
        assert!(sel.is_empty(), "no flags must not narrow anything");
    }

    #[test]
    fn format_summary_reads_naturally() {
        assert_eq!(
            format_summary(42, std::time::Duration::from_secs(3)),
            "42 image(s) classified, done in 3s"
        );
    }
}
