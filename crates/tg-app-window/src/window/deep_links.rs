//! Deep links router (`tg://`, `t.me/`, `telegram.me/`, `telegram.dog/`, `tonsite://`, `internal:`).
//!
//! Port origin:
//! - `Telegram/SourceFiles/core/local_url_handlers.h`
//! - `Telegram/SourceFiles/core/local_url_handlers.cpp`
//! - `Telegram/SourceFiles/core/deep_links/deep_links_types.h`
//! - `Telegram/SourceFiles/core/deep_links/deep_links_router.cpp`
//! - `Telegram/SourceFiles/core/deep_links/deep_links_settings.cpp`
//! - `Telegram/SourceFiles/core/deep_links/deep_links_chats.cpp`
//! - `Telegram/SourceFiles/core/deep_links/deep_links_contacts.cpp`
//! - `Telegram/SourceFiles/core/deep_links/deep_links_new.cpp`

use std::collections::BTreeMap;

use tg_bots::callback::{
    UrlAuthDecision, UrlAuthErrorAction, UrlAuthSource, handle_url_auth_request_error,
    handle_url_auth_request_result,
};
use tg_bots::deep_links::{
    MAX_LOCAL_URL_LEN, is_valid_domain, parse_query_params, parse_requested_admin_rights,
    percent_decode,
};
use tg_bots::markup::{PeerTypes, parse_choose_types};
use tg_bots::stand_in::ChatAdminRights;

use crate::window::layer_stack::ModalBox;
use crate::window::section::{SectionShow, SettingsPage};
use crate::window::session_controller::SessionController;
use tg_app_core::session_api::{AppSession, ChatInvitePreview};
use tg_app_core::stand_in::types::{MsgId, PeerId};
use tg_app_history::bots::backend::BotsBackend;
use tg_app_info::boxes::BoxKind;
use tg_app_info::boxes::add_contact::AddContactBoxState;
use tg_app_info::boxes::click_handlers::{
    BankCardActionBoxState, PhoneActionBoxState, UrlAuthConfirmBoxState, normalize_phone_digits,
};
use tg_app_info::boxes::compose_ai::PreviewAiToneBoxState;
use tg_app_info::boxes::confirm::{ConfirmBoxState, ConfirmPhoneCodeBoxState, ConfirmTag};
use tg_app_info::boxes::create_peer::{CreateChannelBoxState, CreateGroupBoxState};
use tg_app_info::boxes::join_invite::JoinInviteBoxState;
use tg_app_info::boxes::language::LanguageBoxState;
use tg_app_info::boxes::share::ShareBoxState;
use tg_app_info::boxes::sticker_set::StickerSetBoxState;
use tg_app_info::boxes::username::UsernameBoxState;
use tg_app_info::info::{InfoBackend, SharedMediaTab};
use tg_app_info::payments::PaymentsScreen;
use tg_app_info::statistics::StatisticsScreen;
use tg_app_settings::settings::SettingsSection;

/// Standard desktop unsupported-link notice (`DeepLinks::Router::showUnsupportedMessage`).
pub const UNSUPPORTED_LINK_MESSAGE: &str = "This link is not supported on Desktop.";

/// Canonical `internal:` prefix used for `t.me/getpremium` (`kGetPremiumLocal`).
pub const GET_PREMIUM_LOCAL_PREFIX: &str = "internal:tme_getpremium";

/// Maximum `ref` length for `t.me/getpremium` (`kGetPremiumRefMaxLength`).
const GET_PREMIUM_REF_MAX_LEN: usize = 32;

/// Proxy protocol targeted by a `tg://socks`, `tg://proxy`, or `tg://webproxy` link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyLinkKind {
    /// SOCKS5 proxy (`tg://socks?...`).
    Socks5,
    /// MTProto proxy (`tg://proxy?...`).
    Mtproto,
    /// Web proxy (`tg://webproxy?...`).
    Web,
}

/// Resolution mode for a `tg://resolve?...` link (`Window::ResolveType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ResolveType {
    /// Standard chat / post navigation.
    #[default]
    Default,
    /// Direct named Mini App (`t.me/bot/appname`).
    BotApp,
    /// Bot `/start <token>` (`?start=...`).
    BotStart,
    /// Add bot to group (`?startgroup=...`).
    AddToGroup,
    /// Add bot to channel (`?startchannel`).
    AddToChannel,
    /// Share HTML5 game (`?game=...`).
    ShareGame,
    /// Channel boost sheet (`?boost`).
    Boost,
    /// Channel direct messages (`?direct`).
    ChannelDirect,
    /// Open peer profile directly (`?profile`).
    Profile,
}

/// Comment or forum topic / thread target parsed from `comment=`, `topic=`, or `thread=`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepliesByLink {
    /// Jump to a specific comment message inside a post's discussion thread (`comment=<id>`).
    Comment(i32),
    /// Open a forum topic or reply thread (`topic=<id>` or `thread=<id>`).
    Thread(i32),
}

/// Structured parameters for `tg://resolve?...` (`Window::PeerByLinkInfo`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ResolveLinkInfo {
    /// Public username (`domain=`), normalized for `t.me/s/<username>`.
    pub domain: Option<String>,
    /// Numeric phone (`phone=`, digits only).
    pub phone: Option<String>,
    /// Target message ID (`post=`).
    pub post_id: Option<i32>,
    /// Optional comment or topic/thread target.
    pub replies: Option<RepliesByLink>,
    /// Optional story ID or `"live"` (`story=`).
    pub story: Option<String>,
    /// Optional story album ID (`album=`).
    pub story_album_id: Option<i32>,
    /// Optional gift collection ID (`collection=`).
    pub gift_collection_id: Option<i32>,
    /// Optional shared media tab (`tab=`).
    pub shared_media_tab: Option<SharedMediaTab>,
    /// Whether `?sidebar` was requested.
    pub sidebar: bool,
    /// Optional video timestamp in seconds (`t=1h2m3s` or `t=90`).
    pub video_timestamp: Option<i64>,
    /// Optional pre-filled compose text (`text=`).
    pub text: Option<String>,
    /// High-level resolve mode (`Window::ResolveType`).
    pub resolve_type: ResolveType,
    /// Optional referral token (`ref=`).
    pub referral: Option<String>,
    /// Optional no-paid-message token (`free=`).
    pub no_paid_token: Option<String>,
    /// Bot start / game / startgroup token.
    pub start_token: Option<String>,
    /// Requested bot admin rights (`admin=`).
    pub start_admin_rights: ChatAdminRights,
    /// Named Mini App short name (`appname=` or fallback `post` param).
    pub bot_app_name: Option<String>,
    /// Fullscreen Mini App requested (`mode=fullscreen`).
    pub bot_app_fullscreen: bool,
    /// Explicit attach-menu bot username (`attach=`).
    pub attach_bot_username: Option<String>,
    /// Attach-menu or main-app toggle payload (`startattach=` or `startapp=`).
    pub attach_bot_toggle_command: Option<String>,
    /// Main Mini App launch via `?startapp` without `appname`.
    pub attach_bot_main_open: bool,
    /// Compact main Mini App launch (`?startapp&mode=compact`).
    pub attach_bot_main_compact: bool,
    /// Allowed chat types for attach-menu bot (`choose=`).
    pub attach_bot_choose_types: PeerTypes,
    /// Voice chat / video chat / livestream invite hash (`Some("")` if present with empty value).
    pub voicechat_hash: Option<String>,
    /// Poll option bytes decoded from `option=`.
    pub poll_option: Option<String>,
    /// Open in new window hint (`tdesktop_target=blank`).
    pub open_in_new_window: bool,
}

/// Structured parameters for `tg://privatepost?...` (`t.me/c/<channel>[/<topic>]/<post>`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrivatePostLinkInfo {
    /// Raw positive channel ID (without `-100` prefix).
    pub channel_id: u64,
    /// Optional target message ID (`post=`).
    pub post_id: Option<i32>,
    /// Optional comment or forum topic/thread ID.
    pub replies: Option<RepliesByLink>,
    /// Optional shared media tab (`tab=`).
    pub shared_media_tab: Option<SharedMediaTab>,
    /// Whether `?sidebar` was requested.
    pub sidebar: bool,
    /// Optional poll option token (`option=`).
    pub poll_option: Option<String>,
}

/// Structured parameters for `tg://passport?...` / `tg://resolve?domain=telegrampassport&...`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PassportLinkInfo {
    /// Requesting bot user ID.
    pub bot_id: u64,
    /// Requested scope string (JSON object or legacy scope).
    pub scope: String,
    /// Optional callback URL.
    pub callback_url: String,
    /// Bot RSA public key PEM.
    pub public_key: String,
    /// Nonce (`nonce=` for JSON scopes, `payload=` for legacy scopes).
    pub nonce: String,
}

/// Structured parameters for `tg://bg?...` / `t.me/bg/...`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WallPaperLinkInfo {
    /// Solid hex color, gradient spec, or cloud wallpaper slug.
    pub slug_or_color: String,
    /// Whether `slug_or_color` came from `color=` or `gradient=` rather than `slug=`.
    pub is_color_or_gradient: bool,
    /// Full lowercase query parameter map (`mode`, `intensity`, `rotation`, `bg_color`, ...).
    pub params: BTreeMap<String, String>,
}

/// Structured target for `tg://settings/...`, `tg://contacts/...`, `tg://chats/...`, `tg://new/...`
/// (`Core::DeepLinks::Router`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouterDeepLink {
    /// Open a Settings section (with optional control highlight ID).
    Settings {
        /// Coarse shell page for `SessionController::show_settings`.
        page: SettingsPage,
        /// Detailed `SettingsSection` pushed onto `SettingsController`.
        section: SettingsSection,
        /// Optional control highlight ID (`highlight=` or route-specific control).
        highlight: Option<String>,
    },
    /// Open the Language picker (`tg://settings/language[/...]`, `requiresAuth = false`).
    Language {
        /// Optional highlight control ID.
        highlight: Option<String>,
    },
    /// Open Proxy settings (`tg://settings/data/proxy[/...]`).
    ProxySettings {
        /// Optional highlight control ID.
        highlight: Option<String>,
    },
    /// Open Saved Messages (`tg://settings/saved-messages`).
    SavedMessages,
    /// Open self profile (`tg://settings/my-profile[/...]`).
    MyProfile {
        /// Optional highlight control ID.
        highlight: Option<String>,
    },
    /// Open self QR code / profile (`tg://settings/qr-code[/share]`).
    QrCode {
        /// Whether share highlight was requested.
        highlight_share: bool,
    },
    /// Open edit username modal (`tg://settings/edit/username`).
    EditUsername,
    /// Open Payments / Stars / Premium / Business overlay.
    OpenPaymentsOverlay {
        /// Target PaymentsScreen tab.
        screen: PaymentsScreen,
    },
    /// Open Telegram Business overlay (`tg://settings/business[/...]`).
    OpenBusinessOverlay,
    /// Open Recent Calls (`tg://settings/calls/all`, `tg://settings/calls/start-call`).
    OpenRecentCalls,
    /// Open FAQ / Features external page (`requiresAuth = false`).
    OpenExternalInfo {
        /// Target info URL.
        url: String,
    },
    /// Focus global chat search (`tg://chats/search`).
    FocusSearch,
    /// Open Main Menu (`tg://chats/emoji-status`, `tg://settings/appearance/night-mode`).
    OpenMainMenu {
        /// Optional highlight control ID.
        highlight: Option<String>,
    },
    /// Open Contacts / Add Contact modal (`tg://contacts`, `tg://contacts/new`, `tg://new/contact`).
    OpenContacts {
        /// `true` when adding a new contact (`contacts/new`, `new/contact`).
        add_new: bool,
    },
    /// Open Create Group modal (`tg://new/group`).
    NewGroup,
    /// Open Create Channel modal (`tg://new/channel`).
    NewChannel,
    /// Create managed bot (`tg://newbot?manager=...&username=...&name=...`).
    NewBot {
        /// Parent manager bot username.
        manager: String,
        /// Optional suggested bot username.
        username: Option<String>,
        /// Optional suggested display name.
        name: Option<String>,
    },
    /// Informational modal (`tg://settings/edit/change-number`, etc.).
    Inform {
        /// Dialog title.
        title: String,
        /// Dialog body copy.
        body: String,
    },
}

/// Parsed `internal:` URL action (`InternalUrlHandlers()`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InternalDeepLink {
    /// Jump to media timestamp (`internal:media_timestamp?base=...&t=...`).
    MediaTimestamp {
        /// Media base descriptor (e.g. `doc<id>_<peer>_<msg>`).
        base: String,
        /// Position in seconds.
        position_secs: i64,
    },
    /// Copy base64-encoded invite link (`internal:show_invite_link?link=...`).
    ShowInviteLink {
        /// Decoded invite link text.
        link: String,
    },
    /// Copy raw text/peer ID (`internal:copy:<value>`).
    CopyText(String),
    /// Premium promo for saved-message tags (`internal:about_tags`).
    AboutTags,
    /// Open Premium screen from `t.me/getpremium` (`internal:tme_getpremium[?ref=...]`).
    GetPremium {
        /// Normalized source token (`tme_getpremium` or `tme_getpremium_<ref>`).
        source: String,
    },
    /// Open common groups for a peer (`internal:common_groups/<peer_id>`).
    CommonGroups(u64),
    /// Open Edit Peer modal (`internal:edit_peer/<peer_id>`).
    EditPeer(u64),
    /// Copy `@username` or `https://t.me/<username>` link.
    CopyUsername {
        /// Username without `@`.
        username: String,
        /// Whether to format as full `https://t.me/<username>` link.
        as_link: bool,
    },
    /// Other recognized internal links (birthday, personal channel, collectibles, etc.).
    Other {
        /// Internal command path.
        command: String,
    },
}

/// Complete parsed representation of a Telegram deep link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeepLink {
    /// Matched by `Core::DeepLinks::Router` (`settings`, `contacts`, `chats`, `new`, `newbot`).
    Router(RouterDeepLink),
    /// `tg://join?invite=<hash>` / `t.me/+<hash>` / `t.me/joinchat/<hash>`.
    JoinChat {
        /// Invite hash.
        hash: String,
    },
    /// `tg://addlist?slug=<slug>` / `t.me/addlist/<slug>`.
    AddList {
        /// Shareable folder slug.
        slug: String,
    },
    /// `tg://addstickers?set=<name>` / `tg://addemoji?set=<name>`.
    AddStickers {
        /// Sticker or custom-emoji set short name.
        short_name: String,
        /// `true` for `addemoji`, `false` for `addstickers`.
        is_emoji: bool,
    },
    /// `tg://addtheme?slug=<slug>` / `t.me/addtheme/<slug>`.
    AddTheme {
        /// Cloud theme slug.
        slug: String,
    },
    /// `tg://addstyle?slug=<slug>` / `t.me/addstyle/<slug>`.
    AddStyle {
        /// AI compose style slug.
        slug: String,
    },
    /// `tg://setlanguage[?lang=<code>]` / `t.me/setlanguage/<code>`.
    SetLanguage {
        /// Optional target language pack code (`None` opens the Language picker box).
        lang: Option<String>,
    },
    /// `tg://msg_url?url=...&text=...` / `t.me/share/url?...`.
    ShareUrl {
        /// Non-empty share URL (guaranteed not starting with `@`).
        url: String,
        /// Optional accompanying comment text.
        text: String,
    },
    /// `tg://confirmphone?phone=...&hash=...` / `t.me/confirmphone?...`.
    ConfirmPhone {
        /// Phone number string.
        phone: String,
        /// Confirmation hash.
        hash: String,
    },
    /// `tg://socks?...`, `tg://proxy?...`, `tg://webproxy?...`.
    Proxy {
        /// Proxy protocol type.
        kind: ProxyLinkKind,
        /// Parsed lowercase query parameters (`server`, `port`, `user`, `pass`, `secret`, ...).
        params: BTreeMap<String, String>,
    },
    /// `tg://passport?...` or `tg://resolve?domain=telegrampassport&...`.
    Passport(PassportLinkInfo),
    /// `tg://bg?...` / `t.me/bg/...`.
    WallPaper(WallPaperLinkInfo),
    /// `tg://resolve?...` / `t.me/<user>[/...]` / `@username`.
    Resolve(Box<ResolveLinkInfo>),
    /// `tg://contact?token=...[&free=...]`.
    ContactToken {
        /// Contact token.
        token: String,
        /// Optional no-paid-message token.
        no_paid_token: Option<String>,
    },
    /// `tg://privatepost?channel=...` / `t.me/c/<id>[/<topic>]/<msg>`.
    PrivatePost(PrivatePostLinkInfo),
    /// `tg://test_chat_theme?...`.
    TestChatTheme {
        /// Parsed query parameters.
        params: BTreeMap<String, String>,
    },
    /// `tg://invoice?slug=<slug>` / `t.me/invoice/<slug>` / `t.me/$<slug>`.
    Invoice {
        /// Invoice slug.
        slug: String,
    },
    /// `tg://premium_offer[?ref=...]`.
    PremiumOffer {
        /// Normalized referral string (`deeplink` or `deeplink_<ref>`).
        referral: String,
    },
    /// `tg://premium_multigift?...`.
    PremiumMultigift {
        /// Raw query parameters.
        params: BTreeMap<String, String>,
    },
    /// `tg://login?code=<digits>`.
    LoginCode {
        /// Numeric login code.
        code: String,
    },
    /// `tg://boost?domain=...` / `tg://boost?c=...` / `t.me/boost/<domain>`.
    Boost {
        /// Public channel username, if specified.
        domain: Option<String>,
        /// Private channel ID (`c=` or `channel=`), if specified.
        channel_id: Option<u64>,
    },
    /// `tg://message?slug=<slug>` / `t.me/m/<slug>` (business chat link).
    BusinessChatLink {
        /// Business chat link slug.
        slug: String,
    },
    /// `tg://stars_topup?balance=...&purpose=...`.
    StarsTopUp {
        /// Clamped stars amount (`1..=1_000_000`).
        amount: u64,
        /// Optional top-up purpose string.
        purpose: String,
    },
    /// `tg://nft?slug=<slug>` / `t.me/nft/<slug>`.
    UniqueGift {
        /// Collectible Star Gift slug.
        slug: String,
    },
    /// `tg://stargift_auction?slug=<slug>` / `t.me/auction/<slug>`.
    StarGiftAuction {
        /// Auction slug.
        slug: String,
    },
    /// `tg://call?slug=<slug>` / `t.me/call/<slug>`.
    ConferenceCall {
        /// Conference call invite slug.
        slug: String,
    },
    /// `tg://stars`.
    StarsSettings,
    /// `tg://ton` or `tg://grams`.
    TonSettings,
    /// `tg://sendgrams?...`, `tc://...`, or `ton://transfer/...`.
    SendGrams {
        /// Optional recipient.
        to: Option<String>,
        /// Optional amount.
        amount: Option<String>,
        /// Optional TON Connect `startapp` payload.
        startapp: Option<String>,
        /// Optional raw `ton://transfer/...` URI.
        transfer_uri: Option<String>,
    },
    /// `tg://oauth?token=<token>` or `t.me/oauth?startapp=<token>`.
    OAuth {
        /// OAuth token.
        token: String,
    },
    /// `t.me/giftcode/<slug>` (`tg://resolve?domain=giftcode&appname=<slug>`).
    GiftCode {
        /// Premium gift code slug.
        slug: String,
    },
    /// Clicked phone number entity (`tel:<phone>` / `core/phone_click_handler.cpp`).
    Phone {
        /// Phone number string.
        phone: String,
    },
    /// Clicked bank card number entity (`bankcard:<number>` / `core/bank_card_click_handler.cpp`).
    BankCard {
        /// Bank card number string.
        number: String,
    },
    /// `t.me/iv?url=...&rhash=...` (kept as `t.me/iv` URL for `messages.getWebPage`).
    InstantView {
        /// Full `t.me/iv` URL (without `#fragment`).
        url: String,
        /// Optional `#fragment` anchor.
        fragment: Option<String>,
    },
    /// `tonsite://...` or `https://<name>.ton/...`.
    TonSite {
        /// Canonical `tonsite://...` URI.
        uri: String,
    },
    /// `internal:...` action.
    Internal(InternalDeepLink),
    /// Unknown `tg://<path>` fallback (`HandleUnknown` -> `help.getDeepLinkInfo`).
    Unknown {
        /// Command path before `?`/`#`.
        path: String,
    },
    /// Explicitly unsupported deep link (e.g. unrecognized `tg://settings/...` subpath).
    Unsupported {
        /// Original command/URL.
        command: String,
    },
}

/// Converts Telegram web URLs (`t.me/...`, `telegram.me/...`, `telegram.dog/...`,
/// `<user>.t.me/...`, `tc://...`, `*.ton`) to their canonical local `tg://`, `internal:`,
/// or `tonsite://` form.
///
/// Exact port of `Core::TryConvertUrlToLocal` in `core/local_url_handlers.cpp:2060-2264`,
/// preserving tdesktop's exact matching order.
#[must_use]
pub fn try_convert_url_to_local(url: &str) -> String {
    let url = truncate_utf8(url, MAX_LOCAL_URL_LEN);

    // 1. `tc://...` -> `tg://sendgrams?startapp=tonconnect-...` (`local_url_handlers.cpp:2064-2069`)
    if let Some(rest) = strip_prefix_ci(url, "tc://") {
        let query = rest.split_once('?').map_or("", |(_, q)| q);
        return format!(
            "tg://sendgrams?startapp={}",
            tg_wallet::tonconnect::ton_connect_start_param(query)
        );
    }

    // 2. `*.ton` -> `tonsite://...`
    if url.to_ascii_lowercase().contains(".ton")
        && let Some((protocol, rest)) = match_tonsite(url)
    {
        let _ = protocol;
        return format!("tonsite://{rest}");
    }

    // 3. `<username>.t.me[/...][?...]` subdomain form
    if let Some((protocol, name, tail)) = match_tme_subdomain(url) {
        let rebuilt = format!("{protocol}t.me/{name}{tail}");
        let converted = try_convert_url_to_local(&rebuilt);
        if converted.starts_with("tg://resolve?domain=") || is_get_premium_local_url(&converted) {
            return converted;
        }
        return url.to_owned();
    }

    // 4. `(https?://)?(www.)?(telegram.(me|dog)|t.me)/<query>`
    let Some(query) = extract_telegram_me_path(url) else {
        return url.to_owned();
    };
    if query.is_empty() {
        return url.to_owned();
    }

    // Match in exact tdesktop order (`local_url_handlers.cpp:2107-2262`):

    // 4.1 `contact/<token>[?<params>][#...]`
    if let Some(after) = strip_prefix_ci(query, "contact/") {
        let no_frag = after.split('#').next().unwrap_or("");
        let (raw_token, raw_params) = match no_frag.split_once('?') {
            Some((t, p)) => (t, p),
            None => (no_frag, ""),
        };
        let decoded = percent_decode(raw_token, false);
        let encoded = percent_encode_keeping(&decoded, b":");
        return if raw_params.is_empty() {
            format!("tg://contact?token={encoded}")
        } else {
            format!("tg://contact?token={encoded}&{raw_params}")
        };
    }

    // 4.2 `+<digits>(\?|$)` -> `tg://resolve?phone=<digits>[&<params>]`
    if let Some(after_plus) = query.strip_prefix('+') {
        let (digits, rest) = split_at_question(after_plus);
        if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
            return if rest.is_empty() {
                format!("tg://resolve?phone={digits}")
            } else {
                format!("tg://resolve?phone={digits}&{rest}")
            };
        }
    }

    // 4.3 `(joinchat/|\+|%20)([a-zA-Z0-9._-]+)(\?|$)` -> `tg://join?invite=<hash>`
    if let Some(after_join) = strip_prefix_ci(query, "joinchat/")
        .or_else(|| query.strip_prefix('+'))
        .or_else(|| strip_prefix_ci(query, "%20"))
    {
        let (hash, _) = split_at_question(after_join);
        if is_valid_slug_token(hash) {
            return format!("tg://join?invite={}", percent_encode_value(hash));
        }
    }

    // 4.4 `addlist/([a-zA-Z0-9._-]+)(\?|$)` -> `tg://addlist?slug=<slug>`
    if let Some(after) = strip_prefix_ci(query, "addlist/") {
        let (slug, _) = split_at_question(after);
        if is_valid_slug_token(slug) {
            return format!("tg://addlist?slug={}", percent_encode_value(slug));
        }
    }

    // 4.5 `(addstickers|addemoji)/([a-zA-Z0-9._]+)(\?|$)` -> `tg://<kind>?set=<set>`
    if let Some((kind, after)) = strip_prefix_ci(query, "addstickers/")
        .map(|a| ("addstickers", a))
        .or_else(|| strip_prefix_ci(query, "addemoji/").map(|a| ("addemoji", a)))
    {
        let (set_name, _) = split_at_question(after);
        if is_valid_domain(set_name) {
            return format!("tg://{kind}?set={}", percent_encode_value(set_name));
        }
    }

    // 4.6 `addtheme/([a-zA-Z0-9._]+)(\?|$)` -> `tg://addtheme?slug=<slug>`
    if let Some(after) = strip_prefix_ci(query, "addtheme/") {
        let (slug, _) = split_at_question(after);
        if is_valid_domain(slug) {
            return format!("tg://addtheme?slug={}", percent_encode_value(slug));
        }
    }

    // 4.7 `addstyle/([a-zA-Z0-9._]+)(\?|$)` -> `tg://addstyle?slug=<slug>`
    if let Some(after) = strip_prefix_ci(query, "addstyle/") {
        let (slug, _) = split_at_question(after);
        if is_valid_domain(slug) {
            return format!("tg://addstyle?slug={}", percent_encode_value(slug));
        }
    }

    // 4.8 `setlanguage/([a-zA-Z0-9._-]+)(\?|$)` -> `tg://setlanguage?lang=<lang>`
    if let Some(after) = strip_prefix_ci(query, "setlanguage/") {
        let (lang, _) = split_at_question(after);
        if is_valid_slug_token(lang) {
            return format!("tg://setlanguage?lang={}", percent_encode_value(lang));
        }
    }

    // 4.9 `share/url/?\?(.+)$` -> `tg://msg_url?<params>`
    if let Some(params) = strip_prefix_with_opt_slash_and_query(query, "share/url")
        && !params.is_empty()
    {
        return format!("tg://msg_url?{params}");
    }

    // 4.10 `confirmphone/?\?(.+)` -> `tg://confirmphone?<params>`
    if let Some(params) = strip_prefix_with_opt_slash_and_query(query, "confirmphone")
        && !params.is_empty()
    {
        return format!("tg://confirmphone?{params}");
    }

    // 4.11 `iv/?\?(.+)(#|$)` -> return `url` untouched for Instant View (`messages.getWebPage`)
    if let Some(params) = strip_prefix_with_opt_slash_and_query(query, "iv")
        && !params.split('#').next().unwrap_or("").is_empty()
    {
        return url.to_owned();
    }

    // 4.12 `socks/?\?(.+)(#|$)` -> `tg://socks?<params>`
    if let Some(params) = strip_prefix_with_opt_slash_and_query(query, "socks") {
        let before_frag = params.split('#').next().unwrap_or("");
        if !before_frag.is_empty() {
            return format!("tg://socks?{before_frag}");
        }
    }

    // 4.13 `proxy/?\?(.+)(#|$)` -> `tg://proxy?<params>`
    if let Some(params) = strip_prefix_with_opt_slash_and_query(query, "proxy") {
        let before_frag = params.split('#').next().unwrap_or("");
        if !before_frag.is_empty() {
            return format!("tg://proxy?{before_frag}");
        }
    }

    // 4.14 `webproxy/?\?(.+)(#|$)` -> `tg://webproxy?<params>`
    if let Some(params) = strip_prefix_with_opt_slash_and_query(query, "webproxy") {
        let before_frag = params.split('#').next().unwrap_or("");
        if !before_frag.is_empty() {
            return format!("tg://webproxy?{before_frag}");
        }
    }

    // 4.15 `(invoice/|\$)([a-zA-Z0-9_-]+)(\?|#|$)` -> `tg://invoice?slug=<slug>`
    if let Some(after) = strip_prefix_ci(query, "invoice/").or_else(|| query.strip_prefix('$')) {
        let slug = after.split(['?', '#']).next().unwrap_or("");
        if is_valid_invoice_slug(slug) {
            return format!("tg://invoice?slug={slug}");
        }
    }

    // 4.16 `bg/([a-zA-Z0-9._~-]+)(\?(.+)?)?$` -> `tg://bg?<color|gradient|slug>=<bg>[&<params>]`
    if let Some(after) = strip_prefix_ci(query, "bg/") {
        let (bg, params) = split_at_question(after);
        if is_valid_bg_token(bg) {
            let bg_type = if is_hex_color_6(bg) {
                "color"
            } else if is_bg_gradient(bg) {
                "gradient"
            } else {
                "slug"
            };
            return if params.is_empty() {
                format!("tg://bg?{bg_type}={bg}")
            } else {
                format!("tg://bg?{bg_type}={bg}&{params}")
            };
        }
    }

    // 4.17 `m/([a-zA-Z0-9._-]+)(\?|$)` -> `tg://message?slug=<slug>`
    if let Some(after) = strip_prefix_ci(query, "m/") {
        let (slug, _) = split_at_question(after);
        if is_valid_slug_token(slug) {
            return format!("tg://message?slug={slug}");
        }
    }

    // 4.18 `nft/([a-zA-Z0-9._-]+)(\?|$)` -> `tg://nft?slug=<slug>`
    if let Some(after) = strip_prefix_ci(query, "nft/") {
        let (slug, _) = split_at_question(after);
        if is_valid_slug_token(slug) {
            return format!("tg://nft?slug={slug}");
        }
    }

    // 4.19 `auction/([a-zA-Z0-9._-]+)(\?|$)` -> `tg://stargift_auction?slug=<slug>`
    if let Some(after) = strip_prefix_ci(query, "auction/") {
        let (slug, _) = split_at_question(after);
        if is_valid_slug_token(slug) {
            return format!("tg://stargift_auction?slug={slug}");
        }
    }

    // 4.20 `call/([a-zA-Z0-9._-]+)(\?|$)` -> `tg://call?slug=<slug>`
    if let Some(after) = strip_prefix_ci(query, "call/") {
        let (slug, _) = split_at_question(after);
        if is_valid_slug_token(slug) {
            return format!("tg://call?slug={slug}");
        }
    }

    // 4.21 `sendgrams/?(\?(.*))?$` -> `tg://sendgrams[?<params>]`
    if let Some(after) = strip_prefix_ci(query, "sendgrams")
        && (after.is_empty() || after == "/" || after.starts_with('?') || after.starts_with("/?"))
    {
        let params = after
            .strip_prefix("/?")
            .or_else(|| after.strip_prefix('?'))
            .unwrap_or("");
        return if params.is_empty() {
            "tg://sendgrams".to_owned()
        } else {
            format!("tg://sendgrams?{params}")
        };
    }

    // 4.22 `newbot/([a-zA-Z0-9._]+)(/([a-zA-Z0-9._]*))?(/?\?(.+))?$`
    if let Some(after) = strip_prefix_ci(query, "newbot/") {
        let (path_part, params) = split_at_question(after);
        let trimmed_path = path_part.trim_end_matches('/');
        let mut segs = trimmed_path.split('/');
        let manager = segs.next().unwrap_or("");
        let username = segs.next().unwrap_or("");
        if is_valid_domain(manager)
            && segs.next().is_none()
            && (username.is_empty() || is_valid_domain(username))
        {
            let mut out = format!("tg://newbot?manager={}", percent_encode_value(manager));
            if !username.is_empty() {
                out.push_str("&username=");
                out.push_str(&percent_encode_value(username));
            }
            if !params.is_empty() {
                out.push('&');
                out.push_str(params);
            }
            return out;
        }
    }

    // 4.23 `c/(-?\d+)(/?\?|/?$|/\d+/?(\?|$)|/\d+/\d+/?(\?|$))`
    if let Some(after) = strip_prefix_ci(query, "c/") {
        let (path_part, params) = split_at_question(after);
        let trimmed_path = path_part.trim_end_matches('/');
        let mut segs = trimmed_path.split('/');
        let channel = segs.next().unwrap_or("");
        let seg2 = segs.next();
        let seg3 = segs.next();
        if is_signed_digits(channel) && segs.next().is_none() {
            let valid_tail = match (seg2, seg3) {
                (None, None) => true,
                (Some(p), None) => is_digits(p),
                (Some(t), Some(p)) => is_digits(t) && is_digits(p),
                (None, Some(_)) => false,
            };
            if valid_tail {
                if has_boost_query_flag(params) {
                    return format!("tg://boost?channel={channel}");
                }
                let mut out = format!("tg://privatepost?channel={channel}");
                match (seg2, seg3) {
                    (Some(topic), Some(post)) => {
                        out.push_str(&format!("&topic={topic}&post={post}"));
                    }
                    (Some(post), None) => {
                        out.push_str(&format!("&post={post}"));
                    }
                    _ => {}
                }
                if !params.is_empty() {
                    out.push('&');
                    out.push_str(params);
                }
                return out;
            }
        }
    }

    // 4.24 `getpremium(/|\?|#|$)` -> `internal:tme_getpremium[?ref=...]`
    if let Some(after) = strip_prefix_ci(query, "getpremium")
        && (after.is_empty()
            || after.starts_with('/')
            || after.starts_with('?')
            || after.starts_with('#'))
    {
        return get_premium_local_url(query);
    }

    // 4.25 Username / post / topic+post / story / album / collection / appname / boost
    let (raw_path, params) = split_at_question(query);
    let trimmed_path = raw_path.trim_end_matches('/');
    let mut segs = trimmed_path.split('/');
    let domain = segs.next().unwrap_or("");
    if is_valid_domain(domain) {
        let seg2 = segs.next();
        let seg3 = segs.next();
        if segs.next().is_none() {
            // Check boost patterns first (matching `local_url_handlers.cpp:2235-2244`).
            if has_boost_query_flag(params) {
                return format!("tg://boost?domain={domain}");
            }
            if domain.eq_ignore_ascii_case("boost") {
                if let (Some(target), None) = (seg2, seg3)
                    && is_valid_domain(target)
                {
                    return format!("tg://boost?domain={target}");
                }
                if params.to_ascii_lowercase().contains("c=") {
                    return format!("tg://boost?{params}");
                }
            }

            let added = match (seg2, seg3) {
                (None, None) => Some(String::new()),
                (Some(p), None) if is_digits(p) => Some(format!("&post={p}")),
                (Some(app), None) if is_valid_slug_token(app) => Some(format!("&appname={app}")),
                (Some(t), Some(p)) if is_digits(t) && is_digits(p) => {
                    Some(format!("&topic={t}&post={p}"))
                }
                (Some(s), Some(id))
                    if s.eq_ignore_ascii_case("s")
                        && (id.eq_ignore_ascii_case("live") || is_digits(id)) =>
                {
                    Some(format!("&story={id}"))
                }
                (Some(a), Some(id)) if a.eq_ignore_ascii_case("a") && is_digits(id) => {
                    Some(format!("&album={id}"))
                }
                (Some(c), Some(id)) if c.eq_ignore_ascii_case("c") && is_digits(id) => {
                    Some(format!("&collection={id}"))
                }
                _ => None,
            };

            if let Some(added_str) = added {
                let mut out = format!("tg://resolve?domain={}", percent_encode_value(domain));
                out.push_str(&added_str);
                if !params.is_empty() {
                    out.push('&');
                    out.push_str(params);
                }
                return out;
            }
        }
    }

    url.to_owned()
}

/// Returns `true` when `url` is an `internal:tme_getpremium` link (`Core::IsGetPremiumLocalUrl`).
#[must_use]
pub fn is_get_premium_local_url(url: &str) -> bool {
    let Some(rest) = strip_prefix_ci(url, GET_PREMIUM_LOCAL_PREFIX) else {
        return false;
    };
    rest.is_empty() || rest.starts_with('?')
}

/// Returns `true` when `url` resolves to a Mini App link (`Core::IsMiniAppUrl`).
#[must_use]
pub fn is_mini_app_url(url: &str) -> bool {
    let local = try_convert_url_to_local(url);
    let Some(query) = strip_prefix_ci(&local, "tg://resolve?") else {
        return false;
    };
    let params = parse_query_params(query);
    params.contains_key("appname")
        || params.contains_key("startapp")
        || params.contains_key("attach")
}

/// Returns `true` for Telegram Passport links (`Core::InternalPassportLink`).
#[must_use]
pub fn is_internal_passport_link(url: &str) -> bool {
    let trimmed = url.trim();
    let Some(cmd) = strip_prefix_ci(trimmed, "tg://") else {
        return false;
    };
    if strip_prefix_with_opt_slash_and_query(cmd, "passport")
        .is_some_and(|q| !q.split('#').next().unwrap_or("").is_empty())
    {
        return true;
    }
    if let Some(q) = strip_prefix_with_opt_slash_and_query(cmd, "resolve") {
        let params = parse_query_params(q);
        return params
            .get("domain")
            .is_some_and(|d| d.eq_ignore_ascii_case("telegrampassport"));
    }
    false
}

/// Returns `true` for Telegram Passport or OAuth links (`Core::InternalPassportOrOAuthLink`).
#[must_use]
pub fn is_internal_passport_or_oauth_link(url: &str) -> bool {
    if is_internal_passport_link(url) {
        return true;
    }
    let trimmed = url.trim();
    let Some(cmd) = strip_prefix_ci(trimmed, "tg://") else {
        return false;
    };
    if strip_prefix_with_opt_slash_and_query(cmd, "oauth")
        .is_some_and(|q| !q.split('#').next().unwrap_or("").is_empty())
    {
        return true;
    }
    if let Some(q) = strip_prefix_with_opt_slash_and_query(cmd, "resolve") {
        let params = parse_query_params(q);
        return params
            .get("domain")
            .is_some_and(|d| d.eq_ignore_ascii_case("oauth"));
    }
    false
}

/// Parses a video timestamp (`1h2m3s`, `2m30s`, `45s`, or raw integer seconds).
///
/// Exact port of `Core::ParseVideoTimestamp` (`local_url_handlers.cpp:2407-2415`).
#[must_use]
pub fn parse_video_timestamp(value: &str) -> i64 {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return 0;
    }
    if let Ok(raw) = trimmed.parse::<i64>() {
        return raw;
    }
    // Parse optional `(\d+)h`, `(\d+)m`, `(\d+)s` in order.
    let mut rest = trimmed;
    let mut total: i64 = 0;
    let mut matched_any = false;
    for (suffix, mult) in [('h', 3600i64), ('m', 60i64), ('s', 1i64)] {
        if let Some(pos) = rest.find(suffix) {
            let num_part = &rest[..pos];
            if !num_part.is_empty()
                && num_part.bytes().all(|b| b.is_ascii_digit())
                && let Ok(n) = num_part.parse::<i64>()
            {
                total = total.saturating_add(n.saturating_mul(mult));
                matched_any = true;
                rest = &rest[pos + 1..];
                continue;
            }
            return 0;
        }
    }
    if matched_any && rest.is_empty() {
        total
    } else {
        0
    }
}

/// Parses `tab=` shared media query parameter (`ParseSharedMediaType`).
#[must_use]
pub fn parse_shared_media_type(value: &str) -> Option<SharedMediaTab> {
    match value.to_ascii_lowercase().as_str() {
        "media" | "photos" | "videos" => Some(SharedMediaTab::PhotosVideos),
        "gifs" => Some(SharedMediaTab::Gifs),
        "files" => Some(SharedMediaTab::Files),
        "music" => Some(SharedMediaTab::Music),
        "links" => Some(SharedMediaTab::Links),
        "voice" => Some(SharedMediaTab::Voice),
        _ => None,
    }
}

/// Parses any user-clicked or IPC-activated URL into a structured [`DeepLink`], following
/// tdesktop's exact matching order:
/// 1. Direct `@username` mention
/// 2. `TryConvertUrlToLocal` (`t.me/`, `telegram.me/`, `telegram.dog/`, `<user>.t.me/`, `tc://`, `*.ton`)
/// 3. `tonsite://`
/// 4. `internal:` (`InternalUrlHandlers`)
/// 5. Explicit `t.me/iv?...` Instant View URL
/// 6. `tg://`:
///    a. `DeepLinks::Router` (`settings`, `contacts`, `chats`, `new`, `newbot`)
///    b. `LocalUrlHandlers` in exact tdesktop order, ending with `HandleUnknown` fallback
///
/// Returns `None` for ordinary external web URLs (`https://example.com`, etc.).
#[must_use]
pub fn parse_deep_link(url: &str) -> Option<DeepLink> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return None;
    }

    // 1. Direct `@username` mention click.
    if let Some(after_at) = trimmed.strip_prefix('@') {
        let clean = after_at.split(['?', '/', '#', ' ']).next().unwrap_or("");
        if is_valid_domain(clean) {
            return Some(DeepLink::Resolve(Box::new(ResolveLinkInfo {
                domain: Some(clean.to_owned()),
                ..ResolveLinkInfo::default()
            })));
        }
        return None;
    }

    // 1b. Phone entity click (`tel:<phone>` -> `core/phone_click_handler.cpp`).
    if let Some(after_tel) = strip_prefix_ci(trimmed, "tel:") {
        let decoded = percent_decode(after_tel.trim(), false);
        let phone = decoded.trim();
        if phone.chars().any(|c| c.is_ascii_digit()) {
            return Some(DeepLink::Phone {
                phone: phone.to_owned(),
            });
        }
        return None;
    }

    // 1c. Bank card entity click (`bankcard:<number>` -> `core/bank_card_click_handler.cpp`).
    if let Some(after_card) = strip_prefix_ci(trimmed, "bankcard:") {
        let decoded = percent_decode(after_card.trim(), false);
        let number = decoded.trim();
        let digit_count = number.chars().filter(char::is_ascii_digit).count();
        if digit_count >= 13 {
            return Some(DeepLink::BankCard {
                number: number.to_owned(),
            });
        }
        return None;
    }

    // 2. Canonicalize `t.me` / `telegram.me` / `telegram.dog` / `<user>.t.me` / `tc://` / `*.ton`.
    let local = try_convert_url_to_local(trimmed);

    // 3. `tonsite://`
    if strip_prefix_ci(&local, "tonsite://").is_some() {
        return Some(DeepLink::TonSite { uri: local });
    }

    // 3b. `ton://transfer/...` (TON wallet transfer URI)
    if strip_prefix_ci(&local, "ton://").is_some() {
        return Some(DeepLink::SendGrams {
            to: None,
            amount: None,
            startapp: None,
            transfer_uri: Some(local),
        });
    }

    // 4. `internal:` (`Application::openInternalUrl` -> `InternalUrlHandlers`)
    if let Some(after_internal) = strip_prefix_ci(&local, "internal:") {
        return parse_internal_url(after_internal).map(DeepLink::Internal);
    }

    // 5. Explicit `t.me/iv?url=...` Instant View URL
    if is_tme_iv_url(&local) {
        let (base, frag) = split_fragment(&local);
        return Some(DeepLink::InstantView {
            url: base.to_owned(),
            fragment: frag.map(ToOwned::to_owned),
        });
    }

    // 6. `tg://`
    let command = strip_prefix_ci(&local, "tg://")?;
    let command = truncate_utf8(command, MAX_LOCAL_URL_LEN);

    // 6a. `TryRouterForLocalUrl` (`Core::DeepLinks::Router`) runs BEFORE `LocalUrlHandlers()`.
    if let Some(routed) = try_router_for_local_url(command) {
        return Some(routed);
    }

    // 6b. `LocalUrlHandlers()` in exact tdesktop order.
    parse_local_url_handlers(command)
}

/// Splits a URL into `(base_without_fragment, Some(fragment))` when a non-empty `#fragment` exists.
#[must_use]
pub fn split_fragment(url: &str) -> (&str, Option<&str>) {
    match url.split_once('#') {
        Some((base, frag)) if !frag.is_empty() => (base, Some(frag)),
        Some((base, _)) => (base, None),
        None => (url, None),
    }
}

/// Returns `true` if `url` is a `t.me/iv?...`, `telegram.me/iv?...`, or `telegram.dog/iv?...` URL.
#[must_use]
pub fn is_tme_iv_url(url: &str) -> bool {
    let Some(path) = extract_telegram_me_path(url) else {
        return false;
    };
    strip_prefix_with_opt_slash_and_query(path, "iv")
        .is_some_and(|q| !q.split('#').next().unwrap_or("").is_empty())
}

pub use tg_app_core::core_types::window_types::{DeepLinkOverlayRequest, WalletDeepLinkAction};

/// Action returned by [`dispatch_deep_link`] for [`crate::window::controller::WindowController`]
/// to present modals, toasts, compose prefills, WebView/Instant View, or auxiliary overlays.
#[derive(Debug)]
pub enum DeepLinkOutcome {
    /// URL is not a Telegram deep link (caller may probe Instant View or open external browser).
    NotTelegram,
    /// Handled entirely via [`SessionController`] navigation state.
    Handled,
    /// Handled navigation and also pre-fill the active chat's compose text.
    PrefillCompose {
        /// Destination peer ID.
        peer_id: PeerId,
        /// Text to pre-fill in the composer.
        text: String,
    },
    /// Invoke `messages.startBot` (`t.me/<bot>?start=<token>` or `?startgroup=<token>`).
    StartBot {
        /// Target bot user peer ID.
        bot_id: PeerId,
        /// Optional group peer ID for `?startgroup=`; `None` for 1:1 `?start=`.
        peer_id: Option<PeerId>,
        /// Non-empty start parameter payload.
        start_param: String,
    },
    /// Open a specific [`SettingsSection`] inside Settings.
    OpenSettingsSection {
        /// Coarse shell page.
        page: SettingsPage,
        /// Detailed Settings section to push.
        section: SettingsSection,
        /// Optional control highlight ID (e.g. `"experimental/<id>"`).
        highlight: Option<String>,
        /// Optional toast notice.
        toast: Option<String>,
    },
    /// Route a `tg://setlanguage` / `t.me/setlanguage/...` link through
    /// [`tg_app_settings::settings::SettingsController::open_set_language_link`].
    OpenSetLanguageLink {
        /// Canonical `tg://setlanguage[?lang=...]` link.
        url: String,
    },
    /// Push a rich modal box onto `BoxHost`.
    ShowBox(Box<BoxKind>),
    /// Push a lightweight shell modal box onto `LayerStack`.
    ShowModal(ModalBox),
    /// Show a transient toast notification.
    Toast(String),
    /// Open the hamburger Main Menu drawer.
    OpenMainMenu,
    /// Focus global dialogs search (`tg://chats/search`).
    FocusSearch,
    /// Open Recent Calls dialog (`tg://settings/calls/all`).
    OpenRecentCalls,
    /// Open a Bot Mini App or HTML5 game WebView.
    OpenMiniApp {
        /// Bot user peer ID.
        bot_id: PeerId,
        /// Optional launch start parameter.
        start_param: Option<String>,
    },
    /// Open an Instant View article directly (`t.me/iv?...`).
    OpenInstantView {
        /// Target Instant View URL.
        url: String,
        /// Optional `#fragment` anchor.
        fragment: Option<String>,
    },
    /// Open an external URL in the user's browser (e.g. `tg://settings/faq` or `tonsite://`).
    OpenExternalUrl(String),
    /// Open an auxiliary overlay host (Payments, Passport, Business, Stories, Calls).
    OpenOverlay(DeepLinkOverlayRequest),
    /// Open Channel Direct Messages (monoforum) section for `peer_id` (`?direct`).
    OpenChannelDirectMessages {
        /// Target channel peer ID.
        peer_id: PeerId,
    },
    /// Forward a `tg://login?code=<digits>` activation to the login flow.
    LoginCode(String),
}

impl DeepLinkOutcome {
    /// Wraps a box in [`DeepLinkOutcome::ShowBox`] (boxed: `BoxKind` is large).
    #[must_use]
    pub fn show_box(kind: BoxKind) -> Self {
        Self::ShowBox(Box::new(kind))
    }
}

/// Dispatches a parsed deep link against an authenticated [`SessionController`] and [`AppSession`].
///
/// Port of `Application::openLocalUrl` + `LocalUrlHandlers` + `Window::SessionController::showPeerByLink`.
pub fn dispatch_deep_link(
    session: &mut SessionController,
    uri: &str,
    backend: &mut dyn AppSession,
) -> DeepLinkOutcome {
    let Some(link) = parse_deep_link(uri) else {
        return DeepLinkOutcome::NotTelegram;
    };
    dispatch_parsed_deep_link(session, link, backend)
}

/// Dispatches a deep link when no authenticated session is active (e.g. Intro screen).
///
/// Handles `requiresAuth = false` routes (`tg://login`, `tg://setlanguage`, `tg://proxy`,
/// `tg://socks`, `tg://settings/language`, `tg://settings/faq`, `tg://settings/features`).
pub fn dispatch_unauthenticated_deep_link(uri: &str) -> DeepLinkOutcome {
    let Some(link) = parse_deep_link(uri) else {
        return DeepLinkOutcome::NotTelegram;
    };
    match link {
        DeepLink::LoginCode { code } => DeepLinkOutcome::LoginCode(code),
        DeepLink::SetLanguage { lang } => match lang {
            Some(code) => DeepLinkOutcome::Toast(format!(
                "Language pack '{code}' will be applied after signing in."
            )),
            None => DeepLinkOutcome::show_box(BoxKind::Language(LanguageBoxState::new(
                tg_lang::default_language_id(),
            ))),
        },
        DeepLink::Proxy { kind, params } => handle_proxy_deep_link(kind, &params),
        DeepLink::Router(RouterDeepLink::Language { .. }) => DeepLinkOutcome::show_box(
            BoxKind::Language(LanguageBoxState::new(tg_lang::default_language_id())),
        ),
        DeepLink::Router(RouterDeepLink::OpenExternalInfo { url }) => {
            DeepLinkOutcome::OpenExternalUrl(url)
        }
        DeepLink::Unsupported { .. } => DeepLinkOutcome::Toast(UNSUPPORTED_LINK_MESSAGE.to_owned()),
        _ => DeepLinkOutcome::Toast("Sign in to open this Telegram link.".to_owned()),
    }
}

fn dispatch_parsed_deep_link(
    session: &mut SessionController,
    link: DeepLink,
    backend: &mut dyn AppSession,
) -> DeepLinkOutcome {
    match link {
        DeepLink::Router(route) => dispatch_router_link(session, route, backend),
        DeepLink::Resolve(info) => dispatch_resolve_link(session, *info, backend),
        DeepLink::PrivatePost(info) => dispatch_private_post_link(session, info, backend),
        DeepLink::JoinChat { hash } => match backend.check_chat_invite(&hash) {
            Ok(ChatInvitePreview::AlreadyJoined { peer_id, .. }) => {
                session.show_peer_history(peer_id, SectionShow::forward(), MsgId::SHOW_AT_UNREAD);
                DeepLinkOutcome::Handled
            }
            Ok(ChatInvitePreview::Invite {
                hash,
                title,
                about,
                participants_count,
                is_channel,
                is_broadcast,
                request_needed,
                verified,
                color_index,
                participant_names,
                ..
            }) => DeepLinkOutcome::show_box(BoxKind::JoinInvite(JoinInviteBoxState {
                hash,
                title,
                about,
                participants_count,
                is_channel,
                is_broadcast,
                request_needed,
                verified,
                color_index,
                participant_names,
            })),
            Err(err) => DeepLinkOutcome::Toast(format!("Invalid invite link: {err}")),
        },
        DeepLink::AddList { slug } => match backend.check_chatlist_invite(&slug) {
            Ok(preview) => {
                let chats_summary = if preview.peer_titles.is_empty() {
                    format!("{} chat(s)", preview.peers.len())
                } else {
                    preview.peer_titles.join(", ")
                };
                let body = if preview.already_filter_id.is_some() {
                    format!(
                        "Folder \"{}\" is already added. Add {} missing chat(s): {chats_summary}?",
                        preview.title,
                        preview.peers.len()
                    )
                } else {
                    format!(
                        "Add shared folder \"{}\" with {} chat(s): {chats_summary}?",
                        preview.title,
                        preview.peers.len()
                    )
                };
                DeepLinkOutcome::show_box(BoxKind::Confirm(ConfirmBoxState::new(
                    "Add Chat Folder",
                    body,
                    "Add Folder",
                    ConfirmTag::JoinChatlist {
                        slug: preview.slug,
                        peers: preview.peers,
                    },
                )))
            }
            Err(err) => DeepLinkOutcome::Toast(format!("Folder invite unavailable: {err}")),
        },
        DeepLink::AddStickers {
            short_name,
            is_emoji,
        } => match backend.resolve_sticker_set(&short_name, is_emoji) {
            Ok(pack) => DeepLinkOutcome::show_box(BoxKind::StickerSet(
                StickerSetBoxState::from_preview(pack),
            )),
            Err(err) => DeepLinkOutcome::Toast(format!("Could not load sticker pack: {err}")),
        },
        DeepLink::AddTheme { slug } => match backend.resolve_cloud_theme(&slug) {
            Ok(theme) => DeepLinkOutcome::OpenOverlay(DeepLinkOverlayRequest::OpenTheme {
                slug: theme.slug,
                title: Some(theme.title),
            }),
            Err(err) => DeepLinkOutcome::Toast(format!("Could not load theme '{slug}': {err}")),
        },
        DeepLink::AddStyle { slug } => {
            let resolved = backend
                .as_compose_ai_backend()
                .ok_or_else(|| "AI compose styles are unavailable.".to_owned())
                .and_then(|ai| {
                    let tone = ai.resolve_ai_tone(&slug)?;
                    let installed = ai.is_ai_tone_installed(&tone);
                    Ok((tone, installed))
                });
            match resolved {
                Ok((tone, installed)) => DeepLinkOutcome::show_box(BoxKind::PreviewAiTone(
                    PreviewAiToneBoxState::new(tone, installed),
                )),
                Err(err) => {
                    DeepLinkOutcome::Toast(format!("Could not load AI style '{slug}': {err}"))
                }
            }
        }
        DeepLink::SetLanguage { lang } => {
            let canonical = match lang {
                Some(code) => format!("tg://setlanguage?lang={}", percent_encode_value(&code)),
                None => "tg://setlanguage".to_owned(),
            };
            DeepLinkOutcome::OpenSetLanguageLink { url: canonical }
        }
        DeepLink::ShareUrl { url, text } => {
            let mut state = ShareBoxState::new(Vec::new(), Some(url.clone()));
            state.comment = if text.is_empty() {
                url
            } else {
                format!("{url}\n{text}")
            };
            DeepLinkOutcome::show_box(BoxKind::Share(state))
        }
        DeepLink::ConfirmPhone { phone, hash } => match backend.send_confirm_phone_code(&hash) {
            Ok(phone_code_hash) => DeepLinkOutcome::show_box(BoxKind::ConfirmPhoneCode(
                ConfirmPhoneCodeBoxState::new(phone, phone_code_hash),
            )),
            Err(err) => {
                let body = if err.to_ascii_uppercase().contains("FLOOD") {
                    tg_lang::tr::lng_flood_error()
                } else {
                    tg_lang::tr::lng_confirm_phone_link_invalid()
                };
                DeepLinkOutcome::ShowModal(ModalBox::Inform {
                    title: tg_lang::tr::lng_confirm_phone_title(),
                    body,
                })
            }
        },
        DeepLink::Proxy { kind, params } => handle_proxy_deep_link(kind, &params),
        DeepLink::Passport(info) => {
            let mut params = Vec::new();
            if info.bot_id > 0 {
                params.push(format!("bot_id={}", info.bot_id));
            }
            if !info.scope.is_empty() {
                params.push(format!("scope={}", percent_encode_value(&info.scope)));
            }
            if !info.public_key.is_empty() {
                params.push(format!(
                    "public_key={}",
                    percent_encode_value(&info.public_key)
                ));
            }
            if !info.nonce.is_empty() {
                params.push(format!("nonce={}", percent_encode_value(&info.nonce)));
            }
            if !info.callback_url.is_empty() {
                params.push(format!(
                    "callback_url={}",
                    percent_encode_value(&info.callback_url)
                ));
            }
            let slug = (!params.is_empty()).then(|| format!("tg://passport?{}", params.join("&")));
            DeepLinkOutcome::OpenOverlay(DeepLinkOverlayRequest::OpenPayments {
                screen: PaymentsScreen::Passport,
                slug,
            })
        }
        DeepLink::WallPaper(info) => {
            let mode = info.params.get("mode").map(String::as_str).unwrap_or("");
            let blur = mode.split('+').any(|m| m.eq_ignore_ascii_case("blur"));
            let motion = mode.split('+').any(|m| m.eq_ignore_ascii_case("motion"));
            let intensity = info
                .params
                .get("intensity")
                .and_then(|v| v.parse::<i32>().ok());
            let rotation = info
                .params
                .get("rotation")
                .and_then(|v| v.parse::<i32>().ok());
            if info.is_color_or_gradient || info.slug_or_color.is_empty() {
                DeepLinkOutcome::OpenOverlay(DeepLinkOverlayRequest::OpenBackground {
                    slug_or_color: info.slug_or_color,
                    is_color_or_gradient: true,
                    blur,
                    motion,
                    intensity,
                    rotation,
                })
            } else {
                match backend.resolve_wallpaper(&info.slug_or_color) {
                    Ok(wp) => {
                        DeepLinkOutcome::OpenOverlay(DeepLinkOverlayRequest::OpenBackground {
                            slug_or_color: wp.slug,
                            is_color_or_gradient: false,
                            blur,
                            motion,
                            intensity,
                            rotation,
                        })
                    }
                    Err(err) => DeepLinkOutcome::Toast(format!("Could not load background: {err}")),
                }
            }
        }
        DeepLink::ContactToken { token, .. } => {
            if token.is_empty() {
                DeepLinkOutcome::Toast("This contact link is invalid.".to_owned())
            } else {
                DeepLinkOutcome::show_box(BoxKind::AddContact(AddContactBoxState::new()))
            }
        }
        DeepLink::TestChatTheme { .. } => DeepLinkOutcome::OpenSettingsSection {
            page: SettingsPage::ChatSettings,
            section: SettingsSection::Chat,
            highlight: None,
            toast: Some("Opened Chat Settings for test theme.".to_owned()),
        },
        DeepLink::Invoice { slug } => {
            DeepLinkOutcome::OpenOverlay(DeepLinkOverlayRequest::OpenPayments {
                screen: PaymentsScreen::Checkout,
                slug: Some(slug),
            })
        }
        DeepLink::PremiumOffer { referral } => {
            DeepLinkOutcome::OpenOverlay(DeepLinkOverlayRequest::OpenPayments {
                screen: PaymentsScreen::Premium,
                slug: Some(referral),
            })
        }
        DeepLink::PremiumMultigift { .. } => {
            DeepLinkOutcome::OpenOverlay(DeepLinkOverlayRequest::OpenPayments {
                screen: PaymentsScreen::StarGifts,
                slug: None,
            })
        }
        DeepLink::LoginCode { code } => DeepLinkOutcome::LoginCode(code),
        DeepLink::Boost { domain, channel_id } => {
            let resolved = if let Some(ref user) = domain {
                backend.resolve_username_remote(user, None)
            } else if let Some(cid) = channel_id {
                backend
                    .resolve_private_channel_peer(cid)
                    .ok_or_else(|| format!("Private channel {cid} is not loaded."))
            } else {
                Err("Missing boost channel target.".to_owned())
            };
            match resolved {
                Ok(peer_id) => {
                    session.show_statistics(peer_id, StatisticsScreen::Boosts);
                    DeepLinkOutcome::Handled
                }
                Err(err) => DeepLinkOutcome::Toast(format!("Could not open Boosts: {err}")),
            }
        }
        DeepLink::BusinessChatLink { slug } => match backend.resolve_business_chat_link(&slug) {
            Ok((peer_id, msg)) => {
                session.show_peer_history(peer_id, SectionShow::forward(), MsgId::SHOW_AT_UNREAD);
                if msg.is_empty() {
                    DeepLinkOutcome::Handled
                } else {
                    DeepLinkOutcome::PrefillCompose { peer_id, text: msg }
                }
            }
            Err(err) => DeepLinkOutcome::Toast(format!("Business link invalid: {err}")),
        },
        DeepLink::StarsTopUp { amount, purpose } => {
            let slug = if purpose.is_empty() {
                Some(format!("balance={amount}"))
            } else {
                Some(format!("balance={amount}&purpose={purpose}"))
            };
            DeepLinkOutcome::OpenOverlay(DeepLinkOverlayRequest::OpenPayments {
                screen: PaymentsScreen::Stars,
                slug,
            })
        }
        DeepLink::UniqueGift { slug } => {
            DeepLinkOutcome::OpenOverlay(DeepLinkOverlayRequest::OpenPayments {
                screen: PaymentsScreen::StarGifts,
                slug: Some(slug),
            })
        }
        DeepLink::StarGiftAuction { slug } => {
            DeepLinkOutcome::OpenOverlay(DeepLinkOverlayRequest::OpenPayments {
                screen: PaymentsScreen::StarGifts,
                slug: Some(slug),
            })
        }
        DeepLink::ConferenceCall { slug } => {
            DeepLinkOutcome::OpenOverlay(DeepLinkOverlayRequest::JoinCall {
                peer_id: None,
                slug_or_hash: Some(slug),
            })
        }
        DeepLink::StarsSettings | DeepLink::TonSettings => {
            DeepLinkOutcome::OpenOverlay(DeepLinkOverlayRequest::OpenPayments {
                screen: PaymentsScreen::Stars,
                slug: None,
            })
        }
        DeepLink::SendGrams {
            to,
            amount,
            startapp,
            transfer_uri,
        } => dispatch_send_grams(to, amount, startapp, transfer_uri),
        DeepLink::OAuth { token } => {
            let oauth_url = format!("tg://oauth?token={}", percent_encode_value(&token));
            let source = UrlAuthSource::Url { url: oauth_url };
            match BotsBackend::request_url_auth(backend, &source) {
                Ok(result) => match handle_url_auth_request_result(source, result) {
                    UrlAuthDecision::OpenAcceptedUrl { url, .. }
                    | UrlAuthDecision::OpenDefaultUrl { url, .. } => {
                        DeepLinkOutcome::OpenExternalUrl(url)
                    }
                    UrlAuthDecision::ShowConfirmation(box_data) => DeepLinkOutcome::show_box(
                        BoxKind::UrlAuth(UrlAuthConfirmBoxState::new(*box_data)),
                    ),
                    UrlAuthDecision::NoOp => DeepLinkOutcome::Handled,
                },
                Err(err) => match handle_url_auth_request_error(&source, &err) {
                    UrlAuthErrorAction::ShowExpiredToast => {
                        DeepLinkOutcome::Toast("This authorization link has expired.".to_owned())
                    }
                    UrlAuthErrorAction::OpenDefaultUrl { .. } => {
                        DeepLinkOutcome::Toast(format!("OAuth authorization failed: {err}"))
                    }
                },
            }
        }
        DeepLink::GiftCode { slug } => match backend.check_gift_code(&slug) {
            Ok(gift) => {
                let from = gift
                    .from_title
                    .as_deref()
                    .map(|f| format!(" from {f}"))
                    .unwrap_or_default();
                if gift.used_date.is_some() {
                    DeepLinkOutcome::ShowModal(ModalBox::Inform {
                        title: "Telegram Premium Gift Code".to_owned(),
                        body: format!(
                            "Code '{}'{} — {} days of Telegram Premium (Already redeemed).",
                            gift.slug, from, gift.days
                        ),
                    })
                } else {
                    DeepLinkOutcome::show_box(BoxKind::Confirm(ConfirmBoxState::new(
                        "Telegram Premium Gift Code",
                        format!(
                            "Redeem gift code '{}'{} for {} days of Telegram Premium?",
                            gift.slug, from, gift.days
                        ),
                        "Redeem Gift",
                        ConfirmTag::ApplyGiftCode {
                            slug: gift.slug,
                            days: gift.days,
                        },
                    )))
                }
            }
            Err(err) => DeepLinkOutcome::Toast(format!("Gift code invalid: {err}")),
        },
        DeepLink::Phone { phone } => {
            let digits = normalize_phone_digits(&phone);
            let self_digits = backend
                .current_user()
                .and_then(|u| InfoBackend::peer_profile(backend, u.id))
                .and_then(|p| p.phone)
                .map(|p| normalize_phone_digits(&p))
                .unwrap_or_default();
            let is_self_phone = !digits.is_empty() && digits == self_digits;
            let (resolved_peer, status_note) = match backend.resolve_phone_remote(&phone) {
                Ok(pid) => {
                    let name = InfoBackend::peer_profile(backend, pid)
                        .map(|p| p.title)
                        .filter(|t| !t.trim().is_empty())
                        .unwrap_or_else(|| format!("User {}", pid.bare()));
                    (Some((pid, name)), None)
                }
                Err(_) => (
                    None,
                    Some("This phone number is not registered on Telegram.".to_owned()),
                ),
            };
            DeepLinkOutcome::show_box(BoxKind::PhoneAction(PhoneActionBoxState::new(
                phone,
                resolved_peer,
                is_self_phone,
                status_note,
            )))
        }
        DeepLink::BankCard { number } => {
            let state = match backend.get_bank_card_data(&number) {
                Ok(info) => BankCardActionBoxState::resolved(number, info),
                Err(err) => BankCardActionBoxState::failed(number, err),
            };
            DeepLinkOutcome::show_box(BoxKind::BankCardAction(state))
        }
        DeepLink::InstantView { url, fragment } => {
            DeepLinkOutcome::OpenInstantView { url, fragment }
        }
        DeepLink::TonSite { uri } => DeepLinkOutcome::OpenExternalUrl(uri),
        DeepLink::Internal(internal) => dispatch_internal_link(session, internal, backend),
        DeepLink::Unknown { path } => match backend.request_deep_link_info(&path) {
            Ok(Some(info)) if !info.message.is_empty() => {
                DeepLinkOutcome::ShowModal(ModalBox::Inform {
                    title: if info.update_app {
                        "Update Required".to_owned()
                    } else {
                        "Telegram".to_owned()
                    },
                    body: info.message,
                })
            }
            _ => DeepLinkOutcome::Toast(UNSUPPORTED_LINK_MESSAGE.to_owned()),
        },
        DeepLink::Unsupported { .. } => DeepLinkOutcome::Toast(UNSUPPORTED_LINK_MESSAGE.to_owned()),
    }
}

/// Routes `tc://`, `tg://sendgrams`, and `ton://transfer` links to [`DeepLinkOverlayRequest::OpenWallet`]
/// matching `ResolveSendGrams` (`local_url_handlers.cpp:1800-1820`) and
/// `Wallet::OpenTransferLink` / `Wallet::OpenSendGramsLink` (`wallet_panel.cpp:150-185`).
fn dispatch_send_grams(
    to: Option<String>,
    amount: Option<String>,
    startapp: Option<String>,
    transfer_uri: Option<String>,
) -> DeepLinkOutcome {
    if let Some(uri) = transfer_uri {
        let link = match tg_wallet::address::parse_transfer_link(&uri) {
            Ok(parsed) => parsed,
            Err(_) => {
                return DeepLinkOutcome::Toast("Invalid TON transfer link.".to_owned());
            }
        };
        let now_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        if tg_wallet::address::transfer_link_expired(link.expires_at, now_unix) {
            return DeepLinkOutcome::Toast("This TON transfer link has expired.".to_owned());
        }
        let friendly = link
            .address
            .address
            .to_friendly(link.address.bounceable, link.address.testnet);
        return DeepLinkOutcome::OpenOverlay(DeepLinkOverlayRequest::OpenWallet {
            action: WalletDeepLinkAction::Send {
                to: Some(friendly),
                amount: link.amount_nano.map(|n| tg_wallet::format_ton(n as i64)),
                comment: link.comment,
                bounceable: Some(link.address.bounceable),
            },
        });
    }

    if let Some(raw_startapp) = startapp {
        let link = match tg_wallet::tonconnect::parse_ton_connect_link(&raw_startapp) {
            Ok(parsed) => parsed,
            Err(_) => {
                return DeepLinkOutcome::Toast("Invalid TON Connect link.".to_owned());
            }
        };
        let action = if link.kind == tg_wallet::tonconnect::TonConnectLinkKind::Bare {
            WalletDeepLinkAction::OpenOverview
        } else {
            WalletDeepLinkAction::TonConnect {
                raw_link: raw_startapp,
            }
        };
        return DeepLinkOutcome::OpenOverlay(DeepLinkOverlayRequest::OpenWallet { action });
    }

    let to_clean = to.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty());
    let amount_clean = amount
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty());

    // Mirror `Wallet::OpenSendGramsLink` (`wallet_panel.cpp:171-175`):
    // Reject invalid decimal amounts and amounts without a recipient.
    if let Some(ref amt) = amount_clean
        && (tg_wallet::parse_ton_amount(amt).is_err() || to_clean.is_none())
    {
        return DeepLinkOutcome::Toast("Invalid TON transfer link.".to_owned());
    }

    if to_clean.is_none() {
        return DeepLinkOutcome::OpenOverlay(DeepLinkOverlayRequest::OpenWallet {
            action: WalletDeepLinkAction::OpenOverview,
        });
    }

    DeepLinkOutcome::OpenOverlay(DeepLinkOverlayRequest::OpenWallet {
        action: WalletDeepLinkAction::Send {
            to: to_clean,
            amount: amount_clean,
            comment: None,
            bounceable: None,
        },
    })
}

fn dispatch_router_link(
    session: &mut SessionController,
    route: RouterDeepLink,
    backend: &mut dyn AppSession,
) -> DeepLinkOutcome {
    match route {
        RouterDeepLink::Settings {
            page,
            section,
            highlight,
        } => {
            session.show_settings(page);
            DeepLinkOutcome::OpenSettingsSection {
                page,
                section,
                highlight,
                toast: None,
            }
        }
        RouterDeepLink::Language { .. } => DeepLinkOutcome::OpenSetLanguageLink {
            url: "tg://setlanguage".to_owned(),
        },
        RouterDeepLink::ProxySettings { highlight } => DeepLinkOutcome::OpenSettingsSection {
            page: SettingsPage::Connection,
            section: SettingsSection::Advanced,
            highlight,
            toast: None,
        },
        RouterDeepLink::SavedMessages => {
            if let Some(user) = backend.current_user() {
                session.show_peer_history(user.id, SectionShow::forward(), MsgId::SHOW_AT_UNREAD);
                DeepLinkOutcome::Handled
            } else {
                DeepLinkOutcome::Toast("Saved Messages is unavailable.".to_owned())
            }
        }
        RouterDeepLink::MyProfile { .. } | RouterDeepLink::QrCode { .. } => {
            if let Some(user) = backend.current_user() {
                session.show_peer_info(user.id, SectionShow::forward());
                DeepLinkOutcome::Handled
            } else {
                DeepLinkOutcome::OpenSettingsSection {
                    page: SettingsPage::Account,
                    section: SettingsSection::Information,
                    highlight: None,
                    toast: None,
                }
            }
        }
        RouterDeepLink::EditUsername => {
            if let Some(user) = backend.current_user()
                && let Some(profile) = InfoBackend::peer_profile(backend, user.id)
            {
                DeepLinkOutcome::show_box(BoxKind::Username(UsernameBoxState::from_profile(
                    &profile,
                )))
            } else {
                DeepLinkOutcome::OpenSettingsSection {
                    page: SettingsPage::Account,
                    section: SettingsSection::Information,
                    highlight: None,
                    toast: None,
                }
            }
        }
        RouterDeepLink::OpenPaymentsOverlay { screen } => {
            DeepLinkOutcome::OpenOverlay(DeepLinkOverlayRequest::OpenPayments {
                screen,
                slug: None,
            })
        }
        RouterDeepLink::OpenBusinessOverlay => {
            DeepLinkOutcome::OpenOverlay(DeepLinkOverlayRequest::OpenBusiness)
        }
        RouterDeepLink::OpenRecentCalls => DeepLinkOutcome::OpenRecentCalls,
        RouterDeepLink::OpenExternalInfo { url } => DeepLinkOutcome::OpenExternalUrl(url),
        RouterDeepLink::FocusSearch => DeepLinkOutcome::FocusSearch,
        RouterDeepLink::OpenMainMenu { .. } => DeepLinkOutcome::OpenMainMenu,
        RouterDeepLink::OpenContacts { .. } => {
            DeepLinkOutcome::show_box(BoxKind::AddContact(AddContactBoxState::new()))
        }
        RouterDeepLink::NewGroup => {
            DeepLinkOutcome::show_box(BoxKind::CreateGroup(CreateGroupBoxState::new()))
        }
        RouterDeepLink::NewChannel => {
            DeepLinkOutcome::show_box(BoxKind::CreateChannel(CreateChannelBoxState::new()))
        }
        RouterDeepLink::NewBot {
            manager,
            username,
            name,
        } => route_newbot_deep_link(backend, session, &manager, username, name),
        RouterDeepLink::Inform { title, body } => {
            DeepLinkOutcome::ShowModal(ModalBox::Inform { title, body })
        }
    }
}

fn dispatch_resolve_link(
    session: &mut SessionController,
    info: ResolveLinkInfo,
    backend: &mut dyn AppSession,
) -> DeepLinkOutcome {
    let resolved = if let Some(ref domain) = info.domain {
        backend.resolve_username_remote(domain, info.referral.as_deref())
    } else if let Some(ref phone) = info.phone {
        backend.resolve_phone_remote(phone)
    } else {
        return DeepLinkOutcome::Toast("Invalid Telegram link.".to_owned());
    };

    let peer_id = match resolved {
        Ok(pid) => pid,
        Err(err) => return DeepLinkOutcome::Toast(err),
    };

    // Story link: `t.me/<user>/s/<id>`
    if let Some(ref story_param) = info.story {
        let story_id = story_param.parse::<i32>().ok().filter(|&id| id > 0);
        return DeepLinkOutcome::OpenOverlay(DeepLinkOverlayRequest::OpenStory {
            peer_id,
            story_id,
        });
    }

    // Voice chat / video chat / livestream invite
    if let Some(hash) = info.voicechat_hash {
        session.show_peer_history(peer_id, SectionShow::forward(), MsgId::SHOW_AT_UNREAD);
        return DeepLinkOutcome::OpenOverlay(DeepLinkOverlayRequest::JoinCall {
            peer_id: Some(peer_id),
            slug_or_hash: (!hash.is_empty()).then_some(hash),
        });
    }

    // Channel Boost
    if info.resolve_type == ResolveType::Boost {
        session.show_statistics(peer_id, StatisticsScreen::Boosts);
        return DeepLinkOutcome::Handled;
    }

    // Channel Direct Messages (`?direct`)
    if info.resolve_type == ResolveType::ChannelDirect {
        session.show_peer_history(peer_id, SectionShow::forward(), MsgId::SHOW_AT_UNREAD);
        return DeepLinkOutcome::OpenChannelDirectMessages { peer_id };
    }

    // Profile view (`?profile`)
    if info.resolve_type == ResolveType::Profile {
        session.show_peer_info(peer_id, SectionShow::forward());
        return DeepLinkOutcome::Handled;
    }

    // Mini App launch (`appname`, `startapp`, or `startattach` / `attach`)
    if info.resolve_type == ResolveType::BotApp
        || info.attach_bot_main_open
        || info.attach_bot_username.is_some()
        || info.attach_bot_toggle_command.is_some()
    {
        let start_param = info
            .start_token
            .or(info.attach_bot_toggle_command)
            .filter(|s| !s.is_empty());
        return DeepLinkOutcome::OpenMiniApp {
            bot_id: peer_id,
            start_param,
        };
    }

    // Share Game (`?game=<short_name>`)
    if info.resolve_type == ResolveType::ShareGame {
        return DeepLinkOutcome::OpenMiniApp {
            bot_id: peer_id,
            start_param: info.start_token,
        };
    }

    // Standard chat navigation (post / comment / topic / thread / bot start / text prefill).
    let target_msg = match info.replies {
        Some(RepliesByLink::Comment(cid)) => MsgId(cid),
        Some(RepliesByLink::Thread(tid)) => info.post_id.map_or(MsgId(tid), MsgId),
        None => info.post_id.map_or(MsgId::SHOW_AT_UNREAD, MsgId),
    };
    session.show_peer_history(peer_id, SectionShow::forward(), target_msg);

    if let Some(text) = info.text {
        return DeepLinkOutcome::PrefillCompose { peer_id, text };
    }
    if info.resolve_type == ResolveType::BotStart
        && let Some(token) = info.start_token
    {
        return DeepLinkOutcome::StartBot {
            bot_id: peer_id,
            peer_id: None,
            start_param: token,
        };
    }

    DeepLinkOutcome::Handled
}

fn dispatch_private_post_link(
    session: &mut SessionController,
    info: PrivatePostLinkInfo,
    backend: &mut dyn AppSession,
) -> DeepLinkOutcome {
    let Some(peer_id) = backend.resolve_private_channel_peer(info.channel_id) else {
        return DeepLinkOutcome::Toast(
            "This message is in a private channel you are not a member of.".to_owned(),
        );
    };
    let msg_id = match info.replies {
        Some(RepliesByLink::Comment(cid)) => MsgId(cid),
        Some(RepliesByLink::Thread(tid)) => info.post_id.map_or(MsgId(tid), MsgId),
        None => info.post_id.map_or(MsgId::SHOW_AT_UNREAD, MsgId),
    };
    session.show_peer_history(peer_id, SectionShow::forward(), msg_id);
    DeepLinkOutcome::Handled
}

fn dispatch_internal_link(
    session: &mut SessionController,
    internal: InternalDeepLink,
    backend: &mut dyn AppSession,
) -> DeepLinkOutcome {
    match internal {
        InternalDeepLink::GetPremium { source } => {
            DeepLinkOutcome::OpenOverlay(DeepLinkOverlayRequest::OpenPayments {
                screen: PaymentsScreen::Premium,
                slug: Some(source),
            })
        }
        InternalDeepLink::AboutTags => {
            DeepLinkOutcome::OpenOverlay(DeepLinkOverlayRequest::OpenPayments {
                screen: PaymentsScreen::Premium,
                slug: Some("tags_for_messages".to_owned()),
            })
        }
        InternalDeepLink::CopyText(text) => DeepLinkOutcome::Toast(format!("Copied: {text}")),
        InternalDeepLink::ShowInviteLink { link } => {
            DeepLinkOutcome::Toast(format!("Invite link: {link}"))
        }
        InternalDeepLink::CopyUsername { username, as_link } => {
            if as_link {
                DeepLinkOutcome::Toast(format!("Copied https://t.me/{username}"))
            } else {
                DeepLinkOutcome::Toast(format!("Copied @{username}"))
            }
        }
        InternalDeepLink::CommonGroups(raw_peer) => {
            let peer_id = PeerId(raw_peer as i64);
            session.show_peer_info(peer_id, SectionShow::forward());
            DeepLinkOutcome::Handled
        }
        InternalDeepLink::EditPeer(raw_peer) => {
            let peer_id = PeerId(raw_peer as i64);
            if InfoBackend::peer_profile(backend, peer_id).is_some() {
                session.show_peer_info(peer_id, SectionShow::forward());
                DeepLinkOutcome::Handled
            } else {
                DeepLinkOutcome::Toast("Peer not loaded.".to_owned())
            }
        }
        InternalDeepLink::MediaTimestamp {
            base,
            position_secs,
        } => DeepLinkOutcome::Toast(format!("Seek {base} to {position_secs}s")),
        InternalDeepLink::Other { .. } => {
            DeepLinkOutcome::Toast(UNSUPPORTED_LINK_MESSAGE.to_owned())
        }
    }
}

fn handle_proxy_deep_link(
    kind: ProxyLinkKind,
    params: &BTreeMap<String, String>,
) -> DeepLinkOutcome {
    if params.is_empty() {
        return DeepLinkOutcome::ShowModal(ModalBox::ProxySettings);
    }
    let scheme = match kind {
        ProxyLinkKind::Socks5 => "socks",
        ProxyLinkKind::Mtproto => "proxy",
        ProxyLinkKind::Web => "webproxy",
    };
    let query = params
        .iter()
        .map(|(k, v)| format!("{}={}", percent_encode_value(k), percent_encode_value(v)))
        .collect::<Vec<_>>()
        .join("&");
    let canonical = format!("tg://{scheme}?{query}");
    if let Some(proxy_box) = tg_app_info::boxes::add_proxy::route_proxy_link(&canonical) {
        DeepLinkOutcome::show_box(proxy_box)
    } else {
        DeepLinkOutcome::ShowModal(ModalBox::ProxySettings)
    }
}

// ============================================================================
// Core::DeepLinks::Router (`deep_links_router.cpp` + settings/chats/contacts/new)
// ============================================================================

/// Parses and dispatches `tg://<section>[/<path>][?<params>]` through tdesktop's
/// `Core::DeepLinks::Router` (`settings`, `contacts`, `chats`, `new`, `newbot`).
///
/// Matching semantics match `Router::handleSection`:
/// 1. Exact `(section, path)` match
/// 2. Prefix match `path.starts_with(entry.path + '/')`
/// 3. Known section with no matching path -> `Some(DeepLink::Unsupported)`
/// 4. Unknown section -> `None` (falls through to `LocalUrlHandlers()`)
fn try_router_for_local_url(command: &str) -> Option<DeepLink> {
    let (raw_path, query) = match command.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (command, None),
    };
    let params = query.map(parse_router_query_params).unwrap_or_default();
    let path_trimmed = raw_path
        .split('#')
        .next()
        .unwrap_or("")
        .trim()
        .trim_matches('/');
    let (section, subpath) = match path_trimmed.split_once('/') {
        Some((sec, rest)) => (sec.to_ascii_lowercase(), rest.to_ascii_lowercase()),
        None => (path_trimmed.to_ascii_lowercase(), String::new()),
    };

    if section.is_empty() {
        return None;
    }

    match section.as_str() {
        "settings" => Some(dispatch_settings_route(&subpath, &params, command)),
        "contacts" => Some(dispatch_contacts_route(&subpath, command)),
        "chats" => Some(dispatch_chats_route(&subpath, command)),
        "new" => Some(dispatch_new_route(&subpath, command)),
        "newbot" => Some(dispatch_newbot_route(&subpath, &params, command)),
        _ => None,
    }
}

/// Parses query parameters for `Core::DeepLinks::Router` (`deep_links_router.cpp:34-41`),
/// matching `qthelp::url_decode` where `+` decodes to a space before percent-decoding.
fn parse_router_query_params(query: &str) -> BTreeMap<String, String> {
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
        let val = percent_decode(raw_val, true);
        result.insert(key, val);
    }
    result
}

fn dispatch_settings_route(
    subpath: &str,
    params: &BTreeMap<String, String>,
    raw_command: &str,
) -> DeepLink {
    let query_highlight = params.get("highlight").filter(|s| !s.is_empty()).cloned();

    // `tg://settings/experimental/<id>` (+ `experiment/<id>`, `exp/<id>`),
    // matching `showExperimental` in `deep_links_settings.cpp:530-555`.
    if let Some(rest) = subpath
        .strip_prefix("experimental/")
        .or_else(|| subpath.strip_prefix("experiment/"))
        .or_else(|| subpath.strip_prefix("exp/"))
    {
        let key = rest.trim().to_ascii_lowercase();
        let highlight = if !key.is_empty() {
            Some(format!("experimental/{key}"))
        } else {
            query_highlight
        };
        return DeepLink::Router(RouterDeepLink::Settings {
            page: SettingsPage::Advanced,
            section: SettingsSection::Experimental,
            highlight,
        });
    }

    // First try exact match, then prefix match on longest registered path (`entry.path + '/'`).
    if let Some(target) = match_settings_exact(subpath, &query_highlight) {
        return DeepLink::Router(target);
    }

    // Prefix fallback matching `Router::handleSection` (`path.starts_with(entry.path + '/')`).
    let mut prefix = subpath;
    while let Some((parent, _)) = prefix.rsplit_once('/') {
        if let Some(target) = match_settings_exact(parent, &query_highlight) {
            return DeepLink::Router(target);
        }
        prefix = parent;
    }

    DeepLink::Unsupported {
        command: raw_command.to_owned(),
    }
}

fn match_settings_exact(path: &str, query_highlight: &Option<String>) -> Option<RouterDeepLink> {
    let hl = |fixed: &str| Some(fixed.to_owned());
    let settings = |page, section, highlight| RouterDeepLink::Settings {
        page,
        section,
        highlight,
    };

    match path {
        // Root & profile
        "" => Some(settings(
            SettingsPage::Main,
            SettingsSection::Main,
            query_highlight.clone(),
        )),
        "edit" | "my-profile/edit" | "information" | "edit_profile" => Some(settings(
            SettingsPage::Account,
            SettingsSection::Information,
            query_highlight.clone(),
        )),
        "my-profile" => Some(RouterDeepLink::MyProfile {
            highlight: query_highlight.clone(),
        }),
        "my-profile/posts" => Some(RouterDeepLink::MyProfile {
            highlight: hl("my-profile/posts"),
        }),
        "my-profile/posts/add-album" => Some(RouterDeepLink::MyProfile {
            highlight: hl("my-profile/posts/add-album"),
        }),
        "my-profile/gifts" => Some(RouterDeepLink::OpenPaymentsOverlay {
            screen: PaymentsScreen::StarGifts,
        }),
        "my-profile/archived-posts" => Some(RouterDeepLink::MyProfile {
            highlight: hl("my-profile/archived-posts"),
        }),
        "emoji-status" => Some(RouterDeepLink::OpenMainMenu {
            highlight: hl("main-menu/emoji-status"),
        }),
        "profile-color"
        | "profile-color/profile"
        | "profile-color/profile/add-icons"
        | "profile-color/profile/use-gift"
        | "profile-color/profile/reset"
        | "profile-color/name"
        | "profile-color/name/add-icons"
        | "profile-color/name/use-gift"
        | "appearance/your-color"
        | "appearance/your-color/profile"
        | "appearance/your-color/profile/add-icons"
        | "appearance/your-color/profile/use-gift"
        | "appearance/your-color/profile/reset"
        | "appearance/your-color/name"
        | "appearance/your-color/name/add-icons"
        | "appearance/your-color/name/use-gift"
        | "edit/your-color" => Some(settings(
            SettingsPage::Account,
            SettingsSection::Information,
            hl("profile-color"),
        )),
        "profile-photo" | "edit/set-photo" => Some(settings(
            SettingsPage::Main,
            SettingsSection::Main,
            hl("profile-photo"),
        )),
        "profile-photo/use-emoji" => Some(settings(
            SettingsPage::Main,
            SettingsSection::Main,
            hl("profile-photo/use-emoji"),
        )),

        // Edit profile sub-routes
        "edit/first-name" => Some(settings(
            SettingsPage::Account,
            SettingsSection::Information,
            hl("edit/first-name"),
        )),
        "edit/last-name" => Some(settings(
            SettingsPage::Account,
            SettingsSection::Information,
            hl("edit/last-name"),
        )),
        "edit/bio" => Some(settings(
            SettingsPage::Account,
            SettingsSection::Information,
            hl("edit/bio"),
        )),
        "edit/birthday" => Some(settings(
            SettingsPage::Account,
            SettingsSection::Information,
            hl("edit/birthday"),
        )),
        "edit/change-number" | "change_number" => Some(RouterDeepLink::Inform {
            title: "Change Phone Number".to_owned(),
            body: "Changing your phone number is currently supported in Telegram mobile apps."
                .to_owned(),
        }),
        "edit/username" => Some(RouterDeepLink::EditUsername),
        "edit/channel" => Some(settings(
            SettingsPage::Account,
            SettingsSection::Information,
            hl("edit/channel"),
        )),
        "edit/add-account" => Some(settings(
            SettingsPage::Account,
            SettingsSection::Information,
            hl("edit/add-account"),
        )),
        "edit/log-out" => Some(settings(
            SettingsPage::Main,
            SettingsSection::Main,
            hl("settings/log-out"),
        )),

        // Devices / Sessions
        "devices" => Some(settings(
            SettingsPage::PrivacySecurity,
            SettingsSection::ActiveSessions,
            query_highlight.clone(),
        )),
        "devices/terminate-sessions" => Some(settings(
            SettingsPage::PrivacySecurity,
            SettingsSection::ActiveSessions,
            hl("sessions/terminate-all"),
        )),
        "devices/auto-terminate" => Some(settings(
            SettingsPage::PrivacySecurity,
            SettingsSection::ActiveSessions,
            hl("sessions/auto-terminate"),
        )),

        // Folders
        "folders" => Some(settings(
            SettingsPage::Main,
            SettingsSection::Folders,
            query_highlight.clone(),
        )),
        "folders/create" => Some(settings(
            SettingsPage::Main,
            SettingsSection::Folders,
            hl("folders/create"),
        )),
        "folders/add-recommended" => Some(settings(
            SettingsPage::Main,
            SettingsSection::Folders,
            hl("folders/add-recommended"),
        )),
        "folders/show-tags" => Some(settings(
            SettingsPage::Main,
            SettingsSection::Folders,
            hl("folders/show-tags"),
        )),
        "folders/tab-view" => Some(settings(
            SettingsPage::Main,
            SettingsSection::Folders,
            hl("folders/tab-view"),
        )),

        // Notifications
        "notifications"
        | "notifications/accounts"
        | "notifications/private-chats"
        | "notifications/private-chats/edit"
        | "notifications/private-chats/show"
        | "notifications/private-chats/sound"
        | "notifications/private-chats/add-exception"
        | "notifications/private-chats/delete-exceptions"
        | "notifications/groups"
        | "notifications/groups/edit"
        | "notifications/groups/show"
        | "notifications/groups/sound"
        | "notifications/groups/add-exception"
        | "notifications/groups/delete-exceptions"
        | "notifications/channels"
        | "notifications/channels/edit"
        | "notifications/channels/show"
        | "notifications/channels/sound"
        | "notifications/channels/add-exception"
        | "notifications/channels/delete-exceptions"
        | "notifications/include-muted-chats"
        | "notifications/count-unread-messages"
        | "notifications/new-contacts"
        | "notifications/pinned-messages" => Some(settings(
            SettingsPage::Notifications,
            SettingsSection::Notifications,
            if path == "notifications" {
                query_highlight.clone()
            } else {
                Some(path.to_owned())
            },
        )),

        // Advanced
        "advanced" => Some(settings(
            SettingsPage::Advanced,
            SettingsSection::Advanced,
            query_highlight.clone(),
        )),

        // Experimental (`deep_links_settings.cpp:530-555`)
        "experimental" | "experiment" | "exp" => Some(settings(
            SettingsPage::Advanced,
            SettingsSection::Experimental,
            query_highlight.clone(),
        )),

        // Privacy & Security
        "privacy"
        | "privacy/blocked"
        | "privacy/blocked/block-user"
        | "privacy/active-websites"
        | "privacy/active-websites/disconnect-all"
        | "privacy/auto-delete"
        | "privacy/auto-delete/set-custom"
        | "auto_delete"
        | "privacy/passkey"
        | "privacy/passkey/create"
        | "privacy/phone-number"
        | "privacy/phone-number/never"
        | "privacy/phone-number/always"
        | "phone_privacy"
        | "privacy/last-seen"
        | "privacy/last-seen/never"
        | "privacy/last-seen/always"
        | "privacy/last-seen/hide-read-time"
        | "privacy/profile-photos"
        | "privacy/profile-photos/never"
        | "privacy/profile-photos/always"
        | "privacy/profile-photos/set-public"
        | "privacy/profile-photos/update-public"
        | "privacy/profile-photos/remove-public"
        | "privacy/bio"
        | "privacy/bio/never"
        | "privacy/bio/always"
        | "privacy/gifts"
        | "privacy/gifts/show-icon"
        | "privacy/gifts/never"
        | "privacy/gifts/always"
        | "privacy/gifts/accepted-types"
        | "privacy/birthday"
        | "privacy/birthday/add"
        | "privacy/birthday/never"
        | "privacy/birthday/always"
        | "privacy/saved-music"
        | "privacy/saved-music/never"
        | "privacy/saved-music/always"
        | "privacy/forwards"
        | "privacy/forwards/never"
        | "privacy/forwards/always"
        | "privacy/calls"
        | "privacy/calls/never"
        | "privacy/calls/always"
        | "privacy/calls/p2p"
        | "privacy/calls/p2p/never"
        | "privacy/calls/p2p/always"
        | "privacy/voice"
        | "privacy/voice/never"
        | "privacy/voice/always"
        | "privacy/messages"
        | "privacy/messages/set-price"
        | "privacy/messages/remove-fee"
        | "privacy/invites"
        | "privacy/invites/never"
        | "privacy/invites/always"
        | "privacy/self-destruct"
        | "privacy/data-settings/suggest-contacts"
        | "privacy/data-settings/clear-payment-info"
        | "privacy/archive-and-mute" => Some(settings(
            SettingsPage::PrivacySecurity,
            SettingsSection::PrivacySecurity,
            if path == "privacy" {
                query_highlight.clone()
            } else {
                Some(path.to_owned())
            },
        )),

        // Local passcode
        "privacy/passcode"
        | "privacy/passcode/disable"
        | "privacy/passcode/change"
        | "privacy/passcode/auto-lock"
        | "privacy/passcode/face-id"
        | "privacy/passcode/fingerprint" => Some(settings(
            SettingsPage::PrivacySecurity,
            SettingsSection::LocalPasscode,
            Some(path.to_owned()),
        )),

        // Two-Step Verification / Cloud Password / Login email
        "privacy/2sv"
        | "privacy/2sv/change"
        | "privacy/2sv/disable"
        | "privacy/2sv/change-email"
        | "login_email" => Some(settings(
            SettingsPage::PrivacySecurity,
            SettingsSection::CloudPassword,
            Some(path.to_owned()),
        )),

        // Data & Storage / Proxy
        "data/storage" => Some(settings(
            SettingsPage::Advanced,
            SettingsSection::Advanced,
            hl("data/storage"),
        )),
        "data/storage/clear-cache" => Some(settings(
            SettingsPage::Advanced,
            SettingsSection::Advanced,
            hl("storage/clear-cache"),
        )),
        "data/max-cache" => Some(settings(
            SettingsPage::Advanced,
            SettingsSection::Advanced,
            hl("storage/max-cache"),
        )),
        "data/show-18-content" => Some(settings(
            SettingsPage::ChatSettings,
            SettingsSection::Chat,
            hl("chat/show-18-content"),
        )),
        "data/proxy" => Some(RouterDeepLink::ProxySettings { highlight: None }),
        "data/proxy/add-proxy" => Some(RouterDeepLink::ProxySettings {
            highlight: hl("proxy/add-proxy"),
        }),
        "data/proxy/share-list" => Some(RouterDeepLink::ProxySettings {
            highlight: hl("proxy/share-list"),
        }),

        // Appearance / Chat Settings / Power Saving / Themes / Wallpapers / Stickers / Shortcuts
        "appearance"
        | "appearance/themes"
        | "appearance/themes/edit"
        | "appearance/themes/create"
        | "themes"
        | "themes/edit"
        | "themes/create"
        | "appearance/wallpapers"
        | "appearance/wallpapers/set"
        | "appearance/wallpapers/choose-photo"
        | "appearance/auto-night-mode"
        | "appearance/stickers-and-emoji"
        | "appearance/stickers-and-emoji/edit"
        | "appearance/stickers-and-emoji/trending"
        | "appearance/stickers-and-emoji/archived"
        | "appearance/stickers-and-emoji/emoji"
        | "appearance/stickers-and-emoji/emoji/suggest"
        | "appearance/stickers-and-emoji/emoji/quick-reaction"
        | "appearance/stickers-and-emoji/emoji/quick-reaction/choose"
        | "appearance/stickers-and-emoji/suggest-by-emoji"
        | "appearance/stickers-and-emoji/emoji/large"
        | "appearance/shortcuts" => Some(settings(
            SettingsPage::ChatSettings,
            SettingsSection::Chat,
            if path == "appearance" {
                query_highlight.clone()
            } else {
                Some(path.to_owned())
            },
        )),
        "appearance/night-mode" => Some(RouterDeepLink::OpenMainMenu {
            highlight: hl("main-menu/night-mode"),
        }),
        "appearance/text-size" => Some(settings(
            SettingsPage::Main,
            SettingsSection::Main,
            hl("main/scale"),
        )),
        "power-saving"
        | "power-saving/stickers"
        | "power-saving/emoji"
        | "power-saving/effects"
        | "appearance/animations" => Some(settings(
            SettingsPage::Advanced,
            SettingsSection::PowerSaving,
            Some(path.to_owned()),
        )),

        // Language
        "language" => Some(RouterDeepLink::Language { highlight: None }),
        "language/show-button" => Some(RouterDeepLink::Language {
            highlight: hl("language/show-button"),
        }),
        "language/translate-chats" => Some(RouterDeepLink::Language {
            highlight: hl("language/translate-chats"),
        }),
        "language/do-not-translate" => Some(RouterDeepLink::Language {
            highlight: hl("language/do-not-translate"),
        }),

        // Premium / Stars / TON / Business / Gifts
        "premium" => Some(RouterDeepLink::OpenPaymentsOverlay {
            screen: PaymentsScreen::Premium,
        }),
        "stars" | "stars/top-up" | "stars/stats" | "stars/gift" | "stars/earn" | "ton" => {
            Some(RouterDeepLink::OpenPaymentsOverlay {
                screen: PaymentsScreen::Stars,
            })
        }
        "business" | "business/do-not-hide-ads" => Some(RouterDeepLink::OpenBusinessOverlay),
        "send-gift" | "send-gift/self" => Some(RouterDeepLink::OpenPaymentsOverlay {
            screen: PaymentsScreen::StarGifts,
        }),

        // Saved Messages, Calls, FAQ, Ask Question, Features, Search, QR Code
        "saved-messages" => Some(RouterDeepLink::SavedMessages),
        "calls" => Some(settings(
            SettingsPage::Main,
            SettingsSection::Calls,
            query_highlight.clone(),
        )),
        "calls/all" | "calls/start-call" => Some(RouterDeepLink::OpenRecentCalls),
        "faq" => Some(RouterDeepLink::OpenExternalInfo {
            url: "https://telegram.org/faq".to_owned(),
        }),
        "ask-question" => Some(RouterDeepLink::Inform {
            title: "Ask a Question".to_owned(),
            body: "Telegram support is answered by volunteers. Open https://telegram.org/faq for common answers.".to_owned(),
        }),
        "features" => Some(RouterDeepLink::OpenExternalInfo {
            url: "https://t.me/TelegramTips".to_owned(),
        }),
        "search" => Some(settings(
            SettingsPage::Main,
            SettingsSection::Main,
            hl("settings/search"),
        )),
        "qr-code" => Some(RouterDeepLink::QrCode {
            highlight_share: false,
        }),
        "qr-code/share" => Some(RouterDeepLink::QrCode {
            highlight_share: true,
        }),
        _ => None,
    }
}

fn dispatch_contacts_route(subpath: &str, raw_command: &str) -> DeepLink {
    match subpath {
        "" | "search" | "sort" => DeepLink::Router(RouterDeepLink::OpenContacts { add_new: false }),
        "new" => DeepLink::Router(RouterDeepLink::OpenContacts { add_new: true }),
        _ => DeepLink::Unsupported {
            command: raw_command.to_owned(),
        },
    }
}

fn dispatch_chats_route(subpath: &str, raw_command: &str) -> DeepLink {
    match subpath {
        "search" => DeepLink::Router(RouterDeepLink::FocusSearch),
        "emoji-status" => DeepLink::Router(RouterDeepLink::OpenMainMenu {
            highlight: Some("main-menu/emoji-status".to_owned()),
        }),
        _ => DeepLink::Unsupported {
            command: raw_command.to_owned(),
        },
    }
}

fn dispatch_new_route(subpath: &str, raw_command: &str) -> DeepLink {
    match subpath {
        "group" => DeepLink::Router(RouterDeepLink::NewGroup),
        "channel" => DeepLink::Router(RouterDeepLink::NewChannel),
        "contact" => DeepLink::Router(RouterDeepLink::OpenContacts { add_new: true }),
        _ => DeepLink::Unsupported {
            command: raw_command.to_owned(),
        },
    }
}

fn dispatch_newbot_route(
    subpath: &str,
    params: &BTreeMap<String, String>,
    raw_command: &str,
) -> DeepLink {
    if !subpath.is_empty() {
        return DeepLink::Unsupported {
            command: raw_command.to_owned(),
        };
    }
    let manager = params.get("manager").cloned().unwrap_or_default();
    let username = params.get("username").filter(|s| !s.is_empty()).cloned();
    let name = params.get("name").filter(|s| !s.is_empty()).cloned();
    DeepLink::Router(RouterDeepLink::NewBot {
        manager,
        username,
        name,
    })
}

// ============================================================================
// LocalUrlHandlers() (`local_url_handlers.cpp:1852-1984`)
// ============================================================================

fn parse_local_url_handlers(command: &str) -> Option<DeepLink> {
    let no_frag = command.split('#').next().unwrap_or("");

    // 1. `^join/?\?invite=([a-zA-Z0-9._-]+)(&|$)`
    if let Some(q) = strip_prefix_with_opt_slash_and_query(no_frag, "join")
        && let Some(hash) = extract_leading_param(q, "invite")
        && is_valid_slug_token(hash)
    {
        return Some(DeepLink::JoinChat {
            hash: hash.to_owned(),
        });
    }

    // 2. `^addlist/?\?slug=([a-zA-Z0-9._-]+)(&|$)`
    if let Some(q) = strip_prefix_with_opt_slash_and_query(no_frag, "addlist")
        && let Some(slug) = extract_leading_param(q, "slug")
        && is_valid_slug_token(slug)
    {
        return Some(DeepLink::AddList {
            slug: slug.to_owned(),
        });
    }

    // 3. `^(addstickers|addemoji)/?\?set=([a-zA-Z0-9._]+)(&|$)`
    if let Some((is_emoji, q)) = strip_prefix_with_opt_slash_and_query(no_frag, "addstickers")
        .map(|q| (false, q))
        .or_else(|| strip_prefix_with_opt_slash_and_query(no_frag, "addemoji").map(|q| (true, q)))
        && let Some(set_name) = extract_leading_param(q, "set")
        && is_valid_domain(set_name)
    {
        return Some(DeepLink::AddStickers {
            short_name: set_name.to_owned(),
            is_emoji,
        });
    }

    // 4. `^addtheme/?\?slug=([a-zA-Z0-9._]+)(&|$)`
    if let Some(q) = strip_prefix_with_opt_slash_and_query(no_frag, "addtheme")
        && let Some(slug) = extract_leading_param(q, "slug")
        && is_valid_domain(slug)
    {
        return Some(DeepLink::AddTheme {
            slug: slug.to_owned(),
        });
    }

    // 5. `^addstyle/?\?slug=([a-zA-Z0-9._]+)(&|$)`
    if let Some(q) = strip_prefix_with_opt_slash_and_query(no_frag, "addstyle")
        && let Some(slug) = extract_leading_param(q, "slug")
        && is_valid_domain(slug)
    {
        return Some(DeepLink::AddStyle {
            slug: slug.to_owned(),
        });
    }

    // 6. `^setlanguage/?(\?lang=([a-zA-Z0-9._-]+))?(&|$)`
    if let Some(after) = strip_prefix_ci(no_frag, "setlanguage") {
        let after = after.strip_prefix('/').unwrap_or(after);
        if after.is_empty() || after.starts_with('&') {
            return Some(DeepLink::SetLanguage { lang: None });
        }
        if let Some(q) = after.strip_prefix('?')
            && let Some(lang) = extract_leading_param(q, "lang")
            && is_valid_slug_token(lang)
        {
            return Some(DeepLink::SetLanguage {
                lang: Some(lang.to_owned()),
            });
        }
    }

    // 7. `^msg_url/?\?(.+)(#|$)`
    if let Some(q) = strip_prefix_with_opt_slash_and_query(no_frag, "msg_url")
        && !q.is_empty()
    {
        let params = parse_query_params(q);
        let share_url = params.get("url").cloned().unwrap_or_default();
        // Match tdesktop `ShareUrl` guard (`local_url_handlers.cpp:348-355`):
        // reject empty URL or URL whose first non-whitespace/non-ORC character is `@`.
        let first_visible = share_url
            .chars()
            .find(|&ch| !ch.is_whitespace() && ch != '\u{FFFC}');
        if !share_url.is_empty() && first_visible != Some('@') {
            let text = params.get("text").cloned().unwrap_or_default();
            return Some(DeepLink::ShareUrl {
                url: share_url,
                text,
            });
        }
        return None;
    }

    // 8. `^confirmphone/?\?(.+)(#|$)`
    if let Some(q) = strip_prefix_with_opt_slash_and_query(no_frag, "confirmphone")
        && !q.is_empty()
    {
        let params = parse_query_params(q);
        let phone = params.get("phone").cloned().unwrap_or_default();
        let hash = params.get("hash").cloned().unwrap_or_default();
        if !phone.is_empty() && !hash.is_empty() {
            return Some(DeepLink::ConfirmPhone { phone, hash });
        }
        return None;
    }

    // 9. `^socks/?\?(.+)(#|$)`
    if let Some(q) = strip_prefix_with_opt_slash_and_query(no_frag, "socks")
        && !q.is_empty()
    {
        return Some(DeepLink::Proxy {
            kind: ProxyLinkKind::Socks5,
            params: parse_query_params(q),
        });
    }

    // 10. `^proxy/?\?(.+)(#|$)`
    if let Some(q) = strip_prefix_with_opt_slash_and_query(no_frag, "proxy")
        && !q.is_empty()
    {
        let mut params = parse_query_params(q);
        if let Some(secret) = params.get_mut("secret") {
            *secret = secret.replace('+', "-").replace('/', "_");
        }
        return Some(DeepLink::Proxy {
            kind: ProxyLinkKind::Mtproto,
            params,
        });
    }

    // 11. `^webproxy/?\?(.+)(#|$)`
    if let Some(q) = strip_prefix_with_opt_slash_and_query(no_frag, "webproxy")
        && !q.is_empty()
    {
        return Some(DeepLink::Proxy {
            kind: ProxyLinkKind::Web,
            params: parse_query_params(q),
        });
    }

    // 12. `^passport/?\?(.+)(#|$)`
    if let Some(q) = strip_prefix_with_opt_slash_and_query(no_frag, "passport")
        && !q.is_empty()
    {
        return Some(DeepLink::Passport(parse_passport_params(
            &parse_query_params(q),
        )));
    }

    // 13. `^bg/?\?(.+)(#|$)`
    if let Some(q) = strip_prefix_with_opt_slash_and_query(no_frag, "bg")
        && !q.is_empty()
    {
        let params = parse_query_params(q);
        let color = params.get("color").filter(|s| !s.is_empty());
        let gradient = params.get("gradient").filter(|s| !s.is_empty());
        let slug = params.get("slug").filter(|s| !s.is_empty());
        let (slug_or_color, is_color_or_gradient) = if let Some(c) = color {
            (c.clone(), true)
        } else if let Some(g) = gradient {
            (g.clone(), true)
        } else {
            (slug.cloned().unwrap_or_default(), false)
        };
        return Some(DeepLink::WallPaper(WallPaperLinkInfo {
            slug_or_color,
            is_color_or_gradient,
            params,
        }));
    }

    // 14. `^resolve/?\?(.+)(#|$)`
    if let Some(q) = strip_prefix_with_opt_slash_and_query(no_frag, "resolve")
        && !q.is_empty()
    {
        return parse_resolve_handler(q);
    }

    // 15. `^contact/?(?:\?([^#]*))?(?:#.*)?$`
    if let Some(after) = strip_prefix_ci(no_frag, "contact") {
        let after = after.strip_prefix('/').unwrap_or(after);
        if after.is_empty() || after.starts_with('?') {
            let params = after
                .strip_prefix('?')
                .map(parse_query_params)
                .unwrap_or_default();
            let token = params.get("token").cloned().unwrap_or_default();
            let no_paid_token = params.get("free").filter(|s| !s.is_empty()).cloned();
            return Some(DeepLink::ContactToken {
                token,
                no_paid_token,
            });
        }
    }

    // 16. `^privatepost/?\?(.+)(#|$)`
    if let Some(q) = strip_prefix_with_opt_slash_and_query(no_frag, "privatepost")
        && !q.is_empty()
    {
        return parse_private_post_handler(q);
    }

    // 17. `^test_chat_theme/?\?(.+)(#|$)`
    if let Some(q) = strip_prefix_with_opt_slash_and_query(no_frag, "test_chat_theme")
        && !q.is_empty()
    {
        return Some(DeepLink::TestChatTheme {
            params: parse_query_params(q),
        });
    }

    // 18. `^invoice/?\?(.+)(#|$)`
    if let Some(q) = strip_prefix_with_opt_slash_and_query(no_frag, "invoice")
        && !q.is_empty()
    {
        let params = parse_query_params(q);
        let slug = params.get("slug").filter(|s| !s.is_empty())?.clone();
        return Some(DeepLink::Invoice { slug });
    }

    // 19. `^premium_offer/?(\?.+)?(#|$)`
    if let Some(after) = strip_prefix_ci(no_frag, "premium_offer") {
        let after = after.strip_prefix('/').unwrap_or(after);
        if after.is_empty() || after.starts_with('?') {
            let params = after
                .strip_prefix('?')
                .map(parse_query_params)
                .unwrap_or_default();
            let ref_addition = params.get("ref").map(String::as_str).unwrap_or("");
            let referral = if ref_addition.is_empty() {
                "deeplink".to_owned()
            } else {
                format!("deeplink_{ref_addition}")
            };
            return Some(DeepLink::PremiumOffer { referral });
        }
    }

    // 20. `^premium_multigift/?\?(.+)(#|$)`
    if let Some(q) = strip_prefix_with_opt_slash_and_query(no_frag, "premium_multigift")
        && !q.is_empty()
    {
        return Some(DeepLink::PremiumMultigift {
            params: parse_query_params(q),
        });
    }

    // 21. `^login/?(\?code=([0-9]+))(&|$)`
    if let Some(q) = strip_prefix_with_opt_slash_and_query(no_frag, "login")
        && let Some(code) = extract_leading_param(q, "code")
        && is_digits(code)
    {
        return Some(DeepLink::LoginCode {
            code: code.to_owned(),
        });
    }

    // 22. `^boost/?\?(.+)(#|$)`
    if let Some(q) = strip_prefix_with_opt_slash_and_query(no_frag, "boost")
        && !q.is_empty()
    {
        let params = parse_query_params(q);
        let domain = params.get("domain").filter(|s| !s.is_empty()).cloned();
        let channel_id = params
            .get("c")
            .or_else(|| params.get("channel"))
            .and_then(|s| s.trim_start_matches('-').parse::<u64>().ok())
            .filter(|&id| id > 0);
        return Some(DeepLink::Boost { domain, channel_id });
    }

    // 23. `^message/?\?slug=([a-zA-Z0-9._-]+)(&|$)`
    if let Some(q) = strip_prefix_with_opt_slash_and_query(no_frag, "message")
        && let Some(slug) = extract_leading_param(q, "slug")
        && is_valid_slug_token(slug)
    {
        return Some(DeepLink::BusinessChatLink {
            slug: slug.to_owned(),
        });
    }

    // 24. `^stars_topup/?\?(.+)(#|$)`
    if let Some(q) = strip_prefix_with_opt_slash_and_query(no_frag, "stars_topup")
        && !q.is_empty()
    {
        let params = parse_query_params(q);
        let raw_amount = params
            .get("balance")
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
        let amount = raw_amount.clamp(1, 1_000_000);
        let purpose = params.get("purpose").cloned().unwrap_or_default();
        return Some(DeepLink::StarsTopUp { amount, purpose });
    }

    // 25. `^nft/?\?slug=([a-zA-Z0-9._-]+)(&|$)`
    if let Some(q) = strip_prefix_with_opt_slash_and_query(no_frag, "nft")
        && let Some(slug) = extract_leading_param(q, "slug")
        && is_valid_slug_token(slug)
    {
        return Some(DeepLink::UniqueGift {
            slug: slug.to_owned(),
        });
    }

    // 26. `^stargift_auction/?\?slug=([a-zA-Z0-9._-]+)(&|$)`
    if let Some(q) = strip_prefix_with_opt_slash_and_query(no_frag, "stargift_auction")
        && let Some(slug) = extract_leading_param(q, "slug")
        && is_valid_slug_token(slug)
    {
        return Some(DeepLink::StarGiftAuction {
            slug: slug.to_owned(),
        });
    }

    // 27. `^call/?\?slug=([a-zA-Z0-9._-]+)(&|$)`
    if let Some(q) = strip_prefix_with_opt_slash_and_query(no_frag, "call")
        && let Some(slug) = extract_leading_param(q, "slug")
        && is_valid_slug_token(slug)
    {
        return Some(DeepLink::ConferenceCall {
            slug: slug.to_owned(),
        });
    }

    // 28. `^stars/?(#|$)` (and optional query)
    if let Some(after) = strip_prefix_ci(no_frag, "stars") {
        let after = after.strip_prefix('/').unwrap_or(after);
        if after.is_empty() || after.starts_with('?') {
            return Some(DeepLink::StarsSettings);
        }
    }

    // 29. `^(ton|grams)/?(#|$)` (and optional query)
    if let Some(after) =
        strip_prefix_ci(no_frag, "ton").or_else(|| strip_prefix_ci(no_frag, "grams"))
    {
        let after = after.strip_prefix('/').unwrap_or(after);
        if after.is_empty() || after.starts_with('?') {
            return Some(DeepLink::TonSettings);
        }
    }

    // 30. `^sendgrams/?(\?.+)?(#|$)`
    if let Some(after) = strip_prefix_ci(no_frag, "sendgrams") {
        let after = after.strip_prefix('/').unwrap_or(after);
        if after.is_empty() || after.starts_with('?') {
            let params = after
                .strip_prefix('?')
                .map(parse_query_params)
                .unwrap_or_default();
            return Some(DeepLink::SendGrams {
                to: params.get("to").filter(|s| !s.is_empty()).cloned(),
                amount: params.get("amount").filter(|s| !s.is_empty()).cloned(),
                startapp: params.get("startapp").filter(|s| !s.is_empty()).cloned(),
                transfer_uri: None,
            });
        }
    }

    // 31. `^oauth/?\?(.+)(#|$)`
    if let Some(q) = strip_prefix_with_opt_slash_and_query(no_frag, "oauth")
        && !q.is_empty()
    {
        let params = parse_query_params(q);
        let token = params.get("token").filter(|s| !s.is_empty())?.clone();
        return Some(DeepLink::OAuth { token });
    }

    // 31b. `^giftcode/?\?slug=([a-zA-Z0-9._-]+)(&|$)`
    if let Some(q) = strip_prefix_with_opt_slash_and_query(no_frag, "giftcode")
        && let Some(slug) = extract_leading_param(q, "slug")
        && is_valid_slug_token(slug)
    {
        return Some(DeepLink::GiftCode {
            slug: slug.to_owned(),
        });
    }

    // 32. Fallback: `^([^?]+)(\?|#|$)` -> `HandleUnknown` (`help.getDeepLinkInfo`)
    let path = no_frag.split('?').next().unwrap_or("");
    if !path.is_empty() {
        return Some(DeepLink::Unknown {
            path: path.to_owned(),
        });
    }

    None
}

fn parse_resolve_handler(query: &str) -> Option<DeepLink> {
    let params = parse_query_params(query);
    let domain_param = params.get("domain").map(String::as_str).unwrap_or("");
    let appname_param = params.get("appname").map(String::as_str).unwrap_or("");

    // `domain=sendgrams` with `startapp` -> SendGrams / TON Connect
    if domain_param.eq_ignore_ascii_case("sendgrams")
        && let Some(startapp) = params.get("startapp").filter(|s| !s.is_empty())
    {
        return Some(DeepLink::SendGrams {
            to: None,
            amount: None,
            startapp: Some(startapp.clone()),
            transfer_uri: None,
        });
    }

    // `domain=giftcode` & non-empty `appname`
    if domain_param.eq_ignore_ascii_case("giftcode") && !appname_param.is_empty() {
        return Some(DeepLink::GiftCode {
            slug: appname_param.to_owned(),
        });
    }

    // `domain=oauth` & non-empty `startapp`
    if domain_param.eq_ignore_ascii_case("oauth")
        && let Some(token) = params.get("startapp").filter(|s| !s.is_empty())
    {
        return Some(DeepLink::OAuth {
            token: token.clone(),
        });
    }

    // `domain=telegrampassport`
    if domain_param.eq_ignore_ascii_case("telegrampassport") {
        return Some(DeepLink::Passport(parse_passport_params(&params)));
    }

    // Fix `t.me/s/username` web channel preview links.
    let web_channel_preview = domain_param == "s" && !appname_param.is_empty();
    let domain = if web_channel_preview {
        appname_param
    } else {
        domain_param
    };
    let phone = params.get("phone").map(String::as_str).unwrap_or("");

    let domain_ok = is_valid_domain(domain);
    let phone_ok = !phone.is_empty() && is_digits(phone);
    if !domain_ok && !phone_ok {
        return None;
    }

    let shared_media_tab = params.get("tab").and_then(|v| parse_shared_media_type(v));
    let mut resolve_type = if params.contains_key("profile") {
        ResolveType::Profile
    } else {
        ResolveType::Default
    };

    let mut start_token = params.get("start").filter(|s| !s.is_empty()).cloned();
    let referral = params.get("ref").filter(|s| !s.is_empty()).cloned();
    if start_token.is_some() {
        resolve_type = ResolveType::BotStart;
    } else if params.contains_key("startgroup") {
        resolve_type = ResolveType::AddToGroup;
        start_token = params.get("startgroup").filter(|s| !s.is_empty()).cloned();
    } else if params.contains_key("startchannel") {
        resolve_type = ResolveType::AddToChannel;
    } else if params.contains_key("boost") {
        resolve_type = ResolveType::Boost;
    }

    let start_admin_rights = if matches!(
        resolve_type,
        ResolveType::AddToGroup | ResolveType::AddToChannel
    ) {
        params
            .get("admin")
            .map(|v| parse_requested_admin_rights(v))
            .unwrap_or_default()
    } else {
        ChatAdminRights::empty()
    };

    let post_param = params.get("post").map(String::as_str).unwrap_or("");
    let post_id = post_param.parse::<i32>().ok().filter(|&id| id > 0);

    let story = params.get("story").filter(|s| !s.is_empty()).cloned();
    let story_album_id = params
        .get("album")
        .and_then(|s| s.parse::<i32>().ok())
        .filter(|&id| id > 0);
    let gift_collection_id = params
        .get("collection")
        .and_then(|s| s.parse::<i32>().ok())
        .filter(|&id| id > 0);

    let appname = if web_channel_preview {
        ""
    } else {
        appname_param
    };
    let comment_id = params
        .get("comment")
        .and_then(|s| s.parse::<i32>().ok())
        .filter(|&id| id > 0);
    let topic_id = params
        .get("topic")
        .and_then(|s| s.parse::<i32>().ok())
        .filter(|&id| id > 0);
    let thread_id = topic_id.or_else(|| {
        params
            .get("thread")
            .and_then(|s| s.parse::<i32>().ok())
            .filter(|&id| id > 0)
    });
    let replies = comment_id
        .map(RepliesByLink::Comment)
        .or_else(|| thread_id.map(RepliesByLink::Thread));

    let game_param = params.get("game").map(String::as_str).unwrap_or("");
    let video_timestamp = params
        .get("t")
        .filter(|s| !s.is_empty())
        .map(|s| parse_video_timestamp(s));
    let poll_option = params.get("option").filter(|s| !s.is_empty()).cloned();

    if params.contains_key("direct") {
        resolve_type = ResolveType::ChannelDirect;
    }
    if !game_param.is_empty() && is_valid_domain(game_param) {
        start_token = Some(game_param.to_owned());
        resolve_type = ResolveType::ShareGame;
    }
    if !appname.is_empty() {
        resolve_type = ResolveType::BotApp;
        if start_token.is_none() && params.contains_key("startapp") {
            start_token = params.get("startapp").filter(|s| !s.is_empty()).cloned();
        }
    }

    let mode = params.get("mode").map(String::as_str).unwrap_or("");
    let bot_app_name = if !appname.is_empty() {
        Some(appname.to_owned())
    } else if !post_param.is_empty() {
        Some(post_param.to_owned())
    } else {
        None
    };

    let attach_bot_toggle_command = if params.contains_key("startattach") {
        Some(params.get("startattach").cloned().unwrap_or_default())
    } else if appname.is_empty() && params.contains_key("startapp") {
        Some(params.get("startapp").cloned().unwrap_or_default())
    } else {
        None
    };

    let voicechat_hash = if params.contains_key("livestream") {
        Some(params.get("livestream").cloned().unwrap_or_default())
    } else if params.contains_key("videochat") {
        Some(params.get("videochat").cloned().unwrap_or_default())
    } else if params.contains_key("voicechat") {
        Some(params.get("voicechat").cloned().unwrap_or_default())
    } else {
        None
    };

    Some(DeepLink::Resolve(Box::new(ResolveLinkInfo {
        domain: domain_ok.then(|| domain.to_owned()),
        phone: phone_ok.then(|| phone.to_owned()),
        post_id,
        replies,
        story,
        story_album_id,
        gift_collection_id,
        shared_media_tab,
        sidebar: sidebar_requested(&params),
        video_timestamp,
        text: params.get("text").filter(|s| !s.is_empty()).cloned(),
        resolve_type,
        referral,
        no_paid_token: params.get("free").filter(|s| !s.is_empty()).cloned(),
        start_token,
        start_admin_rights,
        bot_app_name,
        bot_app_fullscreen: mode == "fullscreen",
        attach_bot_username: params.get("attach").filter(|s| !s.is_empty()).cloned(),
        attach_bot_toggle_command,
        attach_bot_main_open: appname.is_empty() && params.contains_key("startapp"),
        attach_bot_main_compact: appname.is_empty()
            && params.contains_key("startapp")
            && mode == "compact",
        attach_bot_choose_types: params
            .get("choose")
            .map(|v| parse_choose_types(&v.replace('+', " ")))
            .unwrap_or_default(),
        voicechat_hash,
        poll_option,
        open_in_new_window: params.get("tdesktop_target").is_some_and(|v| v == "blank"),
    })))
}

fn parse_private_post_handler(query: &str) -> Option<DeepLink> {
    let params = parse_query_params(query);
    let channel_raw = params.get("channel")?;
    let channel_id = channel_raw.trim_start_matches('-').parse::<u64>().ok()?;
    if channel_id == 0 {
        return None;
    }
    let post_id = match params.get("post").filter(|s| !s.is_empty()) {
        Some(raw) => {
            let parsed = raw.parse::<i32>().ok()?;
            if parsed <= 0 {
                return None;
            }
            Some(parsed)
        }
        None => None,
    };
    let comment_id = params
        .get("comment")
        .and_then(|s| s.parse::<i32>().ok())
        .filter(|&id| id > 0);
    let topic_id = params
        .get("topic")
        .and_then(|s| s.parse::<i32>().ok())
        .filter(|&id| id > 0);
    let thread_id = topic_id.or_else(|| {
        params
            .get("thread")
            .and_then(|s| s.parse::<i32>().ok())
            .filter(|&id| id > 0)
    });
    let replies = comment_id
        .map(RepliesByLink::Comment)
        .or_else(|| thread_id.map(RepliesByLink::Thread));

    Some(DeepLink::PrivatePost(PrivatePostLinkInfo {
        channel_id,
        post_id,
        replies,
        shared_media_tab: params.get("tab").and_then(|v| parse_shared_media_type(v)),
        sidebar: sidebar_requested(&params),
        poll_option: params.get("option").filter(|s| !s.is_empty()).cloned(),
    }))
}

fn parse_passport_params(params: &BTreeMap<String, String>) -> PassportLinkInfo {
    let bot_id = params
        .get("bot_id")
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);
    let scope = params.get("scope").cloned().unwrap_or_default();
    let callback_url = params.get("callback_url").cloned().unwrap_or_default();
    let public_key = params.get("public_key").cloned().unwrap_or_default();
    // Port of `Passport::NonceNameByScope` (`passport_form_controller.cpp:271-275`).
    let nonce_key = if scope.starts_with('{') && scope.ends_with('}') {
        "nonce"
    } else {
        "payload"
    };
    let nonce = params.get(nonce_key).cloned().unwrap_or_default();
    PassportLinkInfo {
        bot_id,
        scope,
        callback_url,
        public_key,
        nonce,
    }
}

// ============================================================================
// InternalUrlHandlers() (`local_url_handlers.cpp:1986-2058`)
// ============================================================================

fn parse_internal_url(raw: &str) -> Option<InternalDeepLink> {
    // Strip optional `~<tag>~:` prefix (`application.cpp:1391-1396`).
    let cmd = strip_internal_tag_prefix(raw);

    // 1. `^media_timestamp/?\?base=([a-zA-Z0-9._-]+)&t=(\d+)(&|$)`
    if let Some(q) = strip_prefix_with_opt_slash_and_query(cmd, "media_timestamp") {
        let params = parse_query_params(q);
        if let (Some(base), Some(t)) = (params.get("base"), params.get("t"))
            && is_valid_slug_token(base)
            && let Ok(pos) = t.parse::<i64>()
            && pos >= 0
        {
            return Some(InternalDeepLink::MediaTimestamp {
                base: base.clone(),
                position_secs: pos,
            });
        }
    }

    // 2. `^show_invite_link/?\?link=(.+)(&|$)`
    if let Some(q) = strip_prefix_with_opt_slash_and_query(cmd, "show_invite_link")
        && let Some(b64) = extract_leading_param(q, "link")
        && let Some(decoded) = decode_base64_utf8(b64)
        && !decoded.is_empty()
    {
        return Some(InternalDeepLink::ShowInviteLink { link: decoded });
    }

    // 3. `^copy:(.+)$`
    if let Some(rest) = strip_prefix_ci(cmd, "copy:")
        && !rest.is_empty()
    {
        return Some(InternalDeepLink::CopyText(rest.to_owned()));
    }

    // 4. `^about_tags$`
    if cmd.eq_ignore_ascii_case("about_tags") {
        return Some(InternalDeepLink::AboutTags);
    }

    // 5. `^tme_getpremium(\?.*)?$`
    if let Some(after) = strip_prefix_ci(cmd, "tme_getpremium")
        && (after.is_empty() || after.starts_with('?'))
    {
        let ref_val = after
            .strip_prefix('?')
            .map(parse_query_params)
            .and_then(|p| p.get("ref").cloned())
            .unwrap_or_default();
        return Some(InternalDeepLink::GetPremium {
            source: get_premium_source(&ref_val),
        });
    }

    // 6. `^common_groups/([0-9]+)$`
    if let Some(id_str) = strip_prefix_ci(cmd, "common_groups/")
        && is_digits(id_str)
        && let Ok(id) = id_str.parse::<u64>()
    {
        return Some(InternalDeepLink::CommonGroups(id));
    }

    // 7. `^edit_peer/([0-9]+)$`
    if let Some(id_str) = strip_prefix_ci(cmd, "edit_peer/")
        && is_digits(id_str)
        && let Ok(id) = id_str.parse::<u64>()
    {
        return Some(InternalDeepLink::EditPeer(id));
    }

    // 8. `^username_link/([a-zA-Z0-9._-]+)@([0-9]+)$`
    if let Some(rest) = strip_prefix_ci(cmd, "username_link/")
        && let Some((user, id_str)) = rest.split_once('@')
        && is_valid_slug_token(user)
        && is_digits(id_str)
    {
        return Some(InternalDeepLink::CopyUsername {
            username: user.to_owned(),
            as_link: true,
        });
    }

    // 9. `^username_regular/([a-zA-Z0-9._-]+)@([0-9]+)$`
    if let Some(rest) = strip_prefix_ci(cmd, "username_regular/")
        && let Some((user, id_str)) = rest.split_once('@')
        && is_valid_slug_token(user)
        && is_digits(id_str)
    {
        return Some(InternalDeepLink::CopyUsername {
            username: user.to_owned(),
            as_link: false,
        });
    }

    // 10. Recognized miscellaneous internal commands
    let known_prefixes = [
        "edit_birthday",
        "edit_privacy_birthday",
        "edit_personal_channel",
        "collectible_phone/",
        "collectible_username/",
        "edit_paid_messages_fee/",
        "stars_examples",
        "about_popular_apps",
    ];
    if known_prefixes
        .iter()
        .any(|p| strip_prefix_ci(cmd, p).is_some())
    {
        return Some(InternalDeepLink::Other {
            command: cmd.to_owned(),
        });
    }

    None
}

// ============================================================================
// Low-level matching & URL encoding helpers
// ============================================================================

fn truncate_utf8(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    if s.len() >= prefix.len() && s[..prefix.len()].eq_ignore_ascii_case(prefix) {
        Some(&s[prefix.len()..])
    } else {
        None
    }
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

fn match_tonsite(url: &str) -> Option<(&str, &str)> {
    let (protocol, rest) = strip_http_scheme(url);
    let host_end = rest.find('/').unwrap_or(rest.len());
    let host = &rest[..host_end];
    if host.is_empty()
        || host.contains('@')
        || host.contains(':')
        || !host.to_ascii_lowercase().ends_with(".ton")
        || host.len() <= 4
    {
        return None;
    }
    Some((protocol, rest))
}

fn match_tme_subdomain(url: &str) -> Option<(&str, &str, &str)> {
    let (protocol, rest) = strip_http_scheme(url);
    let lower = rest.to_ascii_lowercase();
    let dot_idx = lower.find(".t.me")?;
    let name = &rest[..dot_idx];
    if name.len() <= 1
        || name.eq_ignore_ascii_case("www")
        || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        return None;
    }
    let after = &rest[dot_idx + 5..];
    let tail = after.split('#').next().unwrap_or("");
    if !tail.is_empty() && !tail.starts_with('/') && !tail.starts_with('?') {
        return None;
    }
    Some((protocol, name, tail))
}

fn extract_telegram_me_path(url: &str) -> Option<&str> {
    let (_, rest) = strip_http_scheme(url);
    let without_www = strip_prefix_ci(rest, "www.").unwrap_or(rest);
    strip_prefix_ci(without_www, "t.me/")
        .or_else(|| strip_prefix_ci(without_www, "telegram.me/"))
        .or_else(|| strip_prefix_ci(without_www, "telegram.dog/"))
}

fn strip_prefix_with_opt_slash_and_query<'a>(s: &'a str, name: &str) -> Option<&'a str> {
    let after = strip_prefix_ci(s, name)?;
    let after = after.strip_prefix('/').unwrap_or(after);
    after.strip_prefix('?')
}

fn split_at_question(s: &str) -> (&str, &str) {
    match s.split_once('?') {
        Some((left, right)) => (left, right),
        None => (s, ""),
    }
}

fn extract_leading_param<'a>(query: &'a str, key: &str) -> Option<&'a str> {
    let prefix = format!("{key}=");
    let rest = strip_prefix_ci(query, &prefix)?;
    let val = rest.split('&').next().unwrap_or("");
    (!val.is_empty()).then_some(val)
}

fn is_digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

fn is_signed_digits(s: &str) -> bool {
    let digits = s.strip_prefix('-').unwrap_or(s);
    is_digits(digits)
}

fn is_valid_slug_token(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-')
}

fn is_valid_invoice_slug(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn is_valid_bg_token(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-' || b == b'~')
}

fn is_hex_color_6(s: &str) -> bool {
    s.len() == 6 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

fn is_bg_gradient(s: &str) -> bool {
    if let Some((a, b)) = s.split_once('-') {
        return is_hex_color_6(a) && is_hex_color_6(b);
    }
    let parts: Vec<&str> = s.split('~').collect();
    (2..=4).contains(&parts.len()) && parts.iter().all(|p| is_hex_color_6(p))
}

fn has_boost_query_flag(params: &str) -> bool {
    params
        .split('&')
        .any(|part| part.eq_ignore_ascii_case("boost"))
}

fn sidebar_requested(params: &BTreeMap<String, String>) -> bool {
    let Some(val) = params.get("sidebar") else {
        return false;
    };
    if val.is_empty() {
        return true;
    }
    !val.eq_ignore_ascii_case("false") && val != "0"
}

fn get_premium_source(ref_val: &str) -> String {
    let lower = ref_val.to_ascii_lowercase();
    let valid = !lower.is_empty()
        && lower.len() <= GET_PREMIUM_REF_MAX_LEN
        && lower
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    if valid {
        format!("tme_getpremium_{lower}")
    } else {
        "tme_getpremium".to_owned()
    }
}

fn get_premium_local_url(query: &str) -> String {
    let path = query.split('#').next().unwrap_or("");
    let ref_val = path
        .split_once('?')
        .map(|(_, q)| parse_query_params(q))
        .and_then(|p| p.get("ref").cloned())
        .unwrap_or_default();
    if ref_val.is_empty() {
        GET_PREMIUM_LOCAL_PREFIX.to_owned()
    } else {
        format!(
            "{GET_PREMIUM_LOCAL_PREFIX}?ref={}",
            percent_encode_value(&ref_val)
        )
    }
}

fn strip_internal_tag_prefix(s: &str) -> &str {
    if let Some(rest) = s.strip_prefix('~')
        && let Some(end) = rest.find("~:")
        && !rest[..end].is_empty()
        && rest[..end]
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return &rest[end + 2..];
    }
    s
}

fn percent_encode_value(s: &str) -> String {
    percent_encode_keeping(s, b"")
}

fn percent_encode_keeping(s: &str, extra_unreserved: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        if b.is_ascii_alphanumeric()
            || matches!(b, b'-' | b'_' | b'.' | b'~')
            || extra_unreserved.contains(&b)
        {
            out.push(b as char);
        } else {
            out.push('%');
            out.push(HEX[(b >> 4) as usize] as char);
            out.push(HEX[(b & 0x0F) as usize] as char);
        }
    }
    out
}

fn decode_base64_utf8(input: &str) -> Option<String> {
    let mut buf = Vec::with_capacity(input.len() * 3 / 4);
    let mut acc: u32 = 0;
    let mut bits: u8 = 0;
    for b in input.bytes() {
        if b == b'=' {
            break;
        }
        let val = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(val);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            buf.push(((acc >> bits) & 0xFF) as u8);
        }
    }
    String::from_utf8(buf).ok()
}

// ============================================================================
// Comprehensive unit tests ported from tdesktop URL patterns
// ============================================================================

/// Routes a `tg://newbot?manager=...&username=...&name=...` deep link into
/// [`BoxKind::CreateManagedBot`] (called from `window/deep_links.rs`).
pub fn route_newbot_deep_link(
    backend: &mut dyn AppSession,
    session: &mut SessionController,
    manager: &str,
    username: Option<String>,
    name: Option<String>,
) -> DeepLinkOutcome {
    if manager.is_empty() {
        return DeepLinkOutcome::Handled;
    }
    match backend.resolve_username_remote(manager, None) {
        Ok(peer_id) => {
            session.show_peer_history(peer_id, SectionShow::forward(), MsgId::SHOW_AT_UNREAD);
            let manager_name = InfoBackend::peer_profile(backend, peer_id)
                .map(|p| p.title)
                .unwrap_or_else(|| format!("@{manager}"));
            let box_state = tg_app_info::boxes::managed_bot::CreateManagedBotBoxState::new(
                peer_id,
                manager_name,
                manager.trim_start_matches('@'),
                name,
                username,
                true,
            );
            DeepLinkOutcome::show_box(BoxKind::CreateManagedBot(box_state))
        }
        Err(err) => DeepLinkOutcome::Toast(format!("Could not find @{manager}: {err}")),
    }
}

/// Routes `OpenTheme` / `OpenBackground` deep-link overlay requests directly into
/// [`tg_app_settings::themes::AppThemesHost`]. Returns `true` if `req` was handled.
pub fn dispatch_themes_deep_link_overlay(
    ctx: &egui::Context,
    req: &DeepLinkOverlayRequest,
) -> bool {
    match req {
        DeepLinkOverlayRequest::OpenTheme { slug, title } => {
            tg_app_settings::themes::AppThemesHost::open_theme_deep_link(
                ctx,
                slug,
                title.as_deref(),
            );
            true
        }
        DeepLinkOverlayRequest::OpenBackground {
            slug_or_color,
            is_color_or_gradient,
            blur,
            motion,
            intensity,
            rotation,
        } => {
            tg_app_settings::themes::AppThemesHost::open_background_deep_link(
                ctx,
                slug_or_color,
                *is_color_or_gradient,
                *blur,
                *motion,
                *intensity,
                *rotation,
            );
            true
        }
        DeepLinkOverlayRequest::OpenShareTheme { peer_id, peer_name } => {
            tg_app_settings::themes::AppThemesHost::open_share_theme_for_peer(
                ctx,
                peer_id.0,
                peer_name.clone(),
            );
            true
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_tme_and_subdomain_links_to_local_tg_urls() {
        assert_eq!(
            try_convert_url_to_local("https://t.me/rustgram_log"),
            "tg://resolve?domain=rustgram_log"
        );
        assert_eq!(
            try_convert_url_to_local("http://telegram.me/rustgram_log/102"),
            "tg://resolve?domain=rustgram_log&post=102"
        );
        assert_eq!(
            try_convert_url_to_local("https://www.telegram.dog/forum_chat/5/42?comment=9"),
            "tg://resolve?domain=forum_chat&topic=5&post=42&comment=9"
        );
        assert_eq!(
            try_convert_url_to_local("https://rustgram_log.t.me/102?single"),
            "tg://resolve?domain=rustgram_log&post=102&single"
        );
        assert_eq!(
            try_convert_url_to_local("https://t.me/+1234567890?text=hi"),
            "tg://resolve?phone=1234567890&text=hi"
        );
        assert_eq!(
            try_convert_url_to_local("https://t.me/+AbCdEf_123"),
            "tg://join?invite=AbCdEf_123"
        );
        assert_eq!(
            try_convert_url_to_local("https://t.me/joinchat/AbCdEf-99"),
            "tg://join?invite=AbCdEf-99"
        );
        assert_eq!(
            try_convert_url_to_local("https://t.me/addstickers/AnimalsPack"),
            "tg://addstickers?set=AnimalsPack"
        );
        assert_eq!(
            try_convert_url_to_local("https://t.me/addemoji/CustomIcons"),
            "tg://addemoji?set=CustomIcons"
        );
        assert_eq!(
            try_convert_url_to_local("https://t.me/addtheme/MidnightBlue"),
            "tg://addtheme?slug=MidnightBlue"
        );
        assert_eq!(
            try_convert_url_to_local("https://t.me/addlist/work_chats-1"),
            "tg://addlist?slug=work_chats-1"
        );
        assert_eq!(
            try_convert_url_to_local("https://t.me/setlanguage/uk"),
            "tg://setlanguage?lang=uk"
        );
        assert_eq!(
            try_convert_url_to_local("https://t.me/share/url?url=https://rust-lang.org&text=Check"),
            "tg://msg_url?url=https://rust-lang.org&text=Check"
        );
        assert_eq!(
            try_convert_url_to_local("https://t.me/confirmphone?phone=12345&hash=abcdef"),
            "tg://confirmphone?phone=12345&hash=abcdef"
        );
        assert_eq!(
            try_convert_url_to_local("https://t.me/socks?server=127.0.0.1&port=1080"),
            "tg://socks?server=127.0.0.1&port=1080"
        );
        assert_eq!(
            try_convert_url_to_local("https://t.me/proxy?server=1.2.3.4&port=443&secret=dd0011"),
            "tg://proxy?server=1.2.3.4&port=443&secret=dd0011"
        );
        assert_eq!(
            try_convert_url_to_local("https://t.me/invoice/inv_123"),
            "tg://invoice?slug=inv_123"
        );
        assert_eq!(
            try_convert_url_to_local("https://t.me/$inv_456"),
            "tg://invoice?slug=inv_456"
        );
        assert_eq!(
            try_convert_url_to_local("https://t.me/bg/1a2b3c"),
            "tg://bg?color=1a2b3c"
        );
        assert_eq!(
            try_convert_url_to_local("https://t.me/bg/112233-445566?rotation=90"),
            "tg://bg?gradient=112233-445566&rotation=90"
        );
        assert_eq!(
            try_convert_url_to_local("https://t.me/bg/112233~445566~778899"),
            "tg://bg?gradient=112233~445566~778899"
        );
        assert_eq!(
            try_convert_url_to_local("https://t.me/bg/pattern_slug?mode=blur"),
            "tg://bg?slug=pattern_slug&mode=blur"
        );
        assert_eq!(
            try_convert_url_to_local("https://t.me/c/1234567890/42"),
            "tg://privatepost?channel=1234567890&post=42"
        );
        assert_eq!(
            try_convert_url_to_local("https://t.me/c/1234567890/7/42"),
            "tg://privatepost?channel=1234567890&topic=7&post=42"
        );
        assert_eq!(
            try_convert_url_to_local("https://t.me/c/1234567890?boost"),
            "tg://boost?channel=1234567890"
        );
        assert_eq!(
            try_convert_url_to_local("https://t.me/boost/my_channel"),
            "tg://boost?domain=my_channel"
        );
        assert_eq!(
            try_convert_url_to_local("https://t.me/my_channel?boost"),
            "tg://boost?domain=my_channel"
        );
        assert_eq!(
            try_convert_url_to_local("https://t.me/m/biz_slug-1"),
            "tg://message?slug=biz_slug-1"
        );
        assert_eq!(
            try_convert_url_to_local("https://t.me/nft/PlushPepe-1"),
            "tg://nft?slug=PlushPepe-1"
        );
        assert_eq!(
            try_convert_url_to_local("https://t.me/call/conf_slug_99"),
            "tg://call?slug=conf_slug_99"
        );
        assert_eq!(
            try_convert_url_to_local("https://t.me/getpremium?ref=Promo_1"),
            "internal:tme_getpremium?ref=Promo_1"
        );
        assert_eq!(
            try_convert_url_to_local("https://foundation.ton/docs"),
            "tonsite://foundation.ton/docs"
        );
        // Instant View URL must NOT be turned into `tg://resolve?domain=iv`.
        assert_eq!(
            try_convert_url_to_local("https://t.me/iv?url=https://example.com&rhash=abc"),
            "https://t.me/iv?url=https://example.com&rhash=abc"
        );
    }

    #[test]
    fn parses_resolve_variants_and_special_domains() {
        // Basic username + post + comment + timestamp + text
        let parsed = parse_deep_link(
            "tg://resolve?domain=rustgram_log&post=55&comment=99&t=1h2m3s&text=Hello%20world",
        )
        .expect("resolve");
        let DeepLink::Resolve(info) = parsed else {
            panic!("expected Resolve, got {parsed:?}");
        };
        assert_eq!(info.domain.as_deref(), Some("rustgram_log"));
        assert_eq!(info.post_id, Some(55));
        assert_eq!(info.replies, Some(RepliesByLink::Comment(99)));
        assert_eq!(info.video_timestamp, Some(3723));
        assert_eq!(info.text.as_deref(), Some("Hello world"));

        // Bot start, startgroup + admin, startapp, attach, game, voicechat/livestream, story, boost
        let DeepLink::Resolve(bot_start) =
            parse_deep_link("https://t.me/my_bot?start=payload_1&ref=r42").unwrap()
        else {
            panic!("expected Resolve");
        };
        assert_eq!(bot_start.resolve_type, ResolveType::BotStart);
        assert_eq!(bot_start.start_token.as_deref(), Some("payload_1"));
        assert_eq!(bot_start.referral.as_deref(), Some("r42"));

        let DeepLink::Resolve(bot_group) = parse_deep_link(
            "https://t.me/my_bot?startgroup=grp1&admin=change_info+delete_messages",
        )
        .unwrap() else {
            panic!("expected Resolve");
        };
        assert_eq!(bot_group.resolve_type, ResolveType::AddToGroup);
        assert_eq!(bot_group.start_token.as_deref(), Some("grp1"));
        assert!(
            bot_group
                .start_admin_rights
                .contains(ChatAdminRights::CHANGE_INFO | ChatAdminRights::DELETE_MESSAGES)
        );

        let DeepLink::Resolve(app) =
            parse_deep_link("https://t.me/my_bot/miniapp?startapp=abc&mode=fullscreen").unwrap()
        else {
            panic!("expected Resolve");
        };
        assert_eq!(app.resolve_type, ResolveType::BotApp);
        assert_eq!(app.bot_app_name.as_deref(), Some("miniapp"));
        assert_eq!(app.start_token.as_deref(), Some("abc"));
        assert!(app.bot_app_fullscreen);
        assert!(is_mini_app_url("https://t.me/my_bot/miniapp?startapp=abc"));

        let DeepLink::Resolve(attach) =
            parse_deep_link("https://t.me/my_bot?startattach=go&choose=users+groups").unwrap()
        else {
            panic!("expected Resolve");
        };
        assert_eq!(attach.attach_bot_toggle_command.as_deref(), Some("go"));
        assert!(
            attach
                .attach_bot_choose_types
                .contains(PeerTypes::USER | PeerTypes::GROUP)
        );

        let DeepLink::Resolve(game) = parse_deep_link("https://t.me/game_bot?game=tetris").unwrap()
        else {
            panic!("expected Resolve");
        };
        assert_eq!(game.resolve_type, ResolveType::ShareGame);
        assert_eq!(game.start_token.as_deref(), Some("tetris"));

        let DeepLink::Resolve(vc) =
            parse_deep_link("https://t.me/my_channel?livestream=stream_hash").unwrap()
        else {
            panic!("expected Resolve");
        };
        assert_eq!(vc.voicechat_hash.as_deref(), Some("stream_hash"));

        let DeepLink::Resolve(story) = parse_deep_link("https://t.me/alice/s/77").unwrap() else {
            panic!("expected Resolve");
        };
        assert_eq!(story.story.as_deref(), Some("77"));

        let DeepLink::Resolve(preview) = parse_deep_link("https://t.me/s/my_channel").unwrap()
        else {
            panic!("expected Resolve");
        };
        assert_eq!(preview.domain.as_deref(), Some("my_channel"));
        assert_eq!(preview.resolve_type, ResolveType::Default);

        // Special domains inside `resolve`
        assert_eq!(
            parse_deep_link("https://t.me/giftcode/GIFT_ABC123"),
            Some(DeepLink::GiftCode {
                slug: "GIFT_ABC123".to_owned()
            })
        );
        assert_eq!(
            parse_deep_link("https://t.me/oauth?startapp=tok_xyz"),
            Some(DeepLink::OAuth {
                token: "tok_xyz".to_owned()
            })
        );
    }

    #[test]
    fn parses_all_other_local_handlers_and_preserves_security_guards() {
        // Share URL blocks inline-bot `@` injection.
        assert_eq!(parse_deep_link("tg://msg_url?url=@inlinebot%20query"), None);
        assert_eq!(
            parse_deep_link("https://t.me/share/url?url=https%3A%2F%2Frust-lang.org&text=Nice"),
            Some(DeepLink::ShareUrl {
                url: "https://rust-lang.org".to_owned(),
                text: "Nice".to_owned(),
            })
        );

        // Passport selects `nonce` for JSON scope and `payload` for legacy scope.
        let DeepLink::Passport(p_json) = parse_deep_link(
            "tg://passport?bot_id=42&scope=%7B%22v%22%3A1%7D&public_key=PEM&nonce=N1&payload=P1",
        )
        .unwrap() else {
            panic!("expected Passport");
        };
        assert_eq!(p_json.bot_id, 42);
        assert_eq!(p_json.nonce, "N1");

        let DeepLink::Passport(p_legacy) = parse_deep_link(
            "tg://passport?bot_id=42&scope=id_document&public_key=PEM&nonce=N1&payload=P1",
        )
        .unwrap() else {
            panic!("expected Passport");
        };
        assert_eq!(p_legacy.nonce, "P1");

        // MTProto proxy normalizes `+` -> `-` and `/` -> `_` in secret.
        let DeepLink::Proxy { kind, params } =
            parse_deep_link("tg://proxy?server=1.2.3.4&port=443&secret=ab+cd/ef").unwrap()
        else {
            panic!("expected Proxy");
        };
        assert_eq!(kind, ProxyLinkKind::Mtproto);
        assert_eq!(params.get("secret").map(String::as_str), Some("ab-cd_ef"));

        // Login code, stars_topup clamp, premium_offer, setlanguage, nft, call, iv
        assert_eq!(
            parse_deep_link("tg://login?code=12345"),
            Some(DeepLink::LoginCode {
                code: "12345".to_owned()
            })
        );
        assert_eq!(
            parse_deep_link("tg://stars_topup?balance=99999999&purpose=gift"),
            Some(DeepLink::StarsTopUp {
                amount: 1_000_000,
                purpose: "gift".to_owned(),
            })
        );
        assert_eq!(
            parse_deep_link("tg://premium_offer?ref=banner"),
            Some(DeepLink::PremiumOffer {
                referral: "deeplink_banner".to_owned()
            })
        );
        assert_eq!(
            parse_deep_link("tg://setlanguage"),
            Some(DeepLink::SetLanguage { lang: None })
        );
        assert_eq!(
            parse_deep_link("tg://setlanguage?lang=de"),
            Some(DeepLink::SetLanguage {
                lang: Some("de".to_owned())
            })
        );
        assert_eq!(
            parse_deep_link("https://t.me/iv?url=https://example.com/a&rhash=123#sec2"),
            Some(DeepLink::InstantView {
                url: "https://t.me/iv?url=https://example.com/a&rhash=123".to_owned(),
                fragment: Some("sec2".to_owned()),
            })
        );
    }

    #[test]
    fn router_exact_prefix_and_unsupported_fallback_match_tdesktop() {
        assert_eq!(
            parse_deep_link("tg://settings/privacy"),
            Some(DeepLink::Router(RouterDeepLink::Settings {
                page: SettingsPage::PrivacySecurity,
                section: SettingsSection::PrivacySecurity,
                highlight: None,
            }))
        );
        // Prefix match (`privacy/blocked/extra` -> matches `privacy/blocked`).
        assert!(matches!(
            parse_deep_link("tg://settings/privacy/blocked/unknown-child"),
            Some(DeepLink::Router(RouterDeepLink::Settings {
                section: SettingsSection::PrivacySecurity,
                ..
            }))
        ));
        // Unregistered settings subpath -> Unsupported (never falls through to Unknown).
        assert!(matches!(
            parse_deep_link("tg://settings/nonexistent_mobile_only_section"),
            Some(DeepLink::Unsupported { .. })
        ));
        // Unregistered top-level `tg://` path -> Unknown fallback (`help.getDeepLinkInfo`).
        assert_eq!(
            parse_deep_link("tg://future_feature?x=1"),
            Some(DeepLink::Unknown {
                path: "future_feature".to_owned()
            })
        );
        // Ordinary external URL -> None.
        assert_eq!(parse_deep_link("https://rust-lang.org"), None);
    }

    #[test]
    fn parses_every_remaining_link_form_and_unauthenticated_routes() {
        // Resolve with thread, attach, voicechat, phone, boost, profile, album, collection
        let DeepLink::Resolve(thread_link) =
            parse_deep_link("tg://resolve?domain=forum_group&thread=12&post=88").unwrap()
        else {
            panic!("expected Resolve");
        };
        assert_eq!(thread_link.replies, Some(RepliesByLink::Thread(12)));
        assert_eq!(thread_link.post_id, Some(88));

        let DeepLink::Resolve(attach_link) =
            parse_deep_link("tg://resolve?domain=some_chat&attach=my_bot&startattach=ctx").unwrap()
        else {
            panic!("expected Resolve");
        };
        assert_eq!(attach_link.attach_bot_username.as_deref(), Some("my_bot"));
        assert_eq!(
            attach_link.attach_bot_toggle_command.as_deref(),
            Some("ctx")
        );

        let DeepLink::Resolve(vc_link) =
            parse_deep_link("tg://resolve?domain=voice_group&voicechat=vc_hash").unwrap()
        else {
            panic!("expected Resolve");
        };
        assert_eq!(vc_link.voicechat_hash.as_deref(), Some("vc_hash"));

        let DeepLink::Resolve(phone_link) =
            parse_deep_link("tg://resolve?phone=15550109999&text=hello").unwrap()
        else {
            panic!("expected Resolve");
        };
        assert_eq!(phone_link.phone.as_deref(), Some("15550109999"));
        assert_eq!(phone_link.text.as_deref(), Some("hello"));

        let DeepLink::Resolve(boost_resolve) =
            parse_deep_link("tg://resolve?domain=news_chan&boost").unwrap()
        else {
            panic!("expected Resolve");
        };
        assert_eq!(boost_resolve.resolve_type, ResolveType::Boost);

        // joinchat / +invite, addlist, addstickers, addemoji, addtheme, bg
        assert_eq!(
            parse_deep_link("https://t.me/joinchat/Inv_01"),
            Some(DeepLink::JoinChat {
                hash: "Inv_01".to_owned()
            })
        );
        assert_eq!(
            parse_deep_link("https://t.me/+Inv_02"),
            Some(DeepLink::JoinChat {
                hash: "Inv_02".to_owned()
            })
        );
        assert_eq!(
            parse_deep_link("tg://addlist?slug=dev_folder"),
            Some(DeepLink::AddList {
                slug: "dev_folder".to_owned()
            })
        );
        assert_eq!(
            parse_deep_link("tg://addstickers?set=CatsPack"),
            Some(DeepLink::AddStickers {
                short_name: "CatsPack".to_owned(),
                is_emoji: false
            })
        );
        assert_eq!(
            parse_deep_link("tg://addemoji?set=IconsPack"),
            Some(DeepLink::AddStickers {
                short_name: "IconsPack".to_owned(),
                is_emoji: true
            })
        );
        assert_eq!(
            parse_deep_link("tg://addtheme?slug=NordDark"),
            Some(DeepLink::AddTheme {
                slug: "NordDark".to_owned()
            })
        );
        let DeepLink::WallPaper(bg_color) = parse_deep_link("https://t.me/bg/1a2b3c").unwrap()
        else {
            panic!("expected WallPaper");
        };
        assert_eq!(bg_color.slug_or_color, "1a2b3c");
        assert!(bg_color.is_color_or_gradient);

        let DeepLink::WallPaper(bg_slug) =
            parse_deep_link("https://t.me/bg/forest_wp?mode=blur").unwrap()
        else {
            panic!("expected WallPaper");
        };
        assert_eq!(bg_slug.slug_or_color, "forest_wp");
        assert!(!bg_slug.is_color_or_gradient);

        // privatepost, confirmphone, invoice, nft, call, socks, boost, internal, tonsite
        let DeepLink::PrivatePost(pp) = parse_deep_link("https://t.me/c/987654/7/42").unwrap()
        else {
            panic!("expected PrivatePost");
        };
        assert_eq!(pp.channel_id, 987654);
        assert_eq!(pp.replies, Some(RepliesByLink::Thread(7)));
        assert_eq!(pp.post_id, Some(42));

        assert_eq!(
            parse_deep_link("tg://confirmphone?phone=12345&hash=h99"),
            Some(DeepLink::ConfirmPhone {
                phone: "12345".to_owned(),
                hash: "h99".to_owned(),
            })
        );
        assert_eq!(
            parse_deep_link("https://t.me/$slug_99"),
            Some(DeepLink::Invoice {
                slug: "slug_99".to_owned()
            })
        );
        assert_eq!(
            parse_deep_link("https://t.me/nft/StarGift-7"),
            Some(DeepLink::UniqueGift {
                slug: "StarGift-7".to_owned()
            })
        );
        assert_eq!(
            parse_deep_link("https://t.me/call/room_xyz"),
            Some(DeepLink::ConferenceCall {
                slug: "room_xyz".to_owned()
            })
        );
        assert_eq!(
            parse_deep_link("https://t.me/boost/my_chan"),
            Some(DeepLink::Boost {
                domain: Some("my_chan".to_owned()),
                channel_id: None,
            })
        );
        assert_eq!(
            parse_deep_link("https://example.ton/page"),
            Some(DeepLink::TonSite {
                uri: "tonsite://example.ton/page".to_owned()
            })
        );
        assert!(matches!(
            parse_deep_link("internal:copy:12345"),
            Some(DeepLink::Internal(InternalDeepLink::CopyText(s))) if s == "12345"
        ));

        // Unauthenticated dispatch: proxy routes to AddProxy box, login captures code,
        // unsupported settings subpath displays unsupported toast.
        assert!(matches!(
            dispatch_unauthenticated_deep_link(
                "tg://socks?server=127.0.0.1&port=1080&user=u&pass=p"
            ),
            DeepLinkOutcome::ShowBox(b) if matches!(&*b, BoxKind::AddProxy(p) if p.is_valid())
        ));
        assert!(matches!(
            dispatch_unauthenticated_deep_link("tg://login?code=77711"),
            DeepLinkOutcome::LoginCode(c) if c == "77711"
        ));
        assert!(matches!(
            dispatch_unauthenticated_deep_link("tg://settings/nonexistent"),
            DeepLinkOutcome::Toast(m) if m == UNSUPPORTED_LINK_MESSAGE
        ));
    }
}
