//! Port of `mtproto/special_config_request.cpp` and DoH JSON parsing from
//! `mtproto/details/mtproto_domain_resolver.cpp`.
//!
//! Performs Firebase-free DNS-over-HTTPS (Google DoH + Mozilla/Cloudflare DoH) TXT queries for
//! `apv3.stel.com` / `tapv3.stel.com`, decrypts the RSA+AES-256-CBC signed `help.configSimple`
//! payload using `tg_crypto::rsa::SPECIAL_CONFIG_PUBLIC_KEY`, filters rules by phone prefix,
//! and synchronizes HTTP `Date` unixtime.

use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use rand::Rng;
use rand::seq::SliceRandom;
use reqwest::header::{ACCEPT, HeaderMap, HeaderValue, USER_AGENT};
use tg_crypto::rsa::{PublicKey, SPECIAL_CONFIG_PUBLIC_KEY};
use tg_crypto::{aes_cbc_decrypt, const_time_eq, sha256};
use tg_tl::mtproto::enums::{
    AccessPointRule as TlAccessPointRule, IpPort as TlIpPort,
    help::ConfigSimple as TlConfigSimpleEnum,
};
use tg_tl::mtproto::types::help::ConfigSimple as TlConfigSimple;
use tg_tl::{Cursor, Deserializable};
use thiserror::Error;
use tokio::task::JoinSet;
use tracing::{debug, warn};

use crate::dc_options::DcId;
use crate::time::MtprotoClock;

/// Delay before firing the next staggered DoH attempt (`kSendNextTimeout = 800ms`).
pub const SEND_NEXT_TIMEOUT: Duration = Duration::from_millis(800);
/// Default HTTP request timeout for DoH queries.
pub const DOH_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// Expected Base64 length of the RSA-encrypted special config payload (`kGoodSizeBase64 = 344`).
pub const SPECIAL_CONFIG_BASE64_LEN: usize = 344;
/// Expected decoded binary size of the RSA block (`kGoodSizeData = 256`).
pub const SPECIAL_CONFIG_DATA_LEN: usize = 256;
/// Trailing SHA-256 prefix digest size inside the decrypted AES-CBC payload (`kDigestSize = 16`).
const SPECIAL_CONFIG_DIGEST_LEN: usize = 16;
/// DNS TXT record type (`kTypeRestriction = 16`).
pub const DNS_TYPE_TXT: i32 = 16;

/// User-Agent sent on DoH requests (`DnsUserAgent` in `mtproto_domain_resolver.cpp`).
pub const DNS_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) \
    AppleWebKit/537.36 (KHTML, like Gecko) \
    Chrome/151.0.0.0 Safari/537.36";

/// Default special config TXT domain for production servers.
pub const DEFAULT_TXT_DOMAIN_PRODUCTION: &str = "apv3.stel.com";
/// Default special config TXT domain for test servers.
pub const DEFAULT_TXT_DOMAIN_TEST: &str = "tapv3.stel.com";

/// Errors returned while fetching or decrypting special fallback configs.
#[derive(Debug, Error)]
pub enum SpecialConfigError {
    /// Network or HTTP client error.
    #[error("DoH HTTP error: {0}")]
    Http(String),
    /// Base64 payload had unexpected length or invalid characters.
    #[error("invalid special config Base64 payload: {0}")]
    InvalidBase64(String),
    /// RSA or AES-CBC cryptographic failure.
    #[error("special config crypto error: {0}")]
    Crypto(#[from] tg_crypto::CryptoError),
    /// Truncated SHA-256 digest mismatch.
    #[error("special config SHA-256 digest mismatch")]
    BadDigest,
    /// Invalid inner TL length or deserialization failure.
    #[error("invalid special config TL payload: {0}")]
    InvalidPayload(String),
    /// Config expired or contained no matching rules.
    #[error("special config expired or empty: {0}")]
    ExpiredOrEmpty(String),
    /// All DoH providers failed.
    #[error("all DoH fallback attempts failed")]
    AllAttemptsFailed,
}

/// One parsed DNS JSON answer entry (`DnsEntry` in `mtproto_domain_resolver.h`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DnsEntry {
    /// Raw DNS answer `data` string.
    pub data: String,
    /// Record TTL in seconds.
    pub ttl: u64,
}

/// One fallback DC endpoint extracted from `help.configSimple` (`ConfigLoader::SpecialEndpoint`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecialEndpoint {
    /// Real bare DC ID.
    pub dc_id: DcId,
    /// Dotted IPv4 address.
    pub ip: String,
    /// TCP port.
    pub port: u16,
    /// Optional MTProto transport secret.
    pub secret: Vec<u8>,
}

/// DoH provider kind for Firebase-free fallback config requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DohProvider {
    /// Google Public DNS JSON API (`https://dns.google.com/resolve`).
    Google,
    /// Mozilla / Cloudflare DoH JSON API (`https://mozilla.cloudflare-dns.com/dns-query`).
    Mozilla,
}

/// Generates random alphanumeric padding of length `13..=128` (`GenerateDnsRandomPadding`).
pub fn generate_dns_random_padding() -> String {
    const VALID: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    const MIN_PADDING: usize = 13;
    let mut rng = rand::thread_rng();
    let count = loop {
        let n = 1 + (rng.r#gen::<u8>() as usize / 2);
        if n >= MIN_PADDING {
            break n;
        }
    };
    let mut out = String::with_capacity(count);
    for _ in 0..count {
        let idx = (rng.r#gen::<u8>() as usize) % VALID.len();
        out.push(VALID[idx] as char);
    }
    out
}

/// Builds the DoH request URL and required headers for `provider` and `domain`.
pub fn build_doh_request_url(provider: DohProvider, domain: &str, padding: &str) -> String {
    match provider {
        DohProvider::Google => {
            format!(
                "https://dns.google.com/resolve?name={domain}&type=ANY&random_padding={padding}"
            )
        }
        DohProvider::Mozilla => {
            format!(
                "https://mozilla.cloudflare-dns.com/dns-query?name={domain}&type=16&random_padding={padding}"
            )
        }
    }
}

/// Parses a Google or Cloudflare DoH JSON response (`ParseDnsResponse`).
pub fn parse_dns_response(bytes: &[u8], type_restriction: Option<i32>) -> Vec<DnsEntry> {
    if bytes.is_empty() {
        return Vec::new();
    }
    let Ok(val) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return Vec::new();
    };
    let Some(answer) = val.get("Answer").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    let mut result = Vec::new();
    for elem in answer {
        let Some(obj) = elem.as_object() else {
            continue;
        };
        if let Some(required_type) = type_restriction {
            let Some(ty) = obj.get("type").and_then(|v| v.as_f64()) else {
                continue;
            };
            if ty.round() as i32 != required_type {
                continue;
            }
        }
        let Some(data) = obj.get("data").and_then(|v| v.as_str()) else {
            continue;
        };
        let ttl = obj
            .get("TTL")
            .and_then(|v| v.as_f64())
            .map(|t| t.round().max(0.0) as u64)
            .unwrap_or(0);
        result.push(DnsEntry {
            data: data.to_string(),
            ttl,
        });
    }
    result
}

/// Sorts DNS TXT entries by descending length and concatenates them (`ConcatenateDnsTxtFields`).
///
/// Mirrors `QMultiMap<int, QString>` keyed by `INT_MAX - entry.data.size()` in
/// `special_config_request.cpp`.
pub fn concatenate_dns_txt_fields(entries: &[DnsEntry]) -> Vec<u8> {
    let mut indexed: Vec<(usize, &str)> = entries
        .iter()
        .enumerate()
        .map(|(i, e)| (i, e.data.as_str()))
        .collect();
    // Sort by descending length; preserve original order for equal lengths (stable sort).
    indexed.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then_with(|| a.0.cmp(&b.0)));
    let mut out = Vec::new();
    for (_, s) in indexed {
        out.extend_from_slice(s.as_bytes());
    }
    out
}

/// Evaluates `accessPointRule.phone_prefix_rules` against `phone` (`CheckPhoneByPrefixesRules`).
///
/// Non-digit characters are stripped from `phone`. Comma-separated rules are checked in order:
/// - empty token -> matches (`result = true`)
/// - `+<digits>` -> matches if phone starts with `<digits>`
/// - `-<digits>` -> immediately rejects (`return false`) if phone starts with `<digits>`
pub fn check_phone_by_prefixes_rules(phone: &str, rules: &str) -> bool {
    let digits: String = phone.chars().filter(|c| c.is_ascii_digit()).collect();
    let mut result = false;
    for prefix in rules.split(',') {
        if prefix.is_empty() {
            result = true;
        } else if let Some(pos) = prefix.strip_prefix('+') {
            if digits.starts_with(pos) {
                result = true;
            }
        } else if let Some(neg) = prefix.strip_prefix('-')
            && digits.starts_with(neg)
        {
            return false;
        }
    }
    result
}

/// Parses an RFC 7231 IMF-fixdate HTTP `Date` header (e.g. `Wed, 10 Jul 2019 14:33:38 GMT`)
/// into Unix seconds (`ParseHttpDate` in `special_config_request.cpp`).
pub fn parse_http_date(date: &str) -> Option<i64> {
    let trimmed = date.trim();
    let (_, rest) = trimmed.split_once(", ")?;
    let parts: Vec<&str> = rest.split_whitespace().collect();
    if parts.len() != 5 || parts[4] != "GMT" {
        return None;
    }
    let day: u32 = parts[0].parse().ok()?;
    let month: u32 = match parts[1] {
        "Jan" => 1,
        "Feb" => 2,
        "Mar" => 3,
        "Apr" => 4,
        "May" => 5,
        "Jun" => 6,
        "Jul" => 7,
        "Aug" => 8,
        "Sep" => 9,
        "Oct" => 10,
        "Nov" => 11,
        "Dec" => 12,
        _ => return None,
    };
    let year: i32 = parts[2].parse().ok()?;
    let mut hms = parts[3].split(':');
    let hour: u32 = hms.next()?.parse().ok()?;
    let minute: u32 = hms.next()?.parse().ok()?;
    let second: u32 = hms.next()?.parse().ok()?;
    if hms.next().is_some()
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
        || year < 1970
    {
        return None;
    }

    // Civil date to days since Unix epoch (Howard Hinnant's algorithm).
    let y = i64::from(year) - if month <= 2 { 1 } else { 0 };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let m = i64::from(month);
    let d = i64::from(day);
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + i64::from(hour) * 3_600 + i64::from(minute) * 60 + i64::from(second))
}

/// Decrypts and verifies a Base64-encoded RSA+AES-256-CBC special config blob using
/// `tg_crypto::rsa::SPECIAL_CONFIG_PUBLIC_KEY` (`SpecialConfigRequest::decryptSimpleConfig`).
pub fn decrypt_simple_config(raw_bytes: &[u8]) -> Result<TlConfigSimple, SpecialConfigError> {
    let public_key = PublicKey::from_pem(SPECIAL_CONFIG_PUBLIC_KEY)?;
    decrypt_simple_config_with_key(raw_bytes, &public_key)
}

/// Decrypts a Base64-encoded special config blob using an explicit RSA [`PublicKey`].
///
/// Exposed for unit testing with synthetic RSA keys while using the exact same decryption
/// pipeline as [`decrypt_simple_config`].
pub fn decrypt_simple_config_with_key(
    raw_bytes: &[u8],
    public_key: &PublicKey,
) -> Result<TlConfigSimple, SpecialConfigError> {
    let clean: Vec<u8> = raw_bytes
        .iter()
        .copied()
        .filter(|&ch| ch.is_ascii_alphanumeric() || matches!(ch, b'+' | b'/' | b'='))
        .collect();

    if clean.len() != SPECIAL_CONFIG_BASE64_LEN {
        return Err(SpecialConfigError::InvalidBase64(format!(
            "bad Base64 size {}, expected {SPECIAL_CONFIG_BASE64_LEN}",
            clean.len()
        )));
    }
    let decoded = STANDARD
        .decode(&clean)
        .map_err(|e| SpecialConfigError::InvalidBase64(e.to_string()))?;
    if decoded.len() != SPECIAL_CONFIG_DATA_LEN {
        return Err(SpecialConfigError::InvalidBase64(format!(
            "bad decoded size {}, expected {SPECIAL_CONFIG_DATA_LEN}",
            decoded.len()
        )));
    }
    let mut block = [0u8; SPECIAL_CONFIG_DATA_LEN];
    block.copy_from_slice(&decoded);

    let decrypted = public_key.decrypt_raw(&block)?;
    decrypt_simple_config_rsa_block(&decrypted)
}

/// Decrypts the 256-byte post-RSA block (`aes_key[0..32]`, `aes_iv = aes_key[16..32]`,
/// `aes_encrypted = block[32..256]`) and parses `help.configSimple`.
pub fn decrypt_simple_config_rsa_block(
    decrypted: &[u8; SPECIAL_CONFIG_DATA_LEN],
) -> Result<TlConfigSimple, SpecialConfigError> {
    let mut aes_key = [0u8; 32];
    aes_key.copy_from_slice(&decrypted[0..32]);
    let mut aes_iv = [0u8; 16];
    aes_iv.copy_from_slice(&decrypted[16..32]);
    let aes_encrypted = &decrypted[32..];

    let aes_decrypted = aes_cbc_decrypt(aes_encrypted, &aes_key, &aes_iv)?;
    let data_size = aes_decrypted.len() - SPECIAL_CONFIG_DIGEST_LEN;
    let data = &aes_decrypted[..data_size];
    let trailing_digest = &aes_decrypted[data_size..];

    let computed_hash = sha256(&[data]);
    if !const_time_eq(&computed_hash[..SPECIAL_CONFIG_DIGEST_LEN], trailing_digest) {
        return Err(SpecialConfigError::BadDigest);
    }

    if data.len() < 8 {
        return Err(SpecialConfigError::InvalidPayload(
            "decrypted config data too short".to_string(),
        ));
    }
    let real_length = u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize;
    if real_length < 8 || real_length > data_size || (real_length & 0x03) != 0 {
        return Err(SpecialConfigError::InvalidPayload(format!(
            "bad real length {real_length} (data_size={data_size})"
        )));
    }

    let mut cur = Cursor::new(&data[4..]);
    let TlConfigSimpleEnum::ConfigSimple(simple) = TlConfigSimpleEnum::deserialize(&mut cur)
        .map_err(|e| SpecialConfigError::InvalidPayload(e.to_string()))?;
    let total_read = 4 + cur.position();
    if total_read != real_length {
        return Err(SpecialConfigError::InvalidPayload(format!(
            "bad read length {total_read}, expected {real_length}"
        )));
    }
    Ok(simple)
}

/// Formats a TL `ipv4:int` field (`special_config_request.cpp:521-528`) into a dotted IPv4 string.
pub fn format_tl_ipv4(ipv4: i32) -> String {
    let ip = ipv4 as u32;
    format!(
        "{}.{}.{}.{}",
        (ip >> 24) & 0xFF,
        (ip >> 16) & 0xFF,
        (ip >> 8) & 0xFF,
        ip & 0xFF
    )
}

/// Validates expiry and extracts all [`SpecialEndpoint`]s matching `phone` from a
/// `help.configSimple` payload (`SpecialConfigRequest::handleResponse`).
pub fn extract_special_endpoints(
    config: &TlConfigSimple,
    now_secs: i32,
    phone: &str,
) -> Result<Vec<SpecialEndpoint>, SpecialConfigError> {
    if now_secs > config.expires {
        return Err(SpecialConfigError::ExpiredOrEmpty(format!(
            "simple config expired: {}-{}, now={now_secs}",
            config.date, config.expires
        )));
    }
    if config.rules.is_empty() {
        return Err(SpecialConfigError::ExpiredOrEmpty(
            "empty simple config rules".to_string(),
        ));
    }

    let mut endpoints = Vec::new();
    for rule_enum in &config.rules {
        let TlAccessPointRule::AccessPointRule(rule) = rule_enum;
        if !check_phone_by_prefixes_rules(phone, &rule.phone_prefix_rules) {
            continue;
        }
        for addr in &rule.ips {
            let (ipv4, port, secret) = match addr {
                TlIpPort::IpPort(plain) => (plain.ipv4, plain.port, Vec::new()),
                TlIpPort::Secret(sec) => (sec.ipv4, sec.port, sec.secret.clone()),
            };
            if port <= 0 || port > i32::from(u16::MAX) {
                continue;
            }
            endpoints.push(SpecialEndpoint {
                dc_id: rule.dc_id,
                ip: format_tl_ipv4(ipv4),
                port: port as u16,
                secret,
            });
        }
    }
    Ok(endpoints)
}

fn build_doh_http_client() -> Result<reqwest::Client, SpecialConfigError> {
    let mut headers = HeaderMap::new();
    headers.insert(USER_AGENT, HeaderValue::from_static(DNS_USER_AGENT));
    reqwest::Client::builder()
        .no_proxy()
        .default_headers(headers)
        .timeout(DOH_REQUEST_TIMEOUT)
        .build()
        .map_err(|e| SpecialConfigError::Http(e.to_string()))
}

async fn execute_doh_query(
    client: &reqwest::Client,
    provider: DohProvider,
    domain: &str,
) -> Result<(Vec<u8>, Option<i64>), SpecialConfigError> {
    let padding = generate_dns_random_padding();
    let url = build_doh_request_url(provider, domain, &padding);
    let mut req = client.get(&url);
    if provider == DohProvider::Mozilla {
        req = req.header(ACCEPT, "application/dns-json");
    }
    let resp = req
        .send()
        .await
        .map_err(|e| SpecialConfigError::Http(e.to_string()))?;
    let http_date = resp
        .headers()
        .get("date")
        .and_then(|v| v.to_str().ok())
        .and_then(parse_http_date);
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| SpecialConfigError::Http(e.to_string()))?;
    Ok((bytes.to_vec(), http_date))
}

/// Fetches fallback DC endpoints via Firebase-free DoH (Google + Mozilla/Cloudflare TXT records),
/// staggered by [`SEND_NEXT_TIMEOUT`] (`800ms`), decrypting with `SPECIAL_CONFIG_PUBLIC_KEY`.
pub async fn fetch_special_config(
    domain: &str,
    phone: &str,
    clock: &MtprotoClock,
) -> Result<Vec<SpecialEndpoint>, SpecialConfigError> {
    let client = build_doh_http_client()?;
    let mut providers = vec![DohProvider::Google, DohProvider::Mozilla];
    providers.shuffle(&mut rand::thread_rng());

    let mut set: JoinSet<Result<Vec<SpecialEndpoint>, SpecialConfigError>> = JoinSet::new();
    for (idx, provider) in providers.into_iter().enumerate() {
        let client = client.clone();
        let domain = domain.to_string();
        let phone = phone.to_string();
        let clock = clock.clone();
        set.spawn(async move {
            if idx > 0 {
                tokio::time::sleep(SEND_NEXT_TIMEOUT * (idx as u32)).await;
            }
            let (body, http_date) = execute_doh_query(&client, provider, &domain).await?;
            if let Some(secs) = http_date {
                clock.update_http_time(secs);
            }
            let entries = parse_dns_response(&body, Some(DNS_TYPE_TXT));
            if entries.is_empty() {
                return Err(SpecialConfigError::ExpiredOrEmpty(format!(
                    "no TXT records returned by {provider:?}"
                )));
            }
            let concatenated = concatenate_dns_txt_fields(&entries);
            let simple = decrypt_simple_config(&concatenated)?;
            let now = clock.http_now_secs();
            let endpoints = extract_special_endpoints(&simple, now, &phone)?;
            debug!(
                ?provider,
                count = endpoints.len(),
                "loaded special fallback endpoints via DoH"
            );
            Ok(endpoints)
        });
    }

    let mut last_err = None;
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok(Ok(endpoints)) if !endpoints.is_empty() => {
                set.abort_all();
                return Ok(endpoints);
            }
            Ok(Ok(_)) => {
                last_err = Some(SpecialConfigError::ExpiredOrEmpty(
                    "no matching endpoints for phone rules".to_string(),
                ));
            }
            Ok(Err(e)) => {
                warn!(error = %e, "special config DoH attempt failed");
                last_err = Some(e);
            }
            Err(e) => {
                last_err = Some(SpecialConfigError::Http(e.to_string()));
            }
        }
    }
    Err(last_err.unwrap_or(SpecialConfigError::AllAttemptsFailed))
}

/// Synchronizes HTTP unixtime from Google/Mozilla DoH `Date` headers
/// (`Instance::Private::syncHttpUnixtime` + `SpecialConfigRequest` time-only mode).
pub async fn sync_http_unixtime(
    domain: &str,
    clock: &MtprotoClock,
) -> Result<i64, SpecialConfigError> {
    let client = build_doh_http_client()?;
    let mut providers = vec![DohProvider::Google, DohProvider::Mozilla];
    providers.shuffle(&mut rand::thread_rng());

    let mut set: JoinSet<Result<i64, SpecialConfigError>> = JoinSet::new();
    for (idx, provider) in providers.into_iter().enumerate() {
        let client = client.clone();
        let domain = domain.to_string();
        set.spawn(async move {
            if idx > 0 {
                tokio::time::sleep(SEND_NEXT_TIMEOUT * (idx as u32)).await;
            }
            let (_, http_date) = execute_doh_query(&client, provider, &domain).await?;
            http_date.ok_or_else(|| SpecialConfigError::Http("missing Date header".to_string()))
        });
    }

    let mut last_err = None;
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok(Ok(secs)) => {
                clock.update_http_time(secs);
                set.abort_all();
                return Ok(secs);
            }
            Ok(Err(e)) => last_err = Some(e),
            Err(e) => last_err = Some(SpecialConfigError::Http(e.to_string())),
        }
    }
    Err(last_err.unwrap_or(SpecialConfigError::AllAttemptsFailed))
}
