//! Street-level map detail for one library's places: roads, buildings and
//! names at zooms 9 to 14, cut once from the public Protomaps planet build
//! that Source Cooperative hosts, and served offline afterwards.
//!
//! :warning: **Opt-in, because the cut leaves the machine.** Asking for the
//! tiles around a library's places tells the host roughly where its photos
//! were taken. Off unless `street_detail` is set, and even then the areas
//! asked for are whole 1-degree cells (about 111 x 70 km at Berlin's
//! latitude), never a place's own box. Turning the setting off deletes the
//! archive ([`remove`]), so the setting and the disk always agree.
//!
//! Nothing streams from the host while browsing: its terms ask apps not to
//! hotlink, and range-reading a region once is the use it documents. The
//! world basemap (`crate::basemap`, zooms 0 to 8) is unchanged and still
//! drawn underneath.
//!
//! The planet archive is about 125 GB, so the cut reads only the PMTiles
//! directories and tile bytes the cells need, by HTTP range, pinned to the
//! archive's ETag so a mid-cut republish fails rather than mixing builds.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// The planet build: Protomaps basemap schema v4, zooms 0 to 15, mirrored by
/// Source Cooperative under a versioned name kept long-term.
pub const SOURCE_URL: &str = "https://data.source.coop/protomaps/openstreetmap/v4.pmtiles";

/// The zooms cut. Below 9 the world basemap already draws; 14 shows street
/// names and the map overzooms it past that. Measured on a 53-place library
/// (2026-10-07): zooms 9 to 14 came to 391 MB in per-place boxes, and 15
/// alone would have more than doubled it.
pub const MIN_ZOOM: u8 = 9;
pub const MAX_ZOOM: u8 = 14;

/// Glyphs for street and place names: the Latin ranges of Noto Sans, enough
/// for Turkish, German and the other Latin-script names. A name in another
/// script is drawn without its label.
pub const FONT_STACK: &str = "Noto Sans Regular";
pub const FONT_RANGES: &[&str] = &["0-255", "256-511", "512-767", "7680-7935", "8192-8447"];
const FONT_URL: &str = "https://protomaps.github.io/basemaps-assets/fonts";

const HEADER_LEN: usize = 127;
/// The header and root directory must fit in the archive's first 16 KiB.
const ROOT_LIMIT: usize = 16_384;
/// Tile byte ranges closer than this are fetched in one request.
const MERGE_GAP: u64 = 64 * 1024;
/// And no request is larger than this.
const CHUNK_MAX: u64 = 8 * 1024 * 1024;

/// The archive for one library, beside its state.
pub fn archive_path(state_dir: &Path) -> PathBuf {
    state_dir.join("basemap").join("detail.pmtiles")
}

fn manifest_path(state_dir: &Path) -> PathBuf {
    state_dir.join("basemap").join("detail.json")
}

/// Where the glyph files live: per machine, shared by every library, since
/// they say nothing about anyone's photos.
pub fn fonts_dir(geo: &Path) -> PathBuf {
    geo.join("basemap").join("fonts").join(FONT_STACK)
}

/// A 1-degree cell by its south-west corner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Cell {
    pub lat: i32,
    pub lon: i32,
}

/// What a finished cut covered, written beside the archive.
#[derive(Debug, Serialize, Deserialize)]
struct Manifest {
    cells: Vec<Cell>,
    etag: String,
    bytes: u64,
}

/// The cells covering each place's box: centroid plus or minus its radius.
pub fn cells_for(places: &[(f64, f64, f64)]) -> BTreeSet<Cell> {
    let mut cells = BTreeSet::new();
    for &(lat, lon, radius_km) in places {
        let dlat = radius_km / 111.0;
        let dlon = dlat / lat.to_radians().cos().max(0.01);
        let south = (lat - dlat).max(-85.0).floor() as i32;
        let north = (lat + dlat).min(84.999).floor() as i32;
        let west = (lon - dlon).floor() as i32;
        let east = (lon + dlon).floor() as i32;
        for la in south..=north {
            for lo in west..=east {
                cells.insert(Cell {
                    lat: la,
                    lon: (lo + 180).rem_euclid(360) - 180,
                });
            }
        }
    }
    cells
}

/// The library's places: its location clusters, empty before any.
pub fn places(conn: &rusqlite::Connection) -> Result<Vec<(f64, f64, f64)>> {
    let has: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'location_clusters')",
        [],
        |r| r.get(0),
    )?;
    if !has {
        return Ok(Vec::new());
    }
    let mut stmt =
        conn.prepare("SELECT centroid_lat, centroid_lon, radius_km FROM location_clusters")?;
    let rows = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// The PMTiles tile id: the tiles of every lower zoom, then the tile's place
/// on this zoom's Hilbert curve. Ported from the reference implementation.
pub fn tile_id(z: u8, x: u32, y: u32) -> u64 {
    let n: u64 = 1 << z;
    let mut acc: u64 = (n * n - 1) / 3;
    let (mut tx, mut ty) = (x as i64, y as i64);
    let mut s: i64 = (n / 2) as i64;
    while s > 0 {
        let rx = i64::from(tx & s != 0);
        let ry = i64::from(ty & s != 0);
        acc += (s * s) as u64 * ((3 * rx) ^ ry) as u64;
        if ry == 0 {
            if rx == 1 {
                tx = s - 1 - tx;
                ty = s - 1 - ty;
            }
            std::mem::swap(&mut tx, &mut ty);
        }
        s /= 2;
    }
    acc
}

fn lon_to_x(lon: f64, n: f64) -> f64 {
    (lon + 180.0) / 360.0 * n
}

fn lat_to_y(lat: f64, n: f64) -> f64 {
    let r = lat.to_radians();
    (1.0 - (r.tan() + 1.0 / r.cos()).ln() / std::f64::consts::PI) / 2.0 * n
}

/// Every tile id the cells need, zooms [`MIN_ZOOM`] to [`MAX_ZOOM`], sorted.
pub fn wanted_ids(cells: &BTreeSet<Cell>) -> Vec<u64> {
    let mut ids = BTreeSet::new();
    for z in MIN_ZOOM..=MAX_ZOOM {
        let n = f64::from(1u32 << z);
        let last = (1u32 << z) - 1;
        for cell in cells {
            let clamp = |v: f64| (v.floor().max(0.0) as u32).min(last);
            let x0 = clamp(lon_to_x(f64::from(cell.lon), n));
            let x1 = clamp(lon_to_x(f64::from(cell.lon + 1), n) - 1e-9);
            let y0 = clamp(lat_to_y(f64::from(cell.lat + 1), n));
            let y1 = clamp(lat_to_y(f64::from(cell.lat), n) - 1e-9);
            for x in x0..=x1 {
                for y in y0..=y1 {
                    ids.insert(tile_id(z, x, y));
                }
            }
        }
    }
    ids.into_iter().collect()
}

/// One directory entry. `run_length` 0 points at a leaf directory.
#[derive(Debug, Clone, PartialEq)]
struct Entry {
    tile_id: u64,
    offset: u64,
    length: u32,
    run_length: u32,
}

fn read_varint(buf: &[u8], pos: &mut usize) -> Result<u64> {
    let mut value = 0u64;
    let mut shift = 0;
    loop {
        let byte = *buf.get(*pos).context("truncated varint")?;
        *pos += 1;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
        shift += 7;
        if shift > 63 {
            bail!("varint too long");
        }
    }
}

fn write_varint(out: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        out.push((value as u8 & 0x7f) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

fn decode_dir(buf: &[u8]) -> Result<Vec<Entry>> {
    let mut pos = 0;
    let count = read_varint(buf, &mut pos)? as usize;
    let mut entries = Vec::with_capacity(count.min(1 << 20));
    let mut last = 0u64;
    for _ in 0..count {
        last += read_varint(buf, &mut pos)?;
        entries.push(Entry {
            tile_id: last,
            offset: 0,
            length: 0,
            run_length: 0,
        });
    }
    for e in entries.iter_mut() {
        e.run_length = read_varint(buf, &mut pos)? as u32;
    }
    for e in entries.iter_mut() {
        e.length = read_varint(buf, &mut pos)? as u32;
    }
    for i in 0..count {
        let v = read_varint(buf, &mut pos)?;
        entries[i].offset = if v == 0 && i > 0 {
            entries[i - 1].offset + u64::from(entries[i - 1].length)
        } else {
            v.checked_sub(1).context("bad directory offset")?
        };
    }
    Ok(entries)
}

fn encode_dir(entries: &[Entry]) -> Vec<u8> {
    let mut out = Vec::new();
    write_varint(&mut out, entries.len() as u64);
    let mut last = 0u64;
    for e in entries {
        write_varint(&mut out, e.tile_id - last);
        last = e.tile_id;
    }
    for e in entries {
        write_varint(&mut out, u64::from(e.run_length));
    }
    for e in entries {
        write_varint(&mut out, u64::from(e.length));
    }
    for (i, e) in entries.iter().enumerate() {
        let contiguous =
            i > 0 && e.offset == entries[i - 1].offset + u64::from(entries[i - 1].length);
        write_varint(&mut out, if contiguous { 0 } else { e.offset + 1 });
    }
    out
}

/// PMTiles internal compression: 1 none, 2 gzip. The planet build uses gzip;
/// anything else is refused rather than misread.
fn decompress(compression: u8, bytes: &[u8]) -> Result<Vec<u8>> {
    match compression {
        1 => Ok(bytes.to_vec()),
        2 => {
            let mut out = Vec::new();
            flate2::read::GzDecoder::new(bytes).read_to_end(&mut out)?;
            Ok(out)
        }
        other => bail!("unsupported PMTiles compression {other}"),
    }
}

fn compress(compression: u8, bytes: &[u8]) -> Result<Vec<u8>> {
    match compression {
        1 => Ok(bytes.to_vec()),
        2 => {
            let mut encoder =
                flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            encoder.write_all(bytes)?;
            Ok(encoder.finish()?)
        }
        other => bail!("unsupported PMTiles compression {other}"),
    }
}

/// The fixed 127-byte PMTiles v3 header.
#[derive(Debug, Clone, Default, PartialEq)]
struct Header {
    root_offset: u64,
    root_length: u64,
    metadata_offset: u64,
    metadata_length: u64,
    leaf_offset: u64,
    leaf_length: u64,
    data_offset: u64,
    data_length: u64,
    addressed_tiles: u64,
    tile_entries: u64,
    tile_contents: u64,
    clustered: bool,
    internal_compression: u8,
    tile_compression: u8,
    tile_type: u8,
    min_zoom: u8,
    max_zoom: u8,
    min_lon_e7: i32,
    min_lat_e7: i32,
    max_lon_e7: i32,
    max_lat_e7: i32,
    center_zoom: u8,
    center_lon_e7: i32,
    center_lat_e7: i32,
}

impl Header {
    fn parse(b: &[u8]) -> Result<Header> {
        if b.len() < HEADER_LEN || &b[0..7] != b"PMTiles" {
            bail!("not a PMTiles archive");
        }
        if b[7] != 3 {
            bail!("PMTiles spec version {} is not supported", b[7]);
        }
        let u = |o: usize| u64::from_le_bytes(b[o..o + 8].try_into().unwrap());
        let i = |o: usize| i32::from_le_bytes(b[o..o + 4].try_into().unwrap());
        Ok(Header {
            root_offset: u(8),
            root_length: u(16),
            metadata_offset: u(24),
            metadata_length: u(32),
            leaf_offset: u(40),
            leaf_length: u(48),
            data_offset: u(56),
            data_length: u(64),
            addressed_tiles: u(72),
            tile_entries: u(80),
            tile_contents: u(88),
            clustered: b[96] == 1,
            internal_compression: b[97],
            tile_compression: b[98],
            tile_type: b[99],
            min_zoom: b[100],
            max_zoom: b[101],
            min_lon_e7: i(102),
            min_lat_e7: i(106),
            max_lon_e7: i(110),
            max_lat_e7: i(114),
            center_zoom: b[118],
            center_lon_e7: i(119),
            center_lat_e7: i(123),
        })
    }

    fn to_bytes(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(HEADER_LEN);
        b.extend_from_slice(b"PMTiles");
        b.push(3);
        for v in [
            self.root_offset,
            self.root_length,
            self.metadata_offset,
            self.metadata_length,
            self.leaf_offset,
            self.leaf_length,
            self.data_offset,
            self.data_length,
            self.addressed_tiles,
            self.tile_entries,
            self.tile_contents,
        ] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        b.extend_from_slice(&[
            u8::from(self.clustered),
            self.internal_compression,
            self.tile_compression,
            self.tile_type,
            self.min_zoom,
            self.max_zoom,
        ]);
        for v in [
            self.min_lon_e7,
            self.min_lat_e7,
            self.max_lon_e7,
            self.max_lat_e7,
        ] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        b.push(self.center_zoom);
        b.extend_from_slice(&self.center_lon_e7.to_le_bytes());
        b.extend_from_slice(&self.center_lat_e7.to_le_bytes());
        b
    }
}

/// Byte ranges of the source archive. The HTTP one pins every read to the
/// ETag of its first, so a source republished mid-cut fails the cut.
pub trait RangeSource: Sync {
    fn read(&self, offset: u64, length: u64) -> Result<Vec<u8>>;
    fn etag(&self) -> String;
}

pub struct HttpSource {
    url: String,
    agent: ureq::Agent,
    etag: std::sync::Mutex<Option<String>>,
}

impl HttpSource {
    pub fn new(url: &str) -> HttpSource {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(std::time::Duration::from_secs(300)))
            .http_status_as_error(false)
            .build()
            .new_agent();
        HttpSource {
            url: url.to_string(),
            agent,
            etag: std::sync::Mutex::new(None),
        }
    }
}

impl RangeSource for HttpSource {
    fn read(&self, offset: u64, length: u64) -> Result<Vec<u8>> {
        if length == 0 {
            return Ok(Vec::new());
        }
        let pinned = self.etag.lock().unwrap().clone();
        let mut request = self
            .agent
            .get(&self.url)
            .header("Range", &format!("bytes={offset}-{}", offset + length - 1));
        if let Some(tag) = &pinned {
            request = request.header("If-Match", tag);
        }
        let response = request
            .call()
            .with_context(|| format!("reading the street map from {}", self.url))?;
        match response.status().as_u16() {
            206 => {}
            412 => bail!(
                "the street map at {} changed during the download; try again",
                self.url
            ),
            status => bail!("the street map host answered {status} for a byte range"),
        }
        if pinned.is_none() {
            let tag = response
                .headers()
                .get("etag")
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_string();
            *self.etag.lock().unwrap() = Some(tag);
        }
        let mut body = Vec::with_capacity(length as usize);
        response
            .into_body()
            .into_reader()
            .take(length)
            .read_to_end(&mut body)?;
        if body.len() as u64 != length {
            bail!(
                "short read from the street map: {} of {length} bytes",
                body.len()
            );
        }
        Ok(body)
    }

    fn etag(&self) -> String {
        self.etag.lock().unwrap().clone().unwrap_or_default()
    }
}

/// What a cut will copy: each wanted tile's source bytes, plus the source's
/// header and metadata, and the total size of the distinct tile contents.
pub struct Plan {
    pub cells: BTreeSet<Cell>,
    /// (tile id, source offset in the data section, length), by tile id.
    tiles: Vec<(u64, u64, u32)>,
    header: Header,
    metadata: Vec<u8>,
    pub bytes: u64,
}

/// Reads only the directories the cells need. This is the step that tells
/// the host which areas are wanted.
pub fn plan(source: &dyn RangeSource, cells: BTreeSet<Cell>) -> Result<Plan> {
    let head = source.read(0, ROOT_LIMIT as u64)?;
    let header = Header::parse(&head)?;
    let root_end = (header.root_offset + header.root_length) as usize;
    let root_bytes = if root_end <= head.len() {
        head[header.root_offset as usize..root_end].to_vec()
    } else {
        source.read(header.root_offset, header.root_length)?
    };
    let root = decode_dir(&decompress(header.internal_compression, &root_bytes)?)?;
    let ids = wanted_ids(&cells);
    let mut tiles = Vec::new();
    resolve(source, &header, &root, &ids, 0, &mut tiles)?;
    tiles.sort_by_key(|t| t.0);
    let metadata = source.read(header.metadata_offset, header.metadata_length)?;
    let mut seen = BTreeMap::new();
    for &(_, offset, length) in &tiles {
        seen.insert(offset, u64::from(length));
    }
    let bytes = seen.values().sum();
    Ok(Plan {
        cells,
        tiles,
        header,
        metadata,
        bytes,
    })
}

fn resolve(
    source: &dyn RangeSource,
    header: &Header,
    entries: &[Entry],
    ids: &[u64],
    depth: u8,
    out: &mut Vec<(u64, u64, u32)>,
) -> Result<()> {
    if depth > 4 {
        bail!("PMTiles directories nest deeper than expected");
    }
    let mut leaves: BTreeMap<usize, Vec<u64>> = BTreeMap::new();
    for &id in ids {
        let i = entries.partition_point(|e| e.tile_id <= id);
        if i == 0 {
            continue;
        }
        let e = &entries[i - 1];
        if e.run_length == 0 {
            leaves.entry(i - 1).or_default().push(id);
        } else if id < e.tile_id + u64::from(e.run_length) {
            out.push((id, e.offset, e.length));
        }
    }
    // A library's cells touch many leaf directories; read them in parallel,
    // as the tiles are, since one at a time is latency-bound.
    let leaves: Vec<(usize, Vec<u64>)> = leaves.into_iter().collect();
    let reads: Vec<(u64, u64)> = leaves
        .iter()
        .map(|(index, _)| {
            let e = &entries[*index];
            (header.leaf_offset + e.offset, u64::from(e.length))
        })
        .collect();
    let fetched = read_all(source, &reads)?;
    for ((_, wanted), bytes) in leaves.iter().zip(fetched) {
        let leaf = decode_dir(&decompress(header.internal_compression, &bytes)?)?;
        resolve(source, header, &leaf, wanted, depth + 1, out)?;
    }
    Ok(())
}

/// Reads every (offset, length), eight at a time, returned in order.
fn read_all(source: &dyn RangeSource, reads: &[(u64, u64)]) -> Result<Vec<Vec<u8>>> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let next = AtomicUsize::new(0);
    let results: Vec<std::sync::Mutex<Option<Result<Vec<u8>>>>> =
        reads.iter().map(|_| std::sync::Mutex::new(None)).collect();
    std::thread::scope(|scope| {
        for _ in 0..8.min(reads.len()) {
            scope.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::SeqCst);
                let Some(&(offset, length)) = reads.get(i) else {
                    break;
                };
                *results[i].lock().unwrap() = Some(source.read(offset, length));
            });
        }
    });
    results
        .into_iter()
        .map(|slot| {
            slot.into_inner()
                .unwrap()
                .unwrap_or_else(|| bail!("unread range"))
        })
        .collect()
}

/// The output directories: everything in the root when it fits, otherwise
/// leaves of a size doubled until the root of leaf pointers fits.
fn build_dirs(entries: &[Entry], compression: u8) -> Result<(Vec<u8>, Vec<u8>)> {
    let root = compress(compression, &encode_dir(entries))?;
    if root.len() <= ROOT_LIMIT - HEADER_LEN {
        return Ok((root, Vec::new()));
    }
    let mut leaf_size = 4096;
    loop {
        let mut leaves = Vec::new();
        let mut pointers = Vec::new();
        for chunk in entries.chunks(leaf_size) {
            let bytes = compress(compression, &encode_dir(chunk))?;
            pointers.push(Entry {
                tile_id: chunk[0].tile_id,
                offset: leaves.len() as u64,
                length: bytes.len() as u32,
                run_length: 0,
            });
            leaves.extend_from_slice(&bytes);
        }
        let root = compress(compression, &encode_dir(&pointers))?;
        if root.len() <= ROOT_LIMIT - HEADER_LEN {
            return Ok((root, leaves));
        }
        leaf_size *= 2;
    }
}

/// Writes the planned archive to `out`: header, root, metadata, leaves, then
/// the distinct tile contents in source order, fetched in merged ranges.
/// `progress` gets (bytes done, bytes total) of tile data.
pub fn write(
    source: &dyn RangeSource,
    plan: &Plan,
    out: &Path,
    mut progress: impl FnMut(u64, u64),
) -> Result<()> {
    // Distinct contents in source order, each with its output offset.
    let mut contents: BTreeMap<u64, u32> = BTreeMap::new();
    for &(_, offset, length) in &plan.tiles {
        contents.insert(offset, length);
    }
    let mut out_offset: HashMap<u64, u64> = HashMap::with_capacity(contents.len());
    let mut next = 0u64;
    for (&offset, &length) in &contents {
        out_offset.insert(offset, next);
        next += u64::from(length);
    }
    let data_length = next;

    let mut entries: Vec<Entry> = Vec::new();
    for &(id, offset, length) in &plan.tiles {
        let o = out_offset[&offset];
        if let Some(last) = entries.last_mut() {
            if last.offset == o && last.tile_id + u64::from(last.run_length) == id {
                last.run_length += 1;
                continue;
            }
        }
        entries.push(Entry {
            tile_id: id,
            offset: o,
            length,
            run_length: 1,
        });
    }
    let compression = plan.header.internal_compression;
    let (root, leaves) = build_dirs(&entries, compression)?;

    let (mut south, mut west, mut north, mut east) = (90.0f64, 180.0f64, -90.0f64, -180.0f64);
    for c in &plan.cells {
        south = south.min(f64::from(c.lat));
        north = north.max(f64::from(c.lat + 1));
        west = west.min(f64::from(c.lon));
        east = east.max(f64::from(c.lon + 1));
    }
    let e7 = |v: f64| (v * 1e7).round() as i32;
    let mut header = Header {
        root_offset: HEADER_LEN as u64,
        root_length: root.len() as u64,
        clustered: false,
        internal_compression: compression,
        tile_compression: plan.header.tile_compression,
        tile_type: plan.header.tile_type,
        min_zoom: MIN_ZOOM,
        max_zoom: MAX_ZOOM,
        min_lon_e7: e7(west),
        min_lat_e7: e7(south),
        max_lon_e7: e7(east),
        max_lat_e7: e7(north),
        center_zoom: MIN_ZOOM + 3,
        center_lon_e7: e7((west + east) / 2.0),
        center_lat_e7: e7((south + north) / 2.0),
        addressed_tiles: plan.tiles.len() as u64,
        tile_entries: entries.len() as u64,
        tile_contents: contents.len() as u64,
        ..Header::default()
    };
    header.metadata_offset = header.root_offset + header.root_length;
    header.metadata_length = plan.metadata.len() as u64;
    header.leaf_offset = header.metadata_offset + header.metadata_length;
    header.leaf_length = leaves.len() as u64;
    header.data_offset = header.leaf_offset + header.leaf_length;
    header.data_length = data_length;

    let mut file = std::io::BufWriter::new(
        std::fs::File::create(out).with_context(|| format!("creating {}", out.display()))?,
    );
    file.write_all(&header.to_bytes())?;
    file.write_all(&root)?;
    file.write_all(&plan.metadata)?;
    file.write_all(&leaves)?;

    // Merged ranges over the distinct contents, in source order.
    let mut ranges: Vec<(u64, u64, Vec<(u64, u32)>)> = Vec::new();
    for (&offset, &length) in &contents {
        let end = offset + u64::from(length);
        match ranges.last_mut() {
            Some((start, stop, members))
                if offset <= *stop + MERGE_GAP && end - *start <= CHUNK_MAX =>
            {
                *stop = (*stop).max(end);
                members.push((offset, length));
            }
            _ => ranges.push((offset, end, vec![(offset, length)])),
        }
    }
    // Fetched FETCHERS at a time and written in order. One request at a time
    // is latency-bound: measured 2026-10-07, a 55 MB cell ran at about
    // 250 KB/s sequentially, where the reference tool's parallel reads took
    // 8 s. Workers stay within WINDOW ranges of the writer, so a slow early
    // range never makes the rest pile up in memory.
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    const FETCHERS: usize = 8;
    const WINDOW: usize = 4 * FETCHERS;
    let next = AtomicUsize::new(0);
    let written = AtomicUsize::new(0);
    let abort = AtomicBool::new(false);
    let (tx, rx) = std::sync::mpsc::channel::<(usize, Result<Vec<u8>>)>();
    let data_offset = plan.header.data_offset;
    progress(0, data_length);
    std::thread::scope(|scope| -> Result<()> {
        for _ in 0..FETCHERS {
            let tx = tx.clone();
            let (next, written, abort, ranges) = (&next, &written, &abort, &ranges);
            scope.spawn(move || loop {
                let i = next.fetch_add(1, Ordering::SeqCst);
                if i >= ranges.len() {
                    break;
                }
                while i >= written.load(Ordering::SeqCst) + WINDOW {
                    if abort.load(Ordering::SeqCst) {
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                if abort.load(Ordering::SeqCst) {
                    return;
                }
                let (start, stop, _) = &ranges[i];
                let read = source.read(data_offset + start, stop - start);
                if tx.send((i, read)).is_err() {
                    return;
                }
            });
        }
        drop(tx);
        let mut pending: BTreeMap<usize, Vec<u8>> = BTreeMap::new();
        let mut want = 0usize;
        let mut done = 0u64;
        for (i, read) in rx {
            let bytes = match read {
                Ok(bytes) => bytes,
                Err(e) => {
                    abort.store(true, Ordering::SeqCst);
                    return Err(e);
                }
            };
            pending.insert(i, bytes);
            while let Some(bytes) = pending.remove(&want) {
                let (start, _, members) = &ranges[want];
                for &(offset, length) in members {
                    let at = (offset - start) as usize;
                    if let Err(e) = file.write_all(&bytes[at..at + length as usize]) {
                        abort.store(true, Ordering::SeqCst);
                        return Err(e.into());
                    }
                    done += u64::from(length);
                }
                want += 1;
                written.store(want, Ordering::SeqCst);
                progress(done, data_length);
            }
        }
        if want != ranges.len() {
            bail!(
                "the street map download stopped after {want} of {} parts",
                ranges.len()
            );
        }
        Ok(())
    })?;
    let file = file.into_inner().map_err(|e| e.into_error())?;
    file.sync_all()?;
    Ok(())
}

/// Where a library's detail stands.
#[derive(Debug, PartialEq)]
pub enum DetailStatus {
    Absent,
    /// `stale`: the library has places outside the cells the archive covers.
    Ready {
        bytes: u64,
        stale: bool,
    },
}

pub fn status(state_dir: &Path, wanted: &BTreeSet<Cell>) -> DetailStatus {
    let path = archive_path(state_dir);
    let Ok(meta) = std::fs::metadata(&path) else {
        return DetailStatus::Absent;
    };
    let covered: BTreeSet<Cell> = std::fs::read(manifest_path(state_dir))
        .ok()
        .and_then(|b| serde_json::from_slice::<Manifest>(&b).ok())
        .map(|m| m.cells.into_iter().collect())
        .unwrap_or_default();
    DetailStatus::Ready {
        bytes: meta.len(),
        stale: !wanted.is_subset(&covered),
    }
}

/// Cuts the archive for `cells` from `source` and publishes it atomically
/// with its manifest. One cut per library at a time, by a file lock.
pub fn ensure(
    state_dir: &Path,
    source: &dyn RangeSource,
    cells: BTreeSet<Cell>,
    progress: impl FnMut(u64, u64),
) -> Result<PathBuf> {
    use fs2::FileExt;
    let path = archive_path(state_dir);
    let dir = path.parent().context("detail directory")?;
    std::fs::create_dir_all(dir)?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(dir.join("detail.lock"))?;
    if FileExt::try_lock_exclusive(&lock).is_err() {
        bail!("the street map for this library is already downloading");
    }
    let _held = crate::held_lock::HeldLock::new(lock);
    let plan = plan(source, cells)?;
    let part = path.with_extension("pmtiles.part");
    write(source, &plan, &part, progress)?;
    let bytes = std::fs::metadata(&part)?.len();
    std::fs::rename(&part, &path).with_context(|| format!("publishing {}", path.display()))?;
    let manifest = Manifest {
        cells: plan.cells.iter().copied().collect(),
        etag: source.etag(),
        bytes,
    };
    std::fs::write(manifest_path(state_dir), serde_json::to_vec(&manifest)?)?;
    Ok(path)
}

/// Deletes the archive and its manifest; the archive's size when there was
/// one. Called when `street_detail` is turned off, so the setting and the
/// disk agree.
pub fn remove(state_dir: &Path) -> Result<Option<u64>> {
    let path = archive_path(state_dir);
    let existed = std::fs::metadata(&path).ok().map(|m| m.len());
    for p in [
        path.clone(),
        path.with_extension("pmtiles.part"),
        manifest_path(state_dir),
    ] {
        match std::fs::remove_file(&p) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("removing {}", p.display())),
        }
    }
    Ok(existed)
}

/// Downloads the glyph ranges once per machine; the files are fonts, not
/// anything about a library.
pub fn ensure_fonts(geo: &Path) -> Result<()> {
    let dir = fonts_dir(geo);
    std::fs::create_dir_all(&dir)?;
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(60)))
        .build()
        .new_agent();
    for range in FONT_RANGES {
        let path = dir.join(format!("{range}.pbf"));
        if path.exists() {
            continue;
        }
        let url = format!("{FONT_URL}/Noto%20Sans%20Regular/{range}.pbf");
        let mut body = Vec::new();
        agent
            .get(&url)
            .call()
            .with_context(|| format!("downloading {url}"))?
            .into_body()
            .into_reader()
            .read_to_end(&mut body)?;
        let part = path.with_extension("pbf.part");
        std::fs::write(&part, &body)?;
        std::fs::rename(&part, &path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tile_ids_follow_the_reference_numbering() {
        assert_eq!(tile_id(0, 0, 0), 0);
        assert_eq!(tile_id(1, 0, 0), 1);
        assert_eq!(tile_id(1, 0, 1), 2);
        assert_eq!(tile_id(1, 1, 1), 3);
        assert_eq!(tile_id(1, 1, 0), 4);
        assert_eq!(tile_id(2, 0, 0), 5);
        // Every id on a zoom is used exactly once, after the lower zooms'.
        for z in 2..=5u8 {
            let n = 1u64 << z;
            let below = (n * n - 1) / 3;
            let mut ids = BTreeSet::new();
            for x in 0..n as u32 {
                for y in 0..n as u32 {
                    ids.insert(tile_id(z, x, y));
                }
            }
            assert_eq!(ids.len() as u64, n * n);
            assert_eq!(*ids.first().unwrap(), below);
            assert_eq!(*ids.last().unwrap(), below + n * n - 1);
        }
    }

    #[test]
    fn a_place_maps_to_the_whole_degree_cells_its_box_touches() {
        // Berlin, 15 km: the whole box sits inside one cell.
        let cells = cells_for(&[(52.52, 13.405, 15.0)]);
        assert_eq!(
            cells.into_iter().collect::<Vec<_>>(),
            [Cell { lat: 52, lon: 13 }]
        );
        // Kadıköy, 15 km: its box crosses both 29 E and 41 N.
        let cells = cells_for(&[(40.99, 29.03, 15.0)]);
        assert_eq!(
            cells.into_iter().collect::<Vec<_>>(),
            [
                Cell { lat: 40, lon: 28 },
                Cell { lat: 40, lon: 29 },
                Cell { lat: 41, lon: 28 },
                Cell { lat: 41, lon: 29 }
            ]
        );
        // Two places in one cell ask for it once.
        assert_eq!(
            cells_for(&[(52.52, 13.405, 1.0), (52.40, 13.60, 1.0)]).len(),
            1
        );
    }

    #[test]
    fn directories_round_trip_through_their_encoding() {
        let entries = vec![
            Entry {
                tile_id: 5,
                offset: 0,
                length: 10,
                run_length: 1,
            },
            Entry {
                tile_id: 6,
                offset: 10,
                length: 7,
                run_length: 3,
            },
            Entry {
                tile_id: 20,
                offset: 4,
                length: 3,
                run_length: 0,
            },
        ];
        assert_eq!(decode_dir(&encode_dir(&entries)).unwrap(), entries);
        let header = Header {
            root_offset: 127,
            root_length: 40,
            internal_compression: 2,
            tile_compression: 2,
            tile_type: 1,
            min_zoom: 9,
            max_zoom: 14,
            min_lat_e7: -1,
            center_lon_e7: 134_050_000,
            ..Header::default()
        };
        let bytes = header.to_bytes();
        assert_eq!(bytes.len(), HEADER_LEN);
        assert_eq!(Header::parse(&bytes).unwrap(), header);
    }

    /// An in-memory archive standing in for the planet build.
    struct MemorySource(Vec<u8>, std::sync::atomic::AtomicUsize);

    impl RangeSource for MemorySource {
        fn read(&self, offset: u64, length: u64) -> Result<Vec<u8>> {
            self.1.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let start = offset as usize;
            let end = (start + length as usize).min(self.0.len());
            Ok(self.0[start..end].to_vec())
        }
        fn etag(&self) -> String {
            "\"bellek\"".to_string()
        }
    }

    /// Builds a source archive holding the given tiles, through the same
    /// writer the cut uses, with directories forced into leaves when large.
    fn source_archive(tiles: &[(u64, Vec<u8>)]) -> Vec<u8> {
        let mut data = Vec::new();
        let mut entries = Vec::new();
        for (id, body) in tiles {
            entries.push(Entry {
                tile_id: *id,
                offset: data.len() as u64,
                length: body.len() as u32,
                run_length: 1,
            });
            data.extend_from_slice(body);
        }
        let (root, leaves) = build_dirs(&entries, 2).unwrap();
        let metadata = compress(2, br#"{"name":"deneme"}"#).unwrap();
        let mut header = Header {
            root_offset: 127,
            root_length: root.len() as u64,
            internal_compression: 2,
            tile_compression: 2,
            tile_type: 1,
            max_zoom: 15,
            ..Header::default()
        };
        header.metadata_offset = 127 + root.len() as u64;
        header.metadata_length = metadata.len() as u64;
        header.leaf_offset = header.metadata_offset + header.metadata_length;
        header.leaf_length = leaves.len() as u64;
        header.data_offset = header.leaf_offset + header.leaf_length;
        header.data_length = data.len() as u64;
        let mut out = header.to_bytes();
        out.extend(root);
        out.extend(metadata);
        out.extend(leaves);
        out.extend(data);
        out
    }

    #[test]
    fn a_cut_copies_exactly_the_cells_tiles_and_reads_back() {
        // Every zoom 9-14 tile of one Berlin cell, plus tiles elsewhere that
        // must stay out. Each tile's body names its id.
        let cell: BTreeSet<Cell> = [Cell { lat: 52, lon: 13 }].into();
        let wanted = wanted_ids(&cell);
        let elsewhere = wanted_ids(&[Cell { lat: 40, lon: 29 }].into());
        let mut all: Vec<u64> = wanted.iter().chain(elsewhere.iter()).copied().collect();
        all.sort();
        let tiles: Vec<(u64, Vec<u8>)> = all
            .iter()
            .map(|id| (*id, format!("karo-{id}").into_bytes()))
            .collect();
        let archive = source_archive(&tiles);
        let source = MemorySource(archive, Default::default());

        let plan = plan(&source, cell).unwrap();
        assert_eq!(plan.tiles.len(), wanted.len());
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("detail.pmtiles");
        let mut last = (0, 0);
        write(&source, &plan, &out, |d, t| last = (d, t)).unwrap();
        assert_eq!(last.0, last.1);

        // Read the cut back through the same reader.
        let cut = MemorySource(std::fs::read(&out).unwrap(), Default::default());
        let header = Header::parse(&cut.0).unwrap();
        assert_eq!((header.min_zoom, header.max_zoom), (MIN_ZOOM, MAX_ZOOM));
        let root = decode_dir(
            &decompress(
                2,
                &cut.0[header.root_offset as usize
                    ..(header.root_offset + header.root_length) as usize],
            )
            .unwrap(),
        )
        .unwrap();
        let mut found = Vec::new();
        resolve(&cut, &header, &root, &all, 0, &mut found).unwrap();
        assert_eq!(found.len(), wanted.len(), "only the cell's tiles");
        for (id, offset, length) in found {
            let start = (header.data_offset + offset) as usize;
            assert_eq!(
                &cut.0[start..start + length as usize],
                format!("karo-{id}").as_bytes()
            );
        }
        let metadata = &cut.0[header.metadata_offset as usize
            ..(header.metadata_offset + header.metadata_length) as usize];
        assert_eq!(decompress(2, metadata).unwrap(), br#"{"name":"deneme"}"#);
    }

    #[test]
    fn repeated_contents_are_stored_once_and_runs_stay_runs() {
        let cell: BTreeSet<Cell> = [Cell { lat: 52, lon: 13 }].into();
        let wanted = wanted_ids(&cell);
        // Every tile is the same sea: one content, as the planet stores it.
        let tiles: Vec<(u64, Vec<u8>)> = wanted.iter().map(|id| (*id, b"deniz".to_vec())).collect();
        let mut archive_tiles = Vec::new();
        for (id, body) in &tiles {
            archive_tiles.push((*id, body.clone()));
        }
        let source = MemorySource(source_archive(&archive_tiles), Default::default());
        let plan = plan(&source, cell).unwrap();
        assert_eq!(
            plan.bytes,
            5 * wanted.len() as u64,
            "source stores each copy"
        );
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("d.pmtiles");
        write(&source, &plan, &out, |_, _| {}).unwrap();
        let header = Header::parse(&std::fs::read(&out).unwrap()).unwrap();
        assert_eq!(header.addressed_tiles, wanted.len() as u64);
    }

    /// Serves byte ranges, later ones faster, so parallel reads finish out of
    /// order.
    struct ShuffledSource(Vec<u8>);

    impl RangeSource for ShuffledSource {
        fn read(&self, offset: u64, length: u64) -> Result<Vec<u8>> {
            let late = 40u64.saturating_sub(offset / (1 << 20));
            std::thread::sleep(std::time::Duration::from_millis(late));
            Ok(self.0[offset as usize..(offset + length) as usize].to_vec())
        }
        fn etag(&self) -> String {
            String::new()
        }
    }

    #[test]
    fn parallel_reads_finishing_out_of_order_still_write_every_tile_in_place() {
        // Forty 1 MiB tiles: five 8 MiB requests, the first answered last.
        let mib = 1usize << 20;
        let mut data = Vec::with_capacity(40 * mib);
        let mut tiles = Vec::new();
        for i in 0..40u64 {
            tiles.push((100 + i, data.len() as u64, mib as u32));
            data.extend(std::iter::repeat_n(i as u8, mib));
        }
        let plan = Plan {
            cells: [Cell { lat: 52, lon: 13 }].into(),
            tiles,
            header: Header {
                internal_compression: 2,
                tile_compression: 2,
                tile_type: 1,
                ..Header::default()
            },
            metadata: compress(2, b"{}").unwrap(),
            bytes: 40 * mib as u64,
        };
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("d.pmtiles");
        write(&ShuffledSource(data), &plan, &out, |_, _| {}).unwrap();
        let cut = std::fs::read(&out).unwrap();
        let header = Header::parse(&cut).unwrap();
        for i in 0..40usize {
            let at = header.data_offset as usize + i * mib;
            assert!(cut[at..at + mib].iter().all(|b| *b == i as u8), "tile {i}");
        }
    }

    /// Against the real planet build: run by hand with `--ignored` to check
    /// the reader against `pmtiles extract --bbox=13,52,14,53 --minzoom=9
    /// --maxzoom=14 --dry-run`. Reaches the network, so never in CI.
    #[test]
    #[ignore]
    fn live_plan_for_one_cell() {
        let source = HttpSource::new(SOURCE_URL);
        let plan = plan(&source, [Cell { lat: 52, lon: 13 }].into()).unwrap();
        eprintln!(
            "tiles {} contents bytes {} etag {}",
            plan.tiles.len(),
            plan.bytes,
            source.etag()
        );
        assert!(!plan.tiles.is_empty());
    }

    #[test]
    fn status_reports_absent_ready_and_stale_and_remove_clears_it() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path();
        let berlin: BTreeSet<Cell> = [Cell { lat: 52, lon: 13 }].into();
        assert_eq!(status(state, &berlin), DetailStatus::Absent);

        let tiles: Vec<(u64, Vec<u8>)> = wanted_ids(&berlin)
            .into_iter()
            .map(|id| (id, vec![1, 2, 3]))
            .collect();
        let source = MemorySource(source_archive(&tiles), Default::default());
        ensure(state, &source, berlin.clone(), |_, _| {}).unwrap();
        let DetailStatus::Ready { bytes, stale } = status(state, &berlin) else {
            panic!("not ready");
        };
        assert!(bytes > 0);
        assert!(!stale);

        let more: BTreeSet<Cell> = [Cell { lat: 52, lon: 13 }, Cell { lat: 41, lon: 29 }].into();
        assert!(matches!(
            status(state, &more),
            DetailStatus::Ready { stale: true, .. }
        ));

        assert_eq!(remove(state).unwrap(), Some(bytes));
        assert_eq!(status(state, &berlin), DetailStatus::Absent);
        assert_eq!(remove(state).unwrap(), None);
    }
}
