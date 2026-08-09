use anyhow::{Context, Result};
use rusqlite::Connection;
use serde::Serialize;
use std::path::PathBuf;
use videre::types::{ErrorJson, SCHEMA_VERSION};
use videre_core::location_cluster;

#[derive(clap::Args)]
pub struct LocationsArgs {
    /// SQLite database (default: resolved from ~/.videre; see 'videre config')
    #[arg(long)]
    db: Option<PathBuf>,

    /// Clustering radius in km, how close two coordinates must be to join
    /// the same location cluster. Default 15 ("which city was I in"
    /// granularity).
    #[arg(long, default_value_t = 15.0)]
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

pub fn run(args: LocationsArgs) -> Result<()> {
    let db = super::resolve_reader_db(args.db.clone())?;
    let conn = videre_core::db::open_wal(&db).with_context(|| format!("open {}", db.display()))?;

    if args.json {
        match run_locations_tracked(&args, &db, &conn) {
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
        let clusters = run_locations_tracked(&args, &db, &conn)?;
        println!("{}", to_geojson(&clusters, args.radius));
        Ok(())
    } else {
        let clusters = run_locations_tracked(&args, &db, &conn)?;
        print_summary(&clusters, args.radius, args.silent);
        Ok(())
    }
}

fn run_locations_tracked(
    args: &LocationsArgs,
    db: &std::path::Path,
    conn: &Connection,
) -> Result<Vec<ClusterJson>> {
    videre_core::pipeline_runs::track(conn, db, "locations", || run_locations(args, conn))
}

/// The actual clustering work, wrapped by `track()` above. Full recompute
/// every run: truncates `location_clusters` and clears
/// `file_hashes.location_cluster_id`, then reclusters from scratch over
/// every distinct GPS coordinate. Cluster IDs are therefore not stable
/// across reruns (see the design spec's section 1).
fn run_locations(args: &LocationsArgs, conn: &Connection) -> Result<Vec<ClusterJson>> {
    let tx = conn.unchecked_transaction()?;

    location_cluster::ensure_location_clusters_table(&tx)?;
    location_cluster::ensure_location_cluster_id_column(&tx);

    let coords: Vec<(f64, f64)> = {
        let mut stmt = tx.prepare(
            "SELECT DISTINCT gps_lat, gps_lon FROM file_hashes \
             WHERE gps_lat IS NOT NULL AND gps_lon IS NOT NULL",
        )?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, f64>(0)?, r.get::<_, f64>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };

    tx.execute("DELETE FROM location_clusters", [])?;
    tx.execute(
        "UPDATE file_hashes SET location_cluster_id = NULL WHERE location_cluster_id IS NOT NULL",
        [],
    )?;

    if coords.is_empty() {
        tx.commit()?;
        return Ok(Vec::new());
    }

    let member_groups = location_cluster::cluster_by_distance(&coords, args.radius);

    let mut clusters = Vec::with_capacity(member_groups.len());
    for members in &member_groups {
        let (centroid_lat, centroid_lon) = location_cluster::centroid(&coords, members);
        let name = videre_core::location::location_name(centroid_lat, centroid_lon);

        tx.execute(
            "INSERT INTO location_clusters \
             (centroid_lat, centroid_lon, name, photo_count, radius_km, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, datetime('now'))",
            rusqlite::params![centroid_lat, centroid_lon, name, 0i64, args.radius],
        )?;
        let id = tx.last_insert_rowid();

        let mut photo_count = 0i64;
        for &idx in members {
            let (lat, lon) = coords[idx];
            let affected = tx.execute(
                "UPDATE file_hashes SET location_cluster_id = ?1 \
                 WHERE ROUND(gps_lat, 6) = ROUND(?2, 6) AND ROUND(gps_lon, 6) = ROUND(?3, 6)",
                rusqlite::params![id, lat, lon],
            )?;
            photo_count += affected as i64;
        }

        tx.execute(
            "UPDATE location_clusters SET photo_count = ?1 WHERE id = ?2",
            rusqlite::params![photo_count, id],
        )?;

        clusters.push(ClusterJson {
            id,
            name,
            centroid_lat,
            centroid_lon,
            photo_count,
        });
    }

    clusters.sort_by(|a, b| b.photo_count.cmp(&a.photo_count));
    tx.commit()?;
    Ok(clusters)
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
