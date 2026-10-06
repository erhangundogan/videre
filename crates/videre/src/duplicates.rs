//! Duplicates by kind: what dedupe, the gallery's Duplicates page, the static
//! review page and MCP all mean by "a duplicate". Each kind is a fixed,
//! tested rule with its own keeper; a new criterion becomes a new kind, never
//! a threshold the user types.
//!
//! - `exact`: identical content key. The oldest copy is kept.
//! - `resized`: the same picture at another pixel size, a messenger or export
//!   copy. Fingerprints within [`RESIZED_MAX_BITS`], the upright aspect within
//!   [`ASPECT_TOLERANCE`], and the 64x64 greyscale signatures within
//!   [`RESIZED_MAX_MAD`]. The largest is kept. Same-size near pictures never
//!   group: at one size a burst frame and a recompressed copy overlap.
//! - `creation`: a Google Photos creation (`-edited`, `-EFFECTS`, `-SMILE`)
//!   beside its original, by name (`crate::takeout_names`). The original is
//!   kept.
//! - `similar`: fingerprints within [`SIMILAR_MAX_BITS`]. Review only: it also
//!   takes different shots of one scene, so nothing is chosen to remove.

use crate::types::FileRecord;
use std::collections::{HashMap, HashSet};
use videre_core::image_decode::{signature_mad, PixelSignature};

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, clap::ValueEnum)]
pub enum Kind {
    Exact,
    Resized,
    Creation,
    Similar,
}

impl Kind {
    /// Whether dedupe may remove this kind's copies. `similar` is review only.
    pub fn removable(self) -> bool {
        self != Kind::Similar
    }

    /// The name on the command line, in JSON and on the review pages.
    pub fn name(self) -> &'static str {
        match self {
            Kind::Exact => "exact",
            Kind::Resized => "resized",
            Kind::Creation => "creation",
            Kind::Similar => "similar",
        }
    }

    pub fn parse(name: &str) -> Option<Kind> {
        [Kind::Exact, Kind::Resized, Kind::Creation, Kind::Similar]
            .into_iter()
            .find(|k| k.name() == name)
    }
}

/// One group of one kind. `files[0]` is the keeper; the rest are its copies.
/// A `similar` group's first file is only the oldest: nothing is removed.
#[derive(Debug, Clone)]
pub struct Group {
    pub kind: Kind,
    pub files: Vec<FileRecord>,
}

impl Group {
    pub fn keeper(&self) -> &FileRecord {
        &self.files[0]
    }

    pub fn copies(&self) -> &[FileRecord] {
        &self.files[1..]
    }
}

/// Fingerprint bits within which two files may be resized copies. Measured
/// 2026-10-06 on a 14,000-image Takeout library: every copy the owner
/// confirmed was within 4 bits.
pub const RESIZED_MAX_BITS: u32 = 4;

/// Largest 64x64 greyscale mean absolute difference for a resized copy. On
/// the same library the owner's 41 confirmed copies at different sizes were
/// all at 0.56 or less and every other different-size pair at 0.59 or more;
/// 0.5 leaves the closest non-copies a margin and costs the few copies
/// between 0.5 and 0.56.
pub const RESIZED_MAX_MAD: f32 = 0.5;

/// How far apart two upright aspect ratios may be, as a fraction.
pub const ASPECT_TOLERANCE: f32 = 0.01;

/// Fingerprint bits for the review-only `similar` kind.
pub const SIMILAR_MAX_BITS: u32 = 10;

/// What a set of kinds found, and how many resized candidates could not be
/// checked because their signatures are not stored yet (a page never decodes).
pub struct Found {
    pub groups: Vec<Group>,
    pub unchecked: usize,
    /// How many files have a fingerprint (`videre embed` takes them); the
    /// resized and similar kinds see only those.
    pub fingerprinted: usize,
    /// The kinds asked for, in kind order, once each.
    pub wanted: Vec<Kind>,
}

/// Every group of `kinds` among `records`, given the stored `signatures`.
/// Pure: deciding needs no file and no database. Groups come in kind order,
/// then by keeper path (exact groups by hash, as they always were).
pub fn find_in(
    records: &[FileRecord],
    kinds: &[Kind],
    signatures: &HashMap<String, PixelSignature>,
) -> Found {
    let creation = creation_groups(records);
    // A creation is the creation kind's business, whichever kinds were asked
    // for: as a resized keeper it could outlive its own original.
    let creations = creation_paths(&creation);
    let mut groups = Vec::new();
    let mut unchecked = 0;
    let mut kinds = kinds.to_vec();
    kinds.sort();
    kinds.dedup();
    let removing_creations = kinds.contains(&Kind::Creation);
    for &kind in &kinds {
        match kind {
            Kind::Exact => {
                for g in crate::output::find_duplicate_groups(records) {
                    let mut files = g.files;
                    // With creations going too, keep a copy that is not one of
                    // them; when every copy is, all go and the original they
                    // were made from stays.
                    if removing_creations {
                        if let Some(i) = files
                            .iter()
                            .position(|f| !creations.contains(f.path.as_str()))
                        {
                            let keeper = files.remove(i);
                            files.insert(0, keeper);
                        }
                    }
                    groups.push(Group {
                        kind: Kind::Exact,
                        files,
                    });
                }
            }
            Kind::Resized => {
                let (found, missing) = resized_groups(records, &creations, signatures);
                groups.extend(found);
                unchecked = missing;
            }
            Kind::Creation => groups.extend(creation.clone()),
            Kind::Similar => {
                for g in crate::output::find_similar_groups(records, SIMILAR_MAX_BITS) {
                    groups.push(Group {
                        kind: Kind::Similar,
                        files: g.files,
                    });
                }
            }
        }
    }
    Found {
        groups,
        unchecked,
        fingerprinted: records.iter().filter(|r| r.phash.is_some()).count(),
        wanted: kinds,
    }
}

/// Every group of `kinds` in the library. With `decode`, the resized
/// candidates that have no stored signature are decoded once and stored, so
/// the result is complete; without it (the review pages, which never decode)
/// they are counted in `Found::unchecked`. A file that will not decode is
/// skipped, not fatal.
pub fn find(
    conn: &rusqlite::Connection,
    kinds: &[Kind],
    decode: bool,
    silent: bool,
) -> anyhow::Result<Found> {
    let records = crate::sqlite_output::load_records_from(conn)
        .map_err(|e| anyhow::anyhow!("reading the library database: {e}"))?;
    let mut signatures = HashMap::new();
    if kinds.contains(&Kind::Resized) {
        let candidates = resized_candidates(&records);
        let hashes: Vec<String> = candidates.iter().map(|r| r.hash.clone()).collect();
        signatures = videre_core::pixel_signatures::get_many(conn, &hashes)?;
        let missing: Vec<&FileRecord> = candidates
            .into_iter()
            .filter(|r| !signatures.contains_key(&r.hash))
            .collect();
        if decode && !missing.is_empty() {
            for (hash, sig) in take_signatures(&missing, silent) {
                videre_core::pixel_signatures::put(conn, &hash, &sig)?;
                signatures.insert(hash, sig);
            }
        }
    }
    Ok(find_in(&records, kinds, &signatures))
}

/// Decode each file and take its signature, in parallel. Failures are
/// reported on the progress line and left out.
fn take_signatures(files: &[&FileRecord], silent: bool) -> Vec<(String, PixelSignature)> {
    use rayon::prelude::*;
    if !silent {
        tracing::info!(
            "Checking {} possible resized copies (once; the result is kept).",
            files.len()
        );
    }
    let progress =
        videre_core::progress::Progress::new_counting(files.len() as u64, silent, "files");
    let taken = files
        .par_iter()
        .filter_map(|r| {
            // 256 is only a QuickLook size hint for HEIC; a signature is 64x64.
            let decoded = videre_ml::preprocess::decode(std::path::Path::new(&r.path), 256);
            progress.tick();
            match decoded {
                Ok(img) => Some((
                    r.hash.clone(),
                    videre_core::image_decode::pixel_signature(&img),
                )),
                Err(e) => {
                    progress.skip(&r.path, e);
                    None
                }
            }
        })
        .collect();
    progress.finish();
    taken
}

/// What removing `groups` takes: every removable group's copies, each path
/// once, by the kind that chose it. With `check_originals`, a creation goes
/// only while its original is on disk: the pairing comes from rows, and an
/// original deleted since the last scan would otherwise leave the creation as
/// the only file, then take that too. Shared by dedupe and the gallery.
pub fn removals(groups: &[Group], check_originals: bool) -> Vec<(Kind, std::path::PathBuf)> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for group in groups.iter().filter(|g| g.kind.removable()) {
        if group.kind == Kind::Creation
            && check_originals
            && !std::path::Path::new(&group.keeper().path).exists()
        {
            continue;
        }
        for copy in group.copies() {
            if seen.insert(copy.path.clone()) {
                out.push((group.kind, std::path::PathBuf::from(&copy.path)));
            }
        }
    }
    out
}

/// The kinds a stored list names, in kind order; unknown names are left out,
/// and a list naming none gives `fallback`.
pub fn parse_kinds(names: &[&str], fallback: &[Kind]) -> Vec<Kind> {
    let mut kinds: Vec<Kind> = names.iter().filter_map(|n| Kind::parse(n)).collect();
    kinds.sort();
    kinds.dedup();
    if kinds.is_empty() {
        fallback.to_vec()
    } else {
        kinds
    }
}

/// The groups a query selects: a group qualifies when any member matches, and
/// is then kept whole, so its copies are still judged by its keeper rule.
pub fn filter_groups(groups: Vec<Group>, matches: &dyn Fn(&FileRecord) -> bool) -> Vec<Group> {
    groups
        .into_iter()
        .filter(|g| g.files.iter().any(matches))
        .collect()
}

/// The paths of every paired creation.
fn creation_paths(creation: &[Group]) -> HashSet<String> {
    creation
        .iter()
        .flat_map(|g| g.copies().iter().map(|r| r.path.clone()))
        .collect()
}

/// One group per original with the creations beside it, the original first.
fn creation_groups(records: &[FileRecord]) -> Vec<Group> {
    let rows: Vec<(&str, &str)> = records
        .iter()
        .map(|r| (r.path.as_str(), r.hash.as_str()))
        .collect();
    let by_path: HashMap<&str, &FileRecord> =
        records.iter().map(|r| (r.path.as_str(), r)).collect();
    let mut by_original: std::collections::BTreeMap<&str, Vec<FileRecord>> = Default::default();
    for pair in crate::takeout_names::edited_pairs(&rows) {
        by_original
            .entry(pair.original)
            .or_default()
            .push(by_path[pair.edit].clone());
    }
    by_original
        .into_iter()
        .map(|(original, mut made)| {
            made.sort_by(|a, b| a.path.cmp(&b.path));
            made.insert(0, by_path[original].clone());
            Group {
                kind: Kind::Creation,
                files: made,
            }
        })
        .collect()
}

/// The hashes the resized kind would compare, one record standing for each:
/// those that need a stored signature before `find_in` can decide.
pub fn resized_candidates(records: &[FileRecord]) -> Vec<&FileRecord> {
    let creations = creation_paths(&creation_groups(records));
    let reps = representatives(records, &creations);
    let pairs = candidate_pairs(&reps);
    let mut wanted: Vec<usize> = pairs.iter().flat_map(|&(a, b)| [a, b]).collect();
    wanted.sort_unstable();
    wanted.dedup();
    wanted.into_iter().map(|i| reps[i]).collect()
}

/// One record per hash that resized can compare: an image with a usable
/// fingerprint and known pixel size, not a paired creation. The hash's oldest
/// path stands for it, the copy exact dedupe keeps.
fn representatives<'a>(
    records: &'a [FileRecord],
    creations: &HashSet<String>,
) -> Vec<&'a FileRecord> {
    let mut by_hash: HashMap<&str, &FileRecord> = HashMap::new();
    for r in records {
        if creations.contains(r.path.as_str()) || !comparable(r) {
            continue;
        }
        by_hash
            .entry(r.hash.as_str())
            .and_modify(|kept| {
                if crate::output::best_date(r) < crate::output::best_date(kept) {
                    *kept = r;
                }
            })
            .or_insert(r);
    }
    let mut reps: Vec<&FileRecord> = by_hash.into_values().collect();
    reps.sort_by(|a, b| a.hash.cmp(&b.hash));
    reps
}

fn comparable(r: &FileRecord) -> bool {
    let image = videre_core::mime_probe::is_embeddable(r.mime.as_deref(), &r.ext)
        && !videre_core::embeddings::is_video_ext(&r.ext)
        && !r
            .mime
            .as_deref()
            .is_some_and(videre_core::mime_probe::is_video_mime);
    image && r.phash.is_some_and(|p| !crate::output::degenerate_phash(p))
}

/// Index pairs of `reps` within [`RESIZED_MAX_BITS`]. Their sizes are not
/// compared here: a copy can carry its original's size tags in the row, so
/// only the decoded pixels (`PixelSignature`) say whether two sizes differ.
/// Two fingerprints within 4 bits agree exactly on at least one of five
/// 13-bit slices, so only files sharing a slice are compared.
fn candidate_pairs(reps: &[&FileRecord]) -> Vec<(usize, usize)> {
    const SLICES: u32 = RESIZED_MAX_BITS + 1;
    let width = 64_u32.div_ceil(SLICES);
    let mut seen: HashSet<(usize, usize)> = HashSet::new();
    for s in 0..SLICES {
        let shift = s * width;
        let mask = if shift + width >= 64 {
            u64::MAX >> shift
        } else {
            (1u64 << width) - 1
        };
        let mut buckets: HashMap<u64, Vec<usize>> = HashMap::new();
        for (i, r) in reps.iter().enumerate() {
            buckets
                .entry((r.phash.unwrap() >> shift) & mask)
                .or_default()
                .push(i);
        }
        for bucket in buckets.values() {
            for (x, &a) in bucket.iter().enumerate() {
                for &b in &bucket[x + 1..] {
                    if seen.contains(&(a, b)) {
                        continue;
                    }
                    let near =
                        crate::hasher::hamming(reps[a].phash.unwrap(), reps[b].phash.unwrap())
                            <= RESIZED_MAX_BITS;
                    if near {
                        seen.insert((a, b));
                    }
                }
            }
        }
    }
    let mut pairs: Vec<(usize, usize)> = seen.into_iter().collect();
    pairs.sort_unstable();
    pairs
}

/// One picture at two sizes: the decoded sizes differ, the shapes agree, and
/// the pixels match.
fn confirms(a: &PixelSignature, b: &PixelSignature) -> bool {
    let ratio = a.aspect() / b.aspect();
    a.pixels() != b.pixels()
        && (ratio - 1.0).abs() <= ASPECT_TOLERANCE
        && signature_mad(&a.luma, &b.luma) <= RESIZED_MAX_MAD
}

/// The keeper order: most decoded pixels, then the larger file, the older
/// date, then the path, so the choice never depends on load order.
fn better_keeper(
    (a, sa): (&FileRecord, &PixelSignature),
    (b, sb): (&FileRecord, &PixelSignature),
) -> std::cmp::Ordering {
    sb.pixels()
        .cmp(&sa.pixels())
        .then(b.size_bytes.cmp(&a.size_bytes))
        .then(crate::output::best_date(a).cmp(crate::output::best_date(b)))
        .then(a.path.cmp(&b.path))
}

fn resized_groups(
    records: &[FileRecord],
    creations: &HashSet<String>,
    signatures: &HashMap<String, PixelSignature>,
) -> (Vec<Group>, usize) {
    let reps = representatives(records, creations);
    let pairs = candidate_pairs(&reps);
    let mut missing: HashSet<usize> = HashSet::new();
    let mut parent: Vec<usize> = (0..reps.len()).collect();
    fn root(parent: &mut [usize], mut i: usize) -> usize {
        while parent[i] != i {
            parent[i] = parent[parent[i]];
            i = parent[i];
        }
        i
    }
    for &(a, b) in &pairs {
        let (Some(sa), Some(sb)) = (signatures.get(&reps[a].hash), signatures.get(&reps[b].hash))
        else {
            for i in [a, b] {
                if !signatures.contains_key(&reps[i].hash) {
                    missing.insert(i);
                }
            }
            continue;
        };
        if confirms(sa, sb) {
            let (ra, rb) = (root(&mut parent, a), root(&mut parent, b));
            parent[ra] = rb;
        }
    }
    let mut components: HashMap<usize, Vec<usize>> = HashMap::new();
    for &(a, b) in &pairs {
        for i in [a, b] {
            let r = root(&mut parent, i);
            components.entry(r).or_default().push(i);
        }
    }
    let mut by_hash: HashMap<&str, Vec<&FileRecord>> = HashMap::new();
    for r in records {
        by_hash.entry(r.hash.as_str()).or_default().push(r);
    }
    let mut groups = Vec::new();
    for mut members in components.into_values() {
        members.sort_unstable();
        members.dedup();
        if members.len() < 2 {
            continue;
        }
        let signed = |i: usize| (reps[i], &signatures[&reps[i].hash]);
        members.sort_by(|&a, &b| better_keeper(signed(a), signed(b)));
        let keeper = reps[members[0]];
        let keeper_sig = &signatures[&keeper.hash];
        let mut files = vec![keeper.clone()];
        for &m in &members[1..] {
            let rec = reps[m];
            // A chain must not join two pictures: each copy is confirmed
            // against the keeper itself, at a different size.
            if !confirms(keeper_sig, &signatures[&rec.hash]) {
                continue;
            }
            let mut paths: Vec<&FileRecord> = by_hash[rec.hash.as_str()]
                .iter()
                .copied()
                .filter(|r| !creations.contains(r.path.as_str()))
                .collect();
            paths.sort_by(|a, b| a.path.cmp(&b.path));
            files.extend(paths.into_iter().cloned());
        }
        if files.len() > 1 {
            groups.push(Group {
                kind: Kind::Resized,
                files,
            });
        }
    }
    groups.sort_by(|a, b| a.keeper().path.cmp(&b.keeper().path));
    (groups, missing.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(path: &str, hash: &str, w: u32, h: u32, phash: u64) -> FileRecord {
        FileRecord {
            path: path.to_string(),
            hash: hash.to_string(),
            meta_hash: None,
            size_bytes: 1000,
            created_at: None,
            modified_at: Some("2023-01-01T00:00:00+00:00".to_string()),
            ext: "jpg".to_string(),
            mime: Some("image/jpeg".to_string()),
            phash: Some(phash),
            exif_date: None,
            gps_lat: None,
            gps_lon: None,
            width: Some(w),
            height: Some(h),
            duration_secs: None,
            codec: None,
            date_source: None,
            mvhd_unix: None,
        }
    }

    /// A signature of the decoded pixels: `w` by `h`, whatever the row says.
    fn sig(luma: &[u8], w: u32, h: u32) -> PixelSignature {
        PixelSignature {
            luma: luma.to_vec(),
            width: w,
            height: h,
        }
    }

    const P: u64 = 0x0F0F_3C3C_5A5A_6969;
    const FLAT: [u8; 10] = [0; 10];

    fn sigs(v: Vec<(&str, PixelSignature)>) -> HashMap<String, PixelSignature> {
        v.into_iter().map(|(h, s)| (h.to_string(), s)).collect()
    }

    fn paths(g: &Group) -> Vec<&str> {
        g.files.iter().map(|f| f.path.as_str()).collect()
    }

    #[test]
    fn a_resized_copy_groups_with_the_larger_kept() {
        let records = [
            rec("/m/çiçek-wa.jpg", "small", 1280, 960, P ^ 0b11),
            rec("/m/çiçek.jpg", "big", 1600, 1200, P),
        ];
        let s = sigs(vec![
            ("small", sig(&FLAT, 1280, 960)),
            ("big", sig(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 4], 1600, 1200)),
        ]);
        assert_eq!(resized_candidates(&records).len(), 2);
        let found = find_in(&records, &[Kind::Resized], &s);
        assert_eq!(found.unchecked, 0);
        assert_eq!(found.groups.len(), 1);
        assert_eq!(found.groups[0].kind, Kind::Resized);
        assert_eq!(paths(&found.groups[0]), ["/m/çiçek.jpg", "/m/çiçek-wa.jpg"]);
    }

    #[test]
    fn a_same_size_near_pair_never_groups() {
        let records = [
            rec("/m/IMG_1816.jpg", "a", 1600, 1200, P),
            rec("/m/IMG_1817.jpg", "b", 1600, 1200, P ^ 1),
        ];
        let s = sigs(vec![
            ("a", sig(&FLAT, 1600, 1200)),
            ("b", sig(&FLAT, 1600, 1200)),
        ]);
        assert_eq!(resized_candidates(&records).len(), 2, "checked by pixels");
        assert!(find_in(&records, &[Kind::Resized], &s).groups.is_empty());
    }

    /// A copy can keep its original's size tags: the rows say 1932x2576 for
    /// both, the pixels of one are 1536x2048. The pixels decide.
    #[test]
    fn the_decoded_size_decides_not_the_stored_one() {
        let records = [
            rec("/m/IMG_0004.JPG", "a", 1932, 2576, P),
            rec("/m/IMG_0004(1).JPG", "b", 1932, 2576, P),
        ];
        let s = sigs(vec![
            ("a", sig(&FLAT, 1536, 2048)),
            ("b", sig(&FLAT, 1932, 2576)),
        ]);
        let groups = find_in(&records, &[Kind::Resized], &s).groups;
        assert_eq!(groups.len(), 1);
        assert_eq!(paths(&groups[0]), ["/m/IMG_0004(1).JPG", "/m/IMG_0004.JPG"]);

        let records = [
            rec("/m/IMG_1816.jpg", "a", 1600, 1200, P),
            rec("/m/IMG_1817.jpg", "b", 800, 600, P ^ 1),
        ];
        let s = sigs(vec![
            ("a", sig(&FLAT, 1600, 1200)),
            ("b", sig(&FLAT, 1600, 1200)),
        ]);
        assert!(find_in(&records, &[Kind::Resized], &s).groups.is_empty());
    }

    #[test]
    fn far_fingerprints_a_different_aspect_or_distant_pixels_do_not_group() {
        let records = [
            rec("/m/deniz.jpg", "a", 1600, 1200, P),
            rec("/m/deniz-uzak.jpg", "b", 800, 600, P ^ 0b1_1111),
            rec("/m/deniz-kare.jpg", "c", 1200, 1200, P ^ 1),
            rec("/m/deniz-başka.jpg", "d", 640, 480, P ^ 2),
        ];
        let s = sigs(vec![
            ("a", sig(&FLAT, 1600, 1200)),
            ("b", sig(&FLAT, 800, 600)),
            ("c", sig(&FLAT, 1200, 1200)),
            ("d", sig(&[9; 10], 640, 480)),
        ]);
        assert!(find_in(&records, &[Kind::Resized], &s).groups.is_empty());
    }

    #[test]
    fn a_chain_does_not_join_two_pictures() {
        // a~b and b~c confirm, a and c do not: c stays out of a's group.
        let records = [
            rec("/m/a.jpg", "a", 1600, 1200, P),
            rec("/m/b.jpg", "b", 1280, 960, P ^ 1),
            rec("/m/c.jpg", "c", 800, 600, P ^ 2),
        ];
        let s = sigs(vec![
            ("a", sig(&FLAT, 1600, 1200)),
            ("b", sig(&[3, 0, 0, 0, 0, 0, 0, 0, 0, 0], 1280, 960)),
            ("c", sig(&[3, 3, 0, 0, 0, 0, 0, 0, 0, 0], 800, 600)),
        ]);
        let groups = find_in(&records, &[Kind::Resized], &s).groups;
        assert_eq!(groups.len(), 1);
        assert_eq!(paths(&groups[0]), ["/m/a.jpg", "/m/b.jpg"]);
    }

    #[test]
    fn a_missing_signature_is_counted_not_guessed() {
        let records = [
            rec("/m/a.jpg", "a", 1600, 1200, P),
            rec("/m/b.jpg", "b", 1280, 960, P ^ 1),
        ];
        let s = sigs(vec![("a", sig(&FLAT, 1600, 1200))]);
        let found = find_in(&records, &[Kind::Resized], &s);
        assert!(found.groups.is_empty());
        assert_eq!(found.unchecked, 1);
    }

    #[test]
    fn keeper_ties_break_on_file_size_then_date_then_path() {
        // Equal pixels cannot both be in one group, so the tie-break is
        // between a keeper and a copy of the same pixel count in another
        // order: exercised through the ordering directly.
        let s = sig(&FLAT, 1600, 1200);
        let mut a = rec("/m/a.jpg", "a", 1600, 1200, P);
        let mut b = rec("/m/b.jpg", "b", 1600, 1200, P);
        assert_eq!(
            better_keeper((&a, &sig(&FLAT, 1600, 1201)), (&b, &s)),
            std::cmp::Ordering::Less,
            "most pixels first"
        );
        b.size_bytes = 2000;
        assert_eq!(better_keeper((&b, &s), (&a, &s)), std::cmp::Ordering::Less);
        b.size_bytes = a.size_bytes;
        a.exif_date = Some("2019-01-01T00:00:00".into());
        assert_eq!(better_keeper((&a, &s), (&b, &s)), std::cmp::Ordering::Less);
        a.exif_date = None;
        assert_eq!(better_keeper((&a, &s), (&b, &s)), std::cmp::Ordering::Less);
    }

    #[test]
    fn exact_copies_of_one_hash_join_the_resized_group() {
        let records = [
            rec("/m/çiçek.jpg", "big", 1600, 1200, P),
            rec("/m/çiçek-wa.jpg", "small", 1280, 960, P ^ 1),
            rec("/yedek/çiçek-wa.jpg", "small", 1280, 960, P ^ 1),
        ];
        let s = sigs(vec![
            ("small", sig(&FLAT, 1280, 960)),
            ("big", sig(&FLAT, 1600, 1200)),
        ]);
        let groups = find_in(&records, &[Kind::Resized], &s).groups;
        assert_eq!(
            paths(&groups[0]),
            ["/m/çiçek.jpg", "/m/çiçek-wa.jpg", "/yedek/çiçek-wa.jpg"]
        );
    }

    #[test]
    fn creations_lose_to_their_original_in_one_folder() {
        let records = [
            rec("/t/IMG_0001.jpg", "o", 100, 100, P),
            rec("/t/IMG_0001-EFFECTS.jpg", "e", 100, 100, P),
            rec("/t/IMG_0001-SMILE.jpg", "s", 100, 100, P),
            rec("/t/IMG_0001-edited.jpg", "d", 100, 100, P),
            rec("/başka/IMG_0002-EFFECTS.jpg", "x", 100, 100, P),
            rec("/t/IMG_0002.jpg", "y", 100, 100, P),
            rec("/t/IMG_0003-EFFECTS.jpg", "z", 100, 100, P),
        ];
        let groups = find_in(&records, &[Kind::Creation], &HashMap::new()).groups;
        assert_eq!(groups.len(), 1);
        assert_eq!(
            paths(&groups[0]),
            [
                "/t/IMG_0001.jpg",
                "/t/IMG_0001-EFFECTS.jpg",
                "/t/IMG_0001-SMILE.jpg",
                "/t/IMG_0001-edited.jpg"
            ]
        );
    }

    #[test]
    fn a_creation_is_never_a_resized_keeper() {
        // The effect is larger, but it is the creation kind's: the resized
        // check leaves it out, so it cannot outlive the original.
        let records = [
            rec("/t/IMG_0001.jpg", "o", 1280, 960, P),
            rec("/t/IMG_0001-EFFECTS.jpg", "e", 1600, 1200, P ^ 1),
        ];
        let s = sigs(vec![
            ("o", sig(&FLAT, 1280, 960)),
            ("e", sig(&FLAT, 1600, 1200)),
        ]);
        assert!(find_in(&records, &[Kind::Resized], &s).groups.is_empty());
    }

    #[test]
    fn an_exact_group_keeps_a_copy_that_is_not_a_creation() {
        let mut edit = rec("/t/a-edited.jpg", "h", 1, 1, P);
        edit.exif_date = Some("2010-01-01T00:00:00".into());
        let records = [
            edit,
            rec("/yedek/a.jpg", "h", 1, 1, P),
            rec("/t/a.jpg", "orig", 1, 1, P),
        ];
        let exact = find_in(&records, &[Kind::Exact], &HashMap::new()).groups;
        assert_eq!(exact[0].keeper().path, "/t/a-edited.jpg", "oldest");
        let both = find_in(&records, &[Kind::Exact, Kind::Creation], &HashMap::new()).groups;
        assert_eq!(both[0].kind, Kind::Exact);
        assert_eq!(both[0].keeper().path, "/yedek/a.jpg");
    }

    #[test]
    fn similar_groups_are_review_only() {
        let records = [
            rec("/m/a.jpg", "a", 1600, 1200, P),
            rec("/m/b.jpg", "b", 1600, 1200, P ^ 0b111),
        ];
        let groups = find_in(&records, &[Kind::Similar], &HashMap::new()).groups;
        assert_eq!(groups.len(), 1);
        assert!(!groups[0].kind.removable());
    }

    #[test]
    fn a_query_keeps_a_group_whole_when_only_its_keeper_matches() {
        let records = [
            rec("/m/çiçek.jpg", "h", 1, 1, P),
            rec("/m/çiçek kopya.jpg", "h", 1, 1, P),
            rec("/m/deniz.jpg", "g", 1, 1, P),
            rec("/m/deniz kopya.jpg", "g", 1, 1, P),
        ];
        let groups = find_in(&records, &[Kind::Exact], &HashMap::new()).groups;
        let keeper = groups
            .iter()
            .find(|g| g.keeper().hash == "h")
            .unwrap()
            .keeper()
            .path
            .clone();
        let kept = filter_groups(groups, &|r| r.path == keeper);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].files.len(), 2);
    }
}
