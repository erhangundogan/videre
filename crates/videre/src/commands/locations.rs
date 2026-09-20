use crate::command_context::CommandContext;
use anyhow::Result;
use rusqlite::Connection;
use serde::Serialize;
use videre::types::{ErrorJson, SCHEMA_VERSION};
use videre_core::location_cluster;

#[derive(clap::Args)]
pub struct LocationsArgs {
    /// Clustering radius in km, how close two coordinates must be to join
    /// the same location cluster. Default 15 ("which city was I in"
    /// granularity).
    #[arg(long, default_value_t = location_cluster::DEFAULT_CLUSTER_RADIUS_KM)]
    radius: f64,

    /// Emit a single JSON object on stdout instead of human-readable text
    #[arg(long, conflicts_with = "geojson")]
    json: bool,

    /// Emit a GeoJSON FeatureCollection on stdout instead of human-readable text
    #[arg(long, conflicts_with = "json")]
    geojson: bool,

    /// Suppress the per-run stdout summary (errors always shown)
    #[arg(long)]
    silent: bool,
}

#[derive(Debug, Serialize)]
struct LocationsJson {
    schema_version: u32,
    radius_km: f64,
    clusters: Vec<ClusterJson>,
}

#[derive(Debug, Serialize, Clone)]
struct ClusterJson {
    id: i64,
    name: Option<String>,
    centroid_lat: f64,
    centroid_lon: f64,
    photo_count: i64,
}

impl LocationsArgs {
    /// Pipeline-stage defaults: clap defaults for every knob, silence
    /// controlled by the pipeline. Parsing an empty argv keeps defaults from
    /// drifting from the flag definitions.
    pub(crate) fn for_pipeline(silent: bool) -> Self {
        #[derive(clap::Parser)]
        struct P {
            #[command(flatten)]
            a: LocationsArgs,
        }
        let argv: &[&str] = if silent {
            &["locations", "--silent"]
        } else {
            &["locations"]
        };
        <P as clap::Parser>::parse_from(argv).a
    }
}

pub fn run(args: LocationsArgs, ctx: &CommandContext) -> Result<()> {
    let conn = videre_core::library_db::open_existing(&ctx.library)?;
    // A recompute of the location partition, self-contained within the
    // location tables; ordinary shared activity, excluded only by exclusive
    // maintenance.
    let _activity = videre_core::library_locks::try_activity(
        &ctx.library,
        videre_core::library_locks::ActivityMode::Shared,
    )?;
    let guard = videre_core::library_locks::try_command(&ctx.library, "locations")?;

    if args.json {
        match run_locations_tracked(&args, ctx, &guard, &conn) {
            Ok(clusters) => {
                let doc = LocationsJson {
                    schema_version: SCHEMA_VERSION,
                    radius_km: args.radius,
                    clusters,
                };
                println!("{}", serde_json::to_string(&doc)?);
                Ok(())
            }
            Err(e) => {
                println!("{}", serde_json::to_string(&ErrorJson::from_err(&e))?);
                std::process::exit(1);
            }
        }
    } else if args.geojson {
        let clusters = run_locations_tracked(&args, ctx, &guard, &conn)?;
        println!("{}", to_geojson(&clusters, args.radius));
        Ok(())
    } else {
        let clusters = run_locations_tracked(&args, ctx, &guard, &conn)?;
        print_summary(&clusters, args.radius, args.silent);
        Ok(())
    }
}

fn run_locations_tracked(
    args: &LocationsArgs,
    ctx: &CommandContext,
    guard: &videre_core::library_locks::CommandGuard,
    conn: &Connection,
) -> Result<Vec<ClusterJson>> {
    videre_core::pipeline_runs::track_in(conn, &ctx.library, guard, "locations", || {
        run_locations(args, ctx, conn)
    })
}

/// Delegates the whole-library recompute to `videre-core`, mapping its result
/// into the command's `ClusterJson`. The recompute is a full rebuild every run
/// (cluster ids are not stable across reruns); its rationale lives with
/// `location_cluster::recompute_all`.
fn run_locations(
    args: &LocationsArgs,
    ctx: &CommandContext,
    conn: &Connection,
) -> Result<Vec<ClusterJson>> {
    let quiet = args.silent || args.json || args.geojson;
    let clusters = location_cluster::recompute_all(conn, &ctx.library.cache, args.radius, quiet)?;
    Ok(clusters
        .into_iter()
        .map(|c| ClusterJson {
            id: c.id,
            name: c.name,
            centroid_lat: c.centroid_lat,
            centroid_lon: c.centroid_lon,
            photo_count: c.photo_count,
        })
        .collect())
}

fn print_summary(clusters: &[ClusterJson], radius_km: f64, silent: bool) {
    if silent {
        return;
    }
    for (i, c) in clusters.iter().enumerate() {
        let name = c.name.as_deref().unwrap_or("(unnamed)");
        println!(
            "{}. {name} - {} photo(s) ({:.4}, {:.4})",
            i + 1,
            c.photo_count,
            c.centroid_lat,
            c.centroid_lon
        );
    }
    println!(
        "{} location cluster(s) found (radius={radius_km}km).",
        clusters.len()
    );
}

fn to_geojson(clusters: &[ClusterJson], radius_km: f64) -> String {
    let features: Vec<serde_json::Value> = clusters
        .iter()
        .map(|c| {
            serde_json::json!({
                "type": "Feature",
                "geometry": { "type": "Point", "coordinates": [c.centroid_lon, c.centroid_lat] },
                "properties": {
                    "id": c.id,
                    "name": c.name,
                    "photo_count": c.photo_count,
                    "radius_km": radius_km
                }
            })
        })
        .collect();
    serde_json::to_string(&serde_json::json!({
        "type": "FeatureCollection",
        "features": features
    }))
    .expect("GeoJSON values are all serializable")
}
