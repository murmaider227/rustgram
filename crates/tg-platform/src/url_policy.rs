//! Shared external URL scheme and host validation policy.
//!
//! Ports tdesktop's `File::IsUrlSchemeAllowed` (`Telegram/SourceFiles/core/file_utilities.cpp:135`),
//! `HiddenUrlClickHandler::Open` scheme/host checks (`Telegram/SourceFiles/core/click_handler_types.cpp:134-210, 504-585`),
//! and `UrlClickHandler::SuspiciousRanges` (`Telegram/lib_ui/ui/basic_click_handlers.cpp:155-210`).

use std::fmt;

/// Allowlisted URL schemes permitted for external OS launching (`ShellExecuteW`).
///
/// Matches tdesktop's core allowlist while strictly excluding `file:`, UNC paths, local filesystem
/// paths, and Windows shell/Office/MSDT protocol handlers (`ms-msdt:`, `search-ms:`, etc.).
pub const ALLOWED_EXTERNAL_SCHEMES: &[&str] = &["http", "https", "tg", "ton", "tonsite", "mailto"];

/// The one Windows Settings page a Mini App may open (`web_app_open_location_settings`).
///
/// It is not a general `ms-settings:` allowance: only this exact URI passes
/// [`validate_windows_settings_uri`], and [`validate_external_url`] still rejects it.
pub const WINDOWS_LOCATION_SETTINGS_URI: &str = "ms-settings:privacy-location";

/// Accepts exactly [`WINDOWS_LOCATION_SETTINGS_URI`] (case-insensitive) and nothing else.
pub fn validate_windows_settings_uri(raw: &str) -> Result<String, UrlPolicyError> {
    let trimmed = raw.trim();
    if trimmed.eq_ignore_ascii_case(WINDOWS_LOCATION_SETTINGS_URI) {
        Ok(WINDOWS_LOCATION_SETTINGS_URI.to_owned())
    } else {
        Err(UrlPolicyError::DisallowedScheme(
            trimmed
                .split(':')
                .next()
                .unwrap_or_default()
                .to_ascii_lowercase(),
        ))
    }
}

/// Telegram-owned domains that do not trigger the hidden-link confirmation dialog when opened
/// over `http`/`https` (`DefaultWhitelistedDomains` in `core/click_handler_types.cpp:134-146`,
/// excluding short-link redirectors `t.me`/`telegram.me`/`telegram.dog` which still require
/// confirmation if they fall through to external opening).
pub const TRUSTED_TELEGRAM_DOMAINS: &[&str] = &[
    "telegram.org",
    "telegra.ph",
    "te.legra.ph",
    "graph.org",
    "telesco.pe",
    "fragment.com",
];

/// Short-link Telegram hosts that always require confirmation when hidden if not routed internally
/// (`IsTelegramShortLinkHost` in `core/click_handler_types.cpp:193-200`).
const TELEGRAM_SHORT_LINK_DOMAINS: &[&str] = &["t.me", "telegram.me", "telegram.dog"];

/// Reason why a URL was rejected by [`validate_external_url`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UrlPolicyError {
    /// Input was empty or whitespace-only.
    Empty,
    /// Input contained ASCII control characters (`< 0x20` or `0x7F`).
    ControlCharacters,
    /// Input is a local filesystem path or UNC share (`\\host\share`, `C:\...`, `/...`, `file:`).
    LocalOrUncPath,
    /// Input had no valid `scheme:` prefix.
    MissingScheme,
    /// Input used a scheme outside [`ALLOWED_EXTERNAL_SCHEMES`].
    DisallowedScheme(String),
    /// Input had a malformed or empty authority/host for its scheme.
    InvalidAuthority,
    /// Input carried `user[:password]@` before the host (`https://bank.example@evil.example`).
    UserInfo,
}

impl fmt::Display for UrlPolicyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("URL is empty"),
            Self::ControlCharacters => f.write_str("URL contains control characters"),
            Self::LocalOrUncPath => f.write_str("local file and UNC paths are not allowed"),
            Self::MissingScheme => f.write_str("URL is missing a scheme"),
            Self::DisallowedScheme(s) => write!(f, "URL scheme '{s}:' is not allowed"),
            Self::InvalidAuthority => f.write_str("URL has an invalid or empty host"),
            Self::UserInfo => f.write_str("URL must not contain user info before the host"),
        }
    }
}

impl std::error::Error for UrlPolicyError {}

/// Validates `raw` against the shared external URL policy and returns the trimmed URL if allowed.
///
/// Rejects:
/// - empty strings and strings containing ASCII control characters;
/// - UNC paths (`\\host\share`, `//host/share`) and local filesystem paths (`C:\...`, `/...`, `.\...`);
/// - `file:` and any scheme not in [`ALLOWED_EXTERNAL_SCHEMES`] (`http`, `https`, `tg`, `ton`,
///   `tonsite`, `mailto`);
/// - authority-based URLs (`http`, `https`, `tg`, `ton`, `tonsite`) with missing `://`, backslashes
///   in the authority, `user@` info, or an empty host.
///
/// The returned URL is normalized for `ShellExecuteW`: spaces, quotes, `<>{}|^` and backticks
/// anywhere, and non-ASCII characters after the host, are percent-encoded (like `QUrl::toEncoded`),
/// so the shell receives one URL token and never a second argument.
pub fn validate_external_url(raw: &str) -> Result<String, UrlPolicyError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(UrlPolicyError::Empty);
    }
    if trimmed.bytes().any(|b| b < 0x20 || b == 0x7F) {
        return Err(UrlPolicyError::ControlCharacters);
    }

    // Reject UNC paths (`\\host\share`, `//host/share`) and absolute/relative local paths.
    if trimmed.starts_with("\\\\")
        || trimmed.starts_with("//")
        || trimmed.starts_with('/')
        || trimmed.starts_with('\\')
        || trimmed.starts_with('.')
    {
        return Err(UrlPolicyError::LocalOrUncPath);
    }

    // Reject Windows drive-letter paths (`C:\...`, `C:/...`, `C:file.exe`).
    let bytes = trimmed.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return Err(UrlPolicyError::LocalOrUncPath);
    }

    let Some(colon) = trimmed.find(':') else {
        return Err(UrlPolicyError::MissingScheme);
    };
    let scheme = &trimmed[..colon];
    if scheme.len() < 2
        || !scheme.as_bytes()[0].is_ascii_alphabetic()
        || !scheme
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'))
    {
        return Err(UrlPolicyError::MissingScheme);
    }

    let scheme_lower = scheme.to_ascii_lowercase();
    if scheme_lower == "file" {
        return Err(UrlPolicyError::LocalOrUncPath);
    }
    if !ALLOWED_EXTERNAL_SCHEMES.contains(&scheme_lower.as_str()) {
        return Err(UrlPolicyError::DisallowedScheme(scheme_lower));
    }

    let after_colon = &trimmed[colon + 1..];
    match scheme_lower.as_str() {
        "mailto" => {
            if after_colon.is_empty()
                || after_colon.starts_with('/')
                || after_colon.starts_with('\\')
                || after_colon.contains('\\')
            {
                return Err(UrlPolicyError::InvalidAuthority);
            }
        }
        "http" | "https" | "tg" | "ton" | "tonsite" => {
            let Some(after_slashes) = after_colon.strip_prefix("//") else {
                return Err(UrlPolicyError::InvalidAuthority);
            };
            let auth_end = after_slashes
                .find(['/', '?', '#'])
                .unwrap_or(after_slashes.len());
            let authority = &after_slashes[..auth_end];
            if authority.is_empty() || authority.contains('\\') {
                return Err(UrlPolicyError::InvalidAuthority);
            }
            if authority.contains('@') {
                return Err(UrlPolicyError::UserInfo);
            }
            if authority.bytes().any(|b| SHELL_UNSAFE_ASCII.contains(&b)) {
                return Err(UrlPolicyError::InvalidAuthority);
            }
            if extract_host_slice(authority).is_none() {
                return Err(UrlPolicyError::InvalidAuthority);
            }
            let rest_start = colon + 1 + 2 + auth_end;
            return Ok(format!(
                "{}{}",
                &trimmed[..rest_start],
                percent_encode_unsafe(&trimmed[rest_start..], true)
            ));
        }
        _ => return Err(UrlPolicyError::DisallowedScheme(scheme_lower)),
    }

    Ok(percent_encode_unsafe(trimmed, false))
}

/// ASCII characters that are not valid unescaped in a URL and can split a shell argument.
const SHELL_UNSAFE_ASCII: &[u8] = b" \"<>`{}|^";

/// Percent-encodes [`SHELL_UNSAFE_ASCII`] and, when `encode_non_ascii` is set, every non-ASCII
/// character (as UTF-8 bytes), leaving existing `%XX` escapes alone.
fn percent_encode_unsafe(part: &str, encode_non_ascii: bool) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(part.len());
    for ch in part.chars() {
        let unsafe_ascii = ch.is_ascii() && SHELL_UNSAFE_ASCII.contains(&(ch as u8));
        if unsafe_ascii || (encode_non_ascii && !ch.is_ascii()) {
            let mut buf = [0u8; 4];
            for b in ch.encode_utf8(&mut buf).bytes() {
                let _ = write!(out, "%{b:02X}");
            }
        } else {
            out.push(ch);
        }
    }
    out
}

/// Returns `true` if `path` is an absolute path on a local drive (`C:\dir\file`, or `/dir/file`
/// outside Windows). Relative paths (`..\secret.txt`), UNC shares (`\\host\share`), device
/// and verbatim paths (`\\?\`, `\\.\`) and drive-relative paths (`C:file`) are refused, so a
/// `-sendpath` activation can neither reach outside what the user named nor touch the network.
#[must_use]
pub fn is_local_absolute_path(path: &std::path::Path) -> bool {
    let text = path.to_string_lossy();
    if text.starts_with("\\\\") || text.starts_with("//") || text.starts_with("\\/") {
        return false;
    }
    if text.bytes().any(|b| b < 0x20) {
        return false;
    }
    let b = text.as_bytes();
    if b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && matches!(b[2], b'\\' | b'/') {
        return true;
    }
    cfg!(not(windows)) && text.starts_with('/')
}

/// Returns `true` if `raw` passes [`validate_external_url`].
#[must_use]
pub fn is_external_url_allowed(raw: &str) -> bool {
    validate_external_url(raw).is_ok()
}

/// Extracts the UTF-8 byte range `(start, end)` of the host inside `url`, if `url` has `://`.
///
/// Strips optional `userinfo@` and `:port` (preserving IPv6 `[...]` literals).
#[must_use]
pub fn extract_url_host_range(url: &str) -> Option<(usize, usize)> {
    let scheme_sep = url.find("://")?;
    let auth_start = scheme_sep + 3;
    let rest = &url[auth_start..];
    let auth_end_rel = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..auth_end_rel];
    let (rel_start, rel_end) = extract_host_slice(authority)?;
    Some((auth_start + rel_start, auth_start + rel_end))
}

fn extract_host_slice(authority: &str) -> Option<(usize, usize)> {
    let host_offset = authority.rfind('@').map_or(0, |at| at + 1);
    let host_port = &authority[host_offset..];
    if host_port.is_empty() {
        return None;
    }
    if host_port.starts_with('[') {
        let close = host_port.find(']')?;
        if close <= 1 {
            return None;
        }
        return Some((host_offset, host_offset + close + 1));
    }
    let port_sep = host_port.find(':').unwrap_or(host_port.len());
    let host = &host_port[..port_sep];
    if host.is_empty() {
        return None;
    }
    Some((host_offset, host_offset + port_sep))
}

/// Returns `true` if `host` equals `domain` or is a subdomain (`*.domain`) of any entry in `list`.
#[must_use]
pub fn host_in_domains(host: &str, domains: &[&str]) -> bool {
    let normalized = host.trim_end_matches('.').to_ascii_lowercase();
    if normalized.is_empty() {
        return false;
    }
    domains.iter().any(|domain| {
        let d = domain.to_ascii_lowercase();
        !d.is_empty()
            && (normalized == d
                || (normalized.len() > d.len()
                    && normalized.ends_with(&d)
                    && normalized.as_bytes()[normalized.len() - d.len() - 1] == b'.'))
    })
}

/// Returns `true` if a hidden link (`messageEntityTextUrl`) targeting `url` requires the
/// `"Open this link?"` confirmation dialog before opening externally
/// (`HiddenUrlRequiresConfirmation` in `core/click_handler_types.cpp:202-209`).
///
/// Assumes `url` has already passed [`validate_external_url`].
#[must_use]
pub fn hidden_url_requires_confirmation(url: &str) -> bool {
    let trimmed = url.trim();
    let Some(colon) = trimmed.find(':') else {
        return true;
    };
    let scheme = trimmed[..colon].to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return true;
    }
    let Some((start, end)) = extract_url_host_range(trimmed) else {
        return true;
    };
    let host = &trimmed[start..end];
    if host_in_domains(host, TELEGRAM_SHORT_LINK_DOMAINS) {
        return true;
    }
    !host_in_domains(host, TRUSTED_TELEGRAM_DOMAINS)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CoarseScript {
    Latin,
    Cyrillic,
    Greek,
    Arabic,
    Hebrew,
    Cjk,
    Other,
}

fn coarse_script(ch: char) -> CoarseScript {
    let c = ch as u32;
    if ch.is_ascii_alphabetic() || (0x00C0..=0x024F).contains(&c) || (0x1E00..=0x1EFF).contains(&c)
    {
        CoarseScript::Latin
    } else if (0x0400..=0x052F).contains(&c) {
        CoarseScript::Cyrillic
    } else if (0x0370..=0x03FF).contains(&c) || (0x1F00..=0x1FFF).contains(&c) {
        CoarseScript::Greek
    } else if (0x0600..=0x06FF).contains(&c) {
        CoarseScript::Arabic
    } else if (0x0590..=0x05FF).contains(&c) {
        CoarseScript::Hebrew
    } else if (0x3040..=0x30FF).contains(&c)
        || (0x4E00..=0x9FFF).contains(&c)
        || (0xAC00..=0xD7AF).contains(&c)
    {
        CoarseScript::Cjk
    } else {
        CoarseScript::Other
    }
}

/// Finds UTF-8 byte ranges inside `url`'s host that contain mixed-script / confusable characters
/// (`UrlClickHandler::SuspiciousRanges` in `lib_ui/ui/basic_click_handlers.cpp:160-210`).
#[must_use]
pub fn find_suspicious_host_ranges(url: &str) -> Vec<(usize, usize)> {
    let Some((host_start, host_end)) = extract_url_host_range(url) else {
        return Vec::new();
    };
    let host = &url[host_start..host_end];
    if host.is_empty() || host.starts_with('[') {
        return Vec::new();
    }

    let tld = host.rsplit('.').next().unwrap_or("");
    let latin_tld = !tld.is_empty() && tld.bytes().all(|b| b.is_ascii_alphabetic());
    if !latin_tld && host.is_ascii() {
        return Vec::new();
    }

    let tld_script = if latin_tld {
        CoarseScript::Latin
    } else {
        tld.chars()
            .find(|c| !c.is_ascii_digit() && *c != '-')
            .map_or(CoarseScript::Latin, coarse_script)
    };
    let ascii_only = tld_script == CoarseScript::Latin || tld_script == CoarseScript::Other;

    let mut ranges: Vec<(usize, usize)> = Vec::new();
    let mut label_offset = host_start;
    for label in host.split('.') {
        let is_plain_ascii = label
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-');
        if !ascii_only && is_plain_ascii {
            label_offset += label.len() + 1;
            continue;
        }
        for (rel, ch) in label.char_indices() {
            let allowed = ch.is_ascii_digit()
                || ch == '-'
                || if ascii_only {
                    ch.is_ascii_alphabetic()
                } else {
                    coarse_script(ch) == tld_script
                };
            if !allowed {
                let from = label_offset + rel;
                let to = from + ch.len_utf8();
                if let Some(last) = ranges.last_mut()
                    && last.1 == from
                {
                    last.1 = to;
                } else {
                    ranges.push((from, to));
                }
            }
        }
        label_offset += label.len() + 1;
    }

    ranges
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sec2a_l7_shell_metacharacters_are_encoded_and_userinfo_rejected() {
        assert_eq!(
            validate_external_url("https://example.com/a b\"c<d>").as_deref(),
            Ok("https://example.com/a%20b%22c%3Cd%3E")
        );
        assert_eq!(
            validate_external_url("https://example.com/caf\u{e9}?q=\u{202e}x").as_deref(),
            Ok("https://example.com/caf%C3%A9?q=%E2%80%AEx")
        );
        assert_eq!(
            validate_external_url("mailto:a b@example.com").as_deref(),
            Ok("mailto:a%20b@example.com")
        );
        assert_eq!(
            validate_external_url("https://bank.example@evil.example/login"),
            Err(UrlPolicyError::UserInfo)
        );
        assert_eq!(
            validate_external_url("https://user:pw@evil.example/"),
            Err(UrlPolicyError::UserInfo)
        );
        assert!(validate_external_url("https://ev il.example/").is_err());
        assert_eq!(
            validate_external_url("https://example.com/already%20encoded").as_deref(),
            Ok("https://example.com/already%20encoded")
        );
    }

    #[test]
    fn sec2a_l11_only_local_absolute_paths() {
        use std::path::Path;
        assert!(is_local_absolute_path(Path::new(r"C:\Users\a\file.txt")));
        assert!(is_local_absolute_path(Path::new("D:/docs/report.pdf")));
        for bad in [
            r"..\secret.txt",
            "secret.txt",
            r".\x.txt",
            "C:relative.txt",
            r"\\host\share\x.txt",
            "//host/share/x.txt",
            r"\\?\C:\x.txt",
            r"\\.\pipe\x",
            "",
        ] {
            assert!(!is_local_absolute_path(Path::new(bad)), "{bad:?}");
        }
    }

    #[test]
    fn sec2a_l8_only_the_location_settings_uri_is_allowed() {
        assert_eq!(
            validate_windows_settings_uri("ms-settings:privacy-location").as_deref(),
            Ok(WINDOWS_LOCATION_SETTINGS_URI)
        );
        for bad in [
            "ms-settings:privacy-webcam",
            "ms-settings:",
            "ms-msdt:/id x",
            "https://example.com",
        ] {
            assert!(validate_windows_settings_uri(bad).is_err(), "{bad}");
        }
        assert!(validate_external_url(WINDOWS_LOCATION_SETTINGS_URI).is_err());
    }

    #[test]
    fn allowlist_accepts_expected_schemes() {
        for ok in [
            "https://example.com/path?a=1",
            "http://example.org",
            "HTTPS://RUST-LANG.ORG/learn",
            "tg://resolve?domain=telegram",
            "ton://transfer/EQC123",
            "tonsite://foundation.ton/docs",
            "mailto:user@example.com",
        ] {
            assert!(
                validate_external_url(ok).is_ok(),
                "expected allowed URL: {ok}"
            );
        }
    }

    #[test]
    fn policy_rejects_local_unc_and_disallowed_schemes() {
        for bad in [
            "",
            "   ",
            "file:///C:/Windows/System32/calc.exe",
            "FILE://host/share/x.exe",
            r"\\host\share\x.exe",
            "//host/share/x.exe",
            r"C:\Windows\System32\cmd.exe",
            "C:/Windows/x.dll",
            "c:relative.exe",
            "/etc/passwd",
            r".\evil.bat",
            "ms-msdt:/id PCWDiagnostic /skip force",
            "ms-word:ofe|u|https://evil.example/doc.docx",
            "search-ms:query=test",
            "javascript:alert(1)",
            "vbscript:msgbox(1)",
            "data:text/html,<script>alert(1)</script>",
            "ftp://example.com/file",
            "https://example.com/a\r\nb",
            r"https://evil\host/x",
            "https://",
            "mailto:",
            "mailto:/etc/passwd",
        ] {
            assert!(
                validate_external_url(bad).is_err(),
                "expected rejected URL: {bad}"
            );
        }
    }

    #[test]
    fn hidden_url_confirmation_policy() {
        assert!(!hidden_url_requires_confirmation(
            "https://telegram.org/faq"
        ));
        assert!(!hidden_url_requires_confirmation("https://telegra.ph/page"));
        assert!(!hidden_url_requires_confirmation(
            "https://sub.fragment.com/x"
        ));
        assert!(hidden_url_requires_confirmation(
            "https://evil.example/login"
        ));
        assert!(hidden_url_requires_confirmation(
            "https://nottelegram.org/faq"
        ));
        assert!(hidden_url_requires_confirmation("https://t.me/some_short"));
        assert!(hidden_url_requires_confirmation("mailto:bob@example.com"));
        assert!(hidden_url_requires_confirmation("ton://transfer/abc"));
    }

    #[test]
    fn suspicious_mixed_script_host_detected() {
        // Cyrillic 'а' (U+0430) inside a `.com` domain.
        let spoof = "https://p\u{0430}ypal.com/login";
        let ranges = find_suspicious_host_ranges(spoof);
        assert_eq!(ranges.len(), 1);
        assert_eq!(&spoof[ranges[0].0..ranges[0].1], "\u{0430}");

        // Pure ASCII domain has no suspicious ranges.
        assert!(find_suspicious_host_ranges("https://example.com/path").is_empty());
    }
}
