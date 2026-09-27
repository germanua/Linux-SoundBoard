use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;

use super::UpdateError;

pub const API_BODY_LIMIT: u64 = 2 * 1024 * 1024;
pub const METADATA_BODY_LIMIT: u64 = 1024 * 1024;
pub const ARTIFACT_BODY_LIMIT: u64 = 256 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DownloadProgress {
    pub downloaded: u64,
    pub total: u64,
}

#[derive(Debug, Clone)]
pub struct HttpResponse {
    pub status: u16,
    pub etag: Option<String>,
    pub body: Vec<u8>,
}

pub trait Transport {
    fn get(&self, url: &str, etag: Option<&str>, limit: u64) -> Result<HttpResponse, UpdateError>;
}

fn agent(timeout: Duration) -> ureq::Agent {
    use ureq::tls::{RootCerts, TlsConfig};

    ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .https_only(true)
        .user_agent(format!("Linux-Soundboard/{}", crate::app_meta::APP_VERSION))
        .tls_config(
            TlsConfig::builder()
                .root_certs(RootCerts::PlatformVerifier)
                .unversioned_rustls_crypto_provider(Arc::new(
                    rustls::crypto::ring::default_provider(),
                ))
                .build(),
        )
        .build()
        .new_agent()
}

#[derive(Debug, Clone, Copy, Default)]
pub struct HttpTransport;

fn is_github_api(url: &str) -> bool {
    url.starts_with("https://api.github.com/")
}

fn is_release_asset_api(url: &str) -> bool {
    is_github_api(url) && url.contains("/releases/assets/")
}

impl Transport for HttpTransport {
    fn get(&self, url: &str, etag: Option<&str>, limit: u64) -> Result<HttpResponse, UpdateError> {
        if !url.starts_with("https://") {
            return Err(UpdateError::Network("update URL is not HTTPS".into()));
        }
        let agent = agent(Duration::from_secs(30));
        let accept = if is_release_asset_api(url) {
            "application/octet-stream"
        } else {
            "application/vnd.github+json"
        };
        let mut request = agent.get(url).header("Accept", accept);
        if is_github_api(url) {
            request = request.header("X-GitHub-Api-Version", "2022-11-28");
        }
        if let Some(value) = etag {
            request = request.header("If-None-Match", value);
        }
        let mut response = match request.call() {
            Ok(response) => response,
            Err(ureq::Error::StatusCode(304)) => {
                return Ok(HttpResponse {
                    status: 304,
                    etag: etag.map(str::to_string),
                    body: Vec::new(),
                });
            }
            Err(error) => return Err(UpdateError::Network(error.to_string())),
        };
        let status = response.status().as_u16();
        let response_etag = response
            .headers()
            .get("etag")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let body = response
            .body_mut()
            .with_config()
            .limit(limit)
            .read_to_vec()
            .map_err(|error| UpdateError::Network(error.to_string()))?;
        Ok(HttpResponse {
            status,
            etag: response_etag,
            body,
        })
    }
}

fn stream_artifact<R, W, F>(
    mut reader: R,
    expected_size: u64,
    output: &mut W,
    cancelled: &AtomicBool,
    on_progress: &F,
) -> Result<(u64, String), UpdateError>
where
    R: std::io::Read,
    W: std::io::Write,
    F: Fn(DownloadProgress),
{
    use sha2::{Digest, Sha256};

    if cancelled.load(Ordering::Relaxed) {
        return Err(UpdateError::Cancelled);
    }
    on_progress(DownloadProgress {
        downloaded: 0,
        total: expected_size,
    });
    let mut hasher = Sha256::new();
    let mut total = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        if cancelled.load(Ordering::Relaxed) {
            return Err(UpdateError::Cancelled);
        }
        let read = reader
            .read(&mut buffer)
            .map_err(|error| UpdateError::Network(error.to_string()))?;
        if read == 0 {
            break;
        }
        total = total.saturating_add(read as u64);
        if total > expected_size {
            return Err(UpdateError::Verification(
                "artifact exceeded signed size".into(),
            ));
        }
        output
            .write_all(&buffer[..read])
            .map_err(|error| UpdateError::State(error.to_string()))?;
        hasher.update(&buffer[..read]);
        on_progress(DownloadProgress {
            downloaded: total,
            total: expected_size,
        });
    }
    if cancelled.load(Ordering::Relaxed) {
        return Err(UpdateError::Cancelled);
    }
    if total != expected_size {
        return Err(UpdateError::Verification(format!(
            "artifact is {total} bytes, expected {expected_size}"
        )));
    }
    output
        .flush()
        .map_err(|error| UpdateError::State(error.to_string()))?;
    Ok((total, format!("{:x}", hasher.finalize())))
}

pub fn download_artifact<F>(
    url: &str,
    expected_size: u64,
    output: &mut std::fs::File,
    cancelled: &AtomicBool,
    on_progress: &F,
) -> Result<(u64, String), UpdateError>
where
    F: Fn(DownloadProgress),
{
    if !url.starts_with("https://") {
        return Err(UpdateError::Network("update URL is not HTTPS".into()));
    }
    if expected_size == 0 || expected_size > ARTIFACT_BODY_LIMIT {
        return Err(UpdateError::Metadata("invalid artifact size".into()));
    }
    if cancelled.load(Ordering::Relaxed) {
        return Err(UpdateError::Cancelled);
    }
    let agent = agent(Duration::from_secs(120));
    let mut request = agent.get(url).header("Accept", "application/octet-stream");
    if is_github_api(url) {
        request = request.header("X-GitHub-Api-Version", "2022-11-28");
    }
    let mut response = request
        .call()
        .map_err(|error| UpdateError::Network(error.to_string()))?;
    if response.status().as_u16() != 200 {
        return Err(UpdateError::Network(format!(
            "artifact download returned HTTP {}",
            response.status()
        )));
    }
    if let Some(length) = response
        .headers()
        .get("content-length")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
    {
        if length != expected_size {
            return Err(UpdateError::Verification(format!(
                "artifact Content-Length is {length}, expected {expected_size}"
            )));
        }
    }

    let reader = response
        .body_mut()
        .with_config()
        .limit(expected_size.saturating_add(1))
        .reader();
    let result = stream_artifact(reader, expected_size, output, cancelled, on_progress)?;
    output
        .sync_all()
        .map_err(|error| UpdateError::State(error.to_string()))?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::Digest;
    use std::sync::Mutex;

    #[test]
    fn stream_artifact_reports_byte_progress() {
        let data = vec![0x5a; 150_000];
        let mut output = Vec::new();
        let cancelled = AtomicBool::new(false);
        let progress = Mutex::new(Vec::new());
        let (_, hash) = stream_artifact(
            std::io::Cursor::new(&data),
            data.len() as u64,
            &mut output,
            &cancelled,
            &|value| progress.lock().unwrap().push(value),
        )
        .unwrap();
        assert_eq!(output, data);
        assert_eq!(hash, format!("{:x}", sha2::Sha256::digest(&output)));
        let progress = progress.into_inner().unwrap();
        assert_eq!(progress.first().unwrap().downloaded, 0);
        assert_eq!(progress.last().unwrap().downloaded, output.len() as u64);
        assert_eq!(progress.last().unwrap().total, output.len() as u64);
    }

    #[test]
    fn stream_artifact_stops_when_cancelled() {
        let data = vec![0x33; 180_000];
        let mut output = Vec::new();
        let cancelled = AtomicBool::new(false);
        let result = stream_artifact(
            std::io::Cursor::new(&data),
            data.len() as u64,
            &mut output,
            &cancelled,
            &|value| {
                if value.downloaded > 0 {
                    cancelled.store(true, Ordering::Relaxed);
                }
            },
        );
        assert!(matches!(result, Err(UpdateError::Cancelled)));
        assert!(!output.is_empty());
        assert!(output.len() < data.len());
    }
}
