//! The offline basemap archive: one PMTiles file per machine, downloaded
//! once on first /map use into the shared geo cache, then served to the
//! gallery client by a Range-capable endpoint. View time never touches the
//! network.

use anyhow::Context;
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};

/// The pinned basemap build: a world vector archive, zooms 0 through 8,
/// extracted once from a dated Protomaps build (the documented command is
/// `pmtiles extract <build-url> world-z0-8.pmtiles --maxzoom=8`) and
/// attached as a release asset on this repository. Built by release
/// engineering, not at runtime.
pub const BASEMAP_URL: &str =
    "https://github.com/erhangundogan/videre/releases/download/basemap-v1/world-z0-8.pmtiles";

/// The archive path for one library: the per-library override when it
/// exists (how tests and a future config seed a basemap without the
/// network), else the machine-shared archive under the geo cache.
pub fn archive_path(geo: &Path, state_dir: &Path) -> PathBuf {
    let override_path = state_dir.join("basemap").join("basemap.pmtiles");
    if override_path.exists() {
        return override_path;
    }
    geo.join("basemap").join("basemap.pmtiles")
}

/// The download state of the archive. `Partial` means a `.part` sibling
/// exists from an interrupted download and the next attempt resumes it.
#[derive(Debug)]
pub enum BasemapStatus {
    Absent,
    Partial { bytes: u64 },
    Ready { bytes: u64 },
}

pub fn status(path: &Path) -> BasemapStatus {
    let part = path.with_extension("pmtiles.part");
    if let Ok(meta) = std::fs::metadata(&part) {
        return BasemapStatus::Partial { bytes: meta.len() };
    }
    match std::fs::metadata(path) {
        Ok(meta) => BasemapStatus::Ready { bytes: meta.len() },
        Err(_) => BasemapStatus::Absent,
    }
}

/// Download the basemap archive once per machine, resuming an interrupted
/// `.part` file, verifying the length, and publishing atomically by rename.
/// Returns the archive path. `progress` receives (bytes done, bytes total).
pub fn ensure_downloaded(
    geo: &Path,
    state_dir: &Path,
    progress: impl FnMut(u64, u64),
) -> anyhow::Result<PathBuf> {
    ensure_downloaded_with_url(geo, state_dir, BASEMAP_URL, progress)
}

fn ensure_downloaded_with_url(
    geo: &Path,
    state_dir: &Path,
    url: &str,
    mut progress: impl FnMut(u64, u64),
) -> anyhow::Result<PathBuf> {
    let path = archive_path(geo, state_dir);
    if let BasemapStatus::Ready { bytes } = status(&path) {
        progress(bytes, bytes);
        return Ok(path);
    }
    std::fs::create_dir_all(path.parent().context("basemap parent directory")?)?;
    let part = path.with_extension("pmtiles.part");
    let partial = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);

    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(600)))
        .build()
        .new_agent();
    let response = agent
        .get(url)
        .header("Range", &format!("bytes={partial}-"))
        .call()
        .with_context(|| format!("downloading the basemap from {url}"))?;
    // A server that ignored the Range (status 200) restarts from scratch.
    let resume = response.status() == 206 && partial > 0;
    if !resume {
        std::fs::remove_file(&part).ok();
    }
    let total: u64 = response
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .map(|len: u64| len + if resume { partial } else { 0 })
        .with_context(|| format!("the basemap response carried no content-length ({url})"))?;

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(!resume)
        .open(&part)
        .with_context(|| format!("opening {}", part.display()))?;
    if resume {
        file.seek(std::io::SeekFrom::End(0))?;
    }

    // into_reader consumes the response body.
    let mut reader = response.into_body().into_reader();
    let mut buffer = [0u8; 64 * 1024];
    let mut done = if resume { partial } else { 0 };
    loop {
        let n = reader
            .read(&mut buffer)
            .with_context(|| format!("reading the basemap stream after {done} bytes"))?;
        if n == 0 {
            break;
        }
        file.write_all(&buffer[..n])?;
        done += n as u64;
        progress(done, done.max(total));
    }
    if done < total {
        anyhow::bail!("the basemap download ended early: {done} of {total} bytes");
    }
    file.sync_all()?;
    drop(file);
    std::fs::rename(&part, &path).with_context(|| format!("publishing {}", path.display()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    /// A throwaway HTTP server answering one GET. The handler receives the
    /// request's Range header (lowercased) and returns status plus body.
    fn serve_once<F>(mut handler: F) -> String
    where
        F: FnMut(Option<String>) -> (u16, Vec<u8>) + Send + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            let request = String::from_utf8_lossy(&buf).to_string();
            let range = request
                .to_ascii_lowercase()
                .lines()
                .find(|l| l.starts_with("range:"))
                .map(|l| l.trim_start_matches("range:").trim().to_string());
            let (status, body) = handler(range);
            let head = format!(
                "HTTP/1.1 {status} IGNORED\r\ncontent-length: {}\r\ncontent-type: application/octet-stream\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&body);
        });
        format!("http://{addr}/basemap.pmtiles")
    }

    fn serve_body(body: Vec<u8>) -> String {
        let mut body = Some(body);
        serve_once(move |_range| {
            let body = body.take().unwrap_or_default();
            (200, body)
        })
    }

    fn serve_honoring_range(body: Vec<u8>) -> String {
        let mut body = Some(body);
        serve_once(move |range: Option<String>| {
            let body = body.take().unwrap_or_default();
            match range {
                Some(spec) => {
                    let start: usize = spec
                        .strip_prefix("bytes=")
                        .and_then(|s| s.split('-').next())
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(0);
                    (206, body[start..].to_vec())
                }
                None => (200, body),
            }
        })
    }

    #[test]
    fn status_reports_absent_partial_and_ready() {
        let dir = tempfile::tempdir().unwrap();
        let geo = dir.path().join("geo");
        let state = dir.path().join("state");
        let path = archive_path(&geo, &state);
        assert!(matches!(status(&path), BasemapStatus::Absent));

        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"half").unwrap();
        assert!(matches!(status(&path), BasemapStatus::Ready { bytes: 4 }));

        let part = path.with_extension("pmtiles.part");
        std::fs::write(&part, b"12").unwrap();
        std::fs::remove_file(&path).unwrap();
        match status(&path) {
            BasemapStatus::Partial { bytes } => assert_eq!(bytes, 2),
            other => panic!("expected partial, got {other:?}"),
        }
    }

    #[test]
    fn archive_path_prefers_an_existing_per_library_override() {
        let dir = tempfile::tempdir().unwrap();
        let geo = dir.path().join("geo");
        let state = dir.path().join("state");
        // No override on disk: the shared archive wins.
        let shared = archive_path(&geo, &state);
        assert!(shared.starts_with(&geo));

        // The override exists under the library state dir: it wins.
        let override_path = state.join("basemap").join("basemap.pmtiles");
        std::fs::create_dir_all(override_path.parent().unwrap()).unwrap();
        std::fs::write(&override_path, b"x").unwrap();
        let overridden = archive_path(&geo, &state);
        assert_eq!(
            overridden, override_path,
            "an existing override must win over the shared archive"
        );
    }

    #[test]
    fn ensure_downloaded_fetches_verifies_and_publishes_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let (geo, state) = (dir.path().join("geo"), dir.path().join("state"));
        let url = serve_body(b"tilebytes".to_vec());
        let path = ensure_downloaded_with_url(&geo, &state, &url, |_, _| {}).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"tilebytes");
        assert!(matches!(status(&path), BasemapStatus::Ready { bytes: 9 }));
    }

    #[test]
    fn ensure_downloaded_resumes_a_partial_file() {
        let dir = tempfile::tempdir().unwrap();
        let (geo, state) = (dir.path().join("geo"), dir.path().join("state"));
        let path = archive_path(&geo, &state);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // The first four bytes already sit in the .part file; the server
        // serves the full body and honors the Range, so the resume must
        // append the tail and publish the complete archive.
        std::fs::write(path.with_extension("pmtiles.part"), b"tile").unwrap();
        let url = serve_honoring_range(b"tilebytes".to_vec());
        let path = ensure_downloaded_with_url(&geo, &state, &url, |_, _| {}).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"tilebytes");
    }
}
