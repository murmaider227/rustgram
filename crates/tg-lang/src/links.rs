//! Parsing and formatting of `tg://setlanguage` and `t.me/setlanguage/...` links.
//!
//! Port of `SetLanguage` and `t.me/setlanguage/` URL rewriting in
//! `Telegram/SourceFiles/core/local_url_handlers.cpp` and `Rows::share` in
//! `Telegram/SourceFiles/boxes/language_box.cpp`.

/// Parsed outcome of a Telegram language-pack link (`tg://setlanguage` or `t.me/setlanguage/...`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetLanguageLink {
    /// Bare `tg://setlanguage` (or `tg://setlanguage/`) with no `lang` parameter:
    /// opens the Language picker box (`ShowLanguagesBox` in `local_url_handlers.cpp:325-326`).
    OpenLanguageBox,
    /// `tg://setlanguage?lang=<code>` or `https://t.me/setlanguage/<code>`:
    /// fetches language info and opens the switch confirmation box
    /// (`CloudManager::switchWithWarning` in `local_url_handlers.cpp:328-329`).
    SwitchLanguage(String),
}

/// Formats a shareable `https://t.me/setlanguage/<lang_code>` URL (`Rows::share` in
/// `boxes/language_box.cpp:535`).
#[must_use]
pub fn format_set_language_link(lang_code: &str) -> String {
    format!("https://t.me/setlanguage/{lang_code}")
}

/// Validates a Telegram cloud language code against tdesktop's regex `[a-zA-Z0-9._-]+`
/// (`local_url_handlers.cpp:1875, 2131`).
#[must_use]
pub fn is_valid_language_code(code: &str) -> bool {
    !code.is_empty()
        && code
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-')
}

/// Parses a `tg://setlanguage?...` or `http(s)://t.me/setlanguage/...` (also `telegram.me`,
/// `telegram.dog`, or bare `t.me/setlanguage/...`) URL.
///
/// Returns `None` if `url` is not a valid language-pack link or carries an invalid language code.
#[must_use]
pub fn parse_set_language_link(url: &str) -> Option<SetLanguageLink> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return None;
    }

    // Strip any `#fragment` first.
    let without_fragment = trimmed.split('#').next().unwrap_or(trimmed);

    // 1. `tg://setlanguage...` (case-insensitive scheme).
    if let Some(rest) = strip_prefix_ignore_ascii_case(without_fragment, "tg://") {
        return parse_tg_setlanguage(rest);
    }

    // 2. `https://t.me/setlanguage/...`, `http://telegram.me/setlanguage/...`,
    //    `https://telegram.dog/setlanguage/...`, or bare `t.me/setlanguage/...`.
    let after_scheme = strip_prefix_ignore_ascii_case(without_fragment, "https://")
        .or_else(|| strip_prefix_ignore_ascii_case(without_fragment, "http://"))
        .unwrap_or(without_fragment);
    let after_www = strip_prefix_ignore_ascii_case(after_scheme, "www.").unwrap_or(after_scheme);

    let path = strip_host_prefix(after_www, "t.me/")
        .or_else(|| strip_host_prefix(after_www, "telegram.me/"))
        .or_else(|| strip_host_prefix(after_www, "telegram.dog/"))?;

    parse_web_setlanguage(path)
}

fn parse_tg_setlanguage(rest: &str) -> Option<SetLanguageLink> {
    let after_action = strip_prefix_ignore_ascii_case(rest, "setlanguage")?;
    let after_slash = after_action.strip_prefix('/').unwrap_or(after_action);

    if after_slash.is_empty() {
        return Some(SetLanguageLink::OpenLanguageBox);
    }

    let query = after_slash.strip_prefix('?')?;
    if query.is_empty() {
        return None;
    }

    // Match `local_url_handlers.cpp:1875`: `^setlanguage/?(\?lang=([a-zA-Z0-9._-]+))?(&|$)`
    // Also tolerate `&`-separated query parameters where `lang=...` appears anywhere.
    let mut found_lang: Option<String> = None;
    for pair in query.split('&') {
        if let Some((k, v)) = pair.split_once('=')
            && k.eq_ignore_ascii_case("lang")
        {
            let decoded = percent_decode(v)?;
            if !is_valid_language_code(&decoded) {
                return None;
            }
            found_lang = Some(decoded);
            break;
        }
    }
    found_lang.map(SetLanguageLink::SwitchLanguage)
}

fn parse_web_setlanguage(path: &str) -> Option<SetLanguageLink> {
    // Strip optional `?query` after the path (`local_url_handlers.cpp:2131`:
    // `^setlanguage/([a-zA-Z0-9._-]+)(\?|$)`).
    let path_only = path.split('?').next().unwrap_or(path);
    let after_prefix = strip_prefix_ignore_ascii_case(path_only, "setlanguage/")?;
    // Disallow extra path segments (`setlanguage/de/extra`) or trailing empty slug.
    if after_prefix.is_empty() || after_prefix.contains('/') {
        return None;
    }
    let decoded = percent_decode(after_prefix)?;
    if !is_valid_language_code(&decoded) {
        return None;
    }
    Some(SetLanguageLink::SwitchLanguage(decoded))
}

fn strip_prefix_ignore_ascii_case<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    if s.len() < prefix.len() || !s.is_char_boundary(prefix.len()) {
        return None;
    }
    if s[..prefix.len()].eq_ignore_ascii_case(prefix) {
        Some(&s[prefix.len()..])
    } else {
        None
    }
}

fn strip_host_prefix<'a>(s: &'a str, host_with_slash: &str) -> Option<&'a str> {
    strip_prefix_ignore_ascii_case(s, host_with_slash)
}

fn percent_decode(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return None;
            }
            let hi = hex_val(bytes[i + 1])?;
            let lo = hex_val(bytes[i + 2])?;
            out.push((hi << 4) | lo);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

const fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}
