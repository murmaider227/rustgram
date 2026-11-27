//! Port of `iv/iv_controller.cpp`, `iv/iv_zoom_controls.cpp`, `iv/iv_instance.cpp`,
//! `core/core_settings.cpp` (IV zoom), and `iv/markdown/iv_markdown_prepare_links.cpp`.
//!
//! Provides window-independent Instant View controller logic:
//! - Link classification (in-page anchor, Instant View page, `tg://` / `t.me`, `tonsite://`, external URL)
//! - Zoom levels, clamping, snapping, and modifier step sizes
//! - JS bridge inbound/outbound message protocol (`BridgeMessage`, `HostCommand`)
//! - Navigation history stack with per-page scroll position memory

use serde::{Deserialize, Serialize};

use crate::error::IvError;
use crate::model::normalize_fragment_id;
use crate::prepare::{PreparedPage, safe_url};

/// Minimum allowed Instant View zoom percentage (`kMinIvZoom` in `core_settings.cpp`).
pub const ZOOM_MIN: i32 = 25;
/// Maximum allowed Instant View zoom percentage (`kMaxIvZoom` in `core_settings.cpp`).
pub const ZOOM_MAX: i32 = 400;
/// Default zoom percentage at 100% app/screen scale.
pub const ZOOM_DEFAULT: i32 = 100;
/// Default keyboard/button zoom step (`kZoomStep` in `iv_controller.cpp`).
pub const ZOOM_STEP: i32 = 10;
/// Fine zoom step when Ctrl is held (`kZoomSmallStep` in `iv_zoom_controls.cpp`).
pub const ZOOM_SMALL_STEP: i32 = 5;
/// Tiny zoom step when Alt is held (`kZoomTinyStep` in `iv_zoom_controls.cpp`).
pub const ZOOM_TINY_STEP: i32 = 1;

/// Zoom step granularity matching `iv_zoom_controls.cpp`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ZoomStep {
    /// Standard step (`10%`).
    #[default]
    Normal,
    /// Ctrl-modified step (`5%`).
    Small,
    /// Alt-modified step (`1%`).
    Tiny,
}

impl ZoomStep {
    /// Returns the integer percentage delta for this step.
    pub const fn delta(self) -> i32 {
        match self {
            Self::Normal => ZOOM_STEP,
            Self::Small => ZOOM_SMALL_STEP,
            Self::Tiny => ZOOM_TINY_STEP,
        }
    }
}

/// Computes the default IV zoom percentage from app scale and screen scale,
/// matching `DefaultIvZoom()` in `core_settings.cpp`.
///
/// Snaps `app_scale * 100 / screen_scale` to the nearest multiple of 10 or 25
/// (whichever is closer, preferring 25 on ties).
pub fn default_iv_zoom(app_scale: i32, screen_scale: i32) -> i32 {
    if app_scale <= 0 || screen_scale <= 0 {
        return ZOOM_DEFAULT;
    }
    let exact = (app_scale * 100) / screen_scale;
    let snap10 = ((exact + 5) / 10) * 10;
    let snap25 = ((exact + 12) / 25) * 25;
    let chosen = if (exact - snap25).abs() <= (exact - snap10).abs() {
        snap25
    } else {
        snap10
    };
    chosen.clamp(ZOOM_MIN, ZOOM_MAX)
}

/// Resolves a stored zoom setting (where `0` means "use default"),
/// matching `ResolveIvZoom()` in `core_settings.cpp`.
pub fn resolve_iv_zoom(value: i32, default_zoom: i32) -> i32 {
    let base = if value > 0 { value } else { default_zoom };
    base.clamp(ZOOM_MIN, ZOOM_MAX)
}

/// Zoom state machine for an Instant View session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ZoomState {
    default_zoom: i32,
    current: i32,
}

impl Default for ZoomState {
    fn default() -> Self {
        Self::new(ZOOM_DEFAULT)
    }
}

impl ZoomState {
    /// Creates a new [`ZoomState`] with the given default zoom percentage.
    pub fn new(default_zoom: i32) -> Self {
        let clamped = default_zoom.clamp(ZOOM_MIN, ZOOM_MAX);
        Self {
            default_zoom: clamped,
            current: clamped,
        }
    }

    /// Returns the active zoom percentage (`25..=400`).
    pub const fn percent(&self) -> i32 {
        self.current
    }

    /// Returns the WebView2 zoom factor (`0.25..=4.0`).
    pub fn factor(&self) -> f64 {
        f64::from(self.current) / 100.0
    }

    /// Sets zoom to `value` (`0` resets to default), returning the clamped value.
    pub fn set(&mut self, value: i32) -> i32 {
        self.current = resolve_iv_zoom(value, self.default_zoom);
        self.current
    }

    /// Increases zoom by `step` and returns the new percentage.
    pub fn zoom_in(&mut self, step: ZoomStep) -> i32 {
        self.current = (self.current + step.delta()).clamp(ZOOM_MIN, ZOOM_MAX);
        self.current
    }

    /// Decreases zoom by `step` and returns the new percentage.
    pub fn zoom_out(&mut self, step: ZoomStep) -> i32 {
        self.current = (self.current - step.delta()).clamp(ZOOM_MIN, ZOOM_MAX);
        self.current
    }

    /// Resets zoom to the configured default (`Ctrl+0`).
    pub fn reset(&mut self) -> i32 {
        self.current = self.default_zoom;
        self.current
    }
}

/// Converts a `tonsite://` URI to its `https://*.magic.org` gateway URL,
/// ported from `TonsiteToHttps` in `iv_controller.cpp`.
pub fn tonsite_to_https(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if !trimmed
        .get(..10)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("tonsite://"))
    {
        return None;
    }
    let part = &trimmed[10..];
    let (raw_host, path) = match part.find('/') {
        Some(idx) => (&part[..idx], &part[idx..]),
        None => (part, "/"),
    };
    let host_only = raw_host.split(['?', '#']).next().unwrap_or("");
    if host_only.is_empty()
        || !host_only
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
    {
        return None;
    }
    let encoded = host_only.replace('-', "-h").replace('.', "-d");
    Some(format!("https://{encoded}.magic.org{path}"))
}

/// Converts a `https://*.magic.org` gateway URL back to a `tonsite://` URI,
/// ported from `HttpsToTonsite` in `iv_controller.cpp`.
pub fn https_to_tonsite(value: &str) -> String {
    let trimmed = value.trim();
    if !trimmed
        .get(..8)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("https://"))
    {
        return value.to_string();
    }
    let part = &trimmed[8..];
    let (raw_host, path) = match part.find('/') {
        Some(idx) => (&part[..idx], &part[idx..]),
        None => (part, "/"),
    };
    let lower_host = raw_host.to_ascii_lowercase();
    let Some(prefix) = lower_host.strip_suffix(".magic.org") else {
        return value.to_string();
    };
    if prefix.is_empty() {
        return value.to_string();
    }
    let decoded = raw_host[..prefix.len()]
        .replace("-d", ".")
        .replace("-h", "-");
    format!("tonsite://{decoded}{path}")
}

/// Returns true if `uri` is a valid `tonsite://` URL (`Controller::IsGoodTonSiteUrl`).
pub fn is_good_tonsite_url(uri: &str) -> bool {
    tonsite_to_https(uri).is_some()
}

/// Returns true if Instant View should be preferred for `uri`,
/// ported verbatim from `Iv::PreferForUri` in `iv_instance.cpp`.
pub fn prefer_iv_for_uri(uri: &str) -> bool {
    let Some((host, path)) = parse_http_host_and_path(uri) else {
        return false;
    };
    host == "telegra.ph"
        || host == "te.legra.ph"
        || host == "graph.org"
        || (host == "telegram.org"
            && (path.starts_with("/faq")
                || path.starts_with("/privacy")
                || path.starts_with("/blog")))
}

/// Classification of a clicked link inside an Instant View page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkTarget {
    /// Scroll to an anchor fragment within the currently loaded page.
    Anchor {
        /// Normalized lowercase fragment ID without leading `#`.
        fragment: String,
    },
    /// Open another Instant View page (via `messages.getWebPage`).
    InstantViewPage {
        /// Full target URL (including `#fragment` if present).
        url: String,
        /// Optional known Telegram webpage ID.
        webpage_id: Option<i64>,
        /// Optional normalized anchor fragment to scroll to after load.
        fragment: Option<String>,
    },
    /// Handle internally as a Telegram deep link (`tg://...` or `t.me/...`).
    TelegramLink {
        /// Original `tg://` or `https://t.me/...` URI.
        uri: String,
    },
    /// Open in the TON Site WebView (`tonsite://...`).
    TonSite {
        /// Original `tonsite://` URI.
        uri: String,
        /// Gateway `https://*.magic.org` URL.
        https_gateway_url: String,
    },
    /// Open mail client (`mailto:...`).
    Email {
        /// Decoded email address.
        address: String,
    },
    /// Open phone dialer (`tel:...`).
    Phone {
        /// Phone number string.
        number: String,
    },
    /// Open in the user's external browser (`http://`, `https://`, `ftp://`, `ton://`).
    ExternalUrl {
        /// Validated URL.
        url: String,
    },
    /// Blocked / disallowed link (e.g. `javascript:`, `data:`, `file:`, empty).
    Rejected {
        /// Raw input string.
        raw: String,
        /// Human-readable rejection reason.
        reason: &'static str,
    },
}

/// Classifies a link clicked inside an Instant View page.
///
/// Handles:
/// - `#anchor` and same-page `current_page_url#anchor` links -> [`LinkTarget::Anchor`]
/// - `tg://...` and `t.me` / `telegram.me` / `telegram.dog` links -> [`LinkTarget::TelegramLink`]
/// - `tonsite://...` -> [`LinkTarget::TonSite`]
/// - `mailto:...` -> [`LinkTarget::Email`]
/// - `tel:...` -> [`LinkTarget::Phone`]
/// - Explicit `webpage_id` (non-zero) or [`prefer_iv_for_uri`] hosts -> [`LinkTarget::InstantViewPage`]
/// - Safe `http://` / `https://` / `ftp://` / `ton://` -> [`LinkTarget::ExternalUrl`]
/// - Everything else (`javascript:`, `data:`, `file:`, relative paths) -> [`LinkTarget::Rejected`]
pub fn classify_link(
    raw_href: &str,
    current_page_url: Option<&str>,
    webpage_id: Option<i64>,
) -> LinkTarget {
    let trimmed = raw_href.trim();
    if trimmed.is_empty() {
        return LinkTarget::Rejected {
            raw: raw_href.to_string(),
            reason: "empty url",
        };
    }
    if trimmed.bytes().any(|b| b <= 0x1F || b == 0x7F) {
        return LinkTarget::Rejected {
            raw: raw_href.to_string(),
            reason: "control characters in url",
        };
    }

    if let Some(rest) = trimmed.strip_prefix('#') {
        let fragment = normalize_fragment_id(rest);
        return if fragment.is_empty() {
            LinkTarget::Rejected {
                raw: raw_href.to_string(),
                reason: "empty anchor fragment",
            }
        } else {
            LinkTarget::Anchor { fragment }
        };
    }

    let lower = trimmed.to_ascii_lowercase();

    if lower.starts_with("tg://") {
        return LinkTarget::TelegramLink {
            uri: trimmed.to_string(),
        };
    }

    if lower.starts_with("tonsite://") {
        return match tonsite_to_https(trimmed) {
            Some(gw) => LinkTarget::TonSite {
                uri: trimmed.to_string(),
                https_gateway_url: gw,
            },
            None => LinkTarget::Rejected {
                raw: raw_href.to_string(),
                reason: "invalid tonsite host",
            },
        };
    }

    if lower.starts_with("mailto:") {
        let addr = trimmed[7..].split('?').next().unwrap_or("").trim();
        return if addr.is_empty() {
            LinkTarget::Rejected {
                raw: raw_href.to_string(),
                reason: "empty mailto address",
            }
        } else {
            LinkTarget::Email {
                address: addr.to_string(),
            }
        };
    }

    if lower.starts_with("tel:") {
        let number = trimmed[4..].trim();
        return if number.is_empty() {
            LinkTarget::Rejected {
                raw: raw_href.to_string(),
                reason: "empty tel number",
            }
        } else {
            LinkTarget::Phone {
                number: number.to_string(),
            }
        };
    }

    let Some(safe) = safe_url(trimmed) else {
        return LinkTarget::Rejected {
            raw: raw_href.to_string(),
            reason: "disallowed url scheme",
        };
    };

    // Check same-page anchor links (`https://example.com/post#sec` while on `https://example.com/post`).
    let (base_without_hash, frag_opt) = split_fragment(&safe);
    if let (Some(current), Some(frag)) = (current_page_url, frag_opt.as_deref()) {
        let (cur_base, _) = split_fragment(current.trim());
        if urls_same_document(cur_base, base_without_hash) {
            return LinkTarget::Anchor {
                fragment: frag.to_string(),
            };
        }
    }

    let effective_wp = webpage_id.filter(|&id| id != 0);

    // Telegram links (`t.me`, `telegram.me`, `telegram.dog`) unless explicitly tied to an IV webpage_id.
    if effective_wp.is_none() && is_telegram_domain(&safe) {
        return LinkTarget::TelegramLink { uri: safe };
    }

    if effective_wp.is_some() || prefer_iv_for_uri(&safe) {
        return LinkTarget::InstantViewPage {
            url: safe,
            webpage_id: effective_wp,
            fragment: frag_opt,
        };
    }

    LinkTarget::ExternalUrl { url: safe }
}

fn split_fragment(url: &str) -> (&str, Option<String>) {
    match url.split_once('#') {
        Some((base, frag)) => {
            let norm = normalize_fragment_id(frag);
            (base, if norm.is_empty() { None } else { Some(norm) })
        }
        None => (url, None),
    }
}

fn urls_same_document(a: &str, b: &str) -> bool {
    let norm = |s: &str| s.trim_end_matches('/').to_ascii_lowercase();
    !a.is_empty() && norm(a) == norm(b)
}

fn parse_http_host_and_path(uri: &str) -> Option<(String, String)> {
    let trimmed = uri.trim();
    let lower = trimmed.to_ascii_lowercase();
    let rest = if lower.starts_with("https://") {
        &trimmed[8..]
    } else if lower.starts_with("http://") {
        &trimmed[7..]
    } else {
        return None;
    };
    let (authority, path_raw) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let host = authority
        .split('@')
        .next_back()
        .unwrap_or("")
        .split(':')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    if host.is_empty() {
        return None;
    }
    let path = path_raw
        .split(['?', '#'])
        .next()
        .unwrap_or("/")
        .to_ascii_lowercase();
    Some((host, path))
}

fn is_telegram_domain(uri: &str) -> bool {
    let Some((host, _)) = parse_http_host_and_path(uri) else {
        return false;
    };
    host == "t.me"
        || host == "www.t.me"
        || host == "telegram.me"
        || host == "www.telegram.me"
        || host == "telegram.dog"
        || host == "www.telegram.dog"
}

/// Media kind reported by JS bridge click/context-menu events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    /// Photo (`pageBlockPhoto` / author photo / related photo).
    Photo,
    /// Video (`pageBlockVideo`).
    Video,
    /// Audio (`pageBlockAudio`).
    Audio,
    /// Generic document / inline image (`pageBlockDocument` / `textImage`).
    Document,
}

/// Keyboard shortcuts forwarded from the WebView window (`iv_controller.cpp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShortcutAction {
    /// Escape or Ctrl+W.
    Close,
    /// Ctrl+M.
    Minimize,
    /// Ctrl+Q.
    Quit,
    /// Ctrl++ / Ctrl+=.
    ZoomIn,
    /// Ctrl+-.
    ZoomOut,
    /// Ctrl+0.
    ZoomReset,
}

/// Scroll position snapshot reported by `page.js`.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct ScrollPosition {
    /// Vertical scroll offset in CSS pixels.
    pub y: f64,
    /// Maximum scrollable offset in CSS pixels.
    pub max_y: f64,
    /// Normalized scroll progress in `0.0..=1.0`.
    pub progress: f64,
}

/// Inbound JSON message sent from the Instant View WebView page (`assets/page.js`) to Rust.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BridgeMessage {
    /// User clicked an `<a href="...">` element.
    LinkClick {
        /// Raw `href` attribute value.
        href: String,
        /// Optional `data-webpage-id` attribute.
        #[serde(default)]
        webpage_id: Option<i64>,
        /// True if Ctrl/Cmd/middle-click was used.
        #[serde(default)]
        new_window: bool,
    },
    /// User clicked a media element (`[data-iv-media]`).
    MediaClick {
        /// Media kind.
        kind: MediaKind,
        /// Telegram photo or document ID.
        id: i64,
    },
    /// User clicked a channel title/link (`[data-iv-channel-open]`).
    ChannelOpen {
        /// Channel ID.
        channel_id: i64,
        /// Optional public username.
        #[serde(default)]
        username: Option<String>,
    },
    /// User clicked a channel "Join" button (`[data-iv-channel-join]`).
    ChannelJoin {
        /// Channel ID.
        channel_id: i64,
        /// Optional public username.
        #[serde(default)]
        username: Option<String>,
    },
    /// User requested a custom context menu (`contextmenu` event).
    ContextMenu {
        /// Client X coordinate.
        x: f64,
        /// Client Y coordinate.
        y: f64,
        /// Currently selected text, if any.
        #[serde(default)]
        selection: String,
        /// Hovered link URL, if any.
        #[serde(default)]
        link_url: Option<String>,
        /// Hovered media kind, if any.
        #[serde(default)]
        media_kind: Option<MediaKind>,
        /// Hovered media ID, if any.
        #[serde(default)]
        media_id: Option<i64>,
    },
    /// Debounced scroll position update from the page.
    ScrollPosition {
        /// Vertical scroll offset in CSS pixels.
        y: f64,
        /// Maximum scrollable offset in CSS pixels.
        max_y: f64,
        /// Progress in `0.0..=1.0`.
        progress: f64,
    },
    /// User clicked the "Wrong layout?" report button.
    Report {
        /// Webpage ID to report to `@previews`.
        webpage_id: u64,
    },
    /// Embedded iframe requested a height resize (`TelegramWebviewProxy.postEvent("resize_frame", ...)`).
    ResizeFrame {
        /// Requested height in CSS pixels.
        height: i32,
    },
    /// Keyboard shortcut captured by the page or host.
    KeyShortcut {
        /// Shortcut identifier.
        shortcut: ShortcutAction,
    },
}

impl BridgeMessage {
    /// Parses a raw JSON message from the WebView.
    ///
    /// Supports both the structured object protocol (`{"type": "link_click", ...}`) and
    /// Telegram Desktop's legacy `TelegramWebviewProxy` 2-element array protocol
    /// (`["resize_frame", {"height": 420}]`, ported from `iv_markdown_embed_overlay.cpp`).
    pub fn parse_json(raw: &str) -> Result<Self, IvError> {
        let value: serde_json::Value =
            serde_json::from_str(raw).map_err(|e| IvError::InvalidBridgeMessage(e.to_string()))?;
        if let Some(arr) = value.as_array() {
            if arr.len() >= 2
                && let Some(event_type) = arr[0].as_str()
            {
                if event_type == "resize_frame" {
                    let h = parse_positive_px(&arr[1]["height"]);
                    return Ok(Self::ResizeFrame { height: h });
                }
                return Err(IvError::UnknownBridgeEvent(event_type.to_string()));
            }
            return Err(IvError::InvalidBridgeMessage(
                "array bridge payload must have [eventType, eventData]".to_string(),
            ));
        }
        serde_json::from_value(value).map_err(|e| IvError::InvalidBridgeMessage(e.to_string()))
    }
}

fn parse_positive_px(val: &serde_json::Value) -> i32 {
    let parsed = if let Some(n) = val.as_f64() {
        n
    } else if let Some(s) = val.as_str() {
        let trimmed = s.trim();
        let trimmed = trimmed.strip_suffix("px").unwrap_or(trimmed);
        trimmed.parse::<f64>().unwrap_or(0.0)
    } else {
        0.0
    };
    if parsed.is_finite() && parsed > 0.0 && parsed <= f64::from(i32::MAX) {
        parsed.round() as i32
    } else {
        0
    }
}

/// Outbound command sent from Rust to `window.IvBridge.dispatch(...)` inside the WebView.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HostCommand {
    /// Smoothly scroll to an anchor fragment (expanding parent `<details>` if needed).
    ScrollToAnchor {
        /// Normalized fragment ID.
        fragment: String,
    },
    /// Restore a saved vertical scroll offset.
    ScrollToY {
        /// Scroll offset in CSS pixels.
        y: f64,
    },
    /// Update CSS zoom percentage.
    SetZoom {
        /// Zoom percentage (`25..=400`).
        percent: i32,
    },
    /// Switch between dark and light Instant View themes.
    SetTheme {
        /// True for dark mode, false for light mode.
        dark: bool,
    },
    /// Update a media placeholder's `src` once downloaded.
    UpdateMedia {
        /// Media kind.
        kind: MediaKind,
        /// Photo or document ID.
        id: i64,
        /// New resource URL.
        url: String,
    },
    /// Update a channel card's join button state.
    SetChannelJoined {
        /// Channel ID.
        channel_id: i64,
        /// True if joined.
        joined: bool,
    },
    /// Navigate back in WebView history (used for TON sites).
    HistoryBack,
    /// Navigate forward in WebView history (used for TON sites).
    HistoryForward,
}

impl HostCommand {
    /// Serializes this command into a safe JavaScript snippet invoking `window.IvBridge.dispatch`.
    pub fn to_eval_js(&self) -> Result<String, IvError> {
        let json = serde_json::to_string(self)?;
        Ok(format!(
            "if(window.IvBridge&&typeof window.IvBridge.dispatch==='function'){{window.IvBridge.dispatch({json});}}"
        ))
    }
}

/// High-level action produced by [`IvController`] for the host application (`tg-app`).
#[derive(Debug, Clone, PartialEq)]
pub enum ControllerAction {
    /// Evaluate a command inside the WebView.
    Eval(HostCommand),
    /// Open another Instant View page.
    OpenPage {
        /// Target URL.
        url: String,
        /// Optional known webpage ID.
        webpage_id: Option<i64>,
        /// Optional anchor fragment.
        fragment: Option<String>,
    },
    /// Open a Telegram deep link (`tg://` or `t.me/...`).
    OpenTelegramLink {
        /// URI to dispatch inside the Telegram app.
        uri: String,
    },
    /// Open a TON Site URI in the TON Site viewer.
    OpenTonSite {
        /// Original `tonsite://` URI.
        uri: String,
        /// Resolved `https://*.magic.org` URL.
        https_gateway_url: String,
    },
    /// Open an external URL (`http`, `https`, `mailto`, `tel`, etc.) in the OS shell.
    OpenExternal {
        /// Safe URL to launch externally.
        url: String,
    },
    /// Open a clicked photo/video/audio/document in the app's media viewer.
    OpenMedia {
        /// Media kind.
        kind: MediaKind,
        /// Telegram photo or document ID.
        id: i64,
    },
    /// Navigate to a Telegram channel from a channel banner.
    OpenChannel {
        /// Channel ID.
        channel_id: i64,
        /// Optional username.
        username: Option<String>,
    },
    /// Join a Telegram channel from a channel banner.
    JoinChannel {
        /// Channel ID.
        channel_id: i64,
        /// Optional username.
        username: Option<String>,
    },
    /// Show a native context menu at `(x, y)`.
    ShowContextMenu {
        /// Client X coordinate.
        x: f64,
        /// Client Y coordinate.
        y: f64,
        /// Selected text.
        selection: String,
        /// Optional hovered link URL.
        link_url: Option<String>,
        /// Optional hovered media.
        media: Option<(MediaKind, i64)>,
    },
    /// Report a broken Instant View layout to `@previews`.
    Report {
        /// Webpage ID.
        webpage_id: u64,
        /// Start token (`"webpage<id>"`, matching `iv_instance.cpp`).
        start_token: String,
    },
    /// Resize an embedded frame container.
    ResizeEmbed {
        /// Requested height in CSS pixels.
        height: i32,
    },
    /// Close the Instant View window.
    CloseWindow,
    /// Minimize the Instant View window.
    MinimizeWindow,
    /// Quit the application (`Ctrl+Q`).
    QuitApp,
    /// Persist a new zoom percentage to application settings.
    ZoomChanged {
        /// Active zoom percentage (`25..=400`).
        percent: i32,
    },
}

/// Decision returned to the WebView's `NavigationStarting` hook (`iv_controller.cpp`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NavigationDecision {
    /// Allow the WebView to navigate normally (e.g. `about:blank`, custom IV origin, `.magic.org`).
    Allow,
    /// Cancel WebView navigation and handle the link via `action` (if any).
    Cancel(Option<LinkTarget>),
}

/// One entry in the Instant View back/forward navigation stack.
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryEntry {
    /// Canonical page URL.
    pub url: String,
    /// Webpage ID.
    pub page_id: u64,
    /// Page title.
    pub title: String,
    /// Saved vertical scroll position when leaving this page.
    pub scroll: ScrollPosition,
}

/// Window-independent controller for an Instant View session (`iv/iv_controller.cpp`).
#[derive(Debug, Clone)]
pub struct IvController {
    current_url: String,
    current_page_id: u64,
    current_title: String,
    anchors: Vec<String>,
    zoom: ZoomState,
    scroll: ScrollPosition,
    back_stack: Vec<HistoryEntry>,
    forward_stack: Vec<HistoryEntry>,
    custom_origin: String,
}

impl IvController {
    /// Creates a new controller with a custom-scheme origin (e.g. `"tg-iv://localhost"` or
    /// `"http://desktop-app-resource"`) and default zoom level.
    pub fn new(custom_origin: impl Into<String>, default_zoom: i32) -> Self {
        Self {
            current_url: String::new(),
            current_page_id: 0,
            current_title: String::new(),
            anchors: Vec::new(),
            zoom: ZoomState::new(default_zoom),
            scroll: ScrollPosition::default(),
            back_stack: Vec::new(),
            forward_stack: Vec::new(),
            custom_origin: custom_origin.into(),
        }
    }

    /// Loads a newly prepared Instant View page into the controller, pushing the previous
    /// page onto the back stack if one was active.
    pub fn load_page(
        &mut self,
        url: impl Into<String>,
        page_id: u64,
        prepared: &PreparedPage,
        initial_fragment: Option<&str>,
    ) -> Vec<ControllerAction> {
        let new_url = url.into();
        if !self.current_url.is_empty() && self.current_url != new_url {
            self.back_stack.push(HistoryEntry {
                url: self.current_url.clone(),
                page_id: self.current_page_id,
                title: self.current_title.clone(),
                scroll: self.scroll,
            });
            self.forward_stack.clear();
        }
        self.current_url = new_url;
        self.current_page_id = page_id;
        self.current_title = prepared.title.clone();
        self.anchors = prepared.anchors.clone();
        self.scroll = ScrollPosition::default();

        let mut actions = Vec::new();
        if let Some(frag) = initial_fragment {
            let norm = normalize_fragment_id(frag);
            if !norm.is_empty() {
                actions.push(ControllerAction::Eval(HostCommand::ScrollToAnchor {
                    fragment: norm,
                }));
            }
        }
        actions
    }

    /// Returns the currently loaded page URL.
    pub fn current_url(&self) -> &str {
        &self.current_url
    }

    /// Returns the currently loaded webpage ID.
    pub const fn current_page_id(&self) -> u64 {
        self.current_page_id
    }

    /// Returns the currently loaded page title.
    pub fn current_title(&self) -> &str {
        &self.current_title
    }

    /// Returns the current zoom state.
    pub const fn zoom(&self) -> ZoomState {
        self.zoom
    }

    /// Returns the most recently reported scroll position.
    pub const fn scroll_position(&self) -> ScrollPosition {
        self.scroll
    }

    /// Returns true if a normalized anchor exists in the current page.
    pub fn has_anchor(&self, fragment: &str) -> bool {
        let norm = normalize_fragment_id(fragment);
        self.anchors.iter().any(|a| a == &norm)
    }

    /// Returns true if navigating back is possible.
    pub fn can_go_back(&self) -> bool {
        !self.back_stack.is_empty()
    }

    /// Returns true if navigating forward is possible.
    pub fn can_go_forward(&self) -> bool {
        !self.forward_stack.is_empty()
    }

    /// Pops the previous page from the back stack, saving the current page to the forward stack.
    pub fn go_back(&mut self) -> Option<HistoryEntry> {
        let prev = self.back_stack.pop()?;
        if !self.current_url.is_empty() {
            self.forward_stack.push(HistoryEntry {
                url: self.current_url.clone(),
                page_id: self.current_page_id,
                title: self.current_title.clone(),
                scroll: self.scroll,
            });
        }
        self.current_url = prev.url.clone();
        self.current_page_id = prev.page_id;
        self.current_title = prev.title.clone();
        self.scroll = prev.scroll;
        Some(prev)
    }

    /// Pops the next page from the forward stack, saving the current page to the back stack.
    pub fn go_forward(&mut self) -> Option<HistoryEntry> {
        let next = self.forward_stack.pop()?;
        if !self.current_url.is_empty() {
            self.back_stack.push(HistoryEntry {
                url: self.current_url.clone(),
                page_id: self.current_page_id,
                title: self.current_title.clone(),
                scroll: self.scroll,
            });
        }
        self.current_url = next.url.clone();
        self.current_page_id = next.page_id;
        self.current_title = next.title.clone();
        self.scroll = next.scroll;
        Some(next)
    }

    /// Adjusts zoom in and returns the resulting actions (zoom eval + settings notification).
    pub fn zoom_in(&mut self, step: ZoomStep) -> Vec<ControllerAction> {
        let percent = self.zoom.zoom_in(step);
        self.zoom_actions(percent)
    }

    /// Adjusts zoom out and returns the resulting actions.
    pub fn zoom_out(&mut self, step: ZoomStep) -> Vec<ControllerAction> {
        let percent = self.zoom.zoom_out(step);
        self.zoom_actions(percent)
    }

    /// Sets zoom to an explicit percentage (`0` resets to default).
    pub fn set_zoom(&mut self, value: i32) -> Vec<ControllerAction> {
        let percent = self.zoom.set(value);
        self.zoom_actions(percent)
    }

    /// Resets zoom to default (`Ctrl+0`).
    pub fn reset_zoom(&mut self) -> Vec<ControllerAction> {
        let percent = self.zoom.reset();
        self.zoom_actions(percent)
    }

    fn zoom_actions(&self, percent: i32) -> Vec<ControllerAction> {
        vec![
            ControllerAction::Eval(HostCommand::SetZoom { percent }),
            ControllerAction::ZoomChanged { percent },
        ]
    }

    /// Decides whether a top-level WebView navigation should be allowed or intercepted,
    /// matching `setNavigationPolicyHandler` in `iv_controller.cpp`.
    pub fn navigation_policy(&self, uri: &str, new_window: bool) -> NavigationDecision {
        let trimmed = uri.trim();
        if trimmed == "about:blank" {
            return NavigationDecision::Allow;
        }
        if !new_window && !self.custom_origin.is_empty() {
            let origin = self.custom_origin.trim_end_matches('/');
            if trimmed == origin || trimmed.starts_with(&format!("{origin}/")) {
                return NavigationDecision::Allow;
            }
        }
        if let Some((host, _)) = parse_http_host_and_path(trimmed)
            && !new_window
            && host.ends_with(".magic.org")
        {
            return NavigationDecision::Allow;
        }
        let target = classify_link(trimmed, Some(&self.current_url), None);
        NavigationDecision::Cancel(Some(target))
    }

    /// Returns `true` when `source_url` matches this controller's configured Instant View origin
    /// (`self.custom_origin`), rejecting empty URLs, foreign origins, userinfo spoofing, and
    /// prefix-confused hosts (e.g. `rustgram-internal://iv.evil`).
    #[must_use]
    pub fn is_authorized_bridge_sender(&self, source_url: &str) -> bool {
        let Some(expected) = extract_origin(&self.custom_origin) else {
            return false;
        };
        let Some(actual) = extract_origin(source_url) else {
            return false;
        };
        actual == expected
    }

    /// Processes a raw JSON message sent by `assets/page.js` via the WebView bridge, authorizing
    /// the WebView sender `source_url` against this controller's expected Instant View origin.
    pub fn handle_bridge_json_from_source(
        &mut self,
        raw_json: &str,
        source_url: &str,
    ) -> Result<Vec<ControllerAction>, IvError> {
        if !self.is_authorized_bridge_sender(source_url) {
            return Err(IvError::InvalidBridgeMessage(format!(
                "unauthorized Instant View bridge sender: '{source_url}'"
            )));
        }
        self.handle_bridge_json(raw_json)
    }

    /// Processes a raw JSON message sent by `assets/page.js` via the WebView bridge.
    pub fn handle_bridge_json(&mut self, raw_json: &str) -> Result<Vec<ControllerAction>, IvError> {
        let msg = BridgeMessage::parse_json(raw_json)?;
        Ok(self.handle_bridge_message(msg))
    }

    /// Processes a parsed [`BridgeMessage`] and returns host actions to execute.
    pub fn handle_bridge_message(&mut self, msg: BridgeMessage) -> Vec<ControllerAction> {
        match msg {
            BridgeMessage::LinkClick {
                href,
                webpage_id,
                new_window,
            } => {
                let target = classify_link(&href, Some(&self.current_url), webpage_id);
                if new_window {
                    return match target {
                        LinkTarget::InstantViewPage { url, .. }
                        | LinkTarget::ExternalUrl { url } => {
                            vec![ControllerAction::OpenExternal { url }]
                        }
                        LinkTarget::TelegramLink { uri } => {
                            vec![ControllerAction::OpenTelegramLink { uri }]
                        }
                        LinkTarget::TonSite {
                            uri,
                            https_gateway_url,
                        } => vec![ControllerAction::OpenTonSite {
                            uri,
                            https_gateway_url,
                        }],
                        LinkTarget::Email { address } => vec![ControllerAction::OpenExternal {
                            url: format!("mailto:{address}"),
                        }],
                        LinkTarget::Phone { number } => vec![ControllerAction::OpenExternal {
                            url: format!("tel:{number}"),
                        }],
                        LinkTarget::Anchor { fragment } => {
                            vec![ControllerAction::Eval(HostCommand::ScrollToAnchor {
                                fragment,
                            })]
                        }
                        LinkTarget::Rejected { .. } => Vec::new(),
                    };
                }
                match target {
                    LinkTarget::Anchor { fragment } => {
                        vec![ControllerAction::Eval(HostCommand::ScrollToAnchor {
                            fragment,
                        })]
                    }
                    LinkTarget::InstantViewPage {
                        url,
                        webpage_id,
                        fragment,
                    } => vec![ControllerAction::OpenPage {
                        url,
                        webpage_id,
                        fragment,
                    }],
                    LinkTarget::TelegramLink { uri } => {
                        vec![ControllerAction::OpenTelegramLink { uri }]
                    }
                    LinkTarget::TonSite {
                        uri,
                        https_gateway_url,
                    } => vec![ControllerAction::OpenTonSite {
                        uri,
                        https_gateway_url,
                    }],
                    LinkTarget::Email { address } => vec![ControllerAction::OpenExternal {
                        url: format!("mailto:{address}"),
                    }],
                    LinkTarget::Phone { number } => vec![ControllerAction::OpenExternal {
                        url: format!("tel:{number}"),
                    }],
                    LinkTarget::ExternalUrl { url } => {
                        vec![ControllerAction::OpenExternal { url }]
                    }
                    LinkTarget::Rejected { .. } => Vec::new(),
                }
            }
            BridgeMessage::MediaClick { kind, id } => {
                vec![ControllerAction::OpenMedia { kind, id }]
            }
            BridgeMessage::ChannelOpen {
                channel_id,
                username,
            } => vec![ControllerAction::OpenChannel {
                channel_id,
                username,
            }],
            BridgeMessage::ChannelJoin {
                channel_id,
                username,
            } => vec![ControllerAction::JoinChannel {
                channel_id,
                username,
            }],
            BridgeMessage::ContextMenu {
                x,
                y,
                selection,
                link_url,
                media_kind,
                media_id,
            } => {
                let media = media_kind.zip(media_id);
                vec![ControllerAction::ShowContextMenu {
                    x,
                    y,
                    selection,
                    link_url,
                    media,
                }]
            }
            BridgeMessage::ScrollPosition { y, max_y, progress } => {
                self.scroll = ScrollPosition {
                    y: y.max(0.0),
                    max_y: max_y.max(0.0),
                    progress: progress.clamp(0.0, 1.0),
                };
                Vec::new()
            }
            BridgeMessage::Report { webpage_id } => vec![ControllerAction::Report {
                webpage_id,
                start_token: format!("webpage{webpage_id}"),
            }],
            BridgeMessage::ResizeFrame { height } => {
                if height > 0 {
                    vec![ControllerAction::ResizeEmbed { height }]
                } else {
                    Vec::new()
                }
            }
            BridgeMessage::KeyShortcut { shortcut } => match shortcut {
                ShortcutAction::Close => vec![ControllerAction::CloseWindow],
                ShortcutAction::Minimize => vec![ControllerAction::MinimizeWindow],
                ShortcutAction::Quit => vec![ControllerAction::QuitApp],
                ShortcutAction::ZoomIn => self.zoom_in(ZoomStep::Normal),
                ShortcutAction::ZoomOut => self.zoom_out(ZoomStep::Normal),
                ShortcutAction::ZoomReset => self.reset_zoom(),
            },
        }
    }
}

/// Extracts a normalized lowercase `scheme://authority` origin from `url`, rejecting empty inputs,
/// missing/invalid schemes, whitespace, or userinfo (`@`).
fn extract_origin(url: &str) -> Option<String> {
    let trimmed = url.trim();
    let (scheme, rest) = trimmed.split_once("://")?;
    if scheme.is_empty()
        || !scheme
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'))
    {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.is_empty() || authority.contains('@') || authority.chars().any(char::is_whitespace)
    {
        return None;
    }
    Some(format!(
        "{}://{}",
        scheme.to_ascii_lowercase(),
        authority.to_ascii_lowercase()
    ))
}
