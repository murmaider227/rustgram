//! Local Telegram URL conversion and routing (`tg://proxy`, `tg://socks`, `tg://webproxy`,
//! `t.me/proxy`, `t.me/socks`, `telegram.me/...`, `telegram.dog/...`, `tg://settings`,
//! `tg://resolve`).
//!
//! Port of proxy & local URL handlers from `Telegram/SourceFiles/core/local_url_handlers.cpp`
//! and `ProxyDataFromFields` / `ProxyDataFromLocalUrl` from
//! `Telegram/SourceFiles/boxes/connection_box.cpp`.

use std::collections::HashMap;

use crate::settings_proxy::{ProxyData, ProxyType};

/// Maximum URL length accepted by `Core::TryConvertUrlToLocal`.
pub const MAX_LOCAL_URL_LEN: usize = 8192;

/// Parsed Telegram local URL route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalUrlRoute {
    /// Proxy link (`tg://proxy?...`, `tg://socks?...`, `tg://webproxy?...`,
    /// `t.me/proxy?...`, `t.me/socks?...`, `t.me/webproxy?...`).
    Proxy(ProxyData),
    /// Settings deep link (`tg://settings[/section]`).
    Settings(String),
    /// Username/peer resolution (`tg://resolve?domain=...` or `t.me/<username>[/<post>]`).
    Resolve {
        /// Public username/domain without leading `@`.
        domain: String,
        /// Optional message/post ID.
        post: Option<i32>,
    },
}

/// Converts public Telegram links (`https://t.me/...`, `http://t.me/...`, `t.me/...`,
/// `telegram.me/...`, `telegram.dog/...`) into their canonical `tg://` local URL form
/// (`Core::TryConvertUrlToLocal` in `core/local_url_handlers.cpp`).
///
/// Returns the original URL unchanged when it does not match a known local Telegram route.
#[must_use]
pub fn try_convert_url_to_local(url: &str) -> String {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let capped = if trimmed.len() > MAX_LOCAL_URL_LEN {
        let mut end = MAX_LOCAL_URL_LEN;
        while end > 0 && !trimmed.is_char_boundary(end) {
            end -= 1;
        }
        &trimmed[..end]
    } else {
        trimmed
    };

    if strip_prefix_ascii_ignore_case(capped, "tg://").is_some() {
        return capped.to_owned();
    }

    let after_scheme = strip_prefix_ascii_ignore_case(capped, "https://")
        .or_else(|| strip_prefix_ascii_ignore_case(capped, "http://"))
        .unwrap_or(capped);
    let without_www = strip_prefix_ascii_ignore_case(after_scheme, "www.").unwrap_or(after_scheme);
    let Some(rest) = strip_prefix_ascii_ignore_case(without_www, "t.me/")
        .or_else(|| strip_prefix_ascii_ignore_case(without_www, "telegram.me/"))
        .or_else(|| strip_prefix_ascii_ignore_case(without_www, "telegram.dog/"))
    else {
        return capped.to_owned();
    };

    let without_frag = rest.split('#').next().unwrap_or(rest);
    if let Some(after) = strip_prefix_ascii_ignore_case(without_frag, "proxy")
        && let Some(q) = after.strip_prefix("/?").or_else(|| after.strip_prefix('?'))
        && !q.is_empty()
    {
        return format!("tg://proxy?{q}");
    }
    if let Some(after) = strip_prefix_ascii_ignore_case(without_frag, "socks")
        && let Some(q) = after.strip_prefix("/?").or_else(|| after.strip_prefix('?'))
        && !q.is_empty()
    {
        return format!("tg://socks?{q}");
    }
    if let Some(after) = strip_prefix_ascii_ignore_case(without_frag, "webproxy")
        && let Some(q) = after.strip_prefix("/?").or_else(|| after.strip_prefix('?'))
        && !q.is_empty()
    {
        return format!("tg://webproxy?{q}");
    }

    capped.to_owned()
}

/// Parses a `tg://proxy?...`, `tg://socks?...`, `tg://webproxy?...`, or equivalent `t.me/...`
/// URL into a [`ProxyData`] entry, returning `None` if the URL is not a proxy link.
///
/// Mirrors `ApplySocksProxy`, `ApplyMtprotoProxy`, `ApplyWebProxy` in `local_url_handlers.cpp`
/// and `ProxyDataFromFields` in `connection_box.cpp`.
#[must_use]
pub fn parse_proxy_link(url: &str) -> Option<ProxyData> {
    let local = try_convert_url_to_local(url);
    let without_frag = local.split('#').next().unwrap_or(&local);
    let rest = strip_prefix_ascii_ignore_case(without_frag, "tg://")?;

    let (proxy_type, query) = if let Some(after) = strip_prefix_ascii_ignore_case(rest, "proxy") {
        let q = after
            .strip_prefix("/?")
            .or_else(|| after.strip_prefix('?'))?;
        if q.is_empty() {
            return None;
        }
        (ProxyType::Mtproto, q)
    } else if let Some(after) = strip_prefix_ascii_ignore_case(rest, "socks") {
        let q = after
            .strip_prefix("/?")
            .or_else(|| after.strip_prefix('?'))?;
        if q.is_empty() {
            return None;
        }
        (ProxyType::Socks5, q)
    } else {
        let after = strip_prefix_ascii_ignore_case(rest, "webproxy")?;
        let q = after
            .strip_prefix("/?")
            .or_else(|| after.strip_prefix('?'))?;
        if q.is_empty() {
            return None;
        }
        (ProxyType::Web, q)
    };

    let fields = parse_query_params_lowercase(query);
    let server = fields.get("server").cloned().unwrap_or_default();
    let is_web = proxy_type == ProxyType::Web;
    let port = if is_web {
        443
    } else {
        fields
            .get("port")
            .and_then(|p| p.parse::<u16>().ok())
            .map_or(0, u32::from)
    };

    let mut proxy = ProxyData {
        proxy_type,
        host: if is_web && server.is_empty() {
            fields.get("host").cloned().unwrap_or_default()
        } else {
            server
        },
        port,
        user: String::new(),
        password: String::new(),
    };

    match proxy_type {
        ProxyType::Socks5 => {
            proxy.user = fields.get("user").cloned().unwrap_or_default();
            proxy.password = fields.get("pass").cloned().unwrap_or_default();
        }
        ProxyType::Mtproto | ProxyType::Web => {
            let secret = fields
                .get("secret")
                .cloned()
                .unwrap_or_default()
                .replace('+', "-")
                .replace('/', "_");
            proxy.password = secret;
        }
        ProxyType::None | ProxyType::Http => {}
    }

    Some(proxy)
}

/// Parses a Telegram URL into a structured [`LocalUrlRoute`], handling proxy links first.
#[must_use]
pub fn parse_local_url(url: &str) -> Option<LocalUrlRoute> {
    if let Some(proxy) = parse_proxy_link(url) {
        return Some(LocalUrlRoute::Proxy(proxy));
    }

    let trimmed = url.trim();
    if let Some(rest) = strip_prefix_ascii_ignore_case(trimmed, "tg://") {
        let without_frag = rest.split('#').next().unwrap_or(rest);
        if let Some(settings_rest) = strip_prefix_ascii_ignore_case(without_frag, "settings") {
            let sub = settings_rest
                .trim_start_matches('/')
                .split('?')
                .next()
                .unwrap_or("")
                .to_owned();
            return Some(LocalUrlRoute::Settings(sub));
        }
        if let Some(resolve_rest) = strip_prefix_ascii_ignore_case(without_frag, "resolve")
            && let Some(query) = resolve_rest
                .strip_prefix("/?")
                .or_else(|| resolve_rest.strip_prefix('?'))
        {
            let fields = parse_query_params_lowercase(query);
            if let Some(domain) = fields.get("domain").filter(|d| !d.is_empty()) {
                let post = fields.get("post").and_then(|p| p.parse::<i32>().ok());
                return Some(LocalUrlRoute::Resolve {
                    domain: domain.clone(),
                    post,
                });
            }
        }
    }

    None
}

fn strip_prefix_ascii_ignore_case<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    if s.len() >= prefix.len() && s[..prefix.len()].eq_ignore_ascii_case(prefix) {
        Some(&s[prefix.len()..])
    } else {
        None
    }
}

fn parse_query_params_lowercase(query: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (k, v) = match pair.split_once('=') {
            Some((k, v)) => (k, v),
            None => (pair, ""),
        };
        map.insert(percent_decode(k).to_ascii_lowercase(), percent_decode(v));
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
            && let (Some(hi), Some(lo)) = (hex_digit(bytes[i + 1]), hex_digit(bytes[i + 2]))
        {
            out.push((hi << 4) | lo);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_digit(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(10 + (b - b'a')),
        b'A'..=b'F' => Some(10 + (b - b'A')),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_public_proxy_urls_to_tg_local_urls() {
        assert_eq!(
            try_convert_url_to_local(
                "https://t.me/proxy?server=127.0.0.1&port=443&secret=dd0123456789abcdef0123456789abcdef"
            ),
            "tg://proxy?server=127.0.0.1&port=443&secret=dd0123456789abcdef0123456789abcdef"
        );
        assert_eq!(
            try_convert_url_to_local(
                "http://t.me/socks/?server=proxy.test&port=1080&user=u&pass=p#frag"
            ),
            "tg://socks?server=proxy.test&port=1080&user=u&pass=p"
        );
        assert_eq!(
            try_convert_url_to_local(
                "telegram.me/proxy?server=1.2.3.4&port=8443&secret=00112233445566778899aabbccddeeff"
            ),
            "tg://proxy?server=1.2.3.4&port=8443&secret=00112233445566778899aabbccddeeff"
        );
        assert_eq!(
            try_convert_url_to_local("https://telegram.dog/socks?server=127.0.0.1&port=1080"),
            "tg://socks?server=127.0.0.1&port=1080"
        );
    }

    #[test]
    fn parses_mtproto_and_socks_proxy_links() {
        let mtp = parse_proxy_link(
            "tg://proxy?server=proxy.example.test&port=443&secret=dd0123456789abcdef0123456789abcdef",
        )
        .expect("mtproto link");
        assert_eq!(mtp.proxy_type, ProxyType::Mtproto);
        assert_eq!(mtp.host, "proxy.example.test");
        assert_eq!(mtp.port, 443);
        assert_eq!(mtp.password, "dd0123456789abcdef0123456789abcdef");

        let socks = parse_proxy_link(
            "https://t.me/socks?Server=127.0.0.1&Port=1080&User=alice%20a&Pass=p%26w",
        )
        .expect("socks link");
        assert_eq!(socks.proxy_type, ProxyType::Socks5);
        assert_eq!(socks.host, "127.0.0.1");
        assert_eq!(socks.port, 1080);
        assert_eq!(socks.user, "alice a");
        assert_eq!(socks.password, "p&w");

        assert!(parse_proxy_link("https://t.me/username").is_none());
        assert!(parse_proxy_link("tg://proxy?").is_none());
    }

    #[test]
    fn routes_local_urls() {
        let route = parse_local_url(
            "t.me/proxy?server=127.0.0.1&port=443&secret=0123456789abcdef0123456789abcdef",
        );
        assert!(
            matches!(route, Some(LocalUrlRoute::Proxy(p)) if p.proxy_type == ProxyType::Mtproto)
        );

        let settings = parse_local_url("tg://settings/proxy");
        assert_eq!(settings, Some(LocalUrlRoute::Settings("proxy".to_owned())));
    }
}
