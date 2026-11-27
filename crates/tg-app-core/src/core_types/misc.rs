//! Shared pure constants, formatters, and small cross-module DTOs.

use std::path::{Path, PathBuf};

use tg_calls::StarsColoring;
use tg_core::{
    ProxyData as CoreProxyData, ProxySettingsMode, ProxyType as CoreProxyType, SettingsProxy,
};
use tg_transport::{ProxyData as TransportProxyData, ProxyType as TransportProxyType};
use tg_wallet::toncenter::{NftItem, NftKind};

use crate::stand_in::strings::Strings;
use crate::stand_in::types::{ConnectionStatus, PeerId};

fn short_wallet_addr(addr: &str) -> String {
    if addr.chars().count() <= 14 {
        addr.to_owned()
    } else {
        let head: String = addr.chars().take(6).collect();
        let tail: String = addr
            .chars()
            .rev()
            .take(6)
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        format!("{head}…{tail}")
    }
}

/// Package version read at compile time from `Cargo.toml`.
pub const TGFORK_VERSION: &str = env!("CARGO_PKG_VERSION");

/// URL of the original Telegram Desktop upstream repository.
pub const UPSTREAM_TDESKTOP_URL: &str = "https://github.com/telegramdesktop/tdesktop";

/// URL of the GNU GPL v3.0 license text.
pub const GPL3_LICENSE_URL: &str = "https://www.gnu.org/licenses/gpl-3.0.html";

/// URL of the Telegram API documentation.
pub const TELEGRAM_API_URL: &str = "https://core.telegram.org/api";

/// Default maximum passkeys per Telegram account (`passkeysAccountPasskeysMax` default = 5).
pub const DEFAULT_MAX_PASSKEYS: usize = 5;

/// Converts a `tg-core` [`CoreProxyData`] into a `tg-transport` [`TransportProxyData`].
#[must_use]
pub fn core_to_transport_proxy(p: &CoreProxyData) -> TransportProxyData {
    let proxy_type = match p.proxy_type {
        CoreProxyType::None => TransportProxyType::None,
        CoreProxyType::Socks5 => TransportProxyType::Socks5,
        CoreProxyType::Http => TransportProxyType::Http,
        CoreProxyType::Mtproto => TransportProxyType::Mtproto,
        CoreProxyType::Web => TransportProxyType::Web,
    };
    TransportProxyData {
        proxy_type,
        host: p.host.clone(),
        port: p.port.min(u32::from(u16::MAX)) as u16,
        user: p.user.clone(),
        password: p.password.clone(),
        resolved_ips: Vec::new(),
        resolved_expire_at: None,
    }
}

/// Converts a `tg-transport` [`TransportProxyData`] into a `tg-core` [`CoreProxyData`].
#[must_use]
pub fn transport_to_core_proxy(p: &TransportProxyData) -> CoreProxyData {
    let proxy_type = match p.proxy_type {
        TransportProxyType::None => CoreProxyType::None,
        TransportProxyType::Socks5 => CoreProxyType::Socks5,
        TransportProxyType::Http => CoreProxyType::Http,
        TransportProxyType::Mtproto => CoreProxyType::Mtproto,
        TransportProxyType::Web => CoreProxyType::Web,
    };
    CoreProxyData {
        proxy_type,
        host: p.host.clone(),
        port: u32::from(p.port),
        user: p.user.clone(),
        password: p.password.clone(),
    }
}

/// Formats a proxy summary label (e.g. `"SOCKS5 127.0.0.1:1080"` or `"MTProto proxy.example:443"`).
#[must_use]
pub fn format_proxy_label(p: &CoreProxyData) -> String {
    let kind = match p.proxy_type {
        CoreProxyType::None => "Direct",
        CoreProxyType::Socks5 => "SOCKS5",
        CoreProxyType::Http => "HTTP",
        CoreProxyType::Mtproto => "MTProto",
        CoreProxyType::Web => "Web",
    };
    if p.host.is_empty() {
        kind.to_owned()
    } else if p.proxy_type == CoreProxyType::Web {
        if p.user.is_empty() {
            format!("{kind}  {}", p.host)
        } else {
            format!("{kind}  {}/{}", p.host, p.user)
        }
    } else {
        format!("{kind}  {}:{}", p.host, p.port)
    }
}

/// Computed connection indicator state including proxy labels
/// (`"Connecting to proxy..."`, `"Proxy"`, `"Connecting..."`, `"Reconnecting in Xs..."`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyIndicatorState {
    /// Whether the indicator should be displayed.
    pub visible: bool,
    /// Whether a custom proxy (`ProxySettingsMode::Enabled`) is currently active.
    pub proxy_active: bool,
    /// Whether the connection is still establishing / waiting to retry.
    pub connecting: bool,
    /// Primary user-facing label (`"Connecting to proxy..."`, `"Proxy"`, `"Connecting..."`, ...).
    pub label: String,
    /// Optional detail subtitle (e.g. `"SOCKS5 127.0.0.1:1080"` or `"System proxy"`).
    pub detail: Option<String>,
}

/// Formats the connection status banner string taking active proxy mode into account.
///
/// - Proxy enabled & connected -> `"Proxy"` (`strings.proxy_connected()`)
/// - Proxy enabled & connecting -> `"Connecting to proxy..."` (`strings.connecting_to_proxy()`)
/// - Proxy enabled & waiting -> `"Connecting to proxy..."` (or with retry countdown)
/// - Direct & connected -> `""`
/// - Direct & connecting -> `"Connecting..."`
/// - Direct & waiting -> `"Reconnecting in Xs..."`
#[must_use]
pub fn format_proxy_connection_status(
    status: ConnectionStatus,
    proxy_enabled: bool,
    strings: &dyn Strings,
) -> String {
    if proxy_enabled {
        match status {
            ConnectionStatus::Connected => strings.proxy_connected().to_owned(),
            ConnectionStatus::Connecting => strings.connecting_to_proxy().to_owned(),
            ConnectionStatus::Waiting { remaining_ms } => {
                if remaining_ms < 600 {
                    strings.connecting_to_proxy().to_owned()
                } else {
                    let secs = (remaining_ms / 1000) + 1;
                    format!(
                        "{} ({})",
                        strings.connecting_to_proxy(),
                        strings.reconnecting_in(secs)
                    )
                }
            }
        }
    } else {
        match status {
            ConnectionStatus::Connected => String::new(),
            ConnectionStatus::Connecting => strings.connecting().to_owned(),
            ConnectionStatus::Waiting { remaining_ms } => {
                let secs = (remaining_ms / 1000) + 1;
                strings.reconnecting_in(secs)
            }
        }
    }
}

/// Computes the complete [`ProxyIndicatorState`] for UI rendering and diagnostics.
#[must_use]
pub fn proxy_indicator_state(
    status: ConnectionStatus,
    proxy: &SettingsProxy,
    strings: &dyn Strings,
) -> ProxyIndicatorState {
    let proxy_active = proxy.is_enabled();
    let connecting = !matches!(status, ConnectionStatus::Connected);
    let visible = proxy_active || connecting;
    let label = format_proxy_connection_status(status, proxy_active, strings);
    let detail = match proxy.settings() {
        ProxySettingsMode::Enabled if proxy.selected().is_valid() => {
            Some(format_proxy_label(proxy.selected()))
        }
        _ => None,
    };
    ProxyIndicatorState {
        visible,
        proxy_active,
        connecting,
        label,
        detail,
    }
}

/// Default Telegram paid Stars message visual tiers (`calls_group_stars_coloring.cpp`).
#[must_use]
pub fn default_stars_colorings() -> Vec<StarsColoring> {
    vec![
        StarsColoring {
            bg_light: 0x0016_8ACD,
            bg_dark: 0x001D_5E8A,
            from_stars: 1,
            seconds_pin: 30,
            characters_max: 60,
            emoji_limit: 2,
        },
        StarsColoring {
            bg_light: 0x002F_9E44,
            bg_dark: 0x001F_6B32,
            from_stars: 10,
            seconds_pin: 60,
            characters_max: 110,
            emoji_limit: 4,
        },
        StarsColoring {
            bg_light: 0x00E6_7E22,
            bg_dark: 0x008F_4C11,
            from_stars: 50,
            seconds_pin: 120,
            characters_max: 160,
            emoji_limit: 6,
        },
        StarsColoring {
            bg_light: 0x00D9_480F,
            bg_dark: 0x008C_2D0B,
            from_stars: 100,
            seconds_pin: 180,
            characters_max: 220,
            emoji_limit: 8,
        },
        StarsColoring {
            bg_light: 0x00AE_3EC9,
            bg_dark: 0x006B_247D,
            from_stars: 500,
            seconds_pin: 300,
            characters_max: 300,
            emoji_limit: 12,
        },
        StarsColoring {
            bg_light: 0x00C9_2A2A,
            bg_dark: 0x007A_1C1C,
            from_stars: 1000,
            seconds_pin: 600,
            characters_max: 400,
            emoji_limit: 16,
        },
    ]
}

/// Formats the title text of a `messageActionNoForwardsRequest` card.
#[must_use]
pub fn format_no_forwards_request_title(actor_name: &str, outgoing: bool) -> String {
    if outgoing {
        "You requested to enable sharing in this chat:".to_owned()
    } else {
        format!("{actor_name} requested to enable sharing in this chat:")
    }
}

/// Formats the compact service pill shown for an expired, un-acted `NoForwardsRequest`.
#[must_use]
pub fn format_no_forwards_request_expired_text() -> String {
    "Request to enable sharing expired".to_owned()
}

/// Formats the service pill text for `messageActionNoForwardsToggle`.
#[must_use]
pub fn format_no_forwards_toggle_text(actor_name: &str, outgoing: bool, new_value: bool) -> String {
    match (outgoing, new_value) {
        (true, true) => "You disabled sharing in this chat".to_owned(),
        (true, false) => "You enabled sharing in this chat".to_owned(),
        (false, true) => format!("{actor_name} disabled sharing in this chat"),
        (false, false) => format!("{actor_name} enabled sharing in this chat"),
    }
}

/// Byte or file payload passed to [`InlinePlayback::frame_for`].
#[derive(Debug, Clone)]
pub enum InlineDocumentData {
    /// Encoded document bytes in memory (e.g. MP4/GIF/WebM).
    Bytes(Vec<u8>),
    /// Path to a cached local document file on disk.
    File(PathBuf),
    /// A file served part by part while it downloads: a round video message plays from it
    /// (TGB-3116, `Gif::playAnimation` over a `Media::Streaming` loader).
    Stream(StreamedDocument),
}

/// The loader of a document that plays while it downloads ([`InlineDocumentData::Stream`]).
#[derive(Clone)]
pub struct StreamedDocument(pub std::sync::Arc<dyn tg_video::RangeLoader>);

impl StreamedDocument {
    /// Whether both serve the same download.
    #[must_use]
    pub fn same_loader(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.0, &other.0)
    }
}

impl std::fmt::Debug for StreamedDocument {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "StreamedDocument({} bytes)", self.0.size())
    }
}

/// Descriptor for opening a live video stream inside [`crate::media::overlay::MediaOverlayWidget`]
/// (from `MessageMedia::VideoStream` or a live story).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoStreamDescriptor {
    /// Group call ID (`InputGroupCall.id`).
    pub call_id: i64,
    /// Group call access hash (`InputGroupCall.access_hash`).
    pub access_hash: u64,
    /// Owning channel or user peer ID.
    pub peer_id: i64,
    /// Stream title (or host peer name).
    pub title: String,
    /// Optional conference / call link slug.
    pub call_link_slug: String,
    /// Optional chat message ID where the stream was announced.
    pub join_msg_id: i32,
    /// True if backed by an RTMP/HLS broadcast stream (`messageMediaVideoStream.rtmp_stream`).
    pub rtmp_stream: bool,
    /// Optional HLS playlist URL (`.m3u8`) or RTMP URL for display/diagnostics.
    pub stream_url: Option<String>,
    /// Dedicated stream DC ID (`GroupCall.stream_dc_id`).
    pub stream_dc_id: i32,
    /// True when opened from a live story rather than a chat message.
    pub is_live_story: bool,
}

/// Computes the per-account Mini App bot storage directory inside `tdata`
/// (`<tdata_dir>/bot_storage/<account_key>`).
#[must_use]
pub fn account_bot_storage_dir(tdata_dir: &Path, account_key: &str) -> PathBuf {
    let sanitized: String = account_key
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let folder = if sanitized.is_empty() {
        "account_0".to_owned()
    } else {
        sanitized
    };
    tdata_dir.join("bot_storage").join(folder)
}

/// Summary descriptor for grouping a community's member chats inside the dialogs list
/// (`Data::CommunityInfo` collapsed row model).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommunityDialogGroup {
    /// Community channel peer ID.
    pub community_id: PeerId,
    /// Community display title.
    pub title: String,
    /// Whether this community is collapsed into a single grouped row in the main dialogs list
    /// (`ChannelDataFlag::CommunityCollapsed`).
    pub collapsed_in_dialogs: bool,
    /// Whether the grouped community row itself is pinned.
    pub pinned: bool,
    /// Peer IDs of joined member chats belonging to this community.
    pub member_peer_ids: Vec<PeerId>,
    /// Deterministic avatar palette color index (`0..7`).
    pub color_index: u8,
}

/// Priority order matching `Dialogs::TopBarSuggestions::Priority` (`suggestion.h:43-53`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum SuggestionPriority {
    UserpicSetup = 1,
    PremiumOffer = 2,
    BirthdaySetup = 3,
    BirthdayContacts = 4,
    LowCreditsSubs = 5,
    PremiumGrace = 6,
    CustomPromo = 7,
    GiftAuctions = 8,
    UnreviewedAuth = 9,
}

/// Known server/client top-bar suggestion kinds (`dialogs/suggestions/suggestion_*.cpp`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromoSuggestionKind {
    UserpicSetup,
    PremiumOffer,
    BirthdaySetup,
    BirthdayContacts,
    LowCreditsSubs,
    PremiumGrace,
    CustomPromo { url: String },
    GiftAuctions,
    UnreviewedAuth,
}

/// A single actionable promo suggestion displayed at the top of the dialogs list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromoSuggestion {
    /// Server suggestion key passed to `help.dismissSuggestion` (e.g. `"BIRTHDAY_SETUP"`).
    pub key: String,
    /// Display priority (lower value = higher priority when multiple are pending).
    pub priority: SuggestionPriority,
    /// Specific suggestion kind & metadata.
    pub kind: PromoSuggestionKind,
    /// Banner headline text.
    pub title: String,
    /// Banner description / subtitle text.
    pub description: String,
    /// Optional peer context for `help.dismissSuggestion` (defaults to `InputPeerEmpty`).
    pub peer_id: Option<PeerId>,
    /// Whether the close (`×`) button is shown.
    pub dismissible: bool,
}

/// Parsed result of `help.getPromoData`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PromoDataSnapshot {
    /// Unix expiration timestamp returned by Telegram.
    pub expires: i32,
    /// Pending server suggestion keys (e.g. `"BIRTHDAY_SETUP"`, `"PREMIUM_ANNUAL"`).
    pub pending_suggestions: Vec<String>,
    /// Already dismissed suggestion keys.
    pub dismissed_suggestions: Vec<String>,
    /// Optional custom promo suggestion (`PendingSuggestion`).
    pub custom_suggestion: Option<PromoSuggestion>,
    /// Optional PSA promo channel peer ID.
    pub promo_peer: Option<PeerId>,
    /// Optional PSA type string.
    pub psa_type: Option<String>,
    /// Optional PSA text.
    pub psa_message: Option<String>,
}

/// Action emitted when the user clicks or dismisses the active top-bar suggestion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SuggestionAction {
    /// User clicked the suggestion body to perform its action / open its URL.
    Activate(PromoSuggestion),
    /// User clicked the dismiss (`×`) icon.
    Dismiss {
        /// Server suggestion key.
        key: String,
        /// Optional peer context.
        peer_id: Option<PeerId>,
    },
}

/// Human-readable display title for an [`NftItem`].
#[must_use]
pub fn nft_display_title(item: &NftItem) -> String {
    if !item.name.trim().is_empty() {
        return item.name.clone();
    }
    match item.kind {
        NftKind::TelegramUsername if !item.key.is_empty() => format!("@{}", item.key),
        NftKind::TelegramNumber if !item.key.is_empty() => format!("+{}", item.key),
        NftKind::TelegramGift if !item.key.is_empty() => item.key.clone(),
        _ if !item.domain.is_empty() => item.domain.clone(),
        _ if !item.index.is_empty() => format!("NFT #{}", item.index),
        _ => short_wallet_addr(&item.address),
    }
}

/// Returns `true` when the filename or MIME type identifies a `.tdesktop-theme` /
/// `.tdesktop-palette` file (`DocumentData::isTheme()`).
#[must_use]
pub fn is_theme_file(file_name: &str, mime_type: &str) -> bool {
    let lower_name = file_name.to_ascii_lowercase();
    let lower_mime = mime_type.to_ascii_lowercase();
    lower_name.ends_with(".tdesktop-theme")
        || lower_name.ends_with(".tdesktop-palette")
        || lower_mime == "application/x-tgtheme-tdesktop"
        || lower_mime == "application/x-tgwallpattern"
}

/// Normalizes an arbitrary clockwise rotation in degrees to `{0, 90, 180, 270}`.
#[must_use]
pub const fn normalize_rotation(degrees: i32) -> i32 {
    let r = degrees % 360;
    let pos = if r < 0 { r + 360 } else { r };
    match pos {
        90 | 180 | 270 => pos,
        _ => 0,
    }
}

/// Applies clockwise rotation (`0/90/180/270`) and optional horizontal/vertical flip to a `DynamicImage`.
#[must_use]
pub fn transform_dynamic_image(
    img: &image::DynamicImage,
    rotation_degrees: i32,
    flip_h: bool,
    flip_v: bool,
) -> image::DynamicImage {
    let mut out = match normalize_rotation(rotation_degrees) {
        90 => img.rotate90(),
        180 => img.rotate180(),
        270 => img.rotate270(),
        _ => img.clone(),
    };
    if flip_h {
        out = image::DynamicImage::ImageRgba8(image::imageops::flip_horizontal(&out.to_rgba8()));
    }
    if flip_v {
        out = image::DynamicImage::ImageRgba8(image::imageops::flip_vertical(&out.to_rgba8()));
    }
    out
}

/// Builds a full `tg-theme` [`tg_theme::Palette`] via `tg-ui`'s `ThemeManager` for [`crate::stand_in::theme::ThemeMode`].
#[must_use]
pub fn tg_palette_for_mode(mode: crate::stand_in::theme::ThemeMode) -> tg_theme::Palette {
    let embedded = match mode {
        crate::stand_in::theme::ThemeMode::Day => tg_theme::EmbeddedType::Default,
        crate::stand_in::theme::ThemeMode::Night => tg_theme::EmbeddedType::Night,
    };
    tg_ui::theme::ThemeManager::with_embedded(embedded)
        .palette()
        .clone()
}

/// Computes `tg-theme` [`tg_theme::WebViewThemeParams`] for WebView2 background, scrollbars, and Mini App JSON.
#[must_use]
pub fn build_webview_theme_params(palette: &tg_theme::Palette) -> tg_theme::WebViewThemeParams {
    tg_theme::web_view_params(palette)
}
