//! Date, location, dimensions, duration and codec from a QuickTime/MP4
//! container, without decoding anything.
//!
//! videre extracted no metadata from video at all until this existed:
//! `mime_probe::EXIF_MIMES` covers jpeg/tiff/heic, and `hasher` returns all
//! `None` for anything else. Measured on a real library, that left **13,457
//! videos with no date and no GPS**, so every feature keyed on either
//! (`--after`/`--before`, `--near`, `videre locations`, and any compositional
//! query mixing them) silently excluded 19% of the files while presenting
//! itself as covering the library.
//!
//! Parsing is in-house rather than shelling out to `ffprobe`: `video_probe`
//! already walks these boxes, so this is the same walk one level further. That
//! also avoids putting a subprocess on a user-supplied path, which would need
//! `io_timeout` bounding whose timeout could *not* be size-scaled, since
//! reading a header is not proportional to file length.

use std::io::{Seek, SeekFrom};
use std::path::Path;

/// Everything worth taking from a container.
///
/// `duration_secs` and `codec` are here rather than in a later release because
/// existing rows cannot pick this up incrementally: `--retry-incomplete` keys
/// on `mime IS NULL`, which does not catch "scanned before video metadata
/// existed", so shipping this forces one full re-scan of every library
/// regardless. Adding them separately would have forced a second one.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct VideoMeta {
    /// Local wall-clock, `YYYY-MM-DDTHH:MM:SS`, matching what `extract_exif`
    /// writes for photos. See `parse_apple_date` for why not UTC.
    pub date: Option<String>,
    pub gps_lat: Option<f64>,
    pub gps_lon: Option<f64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_secs: Option<f64>,
    /// The `stsd` format tag verbatim, lowercased: `avc1`, `hvc1`, `prores`.
    /// Not mapped to a friendly name; that is a display concern.
    pub codec: Option<String>,
}

/// Iterate a box payload's direct children as `(type, payload)`.
///
/// Stops rather than erroring on a malformed size, so a truncated tail yields
/// the children that did parse. Every caller here treats absence as "unknown",
/// so partial data is strictly better than none.
fn children(buf: &[u8]) -> Vec<([u8; 4], &[u8])> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i + 8 <= buf.len() {
        let Ok(size_bytes) = buf[i..i + 4].try_into() else {
            break;
        };
        let size = u32::from_be_bytes(size_bytes) as usize;
        let Ok(typ) = buf[i + 4..i + 8].try_into() else {
            break;
        };
        let (size, header_len) = if size == 1 {
            if i + 16 > buf.len() {
                break;
            }
            let Ok(b) = buf[i + 8..i + 16].try_into() else {
                break;
            };
            let Ok(s) = usize::try_from(u64::from_be_bytes(b)) else {
                break;
            };
            (s, 16usize)
        } else if size == 0 {
            (buf.len() - i, 8usize)
        } else {
            (size, 8usize)
        };
        if size < header_len || i + size > buf.len() {
            break;
        }
        out.push((typ, &buf[i + header_len..i + size]));
        i += size;
    }
    out
}

fn find<'a>(buf: &'a [u8], typ: &[u8; 4]) -> Option<&'a [u8]> {
    children(buf)
        .into_iter()
        .find(|(t, _)| t == typ)
        .map(|(_, p)| p)
}

fn be32(b: &[u8], at: usize) -> Option<u32> {
    b.get(at..at + 4)
        .and_then(|s| s.try_into().ok())
        .map(u32::from_be_bytes)
}

fn be64(b: &[u8], at: usize) -> Option<u64> {
    b.get(at..at + 8)
        .and_then(|s| s.try_into().ok())
        .map(u64::from_be_bytes)
}

/// `duration / timescale` from `mvhd`.
///
/// Version 0 uses 32-bit times, version 1 uses 64-bit. The version byte must be
/// read rather than assumed: getting it wrong reads the duration from the wrong
/// offset and yields a plausible-looking wrong number, not an error.
fn parse_mvhd(p: &[u8]) -> Option<f64> {
    let version = *p.first()?;
    let (timescale, duration) = if version == 1 {
        (be32(p, 20)? as f64, be64(p, 24)? as f64)
    } else {
        (be32(p, 12)? as f64, be32(p, 16)? as f64)
    };
    if timescale == 0.0 {
        return None;
    }
    Some(duration / timescale)
}

/// Seconds between 1904-01-01 (the QuickTime epoch) and 1970-01-01 (Unix).
const QT_EPOCH_OFFSET: i64 = 2_082_844_800;

/// `mvhd`'s creation time, as a fallback when the Apple key is absent.
///
/// **This one is UTC**, unlike `com.apple.quicktime.creationdate`, and there is
/// no offset stored anywhere to recover the local time from. Measured on a real
/// corpus, 10 of 260 clips carry only this - all of them re-encoded renders
/// rather than camera originals. A date that may be wrong by a timezone is
/// still far better than no date, which excludes the file from every date
/// filter entirely; but prefer the Apple key whenever it exists.
fn parse_mvhd_date(p: &[u8]) -> Option<String> {
    let version = *p.first()?;
    let secs = if version == 1 {
        be64(p, 4)? as i64
    } else {
        be32(p, 4)? as i64
    };
    let unix = secs.checked_sub(QT_EPOCH_OFFSET)?;
    // A zero or absurd creation time is common in re-muxed files; reject it
    // rather than recording 1904 or 1970 as a capture date.
    if unix <= 0 {
        return None;
    }
    let dt = chrono::DateTime::from_timestamp(unix, 0)?;
    Some(dt.format("%Y-%m-%dT%H:%M:%S").to_string())
}

/// Width and height from `tkhd`, stored as 16.16 fixed point.
///
/// The display matrix that precedes them can encode a rotation, so a portrait
/// video commonly reports landscape dimensions here. Applying the matrix is out
/// of scope: a wrong aspect ratio is cosmetic, where a wrong date is not.
fn parse_tkhd(p: &[u8]) -> Option<(u32, u32)> {
    let version = *p.first()?;
    // version+flags(4) + times/id/reserved/duration + reserved(8) + layer(2)
    // + alternate_group(2) + volume(2) + reserved(2) + matrix(36)
    let at = if version == 1 {
        4 + 32 + 16 + 36
    } else {
        4 + 20 + 16 + 36
    };
    let w = be32(p, at)? >> 16;
    let h = be32(p, at + 4)? >> 16;
    (w > 0 && h > 0).then_some((w, h))
}

/// First sample entry's 4-byte format tag from `stsd`.
fn parse_stsd(p: &[u8]) -> Option<String> {
    // version+flags(4) entry_count(4), then entry: size(4) format(4)
    let tag = p.get(12..16)?;
    let s = std::str::from_utf8(tag).ok()?.trim().to_lowercase();
    (!s.is_empty()).then_some(s)
}

/// `+19.4290-099.1625+2248.823/` -> (lat, lon), altitude discarded.
///
/// Scans for the sign characters rather than assuming widths: the digit counts
/// vary with precision and the altitude segment is optional. Returns None
/// rather than guessing on anything unexpected, because a wrong coordinate puts
/// a file on the wrong continent in `videre locations` and the clustering has
/// no way to notice.
pub fn parse_iso6709(s: &str) -> Option<(f64, f64)> {
    let s = s.trim().trim_end_matches('/');
    let mut parts: Vec<String> = Vec::new();
    let mut cur = String::new();
    for c in s.chars() {
        if (c == '+' || c == '-') && !cur.is_empty() {
            parts.push(std::mem::take(&mut cur));
        }
        if c == '+' || c == '-' || c.is_ascii_digit() || c == '.' {
            cur.push(c);
        } else {
            return None;
        }
    }
    if !cur.is_empty() {
        parts.push(cur);
    }
    if parts.len() < 2 {
        return None;
    }
    let lat: f64 = parts[0].parse().ok()?;
    let lon: f64 = parts[1].parse().ok()?;
    (-90.0..=90.0)
        .contains(&lat)
        .then_some(())
        .and((-180.0..=180.0).contains(&lon).then_some(()))?;
    Some((lat, lon))
}

/// `2024-12-15T21:49:24-0600` -> `2024-12-15T21:49:24`.
///
/// **The local wall-clock is kept and the offset dropped, deliberately.**
/// Photos store `exif_date` as local wall-clock because EXIF carries no
/// timezone, and every date filter, `EFFECTIVE_DATE_SQL` and
/// `output::best_date` compares those strings. QuickTime's `mvhd` time is UTC,
/// so storing that would put video on a different clock in the same column: a
/// video shot at 21:49 local would land on the following day, `--on` would miss
/// it, and a chronological sort would interleave it wrongly against photos
/// taken minutes earlier. Anyone "simplifying" this to the UTC field is
/// introducing a silent, permanent error.
fn parse_apple_date(s: &str) -> Option<String> {
    let s = s.trim();
    if s.len() < 19 {
        return None;
    }
    let head = &s[..19];
    let b = head.as_bytes();
    (b[4] == b'-' && b[7] == b'-' && b[10] == b'T' && b[13] == b':' && b[16] == b':')
        .then(|| head.to_string())
}

/// Apple metadata lives in `moov/meta`, whose children are indexed by `keys`
/// and valued by `ilst`.
///
/// `meta` is a full box (4 bytes of version/flags before its children) in MP4
/// but a plain container in QuickTime. Rather than branch on brand, try both
/// and keep whichever yields a `keys` child.
fn apple_keys(moov: &[u8]) -> Option<(Option<String>, Option<String>)> {
    let meta = find(moov, b"meta")?;
    let body = [meta, meta.get(4..).unwrap_or(&[])]
        .into_iter()
        .find(|b| find(b, b"keys").is_some())?;

    let keys = find(body, b"keys")?;
    let ilst = find(body, b"ilst")?;

    // keys: version/flags(4) entry_count(4), then entries of
    // size(4) namespace(4) name(size-8)
    let mut names: Vec<String> = Vec::new();
    let mut i = 8usize;
    while i + 8 <= keys.len() {
        let size = be32(keys, i)? as usize;
        if size < 8 || i + size > keys.len() {
            break;
        }
        names.push(String::from_utf8_lossy(&keys[i + 8..i + size]).into_owned());
        i += size;
    }

    let (mut date, mut loc) = (None, None);
    // ilst children are 1-based indices into `names`, each holding a `data` box
    // of type_indicator(4) locale(4) value(..).
    for (typ, payload) in children(ilst) {
        let idx = u32::from_be_bytes(typ) as usize;
        let Some(name) = idx.checked_sub(1).and_then(|k| names.get(k)) else {
            continue;
        };
        let Some(data) = find(payload, b"data") else {
            continue;
        };
        let Some(value) = data.get(8..) else { continue };
        let value = String::from_utf8_lossy(value).into_owned();
        match name.as_str() {
            "com.apple.quicktime.creationdate" => date = Some(value),
            "com.apple.quicktime.location.ISO6709" => loc = Some(value),
            _ => {}
        }
    }
    Some((date, loc))
}

/// Parser core, over an in-memory `moov`. Split out so tests drive it with
/// synthetic boxes: `videre-core` reads no fixture files.
pub(crate) fn from_moov(moov: &[u8]) -> VideoMeta {
    let mut m = VideoMeta::default();

    if let Some(d) = find(moov, b"mvhd").and_then(parse_mvhd) {
        m.duration_secs = Some(d);
    }

    if let Some((date, loc)) = apple_keys(moov) {
        m.date = date.as_deref().and_then(parse_apple_date);
        if let Some((lat, lon)) = loc.as_deref().and_then(parse_iso6709) {
            m.gps_lat = Some(lat);
            m.gps_lon = Some(lon);
        }
    }

    // Only when the Apple key is missing: that one carries local time, this one
    // is UTC, and mixing them silently would put some video on a different
    // clock from the photos beside it.
    if m.date.is_none() {
        m.date = find(moov, b"mvhd").and_then(parse_mvhd_date);
    }

    // Dimensions and codec come from the *video* track, so audio traks must be
    // skipped: taking the first trak would report the audio track's zero
    // dimensions and its codec.
    for (typ, trak) in children(moov) {
        if &typ != b"trak" {
            continue;
        }
        let is_video = find(trak, b"mdia")
            .and_then(|mdia| find(mdia, b"hdlr"))
            .is_some_and(|h| h.get(8..12) == Some(b"vide"));
        if !is_video {
            continue;
        }
        if let Some((w, h)) = find(trak, b"tkhd").and_then(parse_tkhd) {
            m.width = Some(w);
            m.height = Some(h);
        }
        m.codec = find(trak, b"mdia")
            .and_then(|x| find(x, b"minf"))
            .and_then(|x| find(x, b"stbl"))
            .and_then(|x| find(x, b"stsd"))
            .and_then(parse_stsd);
        break;
    }

    m
}

/// Read what a container knows about itself.
///
/// Total by construction: a missing file, unreadable file, unknown layout or
/// malformed box all yield `VideoMeta::default()`. A scan must never fail
/// because one file has an odd header.
///
/// Bounded by the constant `DEFAULT_IO_TIMEOUT`, **not** the size-scaled one:
/// this reads a header near the start of the file, so its cost is not
/// proportional to length. See the whole-file-reads-only rule on
/// `io_timeout::timeout_for_size`.
pub fn read(path: &Path) -> VideoMeta {
    let path = path.to_path_buf();
    crate::io_timeout::run_with_timeout(crate::io_timeout::DEFAULT_IO_TIMEOUT, move || {
        let mut f = std::fs::File::open(&path).ok()?;
        let end = f.seek(SeekFrom::End(0)).ok()?;
        f.seek(SeekFrom::Start(0)).ok()?;
        crate::video_probe::read_moov(&mut f, end).map(|moov| from_moov(&moov))
    })
    .ok()
    .flatten()
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bx(typ: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut v = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(typ);
        v.extend_from_slice(payload);
        v
    }

    #[test]
    fn iso6709_parses_both_signs_and_optional_altitude() {
        assert_eq!(
            parse_iso6709("+52.5535+013.4299+050.897/"),
            Some((52.5535, 13.4299))
        );
        assert_eq!(
            parse_iso6709("+19.4290-099.1625+2248.823/"),
            Some((19.4290, -99.1625))
        );
        assert_eq!(parse_iso6709("+52.5535+013.4299"), Some((52.5535, 13.4299)));
        assert_eq!(
            parse_iso6709("-33.8688+151.2093/"),
            Some((-33.8688, 151.2093))
        );
    }

    #[test]
    fn iso6709_refuses_rather_than_guesses() {
        // A wrong coordinate puts a file on the wrong continent in
        // `videre locations`, and nothing downstream can notice.
        assert_eq!(parse_iso6709(""), None);
        assert_eq!(
            parse_iso6709("+52.5535"),
            None,
            "one component is not a position"
        );
        assert_eq!(
            parse_iso6709("52.5535,13.4299"),
            None,
            "comma form is not ISO6709"
        );
        assert_eq!(parse_iso6709("+99.0+013.0/"), None, "latitude out of range");
        assert_eq!(
            parse_iso6709("+52.0+200.0/"),
            None,
            "longitude out of range"
        );
    }

    #[test]
    fn apple_date_keeps_local_wallclock_and_drops_the_offset() {
        // Photos store local wall-clock in the same column; storing UTC here
        // would put a 21:49 local video on the following day.
        assert_eq!(
            parse_apple_date("2024-12-15T21:49:24-0600").as_deref(),
            Some("2024-12-15T21:49:24")
        );
        assert_eq!(
            parse_apple_date("2017-09-08T10:34:23+0200").as_deref(),
            Some("2017-09-08T10:34:23")
        );
        assert_eq!(
            parse_apple_date("2024-12-15"),
            None,
            "too short to be a time"
        );
        assert_eq!(parse_apple_date("not a date at all!!"), None);
    }

    #[test]
    fn mvhd_duration_reads_the_version_rather_than_assuming() {
        // v0: version/flags(4) create(4) modify(4) timescale(4) duration(4)
        let mut v0 = vec![0u8; 4];
        v0.extend_from_slice(&0u32.to_be_bytes());
        v0.extend_from_slice(&0u32.to_be_bytes());
        v0.extend_from_slice(&600u32.to_be_bytes());
        v0.extend_from_slice(&9000u32.to_be_bytes());
        assert_eq!(parse_mvhd(&v0), Some(15.0));

        // v1: version=1, then 64-bit times, timescale(4), 64-bit duration
        let mut v1 = vec![1u8, 0, 0, 0];
        v1.extend_from_slice(&0u64.to_be_bytes());
        v1.extend_from_slice(&0u64.to_be_bytes());
        v1.extend_from_slice(&1000u32.to_be_bytes());
        v1.extend_from_slice(&2500u64.to_be_bytes());
        assert_eq!(parse_mvhd(&v1), Some(2.5));

        let mut zero = v0.clone();
        zero[12..16].copy_from_slice(&0u32.to_be_bytes());
        assert_eq!(parse_mvhd(&zero), None, "timescale 0 must not divide");
    }

    #[test]
    fn tkhd_dimensions_are_16_16_fixed_point() {
        let mut p = vec![0u8; 4 + 20 + 16 + 36];
        p.extend_from_slice(&(1920u32 << 16).to_be_bytes());
        p.extend_from_slice(&(1080u32 << 16).to_be_bytes());
        assert_eq!(parse_tkhd(&p), Some((1920, 1080)));
    }

    #[test]
    fn stsd_yields_the_format_tag() {
        let mut p = vec![0u8; 8];
        p.extend_from_slice(&0u32.to_be_bytes());
        p.extend_from_slice(b"hvc1");
        assert_eq!(parse_stsd(&p).as_deref(), Some("hvc1"));
    }

    #[test]
    fn dimensions_come_from_the_video_track_not_the_first_one() {
        // An audio trak first would otherwise report its zero dimensions.
        let hdlr = |kind: &[u8; 4]| {
            let mut p = vec![0u8; 8];
            p.extend_from_slice(kind);
            bx(b"hdlr", &p)
        };
        let mut tkhd_payload = vec![0u8; 4 + 20 + 16 + 36];
        tkhd_payload.extend_from_slice(&(1920u32 << 16).to_be_bytes());
        tkhd_payload.extend_from_slice(&(1080u32 << 16).to_be_bytes());

        let audio = bx(b"trak", &bx(b"mdia", &hdlr(b"soun")));
        let mut video_children = bx(b"tkhd", &tkhd_payload);
        video_children.extend_from_slice(&bx(b"mdia", &hdlr(b"vide")));
        let video = bx(b"trak", &video_children);

        let mut moov = audio;
        moov.extend_from_slice(&video);
        let m = from_moov(&moov);
        assert_eq!((m.width, m.height), (Some(1920), Some(1080)));
    }

    #[test]
    fn a_malformed_container_yields_defaults_rather_than_panicking() {
        assert_eq!(from_moov(&[]), VideoMeta::default());
        assert_eq!(from_moov(&[0xff; 7]), VideoMeta::default());
        assert_eq!(
            from_moov(&[0, 0, 0, 200, b'm', b'v', b'h', b'd']),
            VideoMeta::default()
        );
    }

    /// Real-file check. Ignored: `videre-core` reads no fixture files, and this
    /// needs the local corpus. Run with
    /// `cargo test -p videre-core --ignored real_video`.
    #[test]
    #[ignore]
    fn real_video_from_the_corpus_parses() {
        let dir = std::path::Path::new(concat!(env!("HOME"), "/videre-test/iphotos"));
        if !dir.exists() {
            return;
        }
        fn movs(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            if out.len() >= 25 {
                return;
            }
            let Ok(rd) = std::fs::read_dir(dir) else {
                return;
            };
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    movs(&p, out);
                } else if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("mov")) {
                    out.push(p);
                }
                if out.len() >= 25 {
                    return;
                }
            }
        }
        let mut found = Vec::new();
        movs(dir, &mut found);
        let checked = found.len();
        let mut with_gps = 0;
        for p in &found {
            let m = read(p);
            // Structural: every container carries these.
            assert!(m.date.is_some(), "no date for {p:?}");
            assert!(
                m.duration_secs.is_some_and(|d| d > 0.0),
                "no duration for {p:?}"
            );
            assert!(m.width.is_some_and(|w| w > 0), "no width for {p:?}");
            assert!(m.codec.is_some(), "no codec for {p:?}");
            // GPS is not structural. Measured on this corpus, 238 of 254 carry
            // it: a clip recorded with location services off has none, and
            // asserting it per file fails on a correct parse of a real file.
            if m.gps_lat.is_some() {
                with_gps += 1;
            }
            // Printed so a value-level diff against ffprobe is one command, not
            // a rebuild: presence assertions cannot catch a swapped lat/lon or
            // a timezone slip.
            if std::env::var_os("VIDERE_DUMP_VIDEO_META").is_some() {
                println!(
                    "{}\t{:?}\t{:?}\t{:?}\t{:?}x{:?}\t{:?}\t{:?}",
                    p.file_name().unwrap().to_string_lossy(),
                    m.date,
                    m.gps_lat,
                    m.gps_lon,
                    m.width,
                    m.height,
                    m.duration_secs.map(|d| (d * 100.0).round() / 100.0),
                    m.codec
                );
            }
        }
        assert!(checked > 0, "corpus present but no .mov found");
        assert!(
            with_gps * 100 >= checked * 70,
            "only {with_gps}/{checked} carried GPS; the corpus measured ~94%"
        );
    }
}

#[cfg(test)]
mod mvhd_date_tests {
    use super::*;

    fn mvhd_v0(created_1904: u32) -> Vec<u8> {
        let mut p = vec![0u8; 4];
        p.extend_from_slice(&created_1904.to_be_bytes());
        p.extend_from_slice(&0u32.to_be_bytes());
        p.extend_from_slice(&600u32.to_be_bytes());
        p.extend_from_slice(&600u32.to_be_bytes());
        p
    }

    #[test]
    fn mvhd_date_converts_from_the_1904_epoch() {
        // 1970-01-01T00:00:00Z is exactly QT_EPOCH_OFFSET seconds in.
        let one_hour_after_unix_epoch = (QT_EPOCH_OFFSET + 3600) as u32;
        assert_eq!(
            parse_mvhd_date(&mvhd_v0(one_hour_after_unix_epoch)).as_deref(),
            Some("1970-01-01T01:00:00")
        );
    }

    #[test]
    fn a_zero_creation_time_is_refused_rather_than_recorded_as_1904() {
        // Common in re-muxed files; recording 1904 as a capture date would put
        // the file at the very top of every chronological sort.
        assert_eq!(parse_mvhd_date(&mvhd_v0(0)), None);
    }

    #[test]
    fn the_apple_key_wins_when_both_are_present() {
        // The Apple key is local time and mvhd is UTC; taking mvhd when both
        // exist would put some video on a different clock from the photos
        // beside it.
        fn bx(typ: &[u8; 4], payload: &[u8]) -> Vec<u8> {
            let mut v = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
            v.extend_from_slice(typ);
            v.extend_from_slice(payload);
            v
        }
        let mut keys = vec![0u8; 8];
        let name = b"com.apple.quicktime.creationdate";
        keys.extend_from_slice(&((name.len() + 8) as u32).to_be_bytes());
        keys.extend_from_slice(b"mdta");
        keys.extend_from_slice(name);

        let mut data = vec![0u8; 8];
        data.extend_from_slice(b"2020-05-06T07:08:09+0300");
        let ilst = bx(&1u32.to_be_bytes(), &bx(b"data", &data));

        let mut meta_body = bx(b"keys", &keys);
        meta_body.extend_from_slice(&ilst_wrap(&ilst));

        let mut moov = bx(b"mvhd", &mvhd_v0((QT_EPOCH_OFFSET + 3600) as u32));
        moov.extend_from_slice(&bx(b"meta", &meta_body));

        assert_eq!(
            from_moov(&moov).date.as_deref(),
            Some("2020-05-06T07:08:09"),
            "local Apple time must beat UTC mvhd"
        );
    }

    fn ilst_wrap(inner: &[u8]) -> Vec<u8> {
        let mut v = ((inner.len() + 8) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(b"ilst");
        v.extend_from_slice(inner);
        v
    }
}
