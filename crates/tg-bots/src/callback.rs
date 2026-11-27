//! Port of bot callback query flow, button activation dispatch, switch-inline flow,
//! and URL authorization (`messages.requestUrlAuth` / `messages.acceptUrlAuth`) from
//! `api/api_bot.cpp`, `boxes/url_auth_box.cpp`, and `core/click_handler_types.cpp`.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::deep_links::try_convert_url_to_local;
use crate::markup::{
    ButtonKind, CreateBotRequest, MarkupButton, PeerTypes, PollRequestRestriction, RequestPeerQuery,
};
use crate::stand_in::{FullMsgId, MsgId, PeerId, UserId};

/// Message and bot context required to decide what activating a [`MarkupButton`] should do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ButtonActivationContext {
    /// Owning message id (`peer` + `msg`).
    pub item_id: FullMsgId,
    /// Optional forum topic root message id.
    pub topic_root_id: Option<MsgId>,
    /// Bot user id associated with the message (`item->getMessageBot()`), if any.
    pub bot_id: Option<UserId>,
    /// Whether the bot is verified (`bot->isVerified()`).
    pub bot_verified: bool,
    /// Peer id to return to after a `SwitchInline` button press (`botInfo->inlineReturnTo()`).
    pub bot_inline_return_to: Option<PeerId>,
    /// Whether the message is a regular server message (`item->isRegular()`).
    pub is_regular: bool,
    /// Optional ephemeral message id when the message is ephemeral.
    pub ephemeral_id: Option<i32>,
    /// Whether this button already has an in-flight RPC (`button->requestId != 0`).
    pub request_in_flight: bool,
}

/// Request parameters for `messages.getBotCallbackAnswer` or `ephemeral.getCallbackAnswer`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallbackQueryRequest {
    /// Target peer id.
    pub peer_id: PeerId,
    /// Message id.
    pub msg_id: MsgId,
    /// `true` for `ButtonKind::Game`.
    pub is_game: bool,
    /// Optional callback payload bytes (`f_data`).
    pub data: Option<Vec<u8>>,
    /// `true` for `ButtonKind::CallbackWithPassword` (requires 2FA SRP confirmation).
    pub requires_password: bool,
    /// Present when this callback targets an ephemeral message (`ephemeral.getCallbackAnswer`).
    pub ephemeral_id: Option<i32>,
}

/// Switch-inline routing decision matching `api_bot.cpp:465-505`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SwitchInlineDecision {
    /// Insert `@bot query` into the current chat, replying to `reply_to`.
    SwitchSamePeer {
        /// Target bot user id.
        bot_id: UserId,
        /// Pre-filled inline query string.
        query: String,
        /// Message id to reply to.
        reply_to: MsgId,
    },
    /// Return to the saved `inlineReturnTo` chat (e.g. after switching to PM via `switch_pm`).
    ReturnToSavedPeer {
        /// Target bot user id.
        bot_id: UserId,
        /// Pre-filled inline query string.
        query: String,
        /// Saved peer to return to.
        return_peer: PeerId,
    },
    /// Open the recipient chooser filtered by `peer_types`.
    ChooseRecipient {
        /// Target bot user id.
        bot_id: UserId,
        /// Pre-filled inline query string.
        query: String,
        /// Allowed peer types for the chooser (`PeerTypes::empty()` means any allowed chat).
        peer_types: PeerTypes,
    },
}

/// Action to execute when a user activates a [`MarkupButton`].
///
/// Port of `Api::ActivateBotButton` (`api/api_bot.cpp:313-628`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BotButtonAction {
    /// No action (e.g. disabled button, missing bot, in-flight request, or invalid message state).
    NoOp,
    /// Send a plain text command message (for `ButtonKind::Default`).
    SendCommand {
        /// Destination peer.
        peer_id: PeerId,
        /// Command / button text to send.
        command: String,
        /// Context message id.
        context: FullMsgId,
        /// Reply-to message id when the source message is regular.
        reply_to: Option<FullMsgId>,
    },
    /// Send a callback query (`messages.getBotCallbackAnswer` / `ephemeral.getCallbackAnswer`).
    SendCallback(CallbackQueryRequest),
    /// Launch payment checkout (`ButtonKind::Buy`).
    OpenCheckout {
        /// Invoice message id.
        item_id: FullMsgId,
    },
    /// Open a URL (`ButtonKind::Url`).
    ///
    /// Confirmation is skipped only when the bot is verified (`api_bot.cpp:365-376`).
    OpenUrl {
        /// Target URL.
        url: String,
        /// Whether to ask user confirmation (`HiddenUrlClickHandler`) before opening.
        require_confirmation: bool,
    },
    /// Inform the user that sharing location is unavailable on desktop; hides single-use keyboard.
    ShowLocationUnavailable {
        /// Whether to dismiss a single-use reply keyboard.
        hide_single_use: bool,
    },
    /// Ask confirmation to share the user's phone number contact.
    ConfirmSharePhone {
        /// Owning message id.
        item_id: FullMsgId,
        /// Optional topic root id.
        topic_root_id: Option<MsgId>,
        /// Whether to dismiss a single-use reply keyboard.
        hide_single_use: bool,
    },
    /// Open poll creation dialog with optional quiz restriction.
    CreatePoll {
        /// Poll restriction (`Any`, `RegularOnly`, `QuizOnly`).
        restriction: PollRequestRestriction,
        /// Whether to dismiss a single-use reply keyboard.
        hide_single_use: bool,
    },
    /// Open peer chooser matching `query` (`messages.sendBotRequestedPeer`).
    ChoosePeer {
        /// Requesting bot user id.
        bot_id: UserId,
        /// Owning message id.
        item_id: FullMsgId,
        /// Button id echoed back to the server.
        button_id: i64,
        /// Peer selection criteria.
        query: RequestPeerQuery,
        /// Whether to dismiss a single-use reply keyboard.
        hide_single_use: bool,
    },
    /// Open managed bot creation flow (`ButtonKind::CreateBot`).
    CreateManagedBot {
        /// Manager bot user id.
        manager_bot_id: UserId,
        /// Owning message id.
        item_id: FullMsgId,
        /// Button id echoed back to the server.
        button_id: i64,
        /// Suggested bot name/username.
        request: CreateBotRequest,
        /// Whether to dismiss a single-use reply keyboard.
        hide_single_use: bool,
    },
    /// Switch to inline bot query mode.
    SwitchInline(SwitchInlineDecision),
    /// Start URL login / OAuth authorization (`messages.requestUrlAuth`).
    RequestUrlAuth(UrlAuthSource),
    /// Open a user's profile page.
    OpenUserProfile {
        /// Target user id.
        user_id: UserId,
    },
    /// Open a Mini App WebView (`WebView` or `SimpleWebView`).
    OpenWebView {
        /// Target bot user id.
        bot_id: UserId,
        /// Button label.
        text: String,
        /// Target WebView URL.
        url: String,
        /// `true` for `SimpleWebView` (`messages.requestSimpleWebView`), `false` for `WebView`.
        simple: bool,
    },
    /// Copy text to the system clipboard and show a copied toast.
    CopyText {
        /// Text to copy.
        text: String,
    },
    /// Accept a suggested channel post.
    SuggestAccept {
        /// Suggested post message id.
        item_id: FullMsgId,
    },
    /// Decline a suggested channel post.
    SuggestDecline {
        /// Suggested post message id.
        item_id: FullMsgId,
    },
    /// Suggest edits to a suggested channel post.
    SuggestChange {
        /// Suggested post message id.
        item_id: FullMsgId,
    },
}

/// Evaluates the action for activating `button` in `ctx`.
///
/// Port of `Api::ActivateBotButton` and `SendBotCallbackData` guards in `api/api_bot.cpp`.
pub fn activate_button(button: &MarkupButton, ctx: &ButtonActivationContext) -> BotButtonAction {
    match &button.kind {
        ButtonKind::Default => BotButtonAction::SendCommand {
            peer_id: ctx.item_id.peer,
            command: button.text.clone(),
            context: ctx.item_id,
            reply_to: if ctx.is_regular {
                Some(ctx.item_id)
            } else {
                None
            },
        },
        ButtonKind::Callback { data } => {
            build_callback_request(ctx, false, Some(data.clone()), false)
                .map_or(BotButtonAction::NoOp, BotButtonAction::SendCallback)
        }
        ButtonKind::CallbackWithPassword { data } => {
            if !ctx.is_regular {
                return BotButtonAction::NoOp;
            }
            build_callback_request(ctx, false, Some(data.clone()), true)
                .map_or(BotButtonAction::NoOp, BotButtonAction::SendCallback)
        }
        ButtonKind::Game => build_callback_request(ctx, true, None, false)
            .map_or(BotButtonAction::NoOp, BotButtonAction::SendCallback),
        ButtonKind::Buy => BotButtonAction::OpenCheckout {
            item_id: ctx.item_id,
        },
        ButtonKind::Url { url } => BotButtonAction::OpenUrl {
            url: url.clone(),
            require_confirmation: !ctx.bot_verified,
        },
        ButtonKind::RequestLocation => BotButtonAction::ShowLocationUnavailable {
            hide_single_use: true,
        },
        ButtonKind::RequestPhone => BotButtonAction::ConfirmSharePhone {
            item_id: ctx.item_id,
            topic_root_id: ctx.topic_root_id,
            hide_single_use: true,
        },
        ButtonKind::RequestPoll { restriction } => BotButtonAction::CreatePoll {
            restriction: *restriction,
            hide_single_use: true,
        },
        ButtonKind::RequestPeer { button_id, query } => match ctx.bot_id {
            Some(bot_id) => BotButtonAction::ChoosePeer {
                bot_id,
                item_id: ctx.item_id,
                button_id: *button_id,
                query: *query,
                hide_single_use: true,
            },
            None => BotButtonAction::NoOp,
        },
        ButtonKind::CreateBot { button_id, request } => match ctx.bot_id {
            Some(manager_bot_id) => BotButtonAction::CreateManagedBot {
                manager_bot_id,
                item_id: ctx.item_id,
                button_id: *button_id,
                request: request.clone(),
                hide_single_use: true,
            },
            None => BotButtonAction::NoOp,
        },
        ButtonKind::SwitchInlineSame { query } => match ctx.bot_id {
            Some(bot_id) => BotButtonAction::SwitchInline(SwitchInlineDecision::SwitchSamePeer {
                bot_id,
                query: query.clone(),
                reply_to: ctx.item_id.msg,
            }),
            None => BotButtonAction::NoOp,
        },
        ButtonKind::SwitchInline { query, peer_types } => match ctx.bot_id {
            Some(bot_id) => {
                if let Some(return_peer) = ctx.bot_inline_return_to {
                    BotButtonAction::SwitchInline(SwitchInlineDecision::ReturnToSavedPeer {
                        bot_id,
                        query: query.clone(),
                        return_peer,
                    })
                } else {
                    BotButtonAction::SwitchInline(SwitchInlineDecision::ChooseRecipient {
                        bot_id,
                        query: query.clone(),
                        peer_types: *peer_types,
                    })
                }
            }
            None => BotButtonAction::NoOp,
        },
        ButtonKind::Auth { url, button_id, .. } => {
            if !ctx.is_regular || ctx.request_in_flight {
                return BotButtonAction::NoOp;
            }
            BotButtonAction::RequestUrlAuth(UrlAuthSource::Button {
                peer_id: ctx.item_id.peer,
                msg_id: ctx.item_id.msg,
                button_id: *button_id,
                url: url.clone(),
            })
        }
        ButtonKind::UserProfile { user_id } => {
            BotButtonAction::OpenUserProfile { user_id: *user_id }
        }
        ButtonKind::WebView { url } => match ctx.bot_id {
            Some(bot_id) => BotButtonAction::OpenWebView {
                bot_id,
                text: button.text.clone(),
                url: url.clone(),
                simple: false,
            },
            None => BotButtonAction::NoOp,
        },
        ButtonKind::SimpleWebView { url } => match ctx.bot_id {
            Some(bot_id) => BotButtonAction::OpenWebView {
                bot_id,
                text: button.text.clone(),
                url: url.clone(),
                simple: true,
            },
            None => BotButtonAction::NoOp,
        },
        ButtonKind::CopyText { text } => {
            if text.is_empty() {
                BotButtonAction::NoOp
            } else {
                BotButtonAction::CopyText { text: text.clone() }
            }
        }
        ButtonKind::Disabled => BotButtonAction::NoOp,
        ButtonKind::SuggestAccept => BotButtonAction::SuggestAccept {
            item_id: ctx.item_id,
        },
        ButtonKind::SuggestDecline => BotButtonAction::SuggestDecline {
            item_id: ctx.item_id,
        },
        ButtonKind::SuggestChange => BotButtonAction::SuggestChange {
            item_id: ctx.item_id,
        },
    }
}

fn build_callback_request(
    ctx: &ButtonActivationContext,
    is_game: bool,
    data: Option<Vec<u8>>,
    requires_password: bool,
) -> Option<CallbackQueryRequest> {
    if ctx.request_in_flight {
        return None;
    }
    let is_ephemeral = ctx.ephemeral_id.is_some();
    if !ctx.is_regular && !is_ephemeral {
        return None;
    }
    if is_ephemeral && (is_game || requires_password) {
        return None;
    }
    Some(CallbackQueryRequest {
        peer_id: ctx.item_id.peer,
        msg_id: ctx.item_id.msg,
        is_game,
        data,
        requires_password,
        ephemeral_id: ctx.ephemeral_id,
    })
}

/// Plain representation of TL `messages.botCallbackAnswer` (`schema/api.tl:861`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CallbackAnswerInput {
    /// If `true`, show `message` as a modal alert box instead of a transient toast.
    pub alert: bool,
    /// `has_url` flag from the server response.
    pub has_url: bool,
    /// `native_ui` flag from the server response.
    pub native_ui: bool,
    /// Optional alert/toast message text.
    pub message: Option<String>,
    /// Optional URL (deep link or game URL) to open.
    pub url: Option<String>,
    /// Client-side cache duration in seconds (`cache_time`).
    pub cache_time: i32,
}

/// Action resulting from a `messages.botCallbackAnswer` response.
///
/// Port of `handleDone` in `api/api_bot.cpp:105-156` and `BotGameUrlClickHandler::onClick`
/// in `core/click_handler_types.cpp:668-726`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CallbackAnswerAction {
    /// Show a modal alert dialog (`Ui::MakeInformBox`).
    ShowAlert {
        /// Alert message text.
        message: String,
    },
    /// Show a transient toast notification (`showToast`).
    ShowToast {
        /// Toast text.
        message: String,
        /// When `true`, close the password confirmation layer first.
        hide_password_layer: bool,
    },
    /// Open a non-game URL directly (`UrlClickHandler::Open`).
    OpenUrl {
        /// Target URL.
        url: String,
    },
    /// Open a game URL in a WebView (or external handler if local `tg://`),
    /// sending `SendProgressType::PlayGame`.
    OpenGameUrl {
        /// Target game URL (converted to local `tg://` if applicable).
        url: String,
        /// `true` if the URL resolved to an internal `tg://` link rather than an HTTP game page.
        is_local_link: bool,
        /// `true` when the bot is neither verified nor previously trusted to open games.
        require_confirmation: bool,
        /// Always `true` for game callbacks (`Api::SendProgressType::PlayGame`).
        send_play_game_progress: bool,
    },
    /// Close the 2FA password layer with no alert/toast/url.
    HidePasswordLayer,
    /// Nothing to display.
    None,
}

/// Decides how to handle a [`CallbackAnswerInput`] response.
///
/// Matches tdesktop's exact precedence (`api/api_bot.cpp:127-155`):
/// 1. Non-empty `message` takes precedence over `url` (alert if `alert`, else toast).
/// 2. Otherwise non-empty `url` opens directly (for non-game) or opens the game WebView
///    with game confirmation rules (`!(bot_verified || bot_trusted_for_game)` for non-`tg://` URLs).
/// 3. Otherwise if `with_password`, dismisses the password layer.
pub fn handle_callback_answer(
    is_game: bool,
    with_password: bool,
    bot_verified: bool,
    bot_trusted_for_game: bool,
    answer: &CallbackAnswerInput,
) -> CallbackAnswerAction {
    if let Some(message) = answer.message.as_deref()
        && !message.is_empty()
    {
        return if answer.alert {
            CallbackAnswerAction::ShowAlert {
                message: message.to_string(),
            }
        } else {
            CallbackAnswerAction::ShowToast {
                message: message.to_string(),
                hide_password_layer: with_password,
            }
        };
    }

    if let Some(link) = answer.url.as_deref()
        && !link.is_empty()
    {
        if !is_game {
            return CallbackAnswerAction::OpenUrl {
                url: link.to_string(),
            };
        }
        let converted = try_convert_url_to_local(link);
        let is_local = converted
            .get(..5)
            .is_some_and(|scheme| scheme.eq_ignore_ascii_case("tg://"));
        return CallbackAnswerAction::OpenGameUrl {
            url: converted,
            is_local_link: is_local,
            require_confirmation: !is_local && !(bot_verified || bot_trusted_for_game),
            send_play_game_progress: true,
        };
    }

    if with_password {
        CallbackAnswerAction::HidePasswordLayer
    } else {
        CallbackAnswerAction::None
    }
}

/// Cache key for storing `messages.botCallbackAnswer` responses honoring `cache_time`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CallbackCacheKey {
    /// Owning message id.
    pub item_id: FullMsgId,
    /// `true` for game callbacks.
    pub is_game: bool,
    /// Callback payload bytes (empty for game callbacks).
    pub data: Vec<u8>,
}

#[derive(Debug, Clone)]
struct CachedCallbackEntry {
    answer: CallbackAnswerInput,
    expires_at_secs: u64,
}

/// In-memory cache of `messages.botCallbackAnswer` results respecting `cache_time` seconds.
#[derive(Debug, Default, Clone)]
pub struct CallbackAnswerCache {
    entries: HashMap<CallbackCacheKey, CachedCallbackEntry>,
}

impl CallbackAnswerCache {
    /// Creates an empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// Inserts `answer` if `answer.cache_time > 0`.
    /// Entries with `cache_time <= 0` are not cached and remove any stale entry for `key`.
    pub fn insert(&mut self, key: CallbackCacheKey, answer: CallbackAnswerInput, now_secs: u64) {
        if answer.cache_time <= 0 {
            self.entries.remove(&key);
            return;
        }
        let expires_at_secs = now_secs.saturating_add(answer.cache_time as u64);
        self.entries.insert(
            key,
            CachedCallbackEntry {
                answer,
                expires_at_secs,
            },
        );
    }

    /// Looks up a non-expired cached answer at `now_secs`.
    pub fn get(&self, key: &CallbackCacheKey, now_secs: u64) -> Option<&CallbackAnswerInput> {
        self.entries
            .get(key)
            .filter(|entry| now_secs < entry.expires_at_secs)
            .map(|entry| &entry.answer)
    }

    /// Evicts all expired entries as of `now_secs`.
    pub fn prune(&mut self, now_secs: u64) {
        self.entries
            .retain(|_, entry| now_secs < entry.expires_at_secs);
    }

    /// Returns the number of stored entries (including any not yet pruned).
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns `true` if no entries are stored.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

// ---------------------------------------------------------------------------
// URL Auth flow (boxes/url_auth_box.cpp)
// ---------------------------------------------------------------------------

/// Origin of a `messages.requestUrlAuth` / `messages.acceptUrlAuth` flow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum UrlAuthSource {
    /// Inline login button on a message (`f_peer | f_msg_id | f_button_id`).
    Button {
        /// Peer owning the message.
        peer_id: PeerId,
        /// Message id.
        msg_id: MsgId,
        /// Server button id.
        button_id: i64,
        /// Fallback URL from the button.
        url: String,
    },
    /// Standalone OAuth / login URL (`f_url`).
    Url {
        /// Original OAuth URL.
        url: String,
    },
}

impl UrlAuthSource {
    /// Returns the original URL associated with this auth source.
    pub fn url(&self) -> &str {
        match self {
            Self::Button { url, .. } | Self::Url { url } => url,
        }
    }
}

/// Plain representation of TL `urlAuthResultRequest` fields.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct UrlAuthRequestData {
    /// Website domain requested for login.
    pub domain: String,
    /// Bot user id associated with the login.
    pub bot_id: UserId,
    /// Bot display name / first name.
    pub bot_name: String,
    /// Whether the bot asks permission to send messages to the user.
    pub request_write_access: bool,
    /// Whether the login asks to share the user's phone number.
    pub request_phone_number: bool,
    /// Whether emoji match codes must be confirmed before showing the full dialog.
    pub match_codes_first: bool,
    /// Whether the login target is a native/third-party app rather than a web domain.
    pub is_app: bool,
    /// Verified app name (present when `is_app` is `true` and app is verified).
    pub verified_app_name: Option<String>,
    /// Optional hinted account user id (`user_id_hint`).
    pub user_id_hint: Option<UserId>,
    /// Verification emoji match codes.
    pub match_codes: Vec<String>,
    /// Client browser string from server (falls back to `"Unknown browser"`).
    pub browser: Option<String>,
    /// Client platform string from server (falls back to `"Unknown platform"`).
    pub platform: Option<String>,
    /// Client IP string from server (falls back to `"Unknown IP"`).
    pub ip: Option<String>,
    /// Client region string from server (falls back to `"Unknown region"`).
    pub region: Option<String>,
}

/// Plain representation of TL `UrlAuthResult` constructors.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum UrlAuthResultInput {
    /// `urlAuthResultAccepted` — server returned an authenticated redirect URL (or empty for in-app).
    Accepted {
        /// Optional redirect URL containing web login tokens.
        url: Option<String>,
    },
    /// `urlAuthResultDefault` — proceed opening the original URL without auto-login.
    Default,
    /// `urlAuthResultRequest` — ask the user for confirmation before accepting auth.
    Request(UrlAuthRequestData),
}

/// Prepared UI-agnostic confirmation dialog data for URL authorization.
///
/// Port of `UrlAuthBox::RequestButton` and `UrlAuthBox::RequestUrl` (`boxes/url_auth_box.cpp:356-615`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UrlAuthConfirmationBox {
    /// Origin of the auth request.
    pub source: UrlAuthSource,
    /// Display name: `verified_app_name` (or `"Unverified App"` when `is_app`), otherwise `domain`.
    pub display_name: String,
    /// Whether this target is an app (`is_app`).
    pub is_app: bool,
    /// Bot name shown on the "Allow bot to write to me" checkbox, if `request_write_access` is true.
    pub write_access_bot_name: Option<String>,
    /// Whether a secondary phone-number confirmation step is required (`request_phone_number`).
    pub request_phone_number: bool,
    /// Whether match codes should be confirmed before showing the full confirmation box.
    pub match_codes_first: bool,
    /// Verification emoji match codes.
    pub match_codes: Vec<String>,
    /// Optional hinted user account id to pre-select.
    pub user_id_hint: Option<UserId>,
    /// Browser string (default `"Unknown browser"`).
    pub browser: String,
    /// Device / platform string (default `"Unknown platform"`).
    pub platform: String,
    /// IP address string (default `"Unknown IP"`).
    pub ip: String,
    /// Region string (default `"Unknown region"`).
    pub region: String,
}

/// Decision produced after `messages.requestUrlAuth` succeeds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum UrlAuthDecision {
    /// Open the accepted URL, preserving web auth tokens (`keepWebAuthTokens = true`).
    OpenAcceptedUrl {
        /// Authenticated redirect URL.
        url: String,
        /// Always `true` for accepted URLs.
        keep_web_auth_tokens: bool,
    },
    /// Open the fallback URL via `HiddenUrlClickHandler` (with confirmation).
    OpenDefaultUrl {
        /// Fallback URL.
        url: String,
        /// Always `true` (matches `HiddenUrlClickHandler::Open`).
        require_confirmation: bool,
    },
    /// Present the confirmation box to the user.
    ShowConfirmation(Box<UrlAuthConfirmationBox>),
    /// Nothing to open (e.g. `urlAuthResultAccepted` with no URL on a button request).
    NoOp,
}

/// Handles a `messages.requestUrlAuth` response.
///
/// Port of `UrlAuthBox::ActivateButton` (`url_auth_box.cpp:295-312`)
/// and `UrlAuthBox::ActivateUrl` (`url_auth_box.cpp:335-346`).
pub fn handle_url_auth_request_result(
    source: UrlAuthSource,
    result: UrlAuthResultInput,
) -> UrlAuthDecision {
    match result {
        UrlAuthResultInput::Accepted { url } => match url {
            Some(u) if !u.is_empty() => UrlAuthDecision::OpenAcceptedUrl {
                url: u,
                keep_web_auth_tokens: true,
            },
            _ => match source {
                UrlAuthSource::Button { .. } => UrlAuthDecision::NoOp,
                UrlAuthSource::Url { .. } => UrlAuthDecision::OpenAcceptedUrl {
                    url: String::new(),
                    keep_web_auth_tokens: true,
                },
            },
        },
        UrlAuthResultInput::Default => UrlAuthDecision::OpenDefaultUrl {
            url: source.url().to_string(),
            require_confirmation: true,
        },
        UrlAuthResultInput::Request(req) => {
            let display_name = if req.is_app {
                req.verified_app_name
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "Unverified App".to_string())
            } else {
                req.domain
            };
            let write_access_bot_name = if req.request_write_access {
                Some(req.bot_name)
            } else {
                None
            };
            UrlAuthDecision::ShowConfirmation(Box::new(UrlAuthConfirmationBox {
                source,
                display_name,
                is_app: req.is_app,
                write_access_bot_name,
                request_phone_number: req.request_phone_number,
                match_codes_first: req.match_codes_first,
                match_codes: req.match_codes,
                user_id_hint: req.user_id_hint,
                browser: req.browser.unwrap_or_else(|| "Unknown browser".to_string()),
                platform: req
                    .platform
                    .unwrap_or_else(|| "Unknown platform".to_string()),
                ip: req.ip.unwrap_or_else(|| "Unknown IP".to_string()),
                region: req.region.unwrap_or_else(|| "Unknown region".to_string()),
            }))
        }
    }
}

/// Action to take when `messages.requestUrlAuth` fails with an RPC error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum UrlAuthErrorAction {
    /// Show the "login link expired" toast (`URL_EXPIRED` on standalone URL auth).
    ShowExpiredToast,
    /// Fall back to opening the original URL with confirmation.
    OpenDefaultUrl {
        /// Fallback URL.
        url: String,
        /// Always `true`.
        require_confirmation: bool,
    },
}

/// Handles a `messages.requestUrlAuth` failure (`url_auth_box.cpp:313-321,346-353`).
pub fn handle_url_auth_request_error(
    source: &UrlAuthSource,
    error_type: &str,
) -> UrlAuthErrorAction {
    if matches!(source, UrlAuthSource::Url { .. }) && error_type == "URL_EXPIRED" {
        UrlAuthErrorAction::ShowExpiredToast
    } else {
        UrlAuthErrorAction::OpenDefaultUrl {
            url: source.url().to_string(),
            require_confirmation: true,
        }
    }
}

/// User's response from the URL auth confirmation dialog.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct UrlAuthUserChoice {
    /// `true` if the user accepted authentication; `false` if declined.
    pub auth: bool,
    /// Whether the user allowed the bot to send messages (`f_write_allowed`).
    pub allow_write: bool,
    /// Whether the user agreed to share their phone number (`f_share_phone_number`).
    pub share_phone: bool,
    /// Selected verification match code, if any (`f_match_code`).
    pub match_code: Option<String>,
}

/// Parameters for `messages.acceptUrlAuth`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptUrlAuthParams {
    /// Target origin (button peer/msg/button_id or standalone url).
    pub source: UrlAuthSource,
    /// `write_allowed` flag.
    pub write_allowed: bool,
    /// `share_phone_number` flag.
    pub share_phone_number: bool,
    /// Optional `match_code` string.
    pub match_code: Option<String>,
}

/// Decision produced after the user answers the URL auth confirmation box.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AcceptUrlAuthDecision {
    /// User declined: send `messages.declineUrlAuth(url)` and open fallback URL (button only).
    Decline {
        /// URL passed to `messages.declineUrlAuth`.
        url: String,
        /// `true` for button source (opens unauthenticated URL); `false` for standalone URL source.
        open_fallback_url: bool,
    },
    /// User accepted: send `messages.acceptUrlAuth`.
    Accept(AcceptUrlAuthParams),
}

/// Builds the decline or accept RPC decision from `source` and `choice`.
///
/// Port of `UrlAuthBox::RequestButton` and `UrlAuthBox::RequestUrl` callbacks
/// (`boxes/url_auth_box.cpp:387-432,513-606`).
pub fn build_accept_url_auth(
    source: UrlAuthSource,
    choice: UrlAuthUserChoice,
) -> AcceptUrlAuthDecision {
    if !choice.auth {
        let open_fallback_url = matches!(source, UrlAuthSource::Button { .. });
        return AcceptUrlAuthDecision::Decline {
            url: source.url().to_string(),
            open_fallback_url,
        };
    }
    let match_code = choice.match_code.filter(|s| !s.is_empty());
    AcceptUrlAuthDecision::Accept(AcceptUrlAuthParams {
        source,
        write_allowed: choice.allow_write,
        share_phone_number: choice.share_phone,
        match_code,
    })
}

/// Outcome after `messages.acceptUrlAuth` succeeds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum UrlAuthAcceptOutcome {
    /// Open the returned URL (with `keep_web_auth_tokens` set when accepted).
    OpenUrl {
        /// URL to open.
        url: String,
        /// `true` when the server returned `urlAuthResultAccepted`.
        keep_web_auth_tokens: bool,
    },
    /// Show success toast without opening a browser URL (empty accepted URL).
    ShowSuccessToast,
    /// Close the dialog without opening a browser URL (standalone URL source returned same URL).
    DoneWithoutNavigation,
}

/// Handles the `MTPUrlAuthResult` returned by `messages.acceptUrlAuth`.
///
/// Port of `finishWithUrl` in `boxes/url_auth_box.cpp:375-427,493-552`.
pub fn handle_url_auth_accept_result(
    source: &UrlAuthSource,
    result: UrlAuthResultInput,
) -> UrlAuthAcceptOutcome {
    let (accepted, target_url) = match result {
        UrlAuthResultInput::Accepted { url } => (true, url.unwrap_or_default()),
        UrlAuthResultInput::Default | UrlAuthResultInput::Request(_) => {
            (false, source.url().to_string())
        }
    };
    match source {
        UrlAuthSource::Button { .. } => {
            if target_url.is_empty() && accepted {
                UrlAuthAcceptOutcome::ShowSuccessToast
            } else {
                UrlAuthAcceptOutcome::OpenUrl {
                    url: target_url,
                    keep_web_auth_tokens: accepted,
                }
            }
        }
        UrlAuthSource::Url { url } => {
            if (target_url.is_empty() && accepted) || target_url == *url {
                UrlAuthAcceptOutcome::DoneWithoutNavigation
            } else {
                UrlAuthAcceptOutcome::OpenUrl {
                    url: target_url,
                    keep_web_auth_tokens: accepted,
                }
            }
        }
    }
}
