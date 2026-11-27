//! Proxy configuration, MTProxy secret parsing/validation, and Telegram proxy link parsing.
//!
//! Port of `mtproto/mtproto_proxy_data.h/.cpp`, proxy URL conversion from
//! `core/local_url_handlers.cpp`, and `ProxyDataFromFields`/`ProxyDataFromLocalUrl` from
//! `boxes/connection_box.cpp`.

use std::collections::HashMap;
use std::net::IpAddr;
use std::time::Instant;

use base64::Engine as _;
use base64::engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::error::Error;

/// Marker byte prepended to base64url-encoded web proxy secrets when a base path is present
/// (`kWebProxyLinkSecretMarker` in `mtproto_proxy_data.cpp`).
const WEB_PROXY_LINK_SECRET_MARKER: u8 = 0x70;

/// Global proxy settings mode (`ProxyData::Settings`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProxySettings {
    /// Use system proxy settings.
    #[default]
    System,
    /// Use the selected explicit proxy.
    Enabled,
    /// Direct connection (no proxy).
    Disabled,
}

/// Proxy protocol type (`ProxyData::Type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ProxyType {
    /// Direct connection without a proxy.
    #[default]
    None,
    /// SOCKS5 proxy (RFC 1928 / RFC 1929).
    Socks5,
    /// HTTP CONNECT (and HTTP transport) proxy.
    Http,
    /// Telegram MTProto proxy (plain, `dd` padded, or `ee` fake-TLS).
    Mtproto,
    /// Telegram Desktop Web proxy.
    Web,
}

/// Validation status for a [`ProxyData`] or MTProto secret (`ProxyData::Status`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProxyStatus {
    /// Configuration and secret are valid and supported.
    Valid,
    /// Secret or proxy variant is well-formed in length/encoding but not supported.
    Unsupported,
    /// Secret appears to have a common user error (e.g., `ee` prefix on a base64url secret).
    IncorrectSecret,
    /// Configuration or secret is syntactically invalid or missing required fields.
    Invalid,
}

/// Parsed MTProto proxy secret classification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MtprotoSecret {
    /// 16-byte plain secret (Version1: abridged `0xEFEFEFEF` + SHA-256 key mixing).
    Plain([u8; 16]),
    /// 17-byte `0xDD` secret (VersionD: padded intermediate `0xDDDDDDDD` + SHA-256 key mixing).
    Padded([u8; 16]),
    /// `>= 21`-byte `0xEE` secret (Fake-TLS socket + VersionD padded intermediate).
    FakeTls {
        /// 16-byte shared key (`secret[1..17]`).
        key: [u8; 16],
        /// SNI domain bytes (`secret[17..]`).
        domain: Vec<u8>,
    },
}

impl MtprotoSecret {
    /// Parse raw secret bytes into a typed [`MtprotoSecret`].
    ///
    /// Mirrors `DcOptions::ValidateSecret` and `TcpConnection::Protocol::Create`.
    pub fn from_bytes(raw: &[u8]) -> Result<Self, Error> {
        if raw.len() >= 21 && raw[0] == 0xEE {
            let mut key = [0u8; 16];
            key.copy_from_slice(&raw[1..17]);
            Ok(Self::FakeTls {
                key,
                domain: raw[17..].to_vec(),
            })
        } else if raw.len() == 17 && raw[0] == 0xDD {
            let mut key = [0u8; 16];
            key.copy_from_slice(&raw[1..17]);
            Ok(Self::Padded(key))
        } else if raw.len() == 16 {
            let mut key = [0u8; 16];
            key.copy_from_slice(raw);
            Ok(Self::Plain(key))
        } else {
            Err(Error::InvalidProxy(format!(
                "unsupported MTProto secret length {} (first byte {:?})",
                raw.len(),
                raw.first()
            )))
        }
    }

    /// Return the 16-byte shared secret key used for obfuscated2 SHA-256 key derivation
    /// (and fake-TLS HMAC-SHA256).
    #[must_use]
    pub const fn key(&self) -> &[u8; 16] {
        match self {
            Self::Plain(k) | Self::Padded(k) | Self::FakeTls { key: k, .. } => k,
        }
    }

    /// Return the fake-TLS SNI domain bytes if this is an `0xEE` secret.
    #[must_use]
    pub fn fake_tls_domain(&self) -> Option<&[u8]> {
        match self {
            Self::FakeTls { domain, .. } => Some(domain),
            Self::Plain(_) | Self::Padded(_) => None,
        }
    }

    /// Serialize back to raw wire secret bytes.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        match self {
            Self::Plain(k) => k.to_vec(),
            Self::Padded(k) => {
                let mut out = Vec::with_capacity(17);
                out.push(0xDD);
                out.extend_from_slice(k);
                out
            }
            Self::FakeTls { key, domain } => {
                let mut out = Vec::with_capacity(17 + domain.len());
                out.push(0xEE);
                out.extend_from_slice(key);
                out.extend_from_slice(domain);
                out
            }
        }
    }
}

/// Validate raw protocol secret bytes (empty secret is allowed for direct DC endpoints).
///
/// Port of `DcOptions::ValidateSecret` (`mtproto/mtproto_dc_options.cpp`).
#[must_use]
pub fn validate_secret_bytes(secret: &[u8]) -> bool {
    secret.is_empty()
        || secret.len() == 16
        || (secret.len() == 17 && secret[0] == 0xDD)
        || (secret.len() >= 21 && secret[0] == 0xEE)
}

/// Proxy configuration (`MTP::ProxyData`).
#[derive(Debug, Clone)]
pub struct ProxyData {
    /// Proxy protocol type.
    pub proxy_type: ProxyType,
    /// Hostname or IP address (IPv4 or IPv6).
    pub host: String,
    /// TCP port (1..=65535; 0 means unset/invalid).
    pub port: u16,
    /// Username (SOCKS5/HTTP) or normalized base path (Web proxy).
    pub user: String,
    /// Password (SOCKS5/HTTP) or hex/base64url secret (MTProto/Web proxy).
    pub password: String,
    /// Cached resolved IP addresses for custom resolution (`ResolvingConnection`).
    pub resolved_ips: Vec<String>,
    /// Expiration instant of `resolved_ips`, if populated.
    pub resolved_expire_at: Option<Instant>,
}

impl Default for ProxyData {
    fn default() -> Self {
        Self::none()
    }
}

impl PartialEq for ProxyData {
    fn eq(&self, other: &Self) -> bool {
        if !self.valid() {
            return !other.valid();
        }
        self.proxy_type == other.proxy_type
            && self.host == other.host
            && self.port == other.port
            && self.user == other.user
            && self.password == other.password
    }
}

impl Eq for ProxyData {}

impl ProxyData {
    /// Create a disabled (`ProxyType::None`) proxy configuration.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            proxy_type: ProxyType::None,
            host: String::new(),
            port: 0,
            user: String::new(),
            password: String::new(),
            resolved_ips: Vec::new(),
            resolved_expire_at: None,
        }
    }

    /// Create a SOCKS5 proxy configuration.
    #[must_use]
    pub fn socks5(
        host: impl Into<String>,
        port: u16,
        user: impl Into<String>,
        password: impl Into<String>,
    ) -> Self {
        Self {
            proxy_type: ProxyType::Socks5,
            host: host.into(),
            port,
            user: user.into(),
            password: password.into(),
            resolved_ips: Vec::new(),
            resolved_expire_at: None,
        }
    }

    /// Create an HTTP CONNECT proxy configuration.
    #[must_use]
    pub fn http(
        host: impl Into<String>,
        port: u16,
        user: impl Into<String>,
        password: impl Into<String>,
    ) -> Self {
        Self {
            proxy_type: ProxyType::Http,
            host: host.into(),
            port,
            user: user.into(),
            password: password.into(),
            resolved_ips: Vec::new(),
            resolved_expire_at: None,
        }
    }

    /// Create an MTProto proxy configuration with a hex or base64url `secret`.
    #[must_use]
    pub fn mtproto(host: impl Into<String>, port: u16, secret: impl Into<String>) -> Self {
        Self {
            proxy_type: ProxyType::Mtproto,
            host: host.into(),
            port,
            user: String::new(),
            password: secret.into(),
            resolved_ips: Vec::new(),
            resolved_expire_at: None,
        }
    }

    /// Return `true` if `self.status() == ProxyStatus::Valid`.
    #[must_use]
    pub fn valid(&self) -> bool {
        self.status() == ProxyStatus::Valid
    }

    /// Validate this proxy configuration (`ProxyData::status`).
    #[must_use]
    pub fn status(&self) -> ProxyStatus {
        match self.proxy_type {
            ProxyType::Web => {
                if self.host.is_empty()
                    || self.host != normalize_web_proxy_host(&self.host)
                    || self.port != 443
                    || (!self.user.is_empty()
                        && self.user != normalize_web_proxy_base_path(&self.user))
                {
                    return ProxyStatus::Invalid;
                }
                let result = Self::mtproto_password_status(&self.password);
                if result != ProxyStatus::Valid {
                    return result;
                }
                let secret = self.secret_from_mtproto_password();
                if secret.len() >= 21 && secret[0] == 0xEE {
                    ProxyStatus::Unsupported
                } else {
                    ProxyStatus::Valid
                }
            }
            ProxyType::None => ProxyStatus::Invalid,
            ProxyType::Mtproto => {
                if self.host.is_empty() || self.port == 0 {
                    return ProxyStatus::Invalid;
                }
                Self::mtproto_password_status(&self.password)
            }
            ProxyType::Socks5 | ProxyType::Http => {
                if self.host.is_empty() || self.port == 0 {
                    ProxyStatus::Invalid
                } else {
                    ProxyStatus::Valid
                }
            }
        }
    }

    /// Whether custom DNS resolution should be used for this proxy host
    /// (`ProxyData::tryCustomResolve`).
    #[must_use]
    pub fn try_custom_resolve(&self) -> bool {
        matches!(self.proxy_type, ProxyType::Socks5 | ProxyType::Mtproto)
            && !self.host.is_empty()
            && self.host.parse::<IpAddr>().is_err()
    }

    /// Decode the MTProto secret bytes from `self.password` (hex or base64url).
    ///
    /// Port of `ProxyData::secretFromMtprotoPassword`.
    #[must_use]
    pub fn secret_from_mtproto_password(&self) -> Vec<u8> {
        if !matches!(self.proxy_type, ProxyType::Mtproto | ProxyType::Web) {
            return Vec::new();
        }
        decode_mtproto_secret(&self.password).unwrap_or_default()
    }

    /// Decode and classify the MTProto secret from `self.password`.
    pub fn parsed_mtproto_secret(&self) -> Result<MtprotoSecret, Error> {
        let raw = self.secret_from_mtproto_password();
        MtprotoSecret::from_bytes(&raw)
    }

    /// Validate a hex or base64url MTProto proxy secret string (`ProxyData::MtprotoPasswordStatus`).
    #[must_use]
    pub fn mtproto_password_status(password: &str) -> ProxyStatus {
        if is_hex_mtproto_password(password) {
            hex_mtproto_password_status(password)
        } else if is_base64url_mtproto_password(password) {
            base64url_mtproto_password_status(password)
        } else {
            ProxyStatus::Invalid
        }
    }

    /// Return the normalized web proxy base path (`ProxyData::webBasePath`).
    #[must_use]
    pub fn web_base_path(&self) -> &str {
        if self.proxy_type == ProxyType::Web {
            &self.user
        } else {
            ""
        }
    }

    /// Return the canonical web proxy address (`host` or `host/path`).
    #[must_use]
    pub fn web_address(&self) -> String {
        let path = self.web_base_path();
        if path.is_empty() {
            self.host.clone()
        } else {
            format!("{}/{path}", self.host)
        }
    }

    /// Parse and set host + base path for a `ProxyType::Web` entry (`ProxyData::setWebAddress`).
    pub fn set_web_address(&mut self, value: &str) {
        let trimmed = strip_web_proxy_scheme(value);
        let (host_part, path_part) = match trimmed.find('/') {
            Some(slash) => (&trimmed[..slash], &trimmed[slash + 1..]),
            None => (trimmed, ""),
        };
        let normalized_host = normalize_web_proxy_host(host_part);
        if normalized_host.is_empty() || !valid_web_proxy_base_path(path_part) {
            self.host.clear();
            self.user.clear();
        } else {
            self.host = normalized_host;
            self.user = normalize_web_proxy_base_path(path_part);
        }
    }

    /// Create a copy of this proxy pinned to one of its `resolved_ips` (`ToDirectIpProxy`).
    #[must_use]
    pub fn to_direct_ip(&self, ip_index: usize) -> Self {
        if !self.try_custom_resolve() || ip_index >= self.resolved_ips.len() {
            return self.clone();
        }
        Self {
            proxy_type: self.proxy_type,
            host: self.resolved_ips[ip_index].clone(),
            port: self.port,
            user: self.user.clone(),
            password: self.password.clone(),
            resolved_ips: Vec::new(),
            resolved_expire_at: None,
        }
    }

    /// Parse a Telegram proxy URL:
    /// - `tg://proxy?server=...&port=...&secret=...`
    /// - `tg://socks?server=...&port=...&user=...&pass=...`
    /// - `tg://webproxy?server=...&secret=...`
    /// - `https://t.me/proxy?...`, `http://t.me/proxy?...`, `t.me/proxy?...`
    /// - `https://t.me/socks?...`, `t.me/socks?...`
    /// - `https://t.me/webproxy?...`, `t.me/webproxy?...`
    /// - Also `telegram.me/...` and `telegram.dog/...` aliases.
    ///
    /// Port of `Core::TryConvertUrlToLocal` + `ProxyDataFromLocalUrl` + `ProxyDataFromFields`.
    pub fn from_link(link: &str) -> Result<Self, Error> {
        let trimmed = link.trim();
        let (proxy_type, query_str) = parse_proxy_link_parts(trimmed)
            .ok_or_else(|| Error::InvalidProxy(format!("not a Telegram proxy link: {trimmed}")))?;

        let fields = parse_query_params_lowercase(query_str);
        Ok(Self::from_fields(proxy_type, &fields))
    }

    /// Build a [`ProxyData`] from lowercased query parameters (`ProxyDataFromFields`).
    #[must_use]
    pub fn from_fields(proxy_type: ProxyType, fields: &HashMap<String, String>) -> Self {
        let mut proxy = Self::none();
        proxy.proxy_type = proxy_type;
        let is_web = proxy_type == ProxyType::Web;
        let server = fields.get("server").cloned().unwrap_or_default();
        proxy.port = if is_web {
            443
        } else {
            fields
                .get("port")
                .and_then(|p| p.parse::<u16>().ok())
                .unwrap_or(0)
        };
        if is_web {
            let addr = if server.is_empty() {
                fields.get("host").map(String::as_str).unwrap_or("")
            } else {
                &server
            };
            proxy.set_web_address(addr);
        } else {
            proxy.host = server;
        }
        match proxy_type {
            ProxyType::Socks5 => {
                proxy.user = fields.get("user").cloned().unwrap_or_default();
                proxy.password = fields.get("pass").cloned().unwrap_or_default();
            }
            ProxyType::Mtproto | ProxyType::Web => {
                let mut secret = fields.get("secret").cloned().unwrap_or_default();
                secret = secret.replace('+', "-").replace('/', "_");
                if is_web {
                    secret =
                        decode_web_proxy_link_secret(&secret, !proxy.web_base_path().is_empty());
                }
                proxy.password = secret;
            }
            ProxyType::None | ProxyType::Http => {}
        }
        proxy
    }

    /// Format this proxy as a `tg://` link if shareable (MTProto or SOCKS5).
    #[must_use]
    pub fn to_tg_link(&self) -> Option<String> {
        if !self.valid() {
            return None;
        }
        match self.proxy_type {
            ProxyType::Mtproto => Some(format!(
                "tg://proxy?server={}&port={}&secret={}",
                percent_encode_query(&self.host),
                self.port,
                percent_encode_query(&self.password),
            )),
            ProxyType::Socks5 => {
                let mut link = format!(
                    "tg://socks?server={}&port={}",
                    percent_encode_query(&self.host),
                    self.port
                );
                if !self.user.is_empty() {
                    link.push_str("&user=");
                    link.push_str(&percent_encode_query(&self.user));
                }
                if !self.password.is_empty() {
                    link.push_str("&pass=");
                    link.push_str(&percent_encode_query(&self.password));
                }
                Some(link)
            }
            ProxyType::Web => Some(format!(
                "tg://webproxy?server={}&secret={}",
                percent_encode_query(&self.web_address()),
                percent_encode_query(&encode_web_proxy_link_secret(self)),
            )),
            ProxyType::None | ProxyType::Http => None,
        }
    }
}

/// Decode a hex or base64url MTProto proxy secret into raw bytes.
pub fn decode_mtproto_secret(password: &str) -> Result<Vec<u8>, Error> {
    if is_hex_mtproto_password(password) {
        decode_hex(password).ok_or_else(|| Error::InvalidProxy("invalid hex secret".into()))
    } else if is_base64url_mtproto_password(password) {
        let inner = base64url_inner(password);
        URL_SAFE_NO_PAD
            .decode(inner.as_bytes())
            .or_else(|_| URL_SAFE.decode(password.as_bytes()))
            .map_err(|e| Error::InvalidProxy(format!("invalid base64url secret: {e}")))
    } else {
        Err(Error::InvalidProxy(
            "secret is neither valid hex nor valid base64url".into(),
        ))
    }
}

fn is_hex_mtproto_password(password: &str) -> bool {
    let bytes = password.as_bytes();
    bytes.len() >= 32 && bytes.len().is_multiple_of(2) && bytes.iter().all(u8::is_ascii_hexdigit)
}

fn hex_mtproto_password_status(password: &str) -> ProxyStatus {
    let size = password.len() / 2;
    let bytes = password.as_bytes();
    let t1 = bytes[0].to_ascii_lowercase();
    let t2 = bytes[1].to_ascii_lowercase();
    let valid = size == 16
        || (size == 17 && t1 == b'd' && t2 == b'd')
        || (size >= 21 && t1 == b'e' && t2 == b'e');
    if valid {
        ProxyStatus::Valid
    } else if size < 16 {
        ProxyStatus::Invalid
    } else {
        ProxyStatus::Unsupported
    }
}

fn decode_hex(hex: &str) -> Option<Vec<u8>> {
    let bytes = hex.as_bytes();
    if !bytes.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 2);
    for chunk in bytes.as_chunks::<2>().0 {
        let hi = from_hex_digit(chunk[0])?;
        let lo = from_hex_digit(chunk[1])?;
        out.push((hi << 4) | lo);
    }
    Some(out)
}

fn from_hex_digit(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(10 + (b - b'a')),
        b'A'..=b'F' => Some(10 + (b - b'A')),
        _ => None,
    }
}

fn base64url_inner(password: &str) -> &str {
    // Skip up to two trailing '=' characters (matching Base64UrlInner).
    let mut end = password.len();
    for _ in 0..2 {
        if end > 0 && password.as_bytes()[end - 1] == b'=' {
            end -= 1;
        } else {
            break;
        }
    }
    &password[..end]
}

fn is_base64url_mtproto_password(password: &str) -> bool {
    let size = password.len();
    if size < 22 || size % 4 == 1 {
        return false;
    }
    let inner = base64url_inner(password);
    inner
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn base64url_mtproto_password_status(password: &str) -> ProxyStatus {
    let inner = base64url_inner(password);
    let size = (inner.len() * 3) / 4;
    let bytes = password.as_bytes();
    let b0 = bytes[0];
    let b1 = bytes[1];
    let valid = size == 16
        || (size == 17
            && b0 == b'3'
            && (b1.is_ascii_uppercase() && (b'Q'..=b'Z').contains(&b1)
                || (b'a'..=b'f').contains(&b1)))
        || (size >= 21 && b0 == b'7' && (b'g'..=b'v').contains(&b1));
    let incorrect = size >= 21 && b0.eq_ignore_ascii_case(&b'e') && b1.eq_ignore_ascii_case(&b'e');
    if size < 16 {
        ProxyStatus::Invalid
    } else if valid {
        ProxyStatus::Valid
    } else if incorrect {
        ProxyStatus::IncorrectSecret
    } else {
        ProxyStatus::Unsupported
    }
}

fn parse_proxy_link_parts(link: &str) -> Option<(ProxyType, &str)> {
    // Strip fragment `#...`.
    let without_fragment = link.split('#').next().unwrap_or(link);

    // Check `tg://...` directly.
    let rest = if let Some(r) = strip_prefix_ascii_ignore_case(without_fragment, "tg://") {
        r
    } else {
        // Check `https://`, `http://`, or bare domain for `t.me`, `telegram.me`, `telegram.dog`.
        let after_scheme = strip_prefix_ascii_ignore_case(without_fragment, "https://")
            .or_else(|| strip_prefix_ascii_ignore_case(without_fragment, "http://"))
            .unwrap_or(without_fragment);
        strip_prefix_ascii_ignore_case(after_scheme, "t.me/")
            .or_else(|| strip_prefix_ascii_ignore_case(after_scheme, "telegram.me/"))
            .or_else(|| strip_prefix_ascii_ignore_case(after_scheme, "telegram.dog/"))?
    };

    if let Some(after) = strip_prefix_ascii_ignore_case(rest, "proxy") {
        let q = after
            .strip_prefix("/?")
            .or_else(|| after.strip_prefix('?'))?;
        if !q.is_empty() {
            return Some((ProxyType::Mtproto, q));
        }
    } else if let Some(after) = strip_prefix_ascii_ignore_case(rest, "socks") {
        let q = after
            .strip_prefix("/?")
            .or_else(|| after.strip_prefix('?'))?;
        if !q.is_empty() {
            return Some((ProxyType::Socks5, q));
        }
    } else if let Some(after) = strip_prefix_ascii_ignore_case(rest, "webproxy") {
        let q = after
            .strip_prefix("/?")
            .or_else(|| after.strip_prefix('?'))?;
        if !q.is_empty() {
            return Some((ProxyType::Web, q));
        }
    }
    None
}

fn strip_prefix_ascii_ignore_case<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let head = s.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &s[prefix.len()..])
}

fn parse_query_params_lowercase(query: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (key, value) = match pair.split_once('=') {
            Some((k, v)) => (k, v),
            None => (pair, ""),
        };
        let decoded_key = percent_decode(key).to_ascii_lowercase();
        let decoded_val = percent_decode(value);
        map.insert(decoded_key, decoded_val);
    }
    map
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) =
                (from_hex_digit(bytes[i + 1]), from_hex_digit(bytes[i + 2]))
        {
            out.push((hi << 4) | lo);
            i += 3;
            continue;
        }
        // Note: Do NOT turn '+' into space here because base64 MTProto secrets in URLs
        // may contain '+' which tdesktop converts to '-' (see ApplyMtprotoProxy).
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn percent_encode_query(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for &b in input.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~' | b':') {
            out.push(b as char);
        } else {
            use std::fmt::Write as _;
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

fn last_label_is_numeric(host: &str) -> bool {
    let label = match host.rfind('.') {
        Some(idx) => &host[idx + 1..],
        None => host,
    };
    if label.is_empty() {
        return false;
    }
    let hex = label.len() >= 2 && label[..2].eq_ignore_ascii_case("0x");
    let digits = if hex { &label[2..] } else { label };
    digits
        .bytes()
        .all(|b| b.is_ascii_digit() || (hex && matches!(b, b'a'..=b'f' | b'A'..=b'F')))
}

fn strip_web_proxy_scheme(value: &str) -> &str {
    let trimmed = value.trim();
    strip_prefix_ascii_ignore_case(trimmed, "https://").unwrap_or(trimmed)
}

fn trim_web_proxy_base_path(value: &str) -> &str {
    let mut s = value.trim();
    if let Some(rest) = s.strip_prefix('/') {
        s = rest;
    }
    if let Some(rest) = s.strip_suffix('/') {
        s = rest;
    }
    s
}

/// Normalize a web proxy hostname (IDNA/punycode + lowercase, rejecting IPs and ports).
///
/// Port of `NormalizeWebProxyHost` (`mtproto/mtproto_proxy_data.cpp`).
#[must_use]
pub fn normalize_web_proxy_host(value: &str) -> String {
    let input = value.trim();
    if input.is_empty() || input.contains([':', '/', '?', '#', '@']) || input.ends_with('.') {
        return String::new();
    }
    // Use `url::Host::parse` for WHATWG IDNA / punycode conversion (`QUrl::toAce`).
    let parsed = match url::Host::parse(input) {
        Ok(url::Host::Domain(domain)) => domain.to_ascii_lowercase(),
        _ => return String::new(),
    };
    if parsed.is_empty() || parsed.len() > 253 || !parsed.contains('.') {
        return String::new();
    }
    for label in parsed.split('.') {
        if label.is_empty()
            || label.len() > 63
            || label.starts_with('-')
            || label.ends_with('-')
            || !label
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return String::new();
        }
    }
    if last_label_is_numeric(&parsed) || parsed.parse::<IpAddr>().is_ok() {
        return String::new();
    }
    parsed
}

/// Normalize a web proxy base path (`NormalizeWebProxyBasePath`).
#[must_use]
pub fn normalize_web_proxy_base_path(value: &str) -> String {
    let result = trim_web_proxy_base_path(value);
    if result.is_empty() || result.len() > 128 {
        return String::new();
    }
    let mut segment_start = true;
    for b in result.bytes() {
        if b == b'/' {
            if segment_start {
                return String::new();
            }
            segment_start = true;
            continue;
        }
        let letter_or_digit = b.is_ascii_alphanumeric();
        let extra = b == b'-' || b == b'_';
        if !letter_or_digit && !(extra && !segment_start) {
            return String::new();
        }
        segment_start = false;
    }
    if segment_start {
        String::new()
    } else {
        result.to_owned()
    }
}

/// Check whether a web proxy base path is empty or valid (`ValidWebProxyBasePath`).
#[must_use]
pub fn valid_web_proxy_base_path(value: &str) -> bool {
    trim_web_proxy_base_path(value).is_empty() || !normalize_web_proxy_base_path(value).is_empty()
}

/// Compute the HMAC-SHA256 base64url web proxy bridge capability token
/// (`ComputeWebProxyBridgeCapability`).
#[must_use]
pub fn compute_web_proxy_bridge_capability(host: &str, base_path: &str, key: &[u8]) -> String {
    let context = if base_path.is_empty() {
        format!("tdesktop-web-proxy-bridge-v1\n{host}")
    } else {
        format!("tdesktop-web-proxy-bridge-v2\n{host}\n{base_path}")
    };
    let mut mac =
        Hmac::<Sha256>::new_from_slice(key).expect("HMAC-SHA256 accepts keys of any length");
    mac.update(context.as_bytes());
    let tag = mac.finalize().into_bytes();
    URL_SAFE_NO_PAD.encode(tag)
}

/// Encode a web proxy secret for a link, adding the `0x70` marker if a base path is set
/// (`EncodeWebProxyLinkSecret`).
#[must_use]
pub fn encode_web_proxy_link_secret(proxy: &ProxyData) -> String {
    let secret = proxy.secret_from_mtproto_password();
    if proxy.web_base_path().is_empty() || secret.is_empty() {
        return proxy.password.clone();
    }
    let mut marked = Vec::with_capacity(1 + secret.len());
    marked.push(WEB_PROXY_LINK_SECRET_MARKER);
    marked.extend_from_slice(&secret);
    URL_SAFE_NO_PAD.encode(marked)
}

/// Decode a web proxy link secret (`DecodeWebProxyLinkSecret`).
#[must_use]
pub fn decode_web_proxy_link_secret(value: &str, has_base_path: bool) -> String {
    let decoded = URL_SAFE_NO_PAD
        .decode(value.as_bytes())
        .or_else(|_| URL_SAFE.decode(value.as_bytes()));
    if let Ok(bytes) = decoded
        && bytes.len() >= 17
        && bytes[0] == WEB_PROXY_LINK_SECRET_MARKER
    {
        let mut hex = String::with_capacity((bytes.len() - 1) * 2);
        for &b in &bytes[1..] {
            use std::fmt::Write as _;
            let _ = write!(hex, "{b:02x}");
        }
        return hex;
    }
    if has_base_path {
        String::new()
    } else {
        value.to_owned()
    }
}
