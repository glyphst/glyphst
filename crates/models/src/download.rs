//! A download that survives a dropped connection and the app quitting:
//! bytes go to `<file>.partial`, with `<file>.partial.json` naming what
//! they are, and a later run asks for the rest (`Range`). The SHA-256 is
//! checked before the file takes its name; a mismatch starts over.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read as _, Write as _},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::manifest::Asset;

/// How a download stands, for the progress bar.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Progress {
    pub done: u64,
    pub total: u64,
    /// Bytes a second, over the last few seconds.
    pub rate: f64,
}

impl Progress {
    pub fn seconds_left(&self) -> Option<u64> {
        (self.rate > 1.0).then(|| (self.total.saturating_sub(self.done) as f64 / self.rate) as u64)
    }
}

/// Why a download stopped; each shows its own words and action.
#[derive(Debug, PartialEq, Eq)]
pub enum DownloadError {
    Cancelled,
    /// The connection failed; what came is kept.
    Network(String),
    /// The server answered something other than the file.
    Http(u16),
    DiskFull,
    Disk(String),
    /// The whole file arrived but is not the one pinned; it was deleted.
    Checksum,
}

impl std::fmt::Display for DownloadError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled => formatter.write_str("stopped"),
            Self::Network(error) => write!(formatter, "network lost: {error}"),
            Self::Http(status) => write!(formatter, "the server answered {status}"),
            Self::DiskFull => formatter.write_str("the disk is full"),
            Self::Disk(error) => write!(formatter, "could not write the file: {error}"),
            Self::Checksum => formatter.write_str("the file did not match its checksum"),
        }
    }
}

impl std::error::Error for DownloadError {}

/// What a partial file holds.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Sidecar {
    url: String,
    sha256: String,
    size: u64,
}

fn partial_path(target: &Path) -> PathBuf {
    with_suffix(target, ".partial")
}

fn sidecar_path(target: &Path) -> PathBuf {
    with_suffix(target, ".partial.json")
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// Bytes already fetched toward `target`, if a partial of this asset is
/// there.
pub fn resumable(target: &Path, asset: &Asset) -> Option<u64> {
    let sidecar = fs::read(sidecar_path(target)).ok()?;
    let sidecar: Sidecar = serde_json::from_slice(&sidecar).ok()?;
    if sidecar.url != asset.url || sidecar.sha256 != asset.sha256 {
        return None;
    }
    fs::metadata(partial_path(target))
        .ok()
        .map(|metadata| metadata.len())
}

/// Drops a partial download of `target`.
pub fn discard(target: &Path) {
    let _ = fs::remove_file(partial_path(target));
    let _ = fs::remove_file(sidecar_path(target));
}

/// Fetches `asset` to `target`, resuming a partial one, reporting progress
/// about ten times a second, and stopping when `cancel` is set.
pub fn fetch(
    asset: &Asset,
    target: &Path,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(Progress),
) -> Result<(), DownloadError> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(disk)?;
    }
    let partial = partial_path(target);
    if resumable(target, asset).is_none() {
        discard(target);
        let sidecar = Sidecar {
            url: asset.url.to_string(),
            sha256: asset.sha256.to_string(),
            size: asset.size,
        };
        fs::write(
            sidecar_path(target),
            serde_json::to_vec(&sidecar).map_err(|error| DownloadError::Disk(error.to_string()))?,
        )
        .map_err(disk)?;
        File::create(&partial).map_err(disk)?;
    }
    // What came before counts toward the hash.
    let mut hasher = Sha256::new();
    let mut have = 0;
    {
        let mut file = File::open(&partial).map_err(disk)?;
        let mut buffer = vec![0; 1 << 20];
        loop {
            if cancel.load(Ordering::Acquire) {
                return Err(DownloadError::Cancelled);
            }
            let read = file.read(&mut buffer).map_err(disk)?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
            have += read as u64;
        }
    }
    if have < asset.size {
        let client = reqwest::blocking::Client::builder()
            .user_agent(concat!("GlyphstReader/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(30))
            // A 300 MB file takes as long as it takes; a dead peer is
            // found by keepalive.
            .timeout(None)
            .tcp_keepalive(Duration::from_secs(20))
            .build()
            .map_err(|error| DownloadError::Network(error.to_string()))?;
        let mut request = client.get(asset.url);
        if have > 0 {
            request = request.header("Range", format!("bytes={have}-"));
        }
        let mut response = request
            .send()
            .map_err(|error| DownloadError::Network(error.to_string()))?;
        let status = response.status().as_u16();
        let mut file = match status {
            206 => OpenOptions::new()
                .append(true)
                .open(&partial)
                .map_err(disk)?,
            // The whole file again: start over.
            200 => {
                hasher = Sha256::new();
                have = 0;
                File::create(&partial).map_err(disk)?
            }
            status => return Err(DownloadError::Http(status)),
        };
        let mut buffer = vec![0; 64 << 10];
        let mut window = (Instant::now(), have);
        let mut rate = 0.0;
        let mut last_report = Instant::now();
        progress(Progress {
            done: have,
            total: asset.size,
            rate,
        });
        loop {
            if cancel.load(Ordering::Acquire) {
                let _ = file.flush();
                return Err(DownloadError::Cancelled);
            }
            let read = match response.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => read,
                Err(error) => {
                    let _ = file.flush();
                    return Err(DownloadError::Network(error.to_string()));
                }
            };
            file.write_all(&buffer[..read]).map_err(disk)?;
            hasher.update(&buffer[..read]);
            have += read as u64;
            let elapsed = window.0.elapsed();
            if elapsed >= Duration::from_secs(2) {
                rate = (have - window.1) as f64 / elapsed.as_secs_f64();
                window = (Instant::now(), have);
            } else if rate == 0.0 && elapsed >= Duration::from_millis(300) {
                rate = (have - window.1) as f64 / elapsed.as_secs_f64();
            }
            if last_report.elapsed() >= Duration::from_millis(100) {
                last_report = Instant::now();
                progress(Progress {
                    done: have,
                    total: asset.size,
                    rate,
                });
            }
        }
        file.flush().map_err(disk)?;
        if have < asset.size {
            return Err(DownloadError::Network(
                "the connection closed early".to_string(),
            ));
        }
    }
    let digest = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    if have != asset.size || digest != asset.sha256 {
        discard(target);
        return Err(DownloadError::Checksum);
    }
    fs::rename(&partial, target).map_err(disk)?;
    let _ = fs::remove_file(sidecar_path(target));
    progress(Progress {
        done: have,
        total: asset.size,
        rate: 0.0,
    });
    Ok(())
}

fn disk(error: io::Error) -> DownloadError {
    // ENOSPC on Unix, ERROR_DISK_FULL / ERROR_HANDLE_DISK_FULL on Windows.
    if error.kind() == io::ErrorKind::StorageFull
        || matches!(error.raw_os_error(), Some(28) | Some(39) | Some(112))
    {
        DownloadError::DiskFull
    } else {
        DownloadError::Disk(error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{Arc, Mutex},
        thread,
    };

    use super::*;

    /// Serves `body`, honouring `Range` unless `ranges` is false; the first
    /// `drop_after` bytes of the first answer arrive, then the connection
    /// closes.
    fn server(
        body: Vec<u8>,
        ranges: bool,
        drop_after: Option<usize>,
    ) -> (String, Arc<Mutex<Vec<String>>>) {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let address = format!("http://{}", server.server_addr().to_ip().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        thread::spawn(move || {
            let mut first = true;
            for request in server.incoming_requests() {
                let range = request
                    .headers()
                    .iter()
                    .find(|header| header.field.equiv("Range"))
                    .map(|header| header.value.to_string());
                log.lock().unwrap().push(range.clone().unwrap_or_default());
                let start = range.filter(|_| ranges).and_then(|range| {
                    range
                        .strip_prefix("bytes=")?
                        .trim_end_matches('-')
                        .parse::<usize>()
                        .ok()
                });
                let (status, slice) = match start {
                    Some(start) => (206, body[start..].to_vec()),
                    None => (200, body.clone()),
                };
                let slice = match drop_after.filter(|_| first) {
                    Some(cut) => slice[..cut.min(slice.len())].to_vec(),
                    None => slice,
                };
                first = false;
                // The length says the whole, so a cut answer ends early.
                let length = if status == 206 {
                    body.len() - start.unwrap_or(0)
                } else {
                    body.len()
                };
                let response = tiny_http::Response::new(
                    tiny_http::StatusCode(status),
                    Vec::new(),
                    std::io::Cursor::new(slice),
                    Some(length),
                    None,
                );
                let _ = request.respond(response);
            }
        });
        (address, seen)
    }

    fn asset(url: &str, body: &[u8]) -> Asset {
        let sha = Sha256::digest(body)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        Asset {
            name: "model.gguf",
            url: Box::leak(url.to_string().into_boxed_str()),
            sha256: Box::leak(sha.into_boxed_str()),
            size: body.len() as u64,
        }
    }

    fn folder(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("glyphst-embed-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_dropped_download_resumes_where_it_stopped_and_checks_its_hash() {
        let body = (0..200_000u32)
            .flat_map(|n| n.to_le_bytes())
            .collect::<Vec<u8>>();
        let (url, seen) = server(body.clone(), true, Some(300_000));
        let asset = asset(&format!("{url}/model.gguf"), &body);
        let dir = folder("resume");
        let target = dir.join("model.gguf");
        let cancel = AtomicBool::new(false);
        let mut last = Progress::default();
        let error = fetch(&asset, &target, &cancel, &mut |p| last = p).unwrap_err();
        assert!(matches!(error, DownloadError::Network(_)), "{error:?}");
        let kept = resumable(&target, &asset).unwrap();
        assert!(kept > 0 && kept < body.len() as u64, "{kept}");
        assert!(!target.exists());

        // As after a restart: the rest is asked for and the file checks out.
        fetch(&asset, &target, &cancel, &mut |p| last = p).unwrap();
        assert_eq!(fs::read(&target).unwrap(), body);
        assert_eq!(last.done, body.len() as u64);
        assert_eq!(seen.lock().unwrap()[1], format!("bytes={kept}-"));
        assert!(resumable(&target, &asset).is_none(), "the partial is gone");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_server_without_ranges_starts_over_and_a_wrong_file_is_refused() {
        let body = vec![7u8; 100_000];
        let (url, _) = server(body.clone(), false, Some(40_000));
        let asset = asset(&format!("{url}/model.gguf"), &body);
        let dir = folder("whole");
        let target = dir.join("model.gguf");
        let cancel = AtomicBool::new(false);
        assert!(fetch(&asset, &target, &cancel, &mut |_| {}).is_err());
        fetch(&asset, &target, &cancel, &mut |_| {}).unwrap();
        assert_eq!(fs::read(&target).unwrap(), body, "200 restarts from zero");

        let other = dir.join("other.gguf");
        let wrong = Asset {
            sha256: "0000000000000000000000000000000000000000000000000000000000000000",
            ..asset
        };
        assert_eq!(
            fetch(&wrong, &other, &cancel, &mut |_| {}),
            Err(DownloadError::Checksum)
        );
        assert!(
            !other.exists() && resumable(&other, &wrong).is_none(),
            "nothing kept"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn cancelling_keeps_what_came_and_another_file_discards_it() {
        let body = vec![1u8; 500_000];
        let (url, _) = server(body.clone(), true, None);
        let asset = asset(&format!("{url}/model.gguf"), &body);
        let dir = folder("cancel");
        let target = dir.join("model.gguf");
        let cancel = AtomicBool::new(false);
        let mut calls = 0;
        let error = fetch(&asset, &target, &cancel, &mut |_| {
            calls += 1;
            cancel.store(true, Ordering::Release);
        })
        .unwrap_err();
        assert_eq!(error, DownloadError::Cancelled);
        assert!(calls >= 1);
        assert!(resumable(&target, &asset).is_some());
        // A different pin: the partial is not resumed.
        let newer = Asset {
            sha256: "1111111111111111111111111111111111111111111111111111111111111111",
            ..asset
        };
        assert!(resumable(&target, &newer).is_none());
        let _ = fs::remove_dir_all(dir);
    }
}
