//! GitHub Releases metadata client, release selector, and loopback HTTP/1.1 client.
//!
//! Port of `HttpChecker` from `Telegram/SourceFiles/core/update_checker.cpp`,
//! adapted to GitHub Releases JSON with a mockable [`HttpClient`] trait.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use serde::Deserialize;

use crate::UpdateError;
use crate::version::{Target, UpdateChannel, Version, channel_allows_update};

/// Default GitHub REST API base URL.
pub const DEFAULT_GITHUB_API_BASE: &str = "https://api.github.com";

/// Maximum metadata JSON response size (1 MiB), matching `kMaxResponseSize` in `update_checker.cpp`.
pub const MAX_METADATA_SIZE: usize = 1024 * 1024;

/// Maximum downloadable update package size (256 MiB), matching `kMaxPayloadSize` in `update_verify.h`.
pub const MAX_PACKAGE_SIZE: usize = 256 * 1024 * 1024;

/// File extension of signed Rustgram update packages.
pub const UPDATE_PACKAGE_EXT: &str = ".tupdate";

/// Filename of the optional SHA-256 checksums manifest attached to a release.
pub const SHA256SUMS_ASSET_NAME: &str = "SHA256SUMS.txt";

/// Minimal HTTP transport abstraction so update checking and downloading can be tested offline
/// or against an in-process HTTP server.
pub trait HttpClient: Send + Sync {
    /// Performs an HTTP GET request to `url` with `headers`, returning at most `max_bytes`.
    fn get_bytes(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        max_bytes: usize,
    ) -> Result<Vec<u8>, UpdateError>;
}

/// One asset attached to a GitHub Release.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct GitHubAsset {
    /// Asset filename, e.g. `rustgram-0.2.0-win-x64.tupdate`.
    pub name: String,
    /// Direct download URL.
    pub browser_download_url: String,
    /// Declared size in bytes.
    #[serde(default)]
    pub size: u64,
    /// MIME content type, if provided.
    #[serde(default)]
    pub content_type: Option<String>,
}

/// One release object from the GitHub Releases REST API (`/repos/{owner}/{repo}/releases`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct GitHubRelease {
    /// Git tag name, e.g. `v0.2.0` or `v0.2.0-beta.1`.
    pub tag_name: String,
    /// Human-readable release title.
    #[serde(default)]
    pub name: Option<String>,
    /// Draft releases are always ignored.
    #[serde(default)]
    pub draft: bool,
    /// Prerelease flag set by GitHub.
    #[serde(default)]
    pub prerelease: bool,
    /// Optional release notes body.
    #[serde(default)]
    pub body: Option<String>,
    /// Attached binary assets.
    #[serde(default)]
    pub assets: Vec<GitHubAsset>,
}

/// Selected update candidate ready to be downloaded and verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvailableUpdate {
    /// Parsed SemVer of the candidate release.
    pub version: Version,
    /// Release channel (`Stable` or `Beta`).
    pub channel: UpdateChannel,
    /// Release title or tag name.
    pub title: String,
    /// Optional release notes markdown.
    pub release_notes: Option<String>,
    /// Filename of the matched `.tupdate` asset.
    pub asset_name: String,
    /// Direct download URL of the matched asset.
    pub download_url: String,
    /// Declared size in bytes.
    pub size: u64,
    /// Optional expected SHA-256 digest of the `.tupdate` asset (if embedded in release notes).
    pub expected_sha256: Option<[u8; 32]>,
    /// Optional download URL for `SHA256SUMS.txt` attached to the release.
    pub sha256_sums_url: Option<String>,
}

/// Returns the GitHub API URL for listing releases of `repo` (e.g. `"rustgram/rustgram"`).
#[must_use]
pub fn releases_api_url(repo: &str) -> String {
    releases_api_url_with_base(DEFAULT_GITHUB_API_BASE, repo)
}

/// Returns the releases listing URL for `repo` against a custom `api_base`
/// (e.g. a local fake HTTP server `http://127.0.0.1:12345`).
#[must_use]
pub fn releases_api_url_with_base(api_base: &str, repo: &str) -> String {
    let base = api_base.trim().trim_end_matches('/');
    let clean = repo.trim().trim_matches('/');
    format!("{base}/repos/{clean}/releases?per_page=20")
}

/// Parses a `SHA256SUMS.txt` document (`<64-hex>  <filename>`) and returns the digest for `asset_name`.
#[must_use]
pub fn parse_sha256_sums(text: &str, asset_name: &str) -> Option<[u8; 32]> {
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let mut parts = trimmed.split_whitespace();
        let Some(hex_part) = parts.next() else {
            continue;
        };
        let Some(name_part) = parts.next() else {
            continue;
        };
        let clean_name = name_part.trim_start_matches('*');
        if clean_name.eq_ignore_ascii_case(asset_name)
            && let Some(digest) = decode_sha256_hex(hex_part)
        {
            return Some(digest);
        }
    }
    None
}

/// Decodes a 64-character hex string into a 32-byte SHA-256 digest.
#[must_use]
pub fn decode_sha256_hex(hex_str: &str) -> Option<[u8; 32]> {
    let trimmed = hex_str.trim();
    if trimmed.len() != 64 {
        return None;
    }
    let bytes = hex::decode(trimmed).ok()?;
    bytes.try_into().ok()
}

/// Extracts an optional inline SHA-256 digest for `asset_name` from release notes if formatted
/// like `SHA256SUMS.txt` lines (`<64-hex>  <asset_name>`).
fn extract_inline_sha256(body: Option<&str>, asset_name: &str) -> Option<[u8; 32]> {
    let body = body?;
    parse_sha256_sums(body, asset_name)
}

/// Parses a GitHub Releases JSON array (or a single release JSON object as returned by
/// `/releases/latest`) into a list of [`GitHubRelease`] entries.
pub fn parse_releases_json(json: &[u8]) -> Result<Vec<GitHubRelease>, UpdateError> {
    if json.len() > MAX_METADATA_SIZE {
        return Err(UpdateError::ResponseTooLarge {
            limit: MAX_METADATA_SIZE,
        });
    }
    let value: serde_json::Value =
        serde_json::from_slice(json).map_err(|e| UpdateError::InvalidReleaseJson(e.to_string()))?;

    match value {
        serde_json::Value::Array(_) => serde_json::from_value::<Vec<GitHubRelease>>(value)
            .map_err(|e| UpdateError::InvalidReleaseJson(e.to_string())),
        serde_json::Value::Object(_) => {
            let single: GitHubRelease = serde_json::from_value(value)
                .map_err(|e| UpdateError::InvalidReleaseJson(e.to_string()))?;
            Ok(vec![single])
        }
        _ => Err(UpdateError::InvalidReleaseJson(
            "expected JSON array or object".to_string(),
        )),
    }
}

/// Selects the highest eligible update from `releases` for `current` version, `allow_beta` flag,
/// and `target` platform.
#[must_use]
pub fn select_release(
    releases: &[GitHubRelease],
    current: &Version,
    allow_beta: bool,
    target: Target,
) -> Option<AvailableUpdate> {
    let expected_suffix = format!("-{}{}", target.asset_suffix(), UPDATE_PACKAGE_EXT);
    let mut best: Option<AvailableUpdate> = None;

    for release in releases {
        if release.draft {
            continue;
        }
        let Ok(version) = Version::parse(&release.tag_name) else {
            continue;
        };
        let channel = if release.prerelease || version.is_prerelease() {
            UpdateChannel::Beta
        } else {
            UpdateChannel::Stable
        };
        if !channel_allows_update(current, &version, channel, allow_beta) {
            continue;
        }

        let Some(asset) = release.assets.iter().find(|a| {
            let lower = a.name.to_ascii_lowercase();
            lower.ends_with(&expected_suffix)
                && a.size <= MAX_PACKAGE_SIZE as u64
                && !a.browser_download_url.is_empty()
        }) else {
            continue;
        };

        let sha256_sums_url = release
            .assets
            .iter()
            .find(|a| {
                a.name.eq_ignore_ascii_case(SHA256SUMS_ASSET_NAME)
                    && !a.browser_download_url.is_empty()
            })
            .map(|a| a.browser_download_url.clone());

        let expected_sha256 = extract_inline_sha256(release.body.as_deref(), &asset.name);

        let is_better = best.as_ref().is_none_or(|prev| version > prev.version);
        if is_better {
            best = Some(AvailableUpdate {
                version,
                channel,
                title: release
                    .name
                    .clone()
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or_else(|| release.tag_name.clone()),
                release_notes: release.body.clone(),
                asset_name: asset.name.clone(),
                download_url: asset.browser_download_url.clone(),
                size: asset.size,
                expected_sha256,
                sha256_sums_url,
            });
        }
    }

    best
}

/// Lightweight plain HTTP/1.1 client over `std::net::TcpStream` for loopback/local servers.
///
/// Supports `http://host[:port]/path`, `Content-Length`, `Transfer-Encoding: chunked`,
/// and strict `max_bytes` ceilings without external TLS/HTTP dependencies.
#[derive(Debug, Clone)]
pub struct LocalHttpClient {
    timeout: Duration,
}

impl Default for LocalHttpClient {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(10),
        }
    }
}

impl LocalHttpClient {
    /// Creates a new [`LocalHttpClient`] with a custom read/write timeout.
    #[must_use]
    pub const fn with_timeout(timeout: Duration) -> Self {
        Self { timeout }
    }
}

impl HttpClient for LocalHttpClient {
    fn get_bytes(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        max_bytes: usize,
    ) -> Result<Vec<u8>, UpdateError> {
        let rest = url.strip_prefix("http://").ok_or_else(|| {
            UpdateError::Network(format!("LocalHttpClient requires http:// URL, got '{url}'"))
        })?;
        let (authority, path) = match rest.find('/') {
            Some(idx) => (&rest[..idx], &rest[idx..]),
            None => (rest, "/"),
        };
        let addr = if authority.contains(':') {
            authority.to_owned()
        } else {
            format!("{authority}:80")
        };

        let mut stream = TcpStream::connect(&addr)
            .map_err(|e| UpdateError::Network(format!("connect {addr}: {e}")))?;
        let _ = stream.set_read_timeout(Some(self.timeout));
        let _ = stream.set_write_timeout(Some(self.timeout));

        let mut req = format!("GET {path} HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n");
        for &(k, v) in headers {
            req.push_str(k);
            req.push_str(": ");
            req.push_str(v);
            req.push_str("\r\n");
        }
        req.push_str("\r\n");

        stream
            .write_all(req.as_bytes())
            .map_err(|e| UpdateError::Network(format!("write request: {e}")))?;
        stream
            .flush()
            .map_err(|e| UpdateError::Network(format!("flush request: {e}")))?;

        let mut reader = BufReader::new(stream);
        let mut status_line = String::new();
        reader
            .read_line(&mut status_line)
            .map_err(|e| UpdateError::Network(format!("read status line: {e}")))?;
        let status_code = status_line
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse::<u16>().ok())
            .ok_or_else(|| {
                UpdateError::Network(format!(
                    "malformed HTTP status line: '{}'",
                    status_line.trim()
                ))
            })?;

        let mut content_length: Option<usize> = None;
        let mut is_chunked = false;
        loop {
            let mut line = String::new();
            let n = reader
                .read_line(&mut line)
                .map_err(|e| UpdateError::Network(format!("read header: {e}")))?;
            if n == 0 || line == "\r\n" || line == "\n" {
                break;
            }
            if let Some((name, val)) = line.split_once(':') {
                let name_lower = name.trim().to_ascii_lowercase();
                let val_trim = val.trim();
                if name_lower == "content-length" {
                    content_length = val_trim.parse::<usize>().ok();
                } else if name_lower == "transfer-encoding"
                    && val_trim.to_ascii_lowercase().contains("chunked")
                {
                    is_chunked = true;
                }
            }
        }

        if !(200..300).contains(&status_code) {
            return Err(UpdateError::Network(format!(
                "GET {url} returned HTTP {status_code}"
            )));
        }

        if let Some(len) = content_length
            && len > max_bytes
        {
            return Err(UpdateError::ResponseTooLarge { limit: max_bytes });
        }

        if is_chunked {
            read_chunked_body(&mut reader, max_bytes)
        } else if let Some(len) = content_length {
            let mut buf = vec![0u8; len];
            reader
                .read_exact(&mut buf)
                .map_err(|e| UpdateError::Network(format!("read body: {e}")))?;
            Ok(buf)
        } else {
            let mut buf = Vec::new();
            reader
                .take((max_bytes as u64).saturating_add(1))
                .read_to_end(&mut buf)
                .map_err(|e| UpdateError::Network(format!("read body: {e}")))?;
            if buf.len() > max_bytes {
                return Err(UpdateError::ResponseTooLarge { limit: max_bytes });
            }
            Ok(buf)
        }
    }
}

fn read_chunked_body<R: BufRead>(reader: &mut R, max_bytes: usize) -> Result<Vec<u8>, UpdateError> {
    let mut out = Vec::new();
    loop {
        let mut size_line = String::new();
        let n = reader
            .read_line(&mut size_line)
            .map_err(|e| UpdateError::Network(format!("read chunk size: {e}")))?;
        if n == 0 {
            return Err(UpdateError::Network(
                "unexpected EOF in chunked body".into(),
            ));
        }
        let hex_len = size_line.split(';').next().unwrap_or("").trim();
        let chunk_len = usize::from_str_radix(hex_len, 16)
            .map_err(|e| UpdateError::Network(format!("invalid chunk length '{hex_len}': {e}")))?;
        if chunk_len == 0 {
            break;
        }
        let new_len = out
            .len()
            .checked_add(chunk_len)
            .ok_or(UpdateError::ResponseTooLarge { limit: max_bytes })?;
        if new_len > max_bytes {
            return Err(UpdateError::ResponseTooLarge { limit: max_bytes });
        }
        let start = out.len();
        out.resize(new_len, 0);
        reader
            .read_exact(&mut out[start..new_len])
            .map_err(|e| UpdateError::Network(format!("read chunk data: {e}")))?;
        let mut crlf = [0u8; 2];
        reader
            .read_exact(&mut crlf)
            .map_err(|e| UpdateError::Network(format!("read chunk terminator: {e}")))?;
    }
    Ok(out)
}
