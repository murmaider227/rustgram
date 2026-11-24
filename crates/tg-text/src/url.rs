//! URL, domain, protocol, TLD, and IDN homograph validation helpers.
//!
//! Port of `Telegram/lib_base/base/qthelp_url.cpp`, `Telegram/lib_ui/ui/basic_click_handlers.cpp`,
//! and `IsValidProtocol` / `IsValidTopDomain` from `Telegram/lib_ui/ui/text/text_entity.cpp`.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use regex::Regex;
use unicode_script::{Script, UnicodeScript};

/// Suspicious character range within a URL domain (UTF-16 offsets)
/// (`UrlClickHandler::SuspiciousRange`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SuspiciousRange {
    /// Start offset in UTF-16 code units.
    pub from: usize,
    /// Length in UTF-16 code units.
    pub length: usize,
}

/// Sorted list of valid protocols accepted by `parse_entities` (`TextUtilities::IsValidProtocol`).
const VALID_PROTOCOLS: &[&str] = &["ftp", "http", "https", "itmss", "tg"];

/// Sorted list of valid top-level domains accepted without an explicit protocol
/// (`TextUtilities::IsValidTopDomain` in `text_entity.cpp`).
const VALID_TOP_DOMAINS: &[&str] = &[
    "ac",
    "ad",
    "ae",
    "aero",
    "af",
    "ag",
    "ai",
    "al",
    "am",
    "an",
    "ao",
    "aq",
    "ar",
    "arpa",
    "as",
    "asia",
    "at",
    "au",
    "aw",
    "ax",
    "az",
    "ba",
    "bb",
    "bd",
    "be",
    "bf",
    "bg",
    "bh",
    "bi",
    "biz",
    "bj",
    "bm",
    "bn",
    "bo",
    "br",
    "bs",
    "bt",
    "bv",
    "bw",
    "by",
    "bz",
    "ca",
    "cat",
    "cc",
    "cd",
    "cf",
    "cg",
    "ch",
    "ci",
    "ck",
    "cl",
    "cm",
    "cn",
    "co",
    "com",
    "coop",
    "cr",
    "cu",
    "cv",
    "cx",
    "cy",
    "cz",
    "de",
    "dj",
    "dk",
    "dm",
    "do",
    "dz",
    "ec",
    "edu",
    "ee",
    "eg",
    "eh",
    "er",
    "es",
    "et",
    "eu",
    "fi",
    "fj",
    "fk",
    "fm",
    "fo",
    "fr",
    "ga",
    "gd",
    "ge",
    "gf",
    "gg",
    "gh",
    "gi",
    "gl",
    "gm",
    "gn",
    "gov",
    "gp",
    "gq",
    "gr",
    "gs",
    "gt",
    "gu",
    "gw",
    "gy",
    "hk",
    "hm",
    "hn",
    "hr",
    "ht",
    "hu",
    "id",
    "ie",
    "il",
    "im",
    "in",
    "info",
    "int",
    "io",
    "iq",
    "ir",
    "is",
    "it",
    "je",
    "jm",
    "jo",
    "jobs",
    "jp",
    "ke",
    "kg",
    "kh",
    "ki",
    "km",
    "kn",
    "kp",
    "kr",
    "kw",
    "ky",
    "kz",
    "la",
    "lb",
    "lc",
    "li",
    "lk",
    "local",
    "lr",
    "ls",
    "lt",
    "lu",
    "lv",
    "ly",
    "ma",
    "mc",
    "md",
    "me",
    "mg",
    "mh",
    "mil",
    "mk",
    "ml",
    "mm",
    "mn",
    "mo",
    "mobi",
    "mp",
    "mq",
    "mr",
    "ms",
    "mt",
    "mu",
    "museum",
    "mv",
    "mw",
    "mx",
    "my",
    "mz",
    "na",
    "name",
    "nc",
    "ne",
    "net",
    "nf",
    "ng",
    "ni",
    "nl",
    "no",
    "np",
    "nr",
    "nu",
    "nz",
    "om",
    "org",
    "pa",
    "pe",
    "pf",
    "pg",
    "ph",
    "pk",
    "pl",
    "pm",
    "pn",
    "post",
    "pr",
    "pro",
    "ps",
    "pt",
    "pw",
    "py",
    "qa",
    "re",
    "ro",
    "rs",
    "ru",
    "rw",
    "sa",
    "sb",
    "sc",
    "sd",
    "se",
    "sg",
    "sh",
    "si",
    "sj",
    "sk",
    "sl",
    "sm",
    "sn",
    "so",
    "sr",
    "ss",
    "st",
    "su",
    "sv",
    "sx",
    "sy",
    "sz",
    "tc",
    "td",
    "tel",
    "tf",
    "tg",
    "th",
    "tj",
    "tk",
    "tl",
    "tm",
    "tn",
    "to",
    "tp",
    "tr",
    "travel",
    "tt",
    "tv",
    "tw",
    "tz",
    "ua",
    "ug",
    "uk",
    "um",
    "us",
    "uy",
    "uz",
    "va",
    "vc",
    "ve",
    "vg",
    "vi",
    "vn",
    "vu",
    "wf",
    "ws",
    "xn--3e0b707e",
    "xn--45brj9c",
    "xn--54b7fta0cc",
    "xn--80ao21a",
    "xn--90a3ac",
    "xn--clchc0ea0b2g2a9gcd",
    "xn--fiqs8s",
    "xn--fiqz9s",
    "xn--fpcrj9c3d",
    "xn--fzc2c9e2c",
    "xn--gecrj9c",
    "xn--h2brj9c",
    "xn--j1amh",
    "xn--j6w193g",
    "xn--kprw13d",
    "xn--kpry57d",
    "xn--l1acc",
    "xn--lgbbat1ad8j",
    "xn--mgba3a4f16a",
    "xn--mgbaam7a8h",
    "xn--mgbai9azgqp6j",
    "xn--mgbayh7gpa",
    "xn--mgb2ddes",
    "xn--mgb9awbf",
    "xn--mgbbh1a71e",
    "xn--mgbc0a9azcg",
    "xn--mgberp4a5d4ar",
    "xn--mgbtf8fl",
    "xn--mgbx4cd0ab",
    "xn--node",
    "xn--o3cw4h",
    "xn--ogbpf8fl",
    "xn--p1ai",
    "xn--pgbs0dh",
    "xn--s9brj9c",
    "xn--wgbh1c",
    "xn--wgbl6a",
    "xn--xkc2al3hye2a",
    "xn--xkc2dl3a5ee0h",
    "xn--yfro4i67o",
    "xn--ygbi2ammx",
    "xxx",
    "ye",
    "yt",
    "yu",
    "za",
    "zm",
    "zw",
    "рф",
];

/// Returns `true` if `protocol` (lowercased) is a valid URL protocol in `parse_entities`
/// (`TextUtilities::IsValidProtocol`).
pub fn is_valid_protocol(protocol: &str) -> bool {
    VALID_PROTOCOLS.binary_search(&protocol).is_ok()
}

/// Returns `true` if `domain` (lowercased) is in tdesktop's valid top-level domain list
/// (`TextUtilities::IsValidTopDomain`).
pub fn is_valid_top_domain(domain: &str) -> bool {
    VALID_TOP_DOMAINS.binary_search(&domain).is_ok()
}

/// Returns `true` if `protocol` (case-insensitive) is accepted by `validate_url`
/// (`qthelp::IsGoodProtocol`).
pub fn is_good_protocol(protocol: &str) -> bool {
    protocol.eq_ignore_ascii_case("http")
        || protocol.eq_ignore_ascii_case("https")
        || protocol.eq_ignore_ascii_case("tg")
        || protocol.eq_ignore_ascii_case("ton")
        || protocol.eq_ignore_ascii_case("tonsite")
}

/// Checks the negative lookbehind `(?<![\w\$\-\_%=.])` used by `RegExpDomain`,
/// `RegExpDomainExplicit`, and `RegExpIpExplicit`.
#[inline]
pub(crate) fn is_domain_bad_preceding(ch: char) -> bool {
    ch.is_alphanumeric() || matches!(ch, '_' | '$' | '-' | '%' | '=' | '.')
}

/// Captured domain match with UTF-8 byte offsets.
#[derive(Debug, Clone)]
pub(crate) struct DomainMatch<'a> {
    pub start: usize,
    pub end: usize,
    pub protocol: &'a str,
    pub top_domain: &'a str,
}

fn regex_domain_raw() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?:([a-zA-Z]+)://)?((?:[A-Za-z\u{0410}-\u{042F}\u{0401}\u{0430}-\u{044F}\u{0451}0-9\-_]+\.){1,10}([A-Za-z\u{0440}\u{0444}\-\d]{2,22})(:\d+)?)",
        )
        .expect("valid domain regex")
    })
}

fn regex_domain_explicit_raw() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?:([a-zA-Z]+)://)((?:[A-Za-z\u{0410}-\u{042F}\u{0401}\u{0430}-\u{044F}\u{0451}0-9\-_]+\.){0,10}([A-Za-z\u{0440}\u{0444}\-\d]{2,22})(:\d+)?)",
        )
        .expect("valid explicit domain regex")
    })
}

fn regex_ip_explicit_raw() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?:([a-zA-Z]+)://)(((25[0-5]|2[0-4][0-9]|[01]?[0-9][0-9]?)\.){3}(25[0-5]|2[0-4][0-9]|[01]?[0-9][0-9]?)(:\d+)?)",
        )
        .expect("valid explicit ip regex")
    })
}

fn regex_protocol() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^([a-zA-Z]+)://").expect("valid protocol regex"))
}

fn find_with_domain_lookbehind<'a>(
    re: &Regex,
    text: &'a str,
    from_byte: usize,
) -> Option<regex::Captures<'a>> {
    let mut pos = from_byte;
    while pos <= text.len() {
        let caps = re.captures_at(text, pos)?;
        let m = caps.get(0)?;
        let prev_char = text[..m.start()].chars().next_back();
        if prev_char.is_none_or(|c| !is_domain_bad_preceding(c)) {
            return Some(caps);
        }
        // Advance by one Unicode scalar past m.start() so the engine retries at the next char,
        // matching PCRE negative lookbehind semantics.
        let step = text[m.start()..].chars().next().map_or(1, char::len_utf8);
        pos = m.start() + step;
    }
    None
}

/// Finds the next match of `qthelp::RegExpDomain` starting at UTF-8 byte offset `from_byte`.
pub(crate) fn match_domain(text: &str, from_byte: usize) -> Option<DomainMatch<'_>> {
    let caps = find_with_domain_lookbehind(regex_domain_raw(), text, from_byte)?;
    let full = caps.get(0)?;
    Some(DomainMatch {
        start: full.start(),
        end: full.end(),
        protocol: caps.get(1).map_or("", |m| m.as_str()),
        top_domain: caps.get(3).map_or("", |m| m.as_str()),
    })
}

/// Finds the next match of `qthelp::RegExpDomainExplicit` starting at UTF-8 byte offset `from_byte`.
pub(crate) fn match_domain_explicit(text: &str, from_byte: usize) -> Option<DomainMatch<'_>> {
    let caps = find_with_domain_lookbehind(regex_domain_explicit_raw(), text, from_byte)?;
    let full = caps.get(0)?;
    Some(DomainMatch {
        start: full.start(),
        end: full.end(),
        protocol: caps.get(1).map_or("", |m| m.as_str()),
        top_domain: caps.get(3).map_or("", |m| m.as_str()),
    })
}

/// Validates and normalizes a user-entered URL (`qthelp::validate_url`).
///
/// Prepends `"http://"` if a bare domain was entered, or returns an empty string if invalid.
pub fn validate_url(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let domain_explicit = find_with_domain_lookbehind(regex_domain_explicit_raw(), trimmed, 0);
    let ip_explicit = find_with_domain_lookbehind(regex_ip_explicit_raw(), trimmed, 0);
    if domain_explicit.is_none() && ip_explicit.is_none() {
        let Some(domain) = find_with_domain_lookbehind(regex_domain_raw(), trimmed, 0) else {
            return String::new();
        };
        if domain.get(0).map_or(usize::MAX, |m| m.start()) != 0 {
            return String::new();
        }
        return format!("http://{trimmed}");
    }
    let dom_at_zero = domain_explicit
        .as_ref()
        .and_then(|c| c.get(0))
        .is_some_and(|m| m.start() == 0);
    let ip_at_zero = ip_explicit
        .as_ref()
        .and_then(|c| c.get(0))
        .is_some_and(|m| m.start() == 0);
    if !dom_at_zero && !ip_at_zero {
        return String::new();
    }
    if let Some(proto_caps) = regex_protocol().captures(trimmed)
        && let Some(proto) = proto_caps.get(1)
        && is_good_protocol(proto.as_str())
    {
        return trimmed.to_string();
    }
    String::new()
}

/// Percent-encodes a URL component matching `QUrl::toPercentEncoding` (`qthelp::url_encode`).
pub fn url_encode(part: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(part.len());
    for &b in part.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push('%');
            out.push(HEX[(b >> 4) as usize] as char);
            out.push(HEX[(b & 0x0F) as usize] as char);
        }
    }
    out
}

/// Decodes a percent-encoded URL component, converting `'+'` to `' '` (`qthelp::url_decode`).
pub fn url_decode(encoded: &str) -> String {
    let bytes = encoded.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                if let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                    out.push((hi << 4) | lo);
                    i += 3;
                } else {
                    out.push(b'%');
                    i += 1;
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[inline]
fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Parses `"p1=v1&p2=v2"` query parameters into a map (`qthelp::url_parse_params`).
///
/// Duplicate parameter names keep the first occurrence; params starting with `'='` are skipped.
pub fn url_parse_params(params: &str, lowercase_names: bool) -> BTreeMap<String, String> {
    let mut result = BTreeMap::new();
    for param in params.split('&') {
        match param.find('=') {
            Some(0) => {} // Skip params without a name.
            Some(sep) => {
                let raw_name = &param[..sep];
                let name = if lowercase_names {
                    raw_name.to_lowercase()
                } else {
                    raw_name.to_string()
                };
                let value = url_decode(&param[sep + 1..]);
                result.entry(name).or_insert(value);
            }
            None => {
                let name = if lowercase_names {
                    param.to_lowercase()
                } else {
                    param.to_string()
                };
                result.entry(name).or_default();
            }
        }
    }
    result
}

/// Appends a query parameter `add` using `'?'` or `'&'` (`qthelp::url_append_query_or_hash`).
pub fn url_append_query_or_hash(url: &str, add: &str) -> String {
    let Some(query) = url.rfind('?') else {
        return format!("{url}?{add}");
    };
    let hash = url.rfind('#');
    let sep = if hash.is_none_or(|h| query > h) {
        '&'
    } else {
        '?'
    };
    format!("{url}{sep}{add}")
}

/// Heuristic check for IPv6 addresses (`qthelp::is_ipv6`).
#[inline]
pub fn is_ipv6(ip: &str) -> bool {
    !ip.contains('.') && ip.contains(':')
}

/// Returns `true` if `url` looks like an email address (`UrlClickHandler::IsEmail`).
#[inline]
pub fn is_email(url: &str) -> bool {
    let Some(at) = url.find('@') else {
        return false;
    };
    if at == 0 {
        return false;
    }
    match url.find('/') {
        None => true,
        Some(slash) => slash > at,
    }
}

/// Encodes an external URL inside an internal wrapper (`UrlClickHandler::EncodeInternalWrappedUrl`).
pub fn encode_internal_wrapped_url(url: &str, extra_query: &str) -> String {
    let mut result = format!("internal:wrapped?url={}", url_encode(url));
    if !extra_query.is_empty() {
        result.push('&');
        result.push_str(extra_query);
    }
    result
}

/// Extracts the external URL from `"internal:wrapped?url=..."`, or returns empty string
/// (`UrlClickHandler::ExternalUrlFromInternalUrl`).
pub fn external_url_from_internal_url(url: &str) -> String {
    const PREFIX: &str = "internal:wrapped?";
    let Some(rest) = url.strip_prefix(PREFIX) else {
        return String::new();
    };
    url_parse_params(rest, false)
        .remove("url")
        .unwrap_or_default()
}

/// Normalizes a URL for opening (`UrlClickHandler::EncodeForOpening`).
pub fn encode_for_opening(original_url: &str) -> String {
    if is_email(original_url) {
        return original_url.to_string();
    }
    static TON_RE: OnceLock<Regex> = OnceLock::new();
    let ton_re = TON_RE.get_or_init(|| Regex::new(r"^[^/@:]+\.ton($|/)").expect("valid ton regex"));
    if ton_re.is_match(&original_url.to_lowercase()) {
        return format!("tonsite://{original_url}");
    }

    static SCHEME_RE: OnceLock<Regex> = OnceLock::new();
    let scheme_re = SCHEME_RE.get_or_init(|| Regex::new(r"^[a-zA-Z]+:").expect("valid scheme"));
    if !original_url.is_empty() && !scheme_re.is_match(original_url) {
        return format!("https://{original_url}");
    }
    original_url.to_string()
}

/// Strips leading `"https://"` or `"http://"` from `link` (`Ui::Text::StripUrlProtocol`).
#[inline]
pub fn strip_url_protocol(link: &str) -> &str {
    link.strip_prefix("https://")
        .or_else(|| link.strip_prefix("http://"))
        .unwrap_or(link)
}

#[inline]
fn is_ascii_digit_or_hyphen(ch: char) -> bool {
    ch.is_ascii_digit() || ch == '-'
}

fn is_plain_ascii_label(label: &str) -> bool {
    label
        .chars()
        .all(|c| c.is_ascii_alphabetic() || is_ascii_digit_or_hyphen(c))
}

fn domain_label_script(label: &str) -> Script {
    let mut result = Script::Common;
    for ch in label.chars() {
        if is_ascii_digit_or_hyphen(ch) {
            continue;
        }
        let script = ch.script();
        if matches!(script, Script::Common | Script::Inherited | Script::Unknown)
            || (result != Script::Common && result != script)
        {
            return Script::Unknown;
        }
        result = script;
    }
    result
}

/// Detects suspicious mixed-script / IDN homograph character ranges in a URL's domain
/// (`UrlClickHandler::SuspiciousRanges`). Returned offsets and lengths are in UTF-16 code units.
pub fn suspicious_ranges(url: &str) -> Vec<SuspiciousRange> {
    static CHECK1: OnceLock<Regex> = OnceLock::new();
    static CHECK2: OnceLock<Regex> = OnceLock::new();
    let check1 = CHECK1.get_or_init(|| {
        Regex::new(r"(?i)^((https?|s?ftp)://)?([^/#:\?]+)([/#:\?]|$)").expect("valid check1")
    });
    let check2 = CHECK2.get_or_init(|| Regex::new(r"^(.*)\.[a-zA-Z]+$").expect("valid check2"));

    let Some(caps) = check1.captures(url) else {
        return Vec::new();
    };
    let Some(domain_match) = caps.get(3) else {
        return Vec::new();
    };
    let domain = domain_match.as_str();
    let latin_tld = check2.is_match(domain);
    if !latin_tld && domain.is_ascii() {
        return Vec::new();
    }

    let tld = domain.rsplit('.').next().unwrap_or(domain);
    let script = if latin_tld {
        Script::Latin
    } else {
        domain_label_script(tld)
    };
    let ascii_only = matches!(script, Script::Latin | Script::Common | Script::Unknown);
    let allowed = |ch: char| -> bool {
        is_ascii_digit_or_hyphen(ch)
            || if ascii_only {
                ch.is_ascii_alphabetic()
            } else {
                ch.script() == script
            }
    };

    let mut result: Vec<SuspiciousRange> = Vec::new();
    let mut cursor_u16 = url[..domain_match.start()].encode_utf16().count();
    for (idx, label) in domain.split('.').enumerate() {
        if idx > 0 {
            cursor_u16 += 1; // '.'
        }
        let label_u16_len = label.encode_utf16().count();
        if !ascii_only && is_plain_ascii_label(label) {
            cursor_u16 += label_u16_len;
            continue;
        }
        let mut pos_u16 = cursor_u16;
        for ch in label.chars() {
            let len_u16 = ch.len_utf16();
            if !allowed(ch) {
                if let Some(last) = result.last_mut()
                    && last.from + last.length == pos_u16
                {
                    last.length += len_u16;
                    pos_u16 += len_u16;
                    continue;
                }
                result.push(SuspiciousRange {
                    from: pos_u16,
                    length: len_u16,
                });
            }
            pos_u16 += len_u16;
        }
        cursor_u16 += label_u16_len;
    }

    result
}

/// Returns `true` if `url` contains suspicious mixed-script characters in its domain
/// (`UrlClickHandler::IsSuspicious`).
#[inline]
pub fn is_suspicious(url: &str) -> bool {
    !suspicious_ranges(url).is_empty()
}
