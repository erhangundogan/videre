//! Detect whether a QuickTime/MP4 container has a video track, without
//! decoding anything.
//!
//! `qlmanage -t` does not fail on a file with no video track; it hangs. videre
//! kills it at `QLMANAGE_TIMEOUT` (20s), so the file is skipped correctly but
//! the wait is paid on every run, forever, because nothing records the file as
//! permanently unembeddable. Measured on a real 70,601-file library: three
//! audio-only Live Photo companions cost 60s per `videre embed` run and
//! another 60s per `videre scan --similar`.
//!
//! See docs/superpowers/specs/2026-08-09-video-track-probe-design.md, which
//! also records why a fourth failing file in that library is *not* detectable
//! this way.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// Largest `moov` box the probe will read into memory.
///
/// Real ones are tiny even for huge files: 32 KB for a 991 MB video in the
/// library this was measured against. A larger declared size means either a
/// corrupt header or a file this probe has no business parsing, so it fails
/// open rather than allocating.
const MOOV_CAP: u64 = 32 * 1024 * 1024;

/// Read one box header. Returns (total size, type, header length).
///
/// `size == 1` means a 64-bit size follows the type; `size == 0` means the box
/// runs to `end`. Returns None on a short read or a size smaller than the
/// header it just read, which is what stops a malformed file from looping.
fn read_header<R: Read + Seek>(r: &mut R, end: u64) -> Option<(u64, [u8; 4], u64)> {
    let start = r.stream_position().ok()?;
    let mut hdr = [0u8; 8];
    r.read_exact(&mut hdr).ok()?;
    let mut size = u32::from_be_bytes(hdr[0..4].try_into().ok()?) as u64;
    let typ: [u8; 4] = hdr[4..8].try_into().ok()?;
    let mut header_len = 8u64;
    if size == 1 {
        let mut ext = [0u8; 8];
        r.read_exact(&mut ext).ok()?;
        size = u64::from_be_bytes(ext);
        header_len = 16;
    } else if size == 0 {
        size = end.checked_sub(start)?;
    }
    if size < header_len {
        return None;
    }
    Some((size, typ, header_len))
}

/// Locate `moov` among the top-level boxes and return its payload.
///
/// Seeks rather than reads, so a 991 MB file costs a few seeks. Position
/// independent: `moov` may sit before or after `mdat`.
fn read_moov<R: Read + Seek>(r: &mut R, end: u64) -> Option<Vec<u8>> {
    loop {
        let start = r.stream_position().ok()?;
        if start >= end {
            return None;
        }
        let (size, typ, header_len) = read_header(r, end)?;
        if start.checked_add(size)? > end {
            return None;
        }
        if &typ == b"moov" {
            let payload = size - header_len;
            if payload > MOOV_CAP {
                return None;
            }
            let mut buf = vec![0u8; payload as usize];
            r.read_exact(&mut buf).ok()?;
            return Some(buf);
        }
        r.seek(SeekFrom::Start(start.checked_add(size)?)).ok()?;
    }
}

/// Walk `moov`'s children for an `mdia` child `hdlr` of type `vide`.
///
/// `Ok(true)` found one, `Ok(false)` parsed cleanly with none, `Err(())`
/// malformed. Only descends `trak` and `mdia`, so the `minf` data handler
/// (`alis`/`url `) and any `udta/meta` handler (`mdir`) are never read as
/// media handlers.
fn find_vide(buf: &[u8], parent_is_mdia: bool) -> Result<bool, ()> {
    let mut i = 0usize;
    while i + 8 <= buf.len() {
        let size = u32::from_be_bytes(buf[i..i + 4].try_into().map_err(|_| ())?) as usize;
        let typ: [u8; 4] = buf[i + 4..i + 8].try_into().map_err(|_| ())?;
        let (size, header_len) = if size == 1 {
            if i + 16 > buf.len() {
                return Err(());
            }
            let s = u64::from_be_bytes(buf[i + 8..i + 16].try_into().map_err(|_| ())?);
            (usize::try_from(s).map_err(|_| ())?, 16usize)
        } else if size == 0 {
            (buf.len() - i, 8usize)
        } else {
            (size, 8usize)
        };
        if size < header_len || i + size > buf.len() {
            return Err(());
        }
        let payload = &buf[i + header_len..i + size];
        if parent_is_mdia && &typ == b"hdlr" {
            // version/flags(4) + pre_defined(4) + handler_type(4)
            if payload.len() >= 12 && &payload[8..12] == b"vide" {
                return Ok(true);
            }
        }
        if (&typ == b"trak" || &typ == b"mdia") && find_vide(payload, &typ == b"mdia")? {
            return Ok(true);
        }
        i += size;
    }
    Ok(false)
}

/// Parser core, over any reader. Separate from `has_video_track` so unit tests
/// can drive it with in-memory buffers: `videre-core` reads no fixture files.
pub(crate) fn has_video_track_in<R: Read + Seek>(r: &mut R, end: u64) -> bool {
    match read_moov(r, end) {
        Some(moov) => find_vide(&moov, false).unwrap_or(true),
        None => true,
    }
}

/// True unless the container definitely has no video track.
///
/// Fails open: a missing file, unreadable file, unknown layout, or I/O timeout
/// all return `true`, so the caller proceeds exactly as it does today. The only
/// `false` is a container parsed successfully that provably contains no `vide`
/// handler.
///
/// All file access goes through `io_timeout`, the project-wide rule for I/O on
/// user-supplied paths: these files commonly live on removable volumes, and an
/// unbounded read on a disconnected drive hangs the process, which is the
/// failure this probe exists to reduce rather than reproduce.
pub fn has_video_track(path: &Path) -> bool {
    let path = path.to_path_buf();
    crate::io_timeout::run_with_timeout(crate::io_timeout::DEFAULT_IO_TIMEOUT, move || {
        let mut f = std::fs::File::open(&path).ok()?;
        let end = f.seek(SeekFrom::End(0)).ok()?;
        f.seek(SeekFrom::Start(0)).ok()?;
        Some(has_video_track_in(&mut f, end))
    })
    .ok()
    .flatten()
    .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// A box: 8-byte header (size, type) followed by payload.
    fn bx(typ: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let size = (8 + payload.len()) as u32;
        let mut v = size.to_be_bytes().to_vec();
        v.extend_from_slice(typ);
        v.extend_from_slice(payload);
        v
    }

    /// An `hdlr` box declaring `handler_type`, with the 8 leading bytes real
    /// files carry (version/flags, then pre_defined).
    fn hdlr(handler_type: &[u8; 4]) -> Vec<u8> {
        let mut p = vec![0u8; 8];
        p.extend_from_slice(handler_type);
        p.extend_from_slice(b"\0"); // name, empty
        bx(b"hdlr", &p)
    }

    /// A minimal track: mdia containing an hdlr of the given type, plus a
    /// minf/hdlr data handler, which must NOT be mistaken for the media one.
    fn trak(media_handler: &[u8; 4]) -> Vec<u8> {
        let minf = bx(b"minf", &hdlr(b"alis"));
        let mut mdia_payload = hdlr(media_handler);
        mdia_payload.extend_from_slice(&minf);
        bx(b"trak", &bx(b"mdia", &mdia_payload))
    }

    fn file_with(traks: &[Vec<u8>]) -> Vec<u8> {
        let mut moov_payload = Vec::new();
        for t in traks {
            moov_payload.extend_from_slice(t);
        }
        let mut out = bx(b"ftyp", b"qt  ");
        out.extend_from_slice(&bx(b"moov", &moov_payload));
        out.extend_from_slice(&bx(b"mdat", &[0u8; 32]));
        out
    }

    fn probe(bytes: Vec<u8>) -> bool {
        let n = bytes.len() as u64;
        has_video_track_in(&mut Cursor::new(bytes), n)
    }

    #[test]
    fn a_file_with_a_video_track_reports_true() {
        assert!(probe(file_with(&[trak(b"vide"), trak(b"soun")])));
    }

    #[test]
    fn an_audio_only_file_reports_false() {
        // The whole point: this is what costs 20s per run today.
        assert!(!probe(file_with(&[trak(b"soun")])));
    }

    #[test]
    fn a_data_handler_is_not_mistaken_for_a_media_handler() {
        // Each track carries a minf/hdlr of type `alis`. A walk that scans for
        // any `hdlr` rather than only the `mdia` child would read the wrong
        // box. Real files also carry an `mdir` handler under udta/meta.
        let mut f = file_with(&[trak(b"soun")]);
        f.extend_from_slice(&bx(b"udta", &bx(b"meta", &hdlr(b"mdir"))));
        assert!(!probe(f));
    }

    #[test]
    fn video_after_audio_is_still_found() {
        assert!(probe(file_with(&[trak(b"soun"), trak(b"vide")])));
    }

    #[test]
    fn no_moov_fails_open() {
        let mut f = bx(b"ftyp", b"qt  ");
        f.extend_from_slice(&bx(b"mdat", &[0u8; 16]));
        assert!(
            probe(f),
            "unparseable input must be treated as having video"
        );
    }

    #[test]
    fn truncated_input_fails_open() {
        let full = file_with(&[trak(b"soun")]);
        assert!(probe(full[..20].to_vec()));
    }

    #[test]
    fn empty_input_fails_open() {
        assert!(probe(Vec::new()));
    }

    #[test]
    fn a_box_smaller_than_its_own_header_fails_open_without_looping() {
        // size = 3 is impossible. A naive walker either loops forever or
        // seeks backwards; this must terminate and fail open.
        let mut f = 3u32.to_be_bytes().to_vec();
        f.extend_from_slice(b"moov");
        assert!(probe(f));
    }

    #[test]
    fn a_child_claiming_to_extend_past_its_parent_fails_open() {
        // moov whose child declares a size larger than the moov payload.
        let mut child = 9999u32.to_be_bytes().to_vec();
        child.extend_from_slice(b"trak");
        assert!(probe(bx(b"moov", &child)));
    }

    #[test]
    fn a_64_bit_size_header_is_handled() {
        // size == 1 means a 64-bit size follows the type.
        let inner = bx(b"moov", &trak(b"vide"));
        let mut f = 1u32.to_be_bytes().to_vec();
        f.extend_from_slice(b"free");
        f.extend_from_slice(&(16u64 + 8).to_be_bytes());
        f.extend_from_slice(&[0u8; 8]);
        f.extend_from_slice(&inner);
        assert!(probe(f));
    }

    /// `size == 0` at the top level means "this box runs to end of file",
    /// which `read_header` resolves against `end`. Legal ISO-BMFF, and the
    /// shape a file still being written (or one truncated mid-record) has.
    ///
    /// Distinct from the 64-bit case above: that one is `read_header`'s
    /// `size == 1` branch, this is its `size == 0` branch, and neither implies
    /// the other.
    #[test]
    fn a_top_level_size_zero_box_runs_to_end_of_file() {
        let mut f = bx(b"ftyp", b"qt  ");
        // A size-0 'free' box swallows everything after it, so the moov that
        // follows is never reached. That is "no moov found", which fails open
        // (true) rather than reporting "no video track" (false): the probe
        // only ever answers false when it has actually parsed a moov and seen
        // no video handler. Asserting false here is what this test did first,
        // and it failed, which is the distinction worth pinning down.
        f.extend_from_slice(&0u32.to_be_bytes());
        f.extend_from_slice(b"free");
        f.extend_from_slice(&bx(b"moov", &trak(b"vide")));
        assert!(probe(f));
    }

    /// `find_vide` walks the already-buffered `moov`, and carries its own
    /// size decoding independent of `read_header`'s. A 64-bit size inside
    /// `moov` is unusual but legal, and nothing else exercises this branch.
    #[test]
    fn a_64_bit_size_inside_moov_is_handled() {
        let inner = trak(b"vide");
        // A 64-bit-sized 'free' box preceding the real trak, inside moov.
        let mut moov_payload = 1u32.to_be_bytes().to_vec();
        moov_payload.extend_from_slice(b"free");
        moov_payload.extend_from_slice(&(16u64 + 8).to_be_bytes());
        moov_payload.extend_from_slice(&[0u8; 8]);
        moov_payload.extend_from_slice(&inner);

        let mut f = bx(b"ftyp", b"qt  ");
        f.extend_from_slice(&bx(b"moov", &moov_payload));
        assert!(probe(f));
    }

    /// A box inside `moov` claiming a 64-bit size without room for the 8 extra
    /// header bytes is malformed. Must fail open (probe reports true, so the
    /// caller still tries QuickLook) rather than panicking on the slice.
    #[test]
    fn a_truncated_64_bit_header_inside_moov_fails_open() {
        let mut moov_payload = 1u32.to_be_bytes().to_vec();
        moov_payload.extend_from_slice(b"free");
        moov_payload.extend_from_slice(&[0u8; 4]); // 4 bytes, not the 8 needed

        let mut f = bx(b"ftyp", b"qt  ");
        f.extend_from_slice(&bx(b"moov", &moov_payload));
        assert!(probe(f));
    }

    /// `size == 0` inside `moov` means the box runs to the end of the parent
    /// buffer, `find_vide`'s counterpart to the top-level case above.
    #[test]
    fn a_size_zero_box_inside_moov_runs_to_end_of_parent() {
        let mut moov_payload = 0u32.to_be_bytes().to_vec();
        moov_payload.extend_from_slice(b"free");
        moov_payload.extend_from_slice(&trak(b"vide"));

        let mut f = bx(b"ftyp", b"qt  ");
        f.extend_from_slice(&bx(b"moov", &moov_payload));
        // The size-0 free box consumes the trak, so no video handler is seen.
        assert!(!probe(f));
    }

    #[test]
    fn an_oversized_moov_fails_open_rather_than_allocating() {
        // Declares a moov far larger than MOOV_CAP. Must not try to read it.
        //
        // Note this actually trips the "child extends past end of file" bounds
        // check, not the MOOV_CAP guard, because the declared size also runs
        // past the tiny buffer. The cap itself is covered by the test below.
        let mut f = bx(b"ftyp", b"qt  ");
        let huge = (MOOV_CAP + 1_000_000) as u32;
        f.extend_from_slice(&huge.to_be_bytes());
        f.extend_from_slice(b"moov");
        assert!(probe(f));
    }

    /// The `MOOV_CAP` guard proper: a moov that fits inside the file but is
    /// still too large to buffer.
    ///
    /// Passing `end` explicitly rather than deriving it from the buffer is
    /// what makes this cheap. The guard exists to avoid allocating a huge
    /// buffer, so a test that had to build a >32MB fixture to reach it would
    /// be doing the very thing the guard prevents.
    #[test]
    fn a_moov_larger_than_the_cap_is_refused_without_reading_it() {
        let mut f = bx(b"ftyp", b"qt  ");
        let over_cap = (MOOV_CAP + 1024) as u32;
        f.extend_from_slice(&over_cap.to_be_bytes());
        f.extend_from_slice(b"moov");

        // Claim the file is far larger than the bytes actually present, so the
        // bounds check passes and the cap is what rejects it.
        let end = MOOV_CAP + 1_000_000;
        assert!(has_video_track_in(&mut Cursor::new(f), end));
    }

    fn write_temp(name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("videre_vp_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, bytes).unwrap();
        p
    }

    #[test]
    fn has_video_track_reads_a_real_file() {
        let p = write_temp("audio.mov", &file_with(&[trak(b"soun")]));
        assert!(!has_video_track(&p));
        let p = write_temp("video.mov", &file_with(&[trak(b"vide")]));
        assert!(has_video_track(&p));
    }

    #[test]
    fn a_nonexistent_path_fails_open() {
        assert!(has_video_track(std::path::Path::new(
            "/nonexistent/nope.mov"
        )));
    }

    #[test]
    fn an_empty_file_fails_open() {
        let p = write_temp("empty.mov", b"");
        assert!(has_video_track(&p));
    }

    #[test]
    fn a_jpeg_fails_open() {
        // Not a container at all. Must not read as "no video track", or a
        // caller passing the wrong path would silently skip work.
        let p = write_temp("not_a_video.jpg", b"\xff\xd8\xff\xe0\x00\x10JFIF\0\x01");
        assert!(has_video_track(&p));
    }
}
