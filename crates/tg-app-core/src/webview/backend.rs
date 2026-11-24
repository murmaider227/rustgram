//! Backend trait and host callback types for Instant View and Bot Mini Apps.
//!
//! Port origin: `Telegram/SourceFiles/iv/iv_instance.cpp` and
//! `Telegram/SourceFiles/inline_bots/bot_attach_web_view.cpp`.

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use serde_json::Value;
use tg_bots::protocol::{BotAppLookupResult, PopularAppBotsPage, RequestedWebViewButton};
use tg_iv::{MediaKind, MediaProvider, Page};
use tg_webview::{InlineChatType, SchemeRouter};

use crate::stand_in::types::PeerId;

/// Error returned by [`WebViewBackend`] operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebViewBackendError {
    /// Machine-readable error code (e.g. `"WEBPAGE_NOT_FOUND"`, `"BOT_INVALID"`).
    pub code: String,
    /// Human-readable diagnostic message.
    pub message: String,
}

impl WebViewBackendError {
    /// Creates a new backend error.
    #[must_use]
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for WebViewBackendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for WebViewBackendError {}

/// Resolved Instant View page model paired with its media store.
pub struct IvPageBundle {
    /// Parsed Telegram Instant View [`Page`].
    pub page: Page,
    /// Cached/downloadable media store for photos, documents, and map tiles.
    pub media: Arc<dyn MediaProvider + Send + Sync>,
}

/// Metadata describing a bot hosting a Telegram Mini App.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BotInfo {
    /// Bot user ID.
    pub id: u64,
    /// Bot username without leading `@`.
    pub username: String,
    /// Display title shown in the Mini App header.
    pub title: String,
    /// Whether to display a verified badge next to the title.
    pub verified: bool,
    /// Whether the bot is installed in the chat attachment menu.
    pub in_attach_menu: bool,
    /// Whether the bot is pinned in the side/main menu.
    pub in_main_menu: bool,
    /// Whether this bot may invoke `web_app_read_text_from_clipboard` after user interaction.
    pub allow_clipboard_read: bool,
    /// Whether this web view hosts an HTML5 game rather than a regular Mini App.
    pub is_game: bool,
}

/// Parameters for requesting a bot Mini App launch URL (`messages.requestWebView` / `requestAppWebView`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebViewRequest {
    /// Target bot user ID.
    pub bot_id: u64,
    /// Optional `start_param` passed to the Mini App.
    pub start_param: Option<String>,
    /// Optional explicit URL (used for inline keyboard `web_app` buttons).
    pub url_override: Option<String>,
    /// Whether fullscreen mode was requested up front.
    pub fullscreen: bool,
    /// Compact JSON of active `tg-theme` palette parameters (`theme_params`).
    pub theme_params_json: String,
}

/// Launch descriptor returned by [`WebViewBackend::request_web_view`].
#[derive(Clone)]
pub struct WebViewLaunchInfo {
    /// Bot metadata for header/menu presentation.
    pub bot: BotInfo,
    /// Resolved WebView URL (HTTPS or `rustgram-internal://...` for local mock apps).
    pub url: String,
    /// Server query ID for `messages.prolongWebView` (`0` for simple/local views).
    pub query_id: u64,
    /// Whether to open directly in fullscreen mode.
    pub fullscreen: bool,
    /// Whether incoming bridge messages must match `url`'s origin.
    pub same_origin: bool,
    /// Optional custom-scheme router for local Mini App assets (`rustgram-internal://`).
    pub local_routes: Option<Arc<SchemeRouter>>,
}

impl fmt::Debug for WebViewLaunchInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WebViewLaunchInfo")
            .field("bot", &self.bot)
            .field("url", &self.url)
            .field("query_id", &self.query_id)
            .field("fullscreen", &self.fullscreen)
            .field("same_origin", &self.same_origin)
            .field("has_local_routes", &self.local_routes.is_some())
            .finish()
    }
}

/// Resolved prepared inline message returned by `messages.getPreparedInlineMessage`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedInlineMessageInfo {
    /// Original prepared inline message identifier.
    pub prepared_id: String,
    /// Inline query ID to pass into `messages.sendInlineBotResult`.
    pub query_id: i64,
    /// Inline result ID (`BotInlineResult::id`) to pass into `messages.sendInlineBotResult`.
    pub result_id: String,
    /// Human-readable preview heading / title.
    pub preview_title: String,
    /// Human-readable message text preview shown in `ConfirmPreparedMessage`.
    pub preview_text: String,
    /// Chat types permitted by the server for sharing this prepared message.
    pub allowed_peer_types: Vec<InlineChatType>,
}

/// Data and RPC abstraction backing both Instant View and Bot Mini App windows.
///
/// Production code will implement this over `tg-api`; tests and offline previews use
/// [`crate::webview::mock::MockWebViewBackend`].
pub trait WebViewBackend {
    /// Fetches an Instant View page for `url` (and optional known `webpage_id`).
    ///
    /// Returns `Ok(None)` when the URL has no Instant View representation and should fall back
    /// to opening in the system browser.
    fn fetch_iv_page(
        &self,
        url: &str,
        webpage_id: Option<i64>,
    ) -> Result<Option<IvPageBundle>, WebViewBackendError>;

    /// Requests a Mini App launch URL and metadata for a bot.
    fn request_web_view(
        &mut self,
        request: WebViewRequest,
    ) -> Result<WebViewLaunchInfo, WebViewBackendError>;

    /// Sends Mini App service data (`messages.sendWebViewData`) back to the bot.
    fn send_web_app_data(
        &mut self,
        bot_id: u64,
        button_text: &str,
        data: &str,
    ) -> Result<(), WebViewBackendError>;

    /// Invokes a Bot WebView custom method (`bots.invokeWebViewCustomMethod`).
    fn invoke_custom_method(
        &mut self,
        bot_id: u64,
        method: &str,
        params: Value,
    ) -> Result<Value, String>;

    /// Joins a channel from an Instant View channel banner (`channels.joinChannel`).
    fn join_channel(&mut self, channel_id: i64) -> Result<(), WebViewBackendError>;

    /// Removes a bot from the attachment/side menu (`messages.toggleBotInAttachMenu(enabled=false)`).
    fn remove_bot_from_menu(&mut self, bot_id: u64) -> Result<(), WebViewBackendError>;

    /// Fetches a prepared inline message preview (`messages.getPreparedInlineMessage`).
    fn get_prepared_inline_message(
        &mut self,
        bot_id: u64,
        prepared_id: &str,
    ) -> Result<PreparedInlineMessageInfo, WebViewBackendError>;

    /// Sends a confirmed prepared inline message (`messages.sendInlineBotResult`).
    fn send_prepared_inline_message(
        &mut self,
        bot_id: u64,
        peer_id: PeerId,
        query_id: i64,
        result_id: &str,
    ) -> Result<(), WebViewBackendError>;

    /// Chats the user may pick as the recipient of a prepared inline message from `bot_id`,
    /// filtered by `allowed` (empty = no restriction), as `(peer, display title)` pairs
    /// (`Window::PrepareChooseRecipientBox` with `typesRestriction`).
    fn prepared_message_recipients(
        &self,
        _bot_id: u64,
        _allowed: &[InlineChatType],
    ) -> Vec<(PeerId, String)> {
        Vec::new()
    }

    /// Optional per-account directory inside `tdata` where per-bot `BotLocalStorage` files are persisted.
    fn bot_storage_dir(&self) -> Option<PathBuf> {
        None
    }

    /// Fetches or validates a named Mini App descriptor via `messages.getBotApp`.
    ///
    /// Port of `WebViewInstance::resolveApp` (`inline_bots/bot_attach_web_view.cpp:1096-1146`).
    fn get_bot_app(
        &mut self,
        _bot_id: u64,
        _short_name: &str,
        _cached_hash: i64,
    ) -> Result<BotAppLookupResult, WebViewBackendError> {
        Err(WebViewBackendError::new(
            "UNSUPPORTED",
            "get_bot_app is not supported by this backend",
        ))
    }

    /// Requests a chat-join verification Mini App via `messages.requestChatJoinWebView`.
    ///
    /// Port of `WebViewInstance::requestChatJoin` (`inline_bots/bot_attach_web_view.cpp:1369-1387`).
    fn request_chat_join_web_view(
        &mut self,
        _bot_id: u64,
        _query_id: u64,
        _theme_params_json: Option<&str>,
    ) -> Result<WebViewLaunchInfo, WebViewBackendError> {
        Err(WebViewBackendError::new(
            "UNSUPPORTED",
            "request_chat_join_web_view is not supported by this backend",
        ))
    }

    /// Checks whether a bot is permitted to message the current user (`bots.canSendMessage`).
    ///
    /// Port of `WebViewInstance::botCheckWriteAccess` (`bot_attach_web_view.cpp:1891-1899`).
    fn can_send_message(&mut self, _bot_id: u64) -> Result<bool, WebViewBackendError> {
        Ok(false)
    }

    /// Grants permission for a bot to message the current user (`bots.allowSendMessage`)
    /// and applies returned `Updates`.
    ///
    /// Port of `WebViewInstance::botAllowWriteAccess` (`bot_attach_web_view.cpp:1901-1910`).
    fn allow_send_message(&mut self, _bot_id: u64) -> Result<bool, WebViewBackendError> {
        Ok(false)
    }

    /// Validates Mini App file download parameters with Telegram (`bots.checkDownloadFileParams`).
    ///
    /// Port of `WebViewInstance::botDownloadFile` (`bot_attach_web_view.cpp:2297-2311`).
    fn check_download_file_params(
        &mut self,
        _bot_id: u64,
        _file_name: &str,
        _url: &str,
    ) -> Result<bool, WebViewBackendError> {
        Ok(false)
    }

    /// Fetches popular Mini App bots (`bots.getPopularAppBots`).
    ///
    /// Port of `AttachWebView::loadPopularAppBots` (`bot_attach_web_view.cpp:2660-2683`).
    fn get_popular_app_bots(
        &mut self,
        _offset: &str,
        _limit: i32,
    ) -> Result<PopularAppBotsPage, WebViewBackendError> {
        Ok(PopularAppBotsPage::default())
    }

    /// Fetches the keyboard button descriptor for `web_app_request_chat`
    /// (`bots.getRequestedWebViewButton`).
    ///
    /// Port of `WebViewInstance::botRequestChat` (`bot_attach_web_view.cpp:2144-2228`).
    fn get_requested_web_view_button(
        &mut self,
        _bot_id: u64,
        _webapp_req_id: &str,
    ) -> Result<RequestedWebViewButton, WebViewBackendError> {
        Err(WebViewBackendError::new(
            "UNSUPPORTED",
            "get_requested_web_view_button is not supported by this backend",
        ))
    }

    /// Enables or disables a bot's permission to update the user's custom emoji status
    /// (`bots.toggleUserEmojiStatusPermission`).
    ///
    /// Port of `WebViewInstance::botRequestEmojiStatusAccess` (`bot_attach_web_view.cpp:1926-1951`).
    fn toggle_user_emoji_status_permission(
        &mut self,
        _bot_id: u64,
        _enabled: bool,
    ) -> Result<bool, WebViewBackendError> {
        Ok(false)
    }
}

/// Events emitted by Instant View and Mini App windows back to the hosting application (`tg-app`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppWebViewEvent {
    /// Route a `tg://` or `https://t.me/...` link back to the main app router.
    OpenTelegramLink {
        /// Telegram URI.
        uri: String,
    },
    /// Open a photo, video, audio, or document in the app's native media viewer.
    OpenMediaViewer {
        /// Media kind.
        kind: MediaKind,
        /// Telegram photo or document ID.
        id: i64,
    },
    /// Open a Telegram channel chat from an Instant View channel banner.
    OpenChannel {
        /// Channel ID.
        channel_id: i64,
        /// Optional public username.
        username: Option<String>,
    },
    /// Open an Instant View page (e.g. requested from a Mini App with `try_instant_view`).
    OpenInstantView {
        /// Target URL.
        url: String,
        /// Optional known webpage ID.
        webpage_id: Option<i64>,
        /// Optional anchor fragment.
        fragment: Option<String>,
    },
    /// Open a TON site URI.
    OpenTonSite {
        /// Original `tonsite://` URI.
        uri: String,
        /// Gateway `https://*.magic.org` URL.
        https_gateway_url: String,
    },
    /// Open an external URL in the user's system browser.
    OpenExternalUrl {
        /// Validated URL.
        url: String,
    },
    /// Open the app's share dialog for a URL or message.
    ShareLink {
        /// URL to share.
        url: String,
        /// Optional title or caption.
        title: String,
    },
    /// Copy text to the OS clipboard.
    CopyToClipboard {
        /// Text copied.
        text: String,
    },
    /// Mini App sent `web_app_data_send` payload to the bot.
    BotDataSent {
        /// Bot user ID.
        bot_id: u64,
        /// Sent UTF-8 payload.
        data: String,
    },
    /// User confirmed and sent a prepared inline message from a Mini App.
    PreparedMessageSent {
        /// Bot user ID.
        bot_id: u64,
        /// Prepared message identifier.
        prepared_id: String,
        /// Server inline query ID.
        query_id: i64,
    },
    /// User removed a bot from the attachment/side menu.
    BotRemovedFromMenu {
        /// Bot user ID.
        bot_id: u64,
    },
    /// Mini App requested switching to inline query mode.
    SwitchInlineQuery {
        /// Bot user ID.
        bot_id: u64,
        /// Allowed chat types.
        chat_types: Vec<InlineChatType>,
        /// Inline query text.
        query: String,
    },
    /// Mini App requested opening a Telegram payment invoice.
    OpenInvoice {
        /// Invoice slug.
        slug: String,
    },
    /// Display a transient status toast in the application.
    Toast {
        /// Toast message text.
        message: String,
    },
    /// Quit the application (`Ctrl+Q` from Instant View).
    QuitApp,
}

/// Callback invoked when Instant View or a Mini App hands off a `tg://` / `t.me` link.
pub type OpenTgLinkCallback = Box<dyn FnMut(&str)>;

/// Callback invoked when an Instant View media element is clicked to open the media viewer.
pub type OpenMediaCallback = Box<dyn FnMut(MediaKind, i64)>;

/// Callback invoked when an Instant View channel banner is clicked.
pub type OpenChannelCallback = Box<dyn FnMut(i64, Option<&str>)>;

/// Callback invoked when the user triggers Share from Instant View or Mini App.
pub type ShareLinkCallback = Box<dyn FnMut(&str, &str)>;

/// Callback invoked when a toast notification should be displayed.
pub type ToastCallback = Box<dyn FnMut(&str)>;

/// Optional host callbacks registered by the enclosing application shell.
#[derive(Default)]
pub struct AppWebViewCallbacks {
    /// Invoked when Instant View or a Mini App hands off a `tg://` / `t.me` link.
    pub on_open_tg_link: Option<OpenTgLinkCallback>,
    /// Invoked when an Instant View media element is clicked to launch the media viewer.
    pub on_open_media: Option<OpenMediaCallback>,
    /// Invoked when an Instant View channel banner is clicked to open the channel.
    pub on_open_channel: Option<OpenChannelCallback>,
    /// Invoked when the user triggers Share from Instant View or Mini App.
    pub on_share_link: Option<ShareLinkCallback>,
    /// Invoked when a toast notification should be displayed.
    pub on_toast: Option<ToastCallback>,
}

impl AppWebViewCallbacks {
    /// Dispatches an [`AppWebViewEvent`] to any registered callback closure.
    pub fn dispatch(&mut self, event: &AppWebViewEvent) {
        match event {
            AppWebViewEvent::OpenTelegramLink { uri } => {
                if let Some(cb) = &mut self.on_open_tg_link {
                    cb(uri);
                }
            }
            AppWebViewEvent::OpenMediaViewer { kind, id } => {
                if let Some(cb) = &mut self.on_open_media {
                    cb(*kind, *id);
                }
            }
            AppWebViewEvent::OpenChannel {
                channel_id,
                username,
            } => {
                if let Some(cb) = &mut self.on_open_channel {
                    cb(*channel_id, username.as_deref());
                }
            }
            AppWebViewEvent::ShareLink { url, title } => {
                if let Some(cb) = &mut self.on_share_link {
                    cb(url, title);
                }
            }
            AppWebViewEvent::Toast { message } => {
                if let Some(cb) = &mut self.on_toast {
                    cb(message);
                }
            }
            _ => {}
        }
    }
}

/// Abstraction over OS browser / mailto launching (backed by `tg-platform`, mockable in tests).
pub trait ExternalOpener {
    /// Opens an external HTTP/HTTPS/tel URL in the default OS handler.
    fn open_url(&self, url: &str) -> Result<(), String>;

    /// Opens a `mailto:` URL in the default mail client.
    fn open_mailto(&self, address: &str) -> Result<(), String> {
        let uri = if address.to_ascii_lowercase().starts_with("mailto:") {
            address.to_owned()
        } else {
            format!("mailto:{address}")
        };
        self.open_url(&uri)
    }
}

/// Production [`ExternalOpener`] delegating to `tg_platform::system_info`.
#[derive(Debug, Default, Clone, Copy)]
pub struct PlatformExternalOpener;

impl ExternalOpener for PlatformExternalOpener {
    fn open_url(&self, url: &str) -> Result<(), String> {
        tg_platform::system_info::open_url(url).map_err(|e| e.to_string())
    }

    fn open_mailto(&self, address: &str) -> Result<(), String> {
        tg_platform::system_info::open_mailto(address).map_err(|e| e.to_string())
    }
}

/// Abstraction over OS clipboard text read/write (backed by Win32, mockable in tests).
pub trait ClipboardAccess {
    /// Reads plain text from the clipboard.
    fn read_text(&self) -> Option<String>;

    /// Writes plain text to the clipboard, returning `true` on success.
    fn write_text(&self, text: &str) -> bool;
}
