use super::faces::format_clustering_only_summary;
use crate::command_context::CommandContext;
use anyhow::Result;
use rayon::prelude::*;
use std::time::Duration;
use videre::{hasher, scanner, sqlite_output, types};
use videre_core::face_db;
use videre_ml::pipeline::{run_clustering, run_face_pipeline_in};

#[derive(clap::Args)]
pub struct WatchArgs {
    /// Narrow the walk, exactly as on `videre scan`. Path-side only.
    #[command(flatten)]
    media: super::selection_args::MediaArgs,

    /// Re-run the scan/hash/EXIF pipeline each cycle
    #[arg(long)]
    scan: bool,
    /// Run incremental face detection each cycle
    #[arg(long)]
    faces: bool,
    /// Pre-convert and cache HEIC thumbnails each cycle
    #[arg(long)]
    heic: bool,
    /// Pre-resolve reverse-geocoded location names each cycle
    #[arg(long)]
    location: bool,
    /// Sync stale rows/cache and clean orphans each cycle (same cleanup as
    /// `videre prune`). Opt-in only, unlike the other four stages, this is
    /// NOT included when no stage flags are passed, so existing `videre
    /// watch` invocations keep their current behavior unchanged. Never
    /// deletes real files, only stale db rows and cache entries for files
    /// already gone from disk.
    #[arg(long)]
    prune: bool,
    /// Write XMP sidecars for updated labels each cycle (opt-in). Also enabled
    /// by `videre config set export-xmp-on-watch true`.
    #[arg(long)]
    export_xmp: bool,

    /// Seconds between cycles
    #[arg(long, default_value = "300")]
    interval: u64,

    #[command(flatten)]
    xmp: crate::xmp::XmpArg,

    #[arg(long)]
    silent: bool,
}

pub fn run(mut args: WatchArgs, ctx: &CommandContext) -> Result<()> {
    // If NO stage flag at all was passed (including --prune), run the
    // original all-four default: the common case is "just keep everything up
    // to date". An explicit --prune is a stage selection and does not turn the
    // others on.
    if !(args.scan || args.faces || args.heic || args.location || args.prune) {
        args.scan = true;
        args.faces = true;
        args.heic = true;
        args.location = true;
    }

    // The XMP export stage is opt-in: the flag, or the library config default.
    if ctx.library.settings.export_xmp_on_watch {
        args.export_xmp = true;
    }

    // Watch is a writer: initialize the selected library so its database and
    // lock directory exist before the lifetime lock is taken.
    videre_core::library_db::initialize(&ctx.library)?;

    // Held for the entire life of this process (released even on kill), so a
    // second `videre watch` against this library is refused rather than racing.
    // No pipeline_runs row: watch has no "finished" moment, only running or not.
    let _watch_lock = videre_core::library_locks::try_command(&ctx.library, "watch")?;

    loop {
        if !args.silent {
            eprintln!(
                "videre watch: cycle starting ({})",
                chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC")
            );
        }
        if let Err(e) = run_cycle(&args, ctx) {
            eprintln!("videre watch: cycle error: {e}");
        }
        if !args.silent {
            eprintln!("videre watch: sleeping {}s", args.interval);
        }
        std::thread::sleep(Duration::from_secs(args.interval));
    }
}

/// Run a tracked stage under the library's activity lease and its own command
/// lock, in that order. A busy activity lease (an exclusive maintenance pass,
/// or another shared operation when this stage needs exclusive) or a busy
/// command lock (a standalone run of that stage) is reported and skipped; the
/// next cycle retries. Never reacquires the watch lifetime lock.
fn tracked_stage(
    ctx: &CommandContext,
    conn: &rusqlite::Connection,
    command: &str,
    mode: videre_core::library_locks::ActivityMode,
    silent: bool,
    f: impl FnOnce() -> Result<()>,
) -> Result<()> {
    let _activity = match videre_core::library_locks::try_activity(&ctx.library, mode) {
        Ok(guard) => guard,
        Err(_) => {
            if !silent {
                eprintln!("videre watch: {command} stage busy (the library is in use); will retry next cycle");
            }
            return Ok(());
        }
    };
    match videre_core::library_locks::try_command(&ctx.library, command) {
        Ok(guard) => videre_core::pipeline_runs::track_in(conn, &ctx.library, &guard, command, f),
        Err(_) => {
            if !silent {
                eprintln!("videre watch: {command} stage busy (a {command} run is active); will retry next cycle");
            }
            Ok(())
        }
    }
}

fn run_cycle(args: &WatchArgs, ctx: &CommandContext) -> Result<()> {
    // Recheck before every cycle: a root renamed or replaced under a
    // long-running watch must fail the cycle rather than process whatever now
    // sits at the path. open_existing rechecks again at the write boundary.
    ctx.library.ensure_root_identity()?;
    let conn = videre_core::library_db::open_existing(&ctx.library)?;
    if args.scan {
        // A scan failure this cycle does not invalidate earlier rows; log and
        // carry on to the stages below.
        if let Err(e) = run_scan_stage(args, ctx, &conn) {
            eprintln!("videre watch: scan stage error: {e}");
        }
    }
    if args.faces || args.heic || args.location || args.prune || args.export_xmp {
        face_db::create_faces_table(&conn)?;
        videre_core::location::ensure_location_column(&conn);
        if args.faces {
            run_faces_stage(args, ctx, &conn)?;
        }
        if args.heic {
            run_heic_stage(args, ctx, &conn)?;
        }
        if args.location {
            run_location_stage(args, ctx, &conn)?;
        }
        if args.prune {
            if let Err(e) = run_prune_stage(args, ctx, &conn) {
                eprintln!("videre watch: prune stage error: {e}");
            }
        }
        if args.export_xmp {
            if let Err(e) = run_export_stage(args, ctx, &conn) {
                eprintln!("videre watch: export stage error: {e}");
            }
        }
    }
    Ok(())
}

/// Writes XMP sidecars for the current labels (marks, named face regions,
/// location, category), merging into any existing sidecar. Runs the same shared
/// writer as `videre export --xmp`, over every file, against the already-open
/// connection.
fn run_export_stage(
    args: &WatchArgs,
    ctx: &CommandContext,
    conn: &rusqlite::Connection,
) -> Result<()> {
    let n = super::export::export_all_in(conn, ctx)?;
    if !args.silent {
        eprintln!("videre watch: export stage wrote {n} sidecar(s)");
    }
    Ok(())
}

/// Runs the same cleanup as `videre prune` (stale-row removal, modified_at
/// sync, orphan embeddings/cache cleanup) against the already-open
/// connection, tracked in `pipeline_runs` under "prune" exactly like a
/// standalone `videre prune` invocation would be.
fn run_prune_stage(
    args: &WatchArgs,
    ctx: &CommandContext,
    conn: &rusqlite::Connection,
) -> Result<()> {
    let prune_args = super::prune::PruneArgs::for_watch_stage(args.silent);
    tracked_stage(
        ctx,
        conn,
        "prune",
        videre_core::library_locks::ActivityMode::Exclusive,
        args.silent,
        || {
            let errors = super::prune::run_prune(&prune_args, &ctx.library, conn)?;
            if !args.silent && errors > 0 {
                eprintln!("videre watch: prune stage finished with {errors} error(s)");
            }
            Ok(())
        },
    )
}

/// Queries (path, hash) pairs from file_hashes matching a SQL WHERE clause,
/// deduped to one representative path per hash.
/// The fixed set of extension filters `dedup_paths_by_hash` supports, a
/// closed enum rather than a raw `&str` WHERE-clause fragment, so there is no
/// way for a future caller to pass runtime-built SQL text into the query.
enum PathExtFilter {
    /// Every extension `videre faces` detects on: images plus HEIC.
    Faces,
    /// HEIC only, for the thumbnail-cache warming stage.
    HeicOnly,
}

fn dedup_paths_by_hash(
    conn: &rusqlite::Connection,
    filter: PathExtFilter,
) -> Result<Vec<(String, String)>> {
    let sql = match filter {
        PathExtFilter::Faces => {
            "SELECT path, hash FROM file_hashes \
             WHERE ext IN ('jpg','jpeg','png','gif','webp','bmp','tiff','heic')"
        }
        PathExtFilter::HeicOnly => "SELECT path, hash FROM file_hashes WHERE ext = 'heic'",
    };
    let mut stmt = conn.prepare(sql)?;
    let rows: Vec<(String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut seen = std::collections::HashSet::new();
    Ok(rows
        .into_iter()
        .filter(|(_, hash)| seen.insert(hash.clone()))
        .collect())
}

fn run_faces_stage(
    args: &WatchArgs,
    ctx: &CommandContext,
    conn: &rusqlite::Connection,
) -> Result<()> {
    tracked_stage(
        ctx,
        conn,
        "faces",
        videre_core::library_locks::ActivityMode::Shared,
        args.silent,
        || {
            let all_paths = dedup_paths_by_hash(conn, PathExtFilter::Faces)?;
            // Skip already-scanned hashes (marker includes no-face images), unioned with
            // hashes that already have faces for pre-marker migration, same resumable
            // skip set as `videre faces`.
            let mut skip_hashes: std::collections::HashSet<String> =
                face_db::scanned_hashes(conn)?.into_iter().collect();
            skip_hashes.extend(face_db::hashes_with_faces(conn)?);
            let to_process: Vec<(String, String)> = all_paths
                .into_iter()
                .filter(|(_, hash)| !skip_hashes.contains(hash))
                .collect();

            if !to_process.is_empty() {
                let workers = std::thread::available_parallelism()
                    .map(|n| n.get())
                    .unwrap_or(4);
                let result = run_face_pipeline_in(
                    &ctx.library,
                    conn,
                    &to_process,
                    8,
                    false,
                    args.silent,
                    None,
                    workers,
                )?;
                if !args.silent {
                    eprintln!(
                        "videre watch: faces stage processed {} new hash(es), {} face(s)",
                        to_process.len(),
                        result.total_faces
                    );
                }
            }
            let clustering = run_clustering(
                conn,
                0.6,
                3,
                videre_core::face_cluster::DEFAULT_MERGE_SIM,
                videre_core::face_cluster::DEFAULT_MIN_FACE_PX,
                videre_core::face_cluster::DEFAULT_MAX_GENERIC_SIM,
                videre_core::face_cluster::DEFAULT_MAX_LANDMARK_ERR,
                videre_core::face_cluster::DEFAULT_MIN_BLUR,
                1.0,
                args.silent,
            )?;
            if !args.silent {
                eprintln!(
                    "videre watch: {}",
                    format_clustering_only_summary(clustering, 0.6)
                );
            }
            Ok(())
        },
    )
}

/// Writes `img` as a JPEG to `tmp_path`, then atomically renames it to
/// `final_path`. Returns true on success.
///
/// `image::DynamicImage::save()` infers the encoder from the file
/// extension via `Path::extension()`; a tmp path like `hash_240.tmp19181`
/// has extension `tmp19181`, which maps to no encoder and always fails.
/// The tmp-file-then-rename pattern is correct for atomic publishing into
/// the cache; only the encode step must not rely on extension inference,
/// so the format is passed explicitly.
fn publish_thumb(
    img: &image::DynamicImage,
    tmp_path: &std::path::Path,
    final_path: &std::path::Path,
) -> bool {
    img.save_with_format(tmp_path, image::ImageFormat::Jpeg)
        .is_ok()
        && std::fs::rename(tmp_path, final_path).is_ok()
}

fn run_heic_stage(
    args: &WatchArgs,
    ctx: &CommandContext,
    conn: &rusqlite::Connection,
) -> Result<()> {
    let cache = &ctx.library.cache;
    let heic_paths = dedup_paths_by_hash(conn, PathExtFilter::HeicOnly)?;
    let mut converted = 0usize;
    let mut failed = 0usize;
    for (path, hash) in heic_paths {
        let need_240 = !videre_core::thumb_cache::thumb_exists_in(cache, &hash, 240);
        let need_1200 = !videre_core::thumb_cache::thumb_exists_in(cache, &hash, 1200);
        // Also feeds `videre faces`'s detection cache (see `load_image` in
        // videre-ml's pipeline.rs), a full-res original cached here means
        // detection can skip its own qlmanage decode entirely for this hash.
        let need_original = !videre_core::thumb_cache::original_exists_in(cache, &hash);
        if !need_240 && !need_1200 && !need_original {
            continue;
        }
        std::fs::create_dir_all(&cache.thumbnails).ok();
        // Convert once, then downscale the same in-memory image for each
        // missing size (largest first) instead of re-running QuickLook per
        // size. None (full resolution), not Some(n): the same decode also
        // seeds the `original_path` cache below, which detection's bbox
        // coordinates depend on being full resolution. See the safety note
        // on heic_via_quicklook. This means this call site does NOT get
        // lever-2a's render-size-cap treatment other watch-adjacent callers
        // do; the two levers are in tension here and full-res wins because
        // avoiding a second full qlmanage decode during `videre faces` is
        // the bigger saving of the two.
        match videre_core::heic::heic_via_quicklook(&path, "watch", None) {
            Some(img) => {
                if need_original {
                    let tmp_path = videre_core::thumb_cache::original_tmp_path_in(cache, &hash);
                    let final_path = videre_core::thumb_cache::original_path_in(cache, &hash);
                    if publish_thumb(&img, &tmp_path, &final_path) {
                        converted += 1;
                    } else {
                        failed += 1;
                        let _ = std::fs::remove_file(&tmp_path);
                    }
                }
                for size in [1200u32, 240] {
                    let need = if size == 240 { need_240 } else { need_1200 };
                    if !need {
                        continue;
                    }
                    let resized = if img.width() > size || img.height() > size {
                        img.resize(size, size, image::imageops::FilterType::Triangle)
                    } else {
                        img.clone()
                    };
                    let tmp_path = videre_core::thumb_cache::thumb_tmp_path_in(cache, &hash, size);
                    let final_path = videre_core::thumb_cache::thumb_path_in(cache, &hash, size);
                    if publish_thumb(&resized, &tmp_path, &final_path) {
                        converted += 1;
                    } else {
                        failed += 1;
                        let _ = std::fs::remove_file(&tmp_path);
                    }
                }
            }
            None => {
                if need_240 {
                    failed += 1;
                }
                if need_1200 {
                    failed += 1;
                }
                if need_original {
                    failed += 1;
                }
            }
        }
    }
    if !args.silent && (converted > 0 || failed > 0) {
        if failed > 0 {
            eprintln!("videre watch: heic stage cached {converted} thumbnail(s), {failed} failed");
        } else {
            eprintln!("videre watch: heic stage cached {converted} thumbnail(s)");
        }
    }
    Ok(())
}

fn run_location_stage(
    args: &WatchArgs,
    ctx: &CommandContext,
    conn: &rusqlite::Connection,
) -> Result<()> {
    let unresolved: Vec<(f64, f64)> = {
        let mut stmt = conn.prepare(
            "SELECT DISTINCT gps_lat, gps_lon FROM file_hashes \
             WHERE gps_lat IS NOT NULL AND gps_lon IS NOT NULL AND location_name IS NULL",
        )?;
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    let mut resolved = 0usize;
    for (lat, lon) in unresolved {
        if let Some(name) = videre_core::location::location_name_in(&ctx.library.cache, lat, lon)? {
            conn.execute(
                "UPDATE file_hashes SET location_name = ?1 \
                 WHERE ROUND(gps_lat, 6) = ROUND(?2, 6) AND ROUND(gps_lon, 6) = ROUND(?3, 6)",
                rusqlite::params![name, lat, lon],
            )?;
            resolved += 1;
        }
    }
    if !args.silent && resolved > 0 {
        eprintln!("videre watch: location stage resolved {resolved} coordinate(s)");
    }
    Ok(())
}

fn run_scan_stage(
    args: &WatchArgs,
    ctx: &CommandContext,
    conn: &rusqlite::Connection,
) -> Result<()> {
    let selection = super::selection_args::path_selection(Some(&args.media), None)?;
    let root = ctx.library.paths.root.clone();
    tracked_stage(
        ctx,
        conn,
        "scan",
        videre_core::library_locks::ActivityMode::Shared,
        args.silent,
        || {
            // Exclude the .videre state directory from the walk, exactly as scan does.
            let paths: Vec<_> = scanner::scan(&root)
                .into_iter()
                .filter(|p| !p.components().any(|c| c.as_os_str() == ".videre"))
                .collect();
            let walked = paths.len();
            let paths: Vec<_> = if selection.is_empty() {
                paths
            } else {
                paths.into_iter().filter(|p| selection.accepts(p)).collect()
            };
            if !selection.is_empty() && !args.silent {
                eprintln!(
                    "videre watch: scan stage considering {} of {} file(s) ({})",
                    paths.len(),
                    walked,
                    selection.describe()
                );
            }
            // Incremental, like `scan`: skip files whose row is already current,
            // so each cycle only hashes new or changed files rather than the
            // whole library. `--similar` is not a watch concept, so no phash.
            let sigs = videre_core::db::stored_signatures(conn).unwrap_or_default();
            let paths: Vec<_> = paths
                .into_iter()
                .filter(|p| videre::incremental::needs_processing(&sigs, p, false))
                .collect();
            let records: Vec<types::FileRecord> = paths
                .par_iter()
                .filter_map(|path| hasher::hash_file(path).ok())
                .collect();
            sqlite_output::write_records_in(conn, &ctx.library, &records)?;
            let prec = args.xmp.resolve_from(&ctx.library.settings)?;
            // Reconcile XMP over the whole library, not just files hashed this
            // cycle (see scan.rs): the incremental skip must not change XMP
            // behaviour.
            crate::xmp::import_xmp_all_in(conn, &ctx.library, prec, args.silent)?;
            if !args.silent {
                eprintln!("videre watch: scan stage wrote {} record(s)", records.len());
            }
            Ok(())
        },
    )
}

#[cfg(test)]
mod publish_thumb_tests {
    use super::*;

    #[test]
    fn publish_thumb_writes_jpeg_despite_tmp_extension() {
        // Regression test: the watch heic stage writes through a tmp path
        // shaped like `hash_240.tmp<pid>` (see thumb_cache::thumb_tmp_path),
        // then renames it into place. image::DynamicImage::save() infers the
        // encoder from the file extension, and a ".tmp<pid>" suffix maps to
        // no encoder, so a plain save() always fails on this tmp path shape.
        let tmp_dir =
            std::env::temp_dir().join(format!("publish_thumb_test_{}", std::process::id()));
        std::fs::create_dir_all(&tmp_dir).unwrap();
        let tmp_path = tmp_dir.join(format!("hash_240.tmp{}", std::process::id()));
        let final_path = tmp_dir.join("hash_240.jpg");

        let img = image::DynamicImage::new_rgb8(4, 4);
        let ok = publish_thumb(&img, &tmp_path, &final_path);

        assert!(
            ok,
            "publish_thumb should succeed even though the tmp path has no recognizable extension"
        );
        assert!(
            final_path.exists(),
            "final thumbnail file should exist after publish"
        );
        assert!(!tmp_path.exists(), "tmp file should be gone after rename");

        let _ = std::fs::remove_dir_all(&tmp_dir);
    }
}

#[cfg(test)]
mod stage_query_tests {
    use super::*;
    use rusqlite::Connection;

    fn db_with(rows: &[(&str, &str, &str)]) -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE file_hashes (
                path TEXT PRIMARY KEY, hash TEXT NOT NULL, size_bytes INTEGER,
                created_at TEXT, modified_at TEXT, ext TEXT, mime TEXT, phash INTEGER,
                exif_date TEXT, gps_lat REAL, gps_lon REAL, width INTEGER, height INTEGER);",
        )
        .unwrap();
        for (path, hash, ext) in rows {
            c.execute(
                "INSERT INTO file_hashes (path, hash, ext) VALUES (?1, ?2, ?3)",
                rusqlite::params![path, hash, ext],
            )
            .unwrap();
        }
        c
    }

    #[test]
    fn the_faces_filter_takes_images_including_heic_and_no_video() {
        let c = db_with(&[
            ("/a.jpg", "h1", "jpg"),
            ("/b.heic", "h2", "heic"),
            ("/c.png", "h3", "png"),
            ("/d.mov", "h4", "mov"),
            ("/e.mp4", "h5", "mp4"),
            ("/f.dng", "h6", "dng"),
        ]);
        let got = dedup_paths_by_hash(&c, PathExtFilter::Faces).unwrap();
        let mut exts: Vec<_> = got
            .iter()
            .map(|(p, _)| p.rsplit('.').next().unwrap())
            .collect();
        exts.sort();
        assert_eq!(
            exts,
            vec!["heic", "jpg", "png"],
            "video and raw must not reach face detection"
        );
    }

    #[test]
    fn the_heic_filter_takes_only_heic() {
        let c = db_with(&[("/a.jpg", "h1", "jpg"), ("/b.heic", "h2", "heic")]);
        let got = dedup_paths_by_hash(&c, PathExtFilter::HeicOnly).unwrap();
        assert_eq!(got.len(), 1);
        assert!(got[0].0.ends_with(".heic"));
    }

    #[test]
    fn duplicates_collapse_to_one_path_per_hash() {
        // The point of the dedup: three copies of one photo cost one decode,
        // not three. Whichever path wins, the hash must appear once.
        let c = db_with(&[
            ("/one.jpg", "same", "jpg"),
            ("/two.jpg", "same", "jpg"),
            ("/three.jpg", "same", "jpg"),
            ("/other.jpg", "different", "jpg"),
        ]);
        let got = dedup_paths_by_hash(&c, PathExtFilter::Faces).unwrap();
        assert_eq!(got.len(), 2);
        let mut hashes: Vec<_> = got.iter().map(|(_, h)| h.as_str()).collect();
        hashes.sort();
        assert_eq!(hashes, vec!["different", "same"]);
    }

    #[test]
    fn an_empty_library_yields_no_work_rather_than_an_error() {
        let c = db_with(&[]);
        assert!(dedup_paths_by_hash(&c, PathExtFilter::Faces)
            .unwrap()
            .is_empty());
        assert!(dedup_paths_by_hash(&c, PathExtFilter::HeicOnly)
            .unwrap()
            .is_empty());
    }
}

#[cfg(test)]
mod scoping_tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct Wrap {
        #[command(flatten)]
        args: WatchArgs,
    }

    fn parse(extra: &[&str]) -> WatchArgs {
        let mut v = vec!["watch"];
        v.extend_from_slice(extra);
        Wrap::parse_from(v).args
    }

    #[test]
    fn watch_accepts_the_media_flags_only() {
        // The walk is rooted at the invocation library and has not opened any
        // file, so it can answer only the media flags (--type/--ext). --date,
        // --location and the data-derived selectors must fail to parse rather
        // than fail at runtime.
        let a = parse(&["--type", "image", "--ext", "heic"]);
        let sel = super::super::selection_args::path_selection(Some(&a.media), None).unwrap();
        assert!(!sel.is_empty());

        for bad in [
            vec!["watch", "--date", "2024"],
            vec!["watch", "--location", "Berlin"],
            vec!["watch", "--person", "Alice"],
            vec!["watch", "--category", "screenshot"],
            vec!["watch", "--path", "/tmp/x"],
        ] {
            assert!(
                Wrap::try_parse_from(&bad).is_err(),
                "watch must reject {:?}: it is not part of watch's vocabulary",
                bad[1]
            );
        }
    }

    #[test]
    fn no_flags_means_an_empty_selection_that_accepts_everything() {
        let a = parse(&[]);
        let sel = super::super::selection_args::path_selection(Some(&a.media), None).unwrap();
        assert!(sel.is_empty(), "an unscoped watch must not filter the walk");
        assert!(sel.accepts(std::path::Path::new("/anything/at/all.mov")));
    }

    #[test]
    fn a_type_filter_narrows_the_walk_the_same_way_scan_does() {
        let a = parse(&["--type", "video"]);
        let sel = super::super::selection_args::path_selection(Some(&a.media), None).unwrap();
        assert!(sel.accepts(std::path::Path::new("/x/clip.mov")));
        assert!(!sel.accepts(std::path::Path::new("/x/photo.jpg")));
    }
}
