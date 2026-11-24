//! Port of bot deep-link and start-parameter parsing from `core/local_url_handlers.cpp`.
//!
//! Handles converting `t.me` / `telegram.me` / `telegram.dog` / `<user>.t.me` URLs
//! and `tg://resolve?...` links into structured bot launch actions (`?start=`,
//! `?startgroup=`, `?startchannel`, `?startapp=`, `?startattach=`, `?game=`,
//! direct mini apps `t.me/bot/app`, `t.me/newbot/...`, and OAuth links).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::markup::{PeerTypes, parse_choose_types};
use crate::stand_in::ChatAdminRights;

/// Maximum URL length accepted by tdesktop's `Core::TryConvertUrlToLocal`.
pub const MAX_LOCAL_URL_LEN: usize = 8192;

/// Maximum length of a standard bot `?start=` parameter per Telegram Bot API.
pub const MAX_START_PARAM_LEN: usize = 64;

/// Maximum length of a Mini App `?startapp=` parameter per Telegram Mini Apps docs.
pub const MAX_STARTAPP_PARAM_LEN: usize = 512;

/// Errors returned when validating a bot start parameter.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum StartParamError {
    /// Parameter is empty.
    #[error("start parameter must not be empty")]
    Empty,
    /// Parameter exceeds the allowed maximum length.
    #[error("start parameter length {len} exceeds maximum {max}")]
    TooLong {
        /// Actual byte length.
        len: usize,
        /// Maximum permitted byte length.
        max: usize,
    },
    /// Parameter contains a disallowed character (only ASCII `A-Z`, `a-z`, `0-9`, `_`, `-` are allowed).
    #[error("start parameter contains invalid character '{ch}' at byte offset {offset}")]
    InvalidChar {
        /// First invalid character.
        ch: char,
        /// Byte offset of the invalid character.
        offset: usize,
    },
}

/// Validates a bot `?start=` / `?startgroup=` payload (`1..=64` ASCII `[A-Za-z0-9_-]`).
pub fn validate_start_param(param: &str) -> Result<(), StartParamError> {
    validate_param_with_max(param, MAX_START_PARAM_LEN)
}

/// Validates a Mini App `?startapp=` / `?startattach=` payload (`1..=512` ASCII `[A-Za-z0-9_-]`).
pub fn validate_startapp_param(param: &str) -> Result<(), StartParamError> {
    validate_param_with_max(param, MAX_STARTAPP_PARAM_LEN)
}

fn validate_param_with_max(param: &str, max: usize) -> Result<(), StartParamError> {
    if param.is_empty() {
        return Err(StartParamError::Empty);
    }
    if param.len() > max {
        return Err(StartParamError::TooLong {
            len: param.len(),
            max,
        });
    }
    for (offset, ch) in param.char_indices() {
        if !(ch.is_ascii_alphanumeric() || ch == '_' || ch == '-') {
            return Err(StartParamError::InvalidChar { ch, offset });
        }
    }
    Ok(())
}

/// Decodes URL percent-encoding (`%XX`), leaving malformed sequences intact.
/// Does not convert `+` into space unless `plus_as_space` is `true`.
pub fn percent_decode(input: &str, plus_as_space: bool) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2]))
        {
            out.push((hi << 4) | lo);
            i += 3;
            continue;
        }
        if plus_as_space && bytes[i] == b'+' {
            out.push(b' ');
        } else {
            out.push(bytes[i]);
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

const fn hex_val(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Parses URL query parameters into a map with lowercase keys, matching
/// tdesktop's `qthelp::url_parse_params(..., ToLower)`.
///
/// Keys without `=` are stored with an empty string value so `contains_key`
/// distinguishes `?startgroup` (present, empty) from absent.
pub fn parse_query_params(query: &str) -> BTreeMap<String, String> {
    let mut result = BTreeMap::new();
    let trimmed = query.split('#').next().unwrap_or("");
    for pair in trimmed.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (raw_key, raw_val) = match pair.split_once('=') {
            Some((k, v)) => (k, v),
            None => (pair, ""),
        };
        let key = percent_decode(raw_key, true).to_ascii_lowercase();
        let val = percent_decode(raw_val, false);
        result.insert(key, val);
    }
    result
}

/// Parses the `admin=` query parameter for `?startgroup` and `?startchannel` links.
///
/// Port of `ParseRequestedAdminRights` in `core/local_url_handlers.cpp:501-546`.
/// Tokens are separated by `+` or ASCII space.
pub fn parse_requested_admin_rights(value: &str) -> ChatAdminRights {
    let mut result = ChatAdminRights::empty();
    for element in value.split(['+', ' ']) {
        match element {
            "change_info" => result |= ChatAdminRights::CHANGE_INFO,
            "post_messages" => result |= ChatAdminRights::POST_MESSAGES,
            "edit_messages" => result |= ChatAdminRights::EDIT_MESSAGES,
            "delete_messages" => result |= ChatAdminRights::DELETE_MESSAGES,
            "restrict_members" => result |= ChatAdminRights::BAN_USERS,
            "invite_users" => result |= ChatAdminRights::INVITE_BY_LINK_OR_ADD,
            "manage_topics" => result |= ChatAdminRights::MANAGE_TOPICS,
            "pin_messages" => result |= ChatAdminRights::PIN_MESSAGES,
            "promote_members" => result |= ChatAdminRights::ADD_ADMINS,
            "post_stories" => result |= ChatAdminRights::POST_STORIES,
            "edit_stories" => result |= ChatAdminRights::EDIT_STORIES,
            "delete_stories" => result |= ChatAdminRights::DELETE_STORIES,
            "manage_video_chats" => result |= ChatAdminRights::MANAGE_CALL,
            "manage_direct_messages" => result |= ChatAdminRights::MANAGE_DIRECT,
            "manage_welcome_messages" => result |= ChatAdminRights::MANAGE_WELCOME_MESSAGES,
            "anonymous" => result |= ChatAdminRights::ANONYMOUS,
            "manage_chat" => result |= ChatAdminRights::OTHER,
            _ => {}
        }
    }
    result
}

/// Returns `true` if `s` is a valid Telegram username/domain token (`^[a-zA-Z0-9._]+$`),
/// matching `validDomain` in `core/local_url_handlers.cpp:683-689`.
pub fn is_valid_domain(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_')
}

/// Returns `true` if `s` is a valid short app name segment (`^[a-zA-Z0-9._-]+$`).
fn is_valid_app_name(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-')
}

/// Returns `true` if `s` is a valid subdomain username (`^[a-zA-Z0-9_]+$`, len > 1, not `"www"`).
fn is_valid_subdomain(s: &str) -> bool {
    s.len() > 1
        && !s.eq_ignore_ascii_case("www")
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// Converts Telegram web URLs (`t.me/...`, `telegram.me/...`, `telegram.dog/...`,
/// `<username>.t.me/...`) to their canonical local `tg://` form for bot-relevant links.
///
/// Port of the username/bot/newbot/app branches of `Core::TryConvertUrlToLocal`
/// in `core/local_url_handlers.cpp:2060-2264`.
pub fn try_convert_url_to_local(url: &str) -> String {
    let url = if url.len() > MAX_LOCAL_URL_LEN {
        // Respect UTF-8 char boundaries when truncating.
        let mut end = MAX_LOCAL_URL_LEN;
        while end > 0 && !url.is_char_boundary(end) {
            end -= 1;
        }
        &url[..end]
    } else {
        url
    };

    let (protocol, rest) = strip_http_scheme(url);

    // Handle `<username>.t.me[/...][?...]` subdomains.
    if let Some(dot_idx) = rest.find(".t.me") {
        let name = &rest[..dot_idx];
        let after = &rest[dot_idx + 5..];
        let tail = after.split('#').next().unwrap_or("");
        if is_valid_subdomain(name)
            && (tail.is_empty() || tail.starts_with('/') || tail.starts_with('?'))
        {
            let rebuilt = format!("{protocol}t.me/{name}{tail}");
            let converted = try_convert_url_to_local(&rebuilt);
            if converted.starts_with("tg://resolve?domain=") {
                return converted;
            }
            return url.to_string();
        }
    }

    // Handle `(www.)?(t.me|telegram.me|telegram.dog)/<path>`
    let without_www = strip_prefix_ignore_ascii_case(rest, "www.").unwrap_or(rest);
    let path_and_query = strip_prefix_ignore_ascii_case(without_www, "t.me/")
        .or_else(|| strip_prefix_ignore_ascii_case(without_www, "telegram.me/"))
        .or_else(|| strip_prefix_ignore_ascii_case(without_www, "telegram.dog/"));

    let Some(query_str) = path_and_query else {
        return url.to_string();
    };
    if query_str.is_empty() {
        return url.to_string();
    }

    // Split off `#fragment`.
    let no_frag = query_str.split('#').next().unwrap_or("");
    let (raw_path, raw_params) = match no_frag.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (no_frag, None),
    };
    let path = raw_path.trim_end_matches('/');

    // `t.me/newbot/<manager>[/<username>][?<params>]`
    if let Some(after_newbot) = strip_prefix_ignore_ascii_case(path, "newbot/") {
        let mut parts = after_newbot.split('/');
        let manager = parts.next().unwrap_or("");
        let username = parts.next().unwrap_or("");
        if is_valid_domain(manager)
            && parts.next().is_none()
            && (username.is_empty() || is_valid_domain(username))
        {
            let mut out = format!("tg://newbot?manager={manager}");
            if !username.is_empty() {
                out.push_str("&username=");
                out.push_str(username);
            }
            if let Some(p) = raw_params
                && !p.is_empty()
            {
                out.push('&');
                out.push_str(p);
            }
            return out;
        }
    }

    // Exclude non-username top-level routes handled separately in tdesktop.
    let mut segments = path.split('/');
    let first = segments.next().unwrap_or("");
    if !is_valid_domain(first) {
        return url.to_string();
    }
    let reserved = [
        "joinchat",
        "addlist",
        "addstickers",
        "addemoji",
        "addtheme",
        "addstyle",
        "setlanguage",
        "share",
        "confirmphone",
        "iv",
        "socks",
        "proxy",
        "webproxy",
        "invoice",
        "bg",
        "m",
        "nft",
        "auction",
        "call",
        "sendgrams",
        "c",
        "contact",
        "getpremium",
        "boost",
    ];
    if reserved.iter().any(|r| first.eq_ignore_ascii_case(r)) {
        return url.to_string();
    }

    let second = segments.next();
    let third = segments.next();
    if segments.next().is_some() {
        return url.to_string();
    }

    let mut out = format!("tg://resolve?domain={first}");
    match (second, third) {
        (None, None) => {}
        (Some(seg2), None) => {
            if seg2.bytes().all(|b| b.is_ascii_digit()) {
                out.push_str("&post=");
                out.push_str(seg2);
            } else if is_valid_app_name(seg2) {
                out.push_str("&appname=");
                out.push_str(seg2);
            } else {
                return url.to_string();
            }
        }
        (Some(seg2), Some(seg3)) => {
            if seg2.bytes().all(|b| b.is_ascii_digit()) && seg3.bytes().all(|b| b.is_ascii_digit())
            {
                out.push_str("&topic=");
                out.push_str(seg2);
                out.push_str("&post=");
                out.push_str(seg3);
            } else if seg2 == "s" && (seg3 == "live" || seg3.bytes().all(|b| b.is_ascii_digit())) {
                out.push_str("&story=");
                out.push_str(seg3);
            } else if seg2 == "a" && seg3.bytes().all(|b| b.is_ascii_digit()) {
                out.push_str("&album=");
                out.push_str(seg3);
            } else if seg2 == "c" && seg3.bytes().all(|b| b.is_ascii_digit()) {
                out.push_str("&collection=");
                out.push_str(seg3);
            } else {
                return url.to_string();
            }
        }
        (None, Some(_)) => unreachable!(),
    }

    if let Some(p) = raw_params
        && !p.is_empty()
    {
        out.push('&');
        out.push_str(p);
    }
    out
}

/// Returns `true` if `url` resolves to a Mini App link (`appname`, `startapp`, or `attach`),
/// matching `Core::IsMiniAppUrl` in `core/local_url_handlers.cpp:2272-2284`.
pub fn is_mini_app_url(url: &str) -> bool {
    let local = try_convert_url_to_local(url);
    let Some(query) = strip_prefix_ignore_ascii_case(&local, "tg://resolve?") else {
        return false;
    };
    let params = parse_query_params(query);
    params.contains_key("appname")
        || params.contains_key("startapp")
        || params.contains_key("attach")
}

/// Structured result of parsing a bot-related Telegram deep link.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BotDeepLink {
    /// `t.me/bot?start=<payload>` (optionally carrying a `ref` referral code).
    StartBot {
        /// Target bot username (without `@`).
        username: String,
        /// Start payload (`start=` parameter).
        start_param: String,
        /// Referral token from `ref=` or matched start referral prefix.
        referral: Option<String>,
    },
    /// `t.me/bot?startgroup[=<payload>][&admin=...]`.
    StartGroup {
        /// Target bot username.
        username: String,
        /// Optional start payload passed after adding the bot to a group.
        start_param: Option<String>,
        /// Requested administrator rights in the group.
        admin_rights: ChatAdminRights,
    },
    /// `t.me/bot?startchannel[&admin=...]`.
    StartChannel {
        /// Target bot username.
        username: String,
        /// Requested administrator rights in the channel.
        admin_rights: ChatAdminRights,
    },
    /// `t.me/bot?startapp[=<payload>][&mode=compact|fullscreen]` (main Mini App).
    StartMainApp {
        /// Target bot username.
        username: String,
        /// Optional `startapp` payload.
        start_param: Option<String>,
        /// True when `mode=compact` was requested.
        compact: bool,
        /// True when `mode=fullscreen` was requested.
        fullscreen: bool,
    },
    /// `t.me/bot/<appname>[?startapp=<payload>][&mode=fullscreen]` (named direct Mini App).
    DirectMiniApp {
        /// Target bot username.
        username: String,
        /// Mini App short name.
        app_name: String,
        /// Optional `startapp` (or `start`) payload.
        start_param: Option<String>,
        /// True when `mode=fullscreen` was requested.
        fullscreen: bool,
    },
    /// `t.me/bot?startattach[=<payload>][&choose=...]` or `t.me/peer?attach=bot[&startattach=...]`.
    StartAttach {
        /// Target domain (bot username, or chat username when `attach_username` is set).
        username: String,
        /// Optional `startattach` payload.
        start_param: Option<String>,
        /// Allowed peer types from `choose=` parameter.
        choose_types: PeerTypes,
        /// Optional explicit attach bot username (`attach=`).
        attach_username: Option<String>,
        /// True when `mode=fullscreen` was requested.
        fullscreen: bool,
    },
    /// `t.me/bot?game=<short_name>`.
    ShareGame {
        /// Bot username hosting the game.
        username: String,
        /// Game short name.
        game_short_name: String,
    },
    /// `t.me/newbot/<manager>[/<username>][?name=...]`.
    NewBot {
        /// Parent manager bot username.
        manager: String,
        /// Optional suggested username.
        username: Option<String>,
        /// Optional suggested display name (`name=`).
        name: Option<String>,
    },
    /// `t.me/oauth?startapp=<token>` / `tg://resolve?domain=oauth&startapp=<token>`.
    OAuthLogin {
        /// OAuth startapp token.
        token: String,
    },
    /// Plain `t.me/<username>` without bot-specific query parameters.
    PeerOnly {
        /// Target username.
        username: String,
    },
}

/// Parses a Telegram bot deep link (`https://t.me/...`, `t.me/...`, `telegram.me/...`,
/// `telegram.dog/...`, `<user>.t.me/...`, or `tg://...`).
///
/// `start_ref_prefixes` corresponds to `AppConfig::startRefPrefixes()` in tdesktop
/// (for extracting star-referral codes embedded in `?start=`). Pass `&[]` when unused.
///
/// Port of `ResolveUsernameOrPhone` in `core/local_url_handlers.cpp:619-824`.
pub fn parse_bot_deep_link(url: &str, start_ref_prefixes: &[&str]) -> Option<BotDeepLink> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return None;
    }
    let local = try_convert_url_to_local(trimmed);

    // `tg://newbot?...`
    if let Some(q) = strip_prefix_ignore_ascii_case(&local, "tg://newbot?")
        .or_else(|| strip_prefix_ignore_ascii_case(&local, "tg://newbot/?"))
    {
        let params = parse_query_params(q);
        let manager = params.get("manager")?.clone();
        if !is_valid_domain(&manager) {
            return None;
        }
        let username = params.get("username").filter(|s| !s.is_empty()).cloned();
        let name = params.get("name").filter(|s| !s.is_empty()).cloned();
        return Some(BotDeepLink::NewBot {
            manager,
            username,
            name,
        });
    }

    // `tg://resolve?...`
    let query = strip_prefix_ignore_ascii_case(&local, "tg://resolve?")
        .or_else(|| strip_prefix_ignore_ascii_case(&local, "tg://resolve/?"))?;
    let params = parse_query_params(query);

    let domain_param = params.get("domain").map(String::as_str).unwrap_or("");
    let appname_param = params.get("appname").map(String::as_str).unwrap_or("");

    // `t.me/oauth?startapp=<token>`
    if domain_param.eq_ignore_ascii_case("oauth") {
        if let Some(token) = params.get("startapp").filter(|s| !s.is_empty()) {
            return Some(BotDeepLink::OAuthLogin {
                token: token.clone(),
            });
        }
        return None;
    }

    // Fix `t.me/s/username` web preview links (tdesktop lines 679-681).
    let web_channel_preview = domain_param == "s" && !appname_param.is_empty();
    let domain = if web_channel_preview {
        appname_param
    } else {
        domain_param
    };
    if !is_valid_domain(domain) {
        return None;
    }
    let appname = if web_channel_preview {
        ""
    } else {
        appname_param
    };

    let mode = params.get("mode").map(String::as_str).unwrap_or("");
    let fullscreen = mode == "fullscreen";
    let compact = mode == "compact";

    // Precedence matches `ResolveUsernameOrPhone` lines 704-810:
    // 1. Direct mini app (`appname` present) overrides start/game.
    if !appname.is_empty() {
        let start_param = params
            .get("start")
            .filter(|s| !s.is_empty())
            .or_else(|| params.get("startapp").filter(|s| !s.is_empty()))
            .cloned();
        return Some(BotDeepLink::DirectMiniApp {
            username: domain.to_string(),
            app_name: appname.to_string(),
            start_param,
            fullscreen,
        });
    }

    // 2. Game share link (`game=` present and valid domain token).
    if let Some(game) = params.get("game")
        && !game.is_empty()
        && is_valid_domain(game)
    {
        return Some(BotDeepLink::ShareGame {
            username: domain.to_string(),
            game_short_name: game.clone(),
        });
    }

    // 3. Attach menu link (`startattach` or `attach`).
    if params.contains_key("startattach") || params.contains_key("attach") {
        let start_param = params.get("startattach").filter(|s| !s.is_empty()).cloned();
        let attach_username = params.get("attach").filter(|s| !s.is_empty()).cloned();
        let choose_types = params
            .get("choose")
            .map(|c| parse_choose_types(c))
            .unwrap_or_default();
        return Some(BotDeepLink::StartAttach {
            username: domain.to_string(),
            start_param,
            choose_types,
            attach_username,
            fullscreen,
        });
    }

    // 4. Main Mini App (`startapp` present without `appname`).
    if params.contains_key("startapp") {
        let start_param = params.get("startapp").filter(|s| !s.is_empty()).cloned();
        return Some(BotDeepLink::StartMainApp {
            username: domain.to_string(),
            start_param,
            compact,
            fullscreen,
        });
    }

    // 5. `?start=<non-empty>`
    if let Some(start_token) = params.get("start")
        && !start_token.is_empty()
    {
        let mut referral = params.get("ref").filter(|s| !s.is_empty()).cloned();
        if referral.is_none() {
            for prefix in start_ref_prefixes {
                if let Some(rest) = start_token.strip_prefix(prefix) {
                    referral = Some(rest.to_string());
                    break;
                }
            }
        }
        return Some(BotDeepLink::StartBot {
            username: domain.to_string(),
            start_param: start_token.clone(),
            referral,
        });
    }

    // 6. `?startgroup[=<payload>][&admin=...]`
    if params.contains_key("startgroup") {
        let start_param = params.get("startgroup").filter(|s| !s.is_empty()).cloned();
        let admin_rights = params
            .get("admin")
            .map(|a| parse_requested_admin_rights(a))
            .unwrap_or_default();
        return Some(BotDeepLink::StartGroup {
            username: domain.to_string(),
            start_param,
            admin_rights,
        });
    }

    // 7. `?startchannel[&admin=...]`
    if params.contains_key("startchannel") {
        let admin_rights = params
            .get("admin")
            .map(|a| parse_requested_admin_rights(a))
            .unwrap_or_default();
        return Some(BotDeepLink::StartChannel {
            username: domain.to_string(),
            admin_rights,
        });
    }

    Some(BotDeepLink::PeerOnly {
        username: domain.to_string(),
    })
}

fn strip_http_scheme(url: &str) -> (&str, &str) {
    if url.len() >= 8 && url[..8].eq_ignore_ascii_case("https://") {
        (&url[..8], &url[8..])
    } else if url.len() >= 7 && url[..7].eq_ignore_ascii_case("http://") {
        (&url[..7], &url[7..])
    } else {
        ("", url)
    }
}

fn strip_prefix_ignore_ascii_case<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    if s.len() >= prefix.len() && s[..prefix.len()].eq_ignore_ascii_case(prefix) {
        Some(&s[prefix.len()..])
    } else {
        None
    }
}
