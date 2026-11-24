//! Port of attach menu bots, Mini App launch parameters, theme params generation,
//! and bot games (including share game score) from:
//! - `inline_bots/bot_attach_web_view.h` / `.cpp`
//! - `ui/chat/attach/attach_bot_webview.h` / `.cpp`
//! - `window/themes/window_theme.cpp` (`WebViewParams`)
//! - `data/data_game.h` and `window/window_peer_menu.cpp` (`ShowShareGameBox`)

use std::collections::BTreeSet;

use bitflags::bitflags;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::commands::BotAppColors;
use crate::markup::{PeerTypes, peer_matches_types};
use crate::stand_in::{
    BotAppId, DocumentId, FullMsgId, GameId, MsgId, PeerId, PeerKind, PhotoId, Rgba, UserId,
};

/// Protocol platform identifier passed to `messages.request*WebView` (`platform:string`).
///
/// # Protocol Decision
/// Telegram's MTProto `messages.requestWebView`, `messages.requestSimpleWebView`,
/// `messages.requestAppWebView`, `messages.requestMainWebView`, and
/// `messages.requestChatJoinWebView` methods require a server-recognized platform token
/// that Telegram embeds into the Mini App launch URL (`#tgWebAppPlatform=<platform>`)
/// and exposes to Mini App JS as `Telegram.WebApp.platform`.
///
/// Telegram's allowlist recognizes fixed protocol tokens (`"android"`, `"ios"`, `"tdesktop"`,
/// `"macos"`, `"web"`, `"weba"`, `"webk"`, `"unigram"`, `"unknown"`), and unrecognized values
/// may be rejected by servers (`PLATFORM_INVALID`) or break Mini Apps that gate desktop UI
/// features on `platform === "tdesktop"`. Therefore `tg-bots` uses `"tdesktop"` by default
/// for wire compatibility, while [`MiniAppLaunchParams::platform`] remains configurable.
pub const MINI_APP_PLATFORM: &str = "tdesktop";

/// Keep-alive interval for `messages.prolongWebView` (`kProlongTimeout = 60s`).
pub const WEBVIEW_PROLONG_TIMEOUT_MS: u64 = 60_000;

/// Refresh interval for `messages.getAttachMenuBots` (`kRefreshBotsTimeout = 1h`).
pub const ATTACH_BOTS_REFRESH_TIMEOUT_MS: u64 = 3_600_000;

/// Maximum popular app bots requested (`kPopularAppBotsLimit = 100`).
pub const POPULAR_APP_BOTS_LIMIT: i32 = 100;

/// Default allowed external link protocols in Mini Apps (`botValidateExternalLink`).
pub const DEFAULT_ALLOWED_EXTERNAL_PROTOCOLS: &[&str] = &["http", "https"];

// ---------------------------------------------------------------------------
// Mini App Theme Params (window/themes/window_theme.cpp:1637-1693)
// ---------------------------------------------------------------------------

/// Palette color inputs needed to generate Telegram Mini App `theme_params`.
///
/// Maps 1-to-1 to the `st::*` palette roles used by `Window::Theme::WebViewParams()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebViewPalette {
    /// `st::windowBg` -> `bg_color`, `header_bg_color`, `bottom_bar_bg_color`, `body_bg`
    pub window_bg: Rgba,
    /// `st::boxDividerBg` -> `secondary_bg_color`
    pub box_divider_bg: Rgba,
    /// `st::windowFg` -> `text_color`
    pub window_fg: Rgba,
    /// `st::windowSubTextFg` -> `hint_color`, `subtitle_text_color`
    pub window_sub_text_fg: Rgba,
    /// `st::windowActiveTextFg` -> `link_color`, `section_header_text_color`
    pub window_active_text_fg: Rgba,
    /// `st::windowBgActive` -> `button_color`
    pub window_bg_active: Rgba,
    /// `st::windowFgActive` -> `button_text_color`
    pub window_fg_active: Rgba,
    /// `st::lightButtonFg` -> `accent_text_color`
    pub light_button_fg: Rgba,
    /// `st::lightButtonBg` -> `section_bg_color`
    pub light_button_bg: Rgba,
    /// `st::attentionButtonFg` -> `destructive_text_color`
    pub attention_button_fg: Rgba,
    /// `st::shadowFg` -> blended over `window_bg` for `section_separator_color`
    pub shadow_fg: Rgba,
    /// `st::scrollBg`
    pub scroll_bg: Rgba,
    /// `st::scrollBgOver`
    pub scroll_bg_over: Rgba,
    /// `st::scrollBarBg`
    pub scroll_bar_bg: Rgba,
    /// `st::scrollBarBgOver`
    pub scroll_bar_bg_over: Rgba,
}

/// Generated Mini App theme parameters (`Webview::ThemeParams`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThemeParams {
    /// Window body background color (possibly overridden by `BotAppColors`).
    pub body_bg: Rgba,
    /// Window title bar background color (`TRANSPARENT` unless overridden by `BotAppColors`).
    pub title_bg: Rgba,
    /// Scrollbar track color.
    pub scroll_bg: Rgba,
    /// Scrollbar track hover color.
    pub scroll_bg_over: Rgba,
    /// Scrollbar thumb color.
    pub scroll_bar_bg: Rgba,
    /// Scrollbar thumb hover color.
    pub scroll_bar_bg_over: Rgba,
    /// Compact JSON payload passed in `MTP_dataJSON(theme_params)`.
    pub json: String,
}

/// Builds [`ThemeParams`] from a [`WebViewPalette`] and optional bot app color overrides.
///
/// Exact port of `Window::Theme::WebViewParams()` (`window/themes/window_theme.cpp:1637-1693`)
/// plus `WebViewInstance::botThemeParams()` (`inline_bots/bot_attach_web_view.cpp:1602-1620`).
pub fn build_theme_params(
    palette: &WebViewPalette,
    bot_app_colors: Option<&BotAppColors>,
    is_night_mode: bool,
) -> ThemeParams {
    let mut obj = Map::new();
    let entries = [
        ("bg_color", palette.window_bg),
        ("secondary_bg_color", palette.box_divider_bg),
        ("text_color", palette.window_fg),
        ("hint_color", palette.window_sub_text_fg),
        ("link_color", palette.window_active_text_fg),
        ("button_color", palette.window_bg_active),
        ("button_text_color", palette.window_fg_active),
        ("header_bg_color", palette.window_bg),
        ("accent_text_color", palette.light_button_fg),
        ("section_bg_color", palette.light_button_bg),
        ("section_header_text_color", palette.window_active_text_fg),
        ("subtitle_text_color", palette.window_sub_text_fg),
        ("destructive_text_color", palette.attention_button_fg),
        ("bottom_bar_bg_color", palette.window_bg),
    ];
    for (key, color) in entries {
        obj.insert(key.to_string(), Value::String(color.to_hex_rgb()));
    }

    // `section_separator_color`: alpha-blend `shadow_fg` over `window_bg`
    // (`window_theme.cpp:1671-1682`).
    let shadow_alpha = f64::from(palette.shadow_fg.a) / 255.0;
    let sep = Rgba::rgb(
        interpolate_channel(palette.window_bg.r, palette.shadow_fg.r, shadow_alpha),
        interpolate_channel(palette.window_bg.g, palette.shadow_fg.g, shadow_alpha),
        interpolate_channel(palette.window_bg.b, palette.shadow_fg.b, shadow_alpha),
    );
    obj.insert(
        "section_separator_color".to_string(),
        Value::String(sep.to_hex_rgb()),
    );

    let mut body_bg = palette.window_bg;
    let mut title_bg = Rgba::TRANSPARENT;
    if let Some(colors) = bot_app_colors {
        let title = if is_night_mode {
            colors.title_night
        } else {
            colors.title_day
        };
        let body = if is_night_mode {
            colors.body_night
        } else {
            colors.body_day
        };
        if let Some(t) = title
            && t.is_opaque()
        {
            title_bg = t;
        }
        if let Some(b) = body
            && b.is_opaque()
        {
            body_bg = b;
        }
    }

    let json = Value::Object(obj).to_string();
    ThemeParams {
        body_bg,
        title_bg,
        scroll_bg: palette.scroll_bg,
        scroll_bg_over: palette.scroll_bg_over,
        scroll_bar_bg: palette.scroll_bar_bg,
        scroll_bar_bg_over: palette.scroll_bar_bg_over,
        json,
    }
}

/// Port of `anim::interpolate(a, b, alpha)` (`a + round((b - a) * alpha)`).
#[inline]
fn interpolate_channel(a: u8, b: u8, alpha: f64) -> u8 {
    let v = f64::from(a) + (f64::from(b) - f64::from(a)) * alpha;
    v.round().clamp(0.0, 255.0) as u8
}

// ---------------------------------------------------------------------------
// Attach Menu Bots & Mini App Launch (inline_bots/bot_attach_web_view.h/.cpp)
// ---------------------------------------------------------------------------

/// An attach-menu / side-menu bot entry (`InlineBots::AttachWebViewBot`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachMenuBot {
    /// Bot user id.
    pub bot_id: UserId,
    /// Peer id of the bot chat.
    pub bot_peer_id: PeerId,
    /// Static icon document id (`name == "default_static"`), if present.
    pub icon_document_id: Option<DocumentId>,
    /// Short display name shown in the attach / side menu.
    pub short_name: String,
    /// Peer types where this bot may be opened from the attach menu.
    pub peer_types: PeerTypes,
    /// `true` if the user has not yet added this bot to their menu (`inactive`).
    pub inactive: bool,
    /// `true` if shown in the main/side menu (`show_in_side_menu`).
    pub in_side_menu: bool,
    /// `true` if shown in the chat attachment menu (`show_in_attach_menu`).
    pub in_attach_menu: bool,
    /// `true` if the side-menu Mini Apps disclaimer must be accepted before opening.
    pub disclaimer_required: bool,
    /// `true` if adding the bot to the menu asks permission for the bot to send messages.
    pub request_write_access: bool,
}

/// Result of attempting to add an attach-menu bot (`AttachWebView::AddToMenuResult`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AddToMenuResult {
    /// Bot is already active in the user's menu.
    AlreadyInMenu,
    /// Bot was newly added to the menu.
    Added,
    /// Bot does not support the attachment menu.
    Unsupported,
    /// User cancelled the confirmation or disclaimer dialog.
    Cancelled,
}

/// Toggle state sent to `messages.toggleBotInAttachMenu` (`AttachWebView::ToggledState`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AttachMenuToggleState {
    /// Remove from menu (`enabled = false`).
    Removed,
    /// Add to menu without write access (`enabled = true, write_allowed = false`).
    Added,
    /// Add to menu with write access (`enabled = true, write_allowed = true`).
    AllowedToWrite,
}

/// Store of attach-menu / side-menu bots (`AttachWebView` data model).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct AttachMenuStore {
    /// Server hash for `messages.getAttachMenuBots(hash)`.
    pub bots_hash: i64,
    /// Loaded attach-menu bots.
    pub bots: Vec<AttachMenuBot>,
    /// Bots for which the user has accepted the Mini Apps disclaimer during this session.
    disclaimer_accepted: BTreeSet<UserId>,
}

impl AttachMenuStore {
    /// Creates an empty [`AttachMenuStore`].
    pub fn new() -> Self {
        Self::default()
    }

    /// Replaces the bot list from `messages.getAttachMenuBots` when modified.
    pub fn update_bots(&mut self, hash: i64, bots: Vec<AttachMenuBot>) {
        self.bots_hash = hash;
        self.bots = bots;
    }

    /// Finds an attach-menu entry by `bot_id`.
    pub fn find(&self, bot_id: UserId) -> Option<&AttachMenuBot> {
        self.bots.iter().find(|b| b.bot_id == bot_id)
    }

    /// Updates or inserts a single bot entry (e.g. after `messages.getAttachMenuBot`).
    pub fn upsert(&mut self, bot: AttachMenuBot) {
        if let Some(existing) = self.bots.iter_mut().find(|b| b.bot_id == bot.bot_id) {
            *existing = bot;
        } else {
            self.bots.push(bot);
        }
    }

    /// Marks the side-menu disclaimer as accepted for `bot_id`.
    pub fn mark_disclaimer_accepted(&mut self, bot_id: UserId) {
        self.disclaimer_accepted.insert(bot_id);
    }

    /// Returns `true` if the disclaimer has been accepted for `bot_id`.
    pub fn is_disclaimer_accepted(&self, bot_id: UserId) -> bool {
        self.disclaimer_accepted.contains(&bot_id)
    }

    /// Returns `true` if the "NEW" badge should be shown for `bot` in the side menu.
    ///
    /// Port of `AttachWebView::showMainMenuNewBadge` (`bot_attach_web_view.cpp:2739-2744`).
    pub fn show_main_menu_new_badge(&self, bot: &AttachMenuBot) -> bool {
        bot.in_side_menu && bot.disclaimer_required && !self.is_disclaimer_accepted(bot.bot_id)
    }

    /// Returns bots visible in the attachment menu for a given chat (`!inactive && in_attach_menu`
    /// and matching the peer's type).
    pub fn visible_in_attach_menu(
        &self,
        peer_id: PeerId,
        peer_kind: PeerKind,
    ) -> Vec<&AttachMenuBot> {
        self.bots
            .iter()
            .filter(|b| {
                !b.inactive
                    && b.in_attach_menu
                    && peer_matches_types(peer_id, peer_kind, b.bot_peer_id, b.peer_types)
            })
            .collect()
    }

    /// Returns bots visible in the main side menu (`in_side_menu`).
    pub fn visible_in_side_menu(&self) -> Vec<&AttachMenuBot> {
        self.bots.iter().filter(|b| b.in_side_menu).collect()
    }
}

/// Source that initiated a Mini App / Game WebView (`InlineBots::WebViewSource`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WebViewSource {
    /// Inline `WebView` or reply-keyboard `SimpleWebView` button.
    Button {
        /// `true` for `SimpleWebView`.
        simple: bool,
    },
    /// Inline results `switch_webview` button.
    SwitchWebView,
    /// Direct Mini App link `t.me/bot/appname`.
    LinkApp {
        /// Mini App short name.
        app_name: String,
        /// Start parameter token.
        start_param: String,
    },
    /// Attach menu link `?startattach`.
    LinkAttachMenu {
        /// Allowed peer types from `choose=`.
        choose: PeerTypes,
        /// Start parameter token.
        start_param: String,
    },
    /// Main Mini App link `t.me/bot?startapp`.
    LinkBotProfile {
        /// Start parameter token.
        start_param: String,
        /// Compact mode requested (`mode=compact`).
        compact: bool,
    },
    /// Opened from the main side menu.
    MainMenu,
    /// Opened from a chat's attachment menu.
    AttachMenu {
        /// Active chat peer id.
        peer_id: PeerId,
    },
    /// Opened from the bot's compose menu button.
    BotMenu,
    /// HTML5 bot game launched from a game message.
    Game {
        /// Game message id.
        message_id: FullMsgId,
        /// Game title.
        title: String,
    },
    /// Opened from the bot's profile "Open App" button.
    BotProfile,
    /// Chat join request verification WebView.
    JoinChat {
        /// Server query id.
        query_id: u64,
    },
}

/// Parsed response of `MTPWebViewResult` (`InlineBots::WebViewResultData`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct WebViewResultData {
    /// Launch URL loaded in the WebView.
    pub url: String,
    /// Query id for `messages.prolongWebView` (`0` if non-prolongable).
    pub query_id: u64,
    /// Whether fullscreen mode was granted.
    pub fullscreen: bool,
    /// Whether fullsize mode was granted.
    pub fullsize: bool,
    /// Whether `same_origin` is set.
    pub same_origin: bool,
}

/// Plain launch parameters for requesting a Mini App WebView from Telegram servers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MiniAppLaunchParams {
    /// Target bot user id.
    pub bot_id: UserId,
    /// Chat peer context, if applicable.
    pub peer_id: Option<PeerId>,
    /// Launch source.
    pub source: WebViewSource,
    /// Optional explicit button URL.
    pub url: Option<String>,
    /// Optional `start_param` payload.
    pub start_param: Option<String>,
    /// Optional direct app identity (`BotAppId` + `access_hash`) for `messages.requestAppWebView`.
    pub app_id: Option<(BotAppId, u64)>,
    /// Theme parameters JSON and window colors.
    pub theme_params: ThemeParams,
    /// Protocol platform identifier (defaults to [`MINI_APP_PLATFORM`] = `"tdesktop"`).
    pub platform: String,
    /// Request fullscreen display.
    pub fullscreen: bool,
    /// Request compact display (main Mini App).
    pub compact: bool,
    /// User granted write access during app open confirmation.
    pub write_allowed: bool,
    /// Optional reply-to message id.
    pub reply_to: Option<MsgId>,
    /// Send silently flag.
    pub silent: bool,
}

impl MiniAppLaunchParams {
    /// Creates launch parameters with [`MINI_APP_PLATFORM`] as the default platform.
    pub fn new(bot_id: UserId, source: WebViewSource, theme_params: ThemeParams) -> Self {
        Self {
            bot_id,
            peer_id: None,
            source,
            url: None,
            start_param: None,
            app_id: None,
            theme_params,
            platform: MINI_APP_PLATFORM.to_string(),
            fullscreen: false,
            compact: false,
            write_allowed: false,
            reply_to: None,
            silent: false,
        }
    }
}

bitflags! {
    /// Overflow menu buttons shown in a Mini App / Game panel (`Ui::BotWebView::MenuButton`).
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
    pub struct WebViewMenuButtons: u8 {
        /// Open bot's chat (`OpenBot`).
        const OPEN_BOT             = 0x01;
        /// Remove bot from chat attachment menu (`RemoveFromMenu`).
        const REMOVE_FROM_MENU     = 0x02;
        /// Remove bot from main side menu (`RemoveFromMainMenu`).
        const REMOVE_FROM_MAIN     = 0x04;
        /// Share game score / game message (`ShareGame`).
        const SHARE_GAME           = 0x08;
        /// Report bot (`Report`).
        const REPORT               = 0x10;
    }
}

/// Computes which overflow menu buttons should be shown for a WebView panel.
///
/// Port of `WebViewInstance::show` (`inline_bots/bot_attach_web_view.cpp:1484-1499`).
pub fn compute_webview_menu_buttons(
    source: &WebViewSource,
    attached: Option<&AttachMenuBot>,
    chat_is_same_bot: bool,
    can_report: bool,
) -> WebViewMenuButtons {
    if matches!(source, WebViewSource::Game { .. }) {
        return WebViewMenuButtons::SHARE_GAME;
    }
    let mut buttons = WebViewMenuButtons::empty();
    let has_open_bot = matches!(source, WebViewSource::MainMenu) || !chat_is_same_bot;
    if has_open_bot {
        buttons |= WebViewMenuButtons::OPEN_BOT;
    }
    let is_menu_source = matches!(
        source,
        WebViewSource::MainMenu
            | WebViewSource::AttachMenu { .. }
            | WebViewSource::LinkAttachMenu { .. }
    );
    if let Some(bot) = attached
        && (!bot.inactive || bot.in_side_menu)
        && is_menu_source
    {
        if bot.in_side_menu {
            buttons |= WebViewMenuButtons::REMOVE_FROM_MAIN;
        } else {
            buttons |= WebViewMenuButtons::REMOVE_FROM_MENU;
        }
    }
    if can_report {
        buttons |= WebViewMenuButtons::REPORT;
    }
    buttons
}

/// Returns `true` if a Mini App WebView is allowed to read the system clipboard.
///
/// Port of `allowClipboardRead` (`bot_attach_web_view.cpp:1500-1503`).
pub fn allow_webview_clipboard_read(
    source: &WebViewSource,
    attached: Option<&AttachMenuBot>,
) -> bool {
    matches!(
        source,
        WebViewSource::MainMenu | WebViewSource::AttachMenu { .. }
    ) || attached.is_some_and(|b| b.in_attach_menu || b.in_side_menu)
}

/// Returns `true` if opening a Mini App requires a user confirmation prompt.
///
/// Port of `WebViewInstance::confirmOpen` (`bot_attach_web_view.cpp:1148-1155`).
pub const fn needs_webview_open_confirmation(
    force_confirmation: bool,
    bot_verified: bool,
    peer_trusted_for_webview: bool,
) -> bool {
    force_confirmation || !(bot_verified || peer_trusted_for_webview)
}

/// Validates an external link requested by a Mini App (`web_app_open_link`).
///
/// Port of `WebViewInstance::botValidateExternalLink` (`bot_attach_web_view.cpp:1816-1827`).
pub fn validate_webview_external_link(uri: &str, allowed_protocols: &[&str]) -> bool {
    let lower = uri.to_ascii_lowercase();
    let protocols = if allowed_protocols.is_empty() {
        DEFAULT_ALLOWED_EXTERNAL_PROTOCOLS
    } else {
        allowed_protocols
    };
    protocols
        .iter()
        .any(|proto| lower.starts_with(&format!("{}://", proto.to_ascii_lowercase())))
}

// ---------------------------------------------------------------------------
// Games & Share Game Score (data/data_game.h, window/window_peer_menu.cpp)
// ---------------------------------------------------------------------------

/// Bot game metadata (`GameData` in `data/data_game.h:13-24`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameData {
    /// Game unique identifier.
    pub id: GameId,
    /// Server access hash.
    pub access_hash: u64,
    /// Short name used in `t.me/<bot>?game=<short_name>`.
    pub short_name: String,
    /// Display title.
    pub title: String,
    /// Game description.
    pub description: String,
    /// Optional preview photo id.
    pub photo_id: Option<PhotoId>,
    /// Optional preview animation document id.
    pub document_id: Option<DocumentId>,
}

/// A single entry in a game's high-score table (`highScore`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GameHighScore {
    /// 1-based leaderboard rank.
    pub pos: i32,
    /// Player user id.
    pub user_id: UserId,
    /// Numeric score.
    pub score: i32,
}

/// Plain request struct for `messages.setGameScore` (`schema/api.tl:2687`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SetGameScoreRequest {
    /// Edit the game message to display updated scores (`flags.0`).
    pub edit_message: bool,
    /// Allow decreasing a previously higher score (`flags.1`).
    pub force: bool,
    /// Chat peer containing the game message.
    pub peer_id: PeerId,
    /// Game message id.
    pub msg_id: MsgId,
    /// Player user id.
    pub user_id: UserId,
    /// New score value.
    pub score: i32,
}

/// Action triggered by the WebView `"share_score"` JS bridge command or the panel's
/// `ShareGame` menu button (`attach_bot_webview.cpp:2532-2534`, `bot_attach_web_view.cpp:1783-1794`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ShareGameScoreAction {
    /// Forward/share the game message (`FastShareMessage`).
    ShareMessage {
        /// Message id of the game being played.
        message_id: FullMsgId,
    },
    /// No valid game message id is associated with this WebView session.
    Unavailable,
}

/// Resolves the action for a `"share_score"` / `MenuButton::ShareGame` trigger.
pub fn handle_share_game_score(source: &WebViewSource) -> ShareGameScoreAction {
    match source {
        WebViewSource::Game { message_id, .. } if message_id.is_valid() => {
            ShareGameScoreAction::ShareMessage {
                message_id: *message_id,
            }
        }
        _ => ShareGameScoreAction::Unavailable,
    }
}

/// Evaluates whether a game may be shared into a target peer (`ShowShareGameBox` filter
/// in `window/window_peer_menu.cpp:3937-3941`).
pub const fn can_share_game_to_peer(is_self: bool, can_send_games: bool, is_forum: bool) -> bool {
    !is_self && (can_send_games || is_forum)
}

/// Builds the canonical Telegram share link for a bot game (`https://t.me/<bot>?game=<short_name>`).
pub fn build_game_share_link(bot_username: &str, short_name: &str) -> String {
    format!("https://t.me/{bot_username}?game={short_name}")
}
