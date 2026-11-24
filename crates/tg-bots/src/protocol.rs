//! Protocol DTOs, state machines, and request helpers for bot startup, Mini App descriptors,
//! write-access permissions, URL auth match-code verification, and bot discovery.
//!
//! Port origin:
//! - `Telegram/SourceFiles/apiwrap.cpp` (`ApiWrap::sendBotStart`, `messages.startBot`)
//! - `Telegram/SourceFiles/inline_bots/bot_attach_web_view.cpp` (`resolveApp`, `requestChatJoin`,
//!   `botCheckWriteAccess`, `botAllowWriteAccess`, `botRequestEmojiStatusAccess`,
//!   `botDownloadFile`, `botRequestChat`, `loadPopularAppBots`)
//! - `Telegram/SourceFiles/boxes/url_auth_box.cpp` (`messages.checkUrlAuthMatchCode`)
//! - `Telegram/SourceFiles/api/api_chat_participants.cpp` (`bots.getBotRecommendations`)
//! - `Telegram/SourceFiles/data/data_session.cpp` (`findBotApp`, `processBotApp`)

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use tg_tl::{enums as tl_enums, functions as tl_fns};
use thiserror::Error;

use crate::markup::{CreateBotRequest, RequestPeerQuery};
use crate::mini_app::MINI_APP_PLATFORM;
use crate::stand_in::{BotAppId, DocumentId, PeerId, PeerKind, PhotoId, UserId};

// ---------------------------------------------------------------------------
// messages.startBot (`apiwrap.cpp:5096-5139`)
// ---------------------------------------------------------------------------

/// Error validating a `messages.startBot` dispatch.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum StartBotError {
    /// Broadcast channels cannot receive bot start commands (`apiwrap.cpp:5103-5106` -> `"USER_BOT"`).
    #[error("USER_BOT: bots cannot be started inside a broadcast channel")]
    BroadcastChannelUnsupported,
    /// Target bot user id is zero/unset.
    #[error("BOT_INVALID: bot user id must be non-zero")]
    InvalidBotId,
}

/// Parameters for invoking `messages.startBot` (`schema/api.tl:2659`).
///
/// Port of `ApiWrap::sendBotStart` (`apiwrap.cpp:5096-5139`):
/// - For 1:1 `?start=<token>`, `peer_id` is `None`, which encodes `InputPeer::Empty` on the wire
///   (`chat ? chat->input() : MTP_inputPeerEmpty()`).
/// - For group `?startgroup=<token>`, `peer_id` is `Some(group_peer_id)`.
/// - When `start_param` is non-empty, the client MUST send `messages.startBot` and NEVER
///   substitute plain-text `"/start <token>"` via `messages.sendMessage`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartBotParams {
    /// Target bot user id (`bot:InputUser`).
    pub bot_id: UserId,
    /// Group chat peer id for `?startgroup=`, or `None` for 1:1 bot DM `?start=` (`InputPeer::Empty`).
    pub peer_id: Option<PeerId>,
    /// Start payload (`start_param:string`).
    pub start_param: String,
    /// Client-generated 64-bit random message id (`random_id:long`).
    /// Pass `0` to let the live session allocate a unique non-zero random id.
    pub random_id: i64,
}

impl StartBotParams {
    /// Builds `messages.startBot` parameters for a 1:1 `?start=<token>` deep link.
    #[must_use]
    pub fn for_private_chat(
        bot_id: UserId,
        start_param: impl Into<String>,
        random_id: i64,
    ) -> Self {
        Self {
            bot_id,
            peer_id: None,
            start_param: start_param.into(),
            random_id,
        }
    }

    /// Builds `messages.startBot` parameters for a group `?startgroup=<token>` deep link.
    #[must_use]
    pub fn for_group_chat(
        bot_id: UserId,
        group_peer_id: PeerId,
        start_param: impl Into<String>,
        random_id: i64,
    ) -> Self {
        Self {
            bot_id,
            peer_id: Some(group_peer_id),
            start_param: start_param.into(),
            random_id,
        }
    }

    /// Decides whether to invoke `messages.startBot` or send plain `/start` following
    /// `ApiWrap::sendBotStart` (`apiwrap.cpp:5103-5131`).
    ///
    /// - Rejects `PeerKind::Broadcast` with [`StartBotError::BroadcastChannelUnsupported`].
    /// - Empty `start_param` dispatches plain `"/start"` (1:1) or `"/start@<bot_username>"` (group).
    /// - Non-empty `start_param` always dispatches `messages.startBot`.
    pub fn resolve_dispatch(
        &self,
        target_peer_kind: Option<PeerKind>,
        bot_username: &str,
    ) -> Result<StartBotDispatch, StartBotError> {
        if self.bot_id.0 == 0 {
            return Err(StartBotError::InvalidBotId);
        }
        if self.peer_id.is_some() && matches!(target_peer_kind, Some(PeerKind::Broadcast)) {
            return Err(StartBotError::BroadcastChannelUnsupported);
        }
        if self.start_param.is_empty() {
            let text = match self.peer_id {
                Some(_) if !bot_username.is_empty() => {
                    format!("/start@{}", bot_username.trim_start_matches('@'))
                }
                _ => "/start".to_owned(),
            };
            return Ok(StartBotDispatch::SendPlainStart {
                peer_id: self.peer_id,
                text,
            });
        }
        Ok(StartBotDispatch::InvokeStartBot(self.clone()))
    }

    /// Encodes the exact TL `messages.startBot#e6df7378` request.
    #[must_use]
    pub fn to_tl_request(
        &self,
        bot: tl_enums::InputUser,
        peer: tl_enums::InputPeer,
        random_id: i64,
    ) -> tl_fns::messages::StartBot {
        tl_fns::messages::StartBot {
            bot,
            peer,
            random_id,
            start_param: self.start_param.clone(),
        }
    }
}

/// Dispatch outcome of [`StartBotParams::resolve_dispatch`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum StartBotDispatch {
    /// Invoke `messages.startBot(bot, peer, random_id, start_param)` over MTProto.
    InvokeStartBot(StartBotParams),
    /// Empty start token fallback: send plain `"/start"` or `"/start@bot"` via `messages.sendMessage`.
    SendPlainStart {
        /// Group chat peer id, or `None` for the bot's private chat.
        peer_id: Option<PeerId>,
        /// Command text to send (`"/start"` or `"/start@username"`).
        text: String,
    },
}

// ---------------------------------------------------------------------------
// messages.getBotApp (`bot_attach_web_view.cpp:1096-1146`, `data_session.cpp:4834-4862`)
// ---------------------------------------------------------------------------

/// Cached descriptor for a named Telegram Mini App (`Data::BotAppData` / TL `botApp#95fcd1d6`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotAppDescriptor {
    /// Unique Mini App identifier.
    pub id: BotAppId,
    /// Server access hash (`inputBotAppID.access_hash`).
    pub access_hash: i64,
    /// Owning bot user id.
    pub bot_id: UserId,
    /// Short name used in `t.me/<bot>/<short_name>`.
    pub short_name: String,
    /// Display title shown in the Mini App header.
    pub title: String,
    /// Description shown in previews and confirmations.
    pub description: String,
    /// Preview photo id, if present (`photo#...`, not `photoEmpty`).
    pub photo_id: Option<PhotoId>,
    /// Optional preview animation/document id (`flags.0?Document`).
    pub document_id: Option<DocumentId>,
    /// Server-provided cache hash passed to subsequent `messages.getBotApp(hash)` calls.
    pub hash: i64,
}

/// Parsed response of `messages.getBotApp` (`messages.botApp#eb50adf5`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct BotAppLookupResult {
    /// Full descriptor when the server returned `botApp`, or `None` for `botAppNotModified`.
    pub app: Option<BotAppDescriptor>,
    /// `true` when the server returned `botAppNotModified#5da674b7`.
    pub not_modified: bool,
    /// `true` when the Mini App is inactive / not yet launched by the user (`flags.0`).
    pub inactive: bool,
    /// `true` when opening the Mini App should ask permission for the bot to send messages (`flags.1`).
    pub request_write_access: bool,
    /// `true` when the Mini App exposes a settings screen (`flags.2`).
    pub has_settings: bool,
}

impl BotAppLookupResult {
    /// Resolves the effective [`BotAppDescriptor`] using `cached` when `not_modified` is `true`,
    /// matching `received ? received : already` in `bot_attach_web_view.cpp:1112`.
    #[must_use]
    pub fn resolve_with_cache(self, cached: Option<&BotAppDescriptor>) -> Option<BotAppDescriptor> {
        self.app.or_else(|| cached.cloned())
    }

    /// Computes `(confirm, force_confirmation)` matching `WebViewInstance::resolveApp`
    /// (`bot_attach_web_view.cpp:1119-1123`).
    #[must_use]
    pub const fn confirmation_flags(
        &self,
        confirm_once: bool,
        confirm_always: bool,
    ) -> (bool, bool) {
        let confirm = self.inactive || confirm_once || confirm_always;
        let force_confirmation = self.inactive || confirm_always;
        (confirm, force_confirmation)
    }
}

/// Per-session cache of resolved [`BotAppDescriptor`] entries keyed by `(bot_id, short_name)`.
///
/// Port of `Data::Session::findBotApp` and `Data::Session::processBotApp`
/// (`data/data_session.cpp:4834-4862`).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct BotAppCache {
    by_key: HashMap<(UserId, String), BotAppDescriptor>,
}

impl BotAppCache {
    /// Creates an empty [`BotAppCache`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Looks up a cached Mini App descriptor by `(bot_id, short_name)`.
    #[must_use]
    pub fn get(&self, bot_id: UserId, short_name: &str) -> Option<&BotAppDescriptor> {
        self.by_key.get(&(bot_id, short_name.to_owned()))
    }

    /// Returns the cached `hash` for `messages.getBotApp`, or `0` when absent.
    #[must_use]
    pub fn hash_for(&self, bot_id: UserId, short_name: &str) -> i64 {
        self.get(bot_id, short_name).map_or(0, |d| d.hash)
    }

    /// Stores or updates `descriptor`.
    pub fn upsert(&mut self, descriptor: BotAppDescriptor) {
        let key = (descriptor.bot_id, descriptor.short_name.clone());
        self.by_key.insert(key, descriptor);
    }

    /// Applies a [`BotAppLookupResult`], updating the cache on fresh `botApp` payloads or
    /// filling `result.app` from cache on `botAppNotModified`.
    pub fn apply_lookup(
        &mut self,
        bot_id: UserId,
        short_name: &str,
        mut result: BotAppLookupResult,
    ) -> BotAppLookupResult {
        if let Some(ref desc) = result.app {
            self.upsert(desc.clone());
        } else if result.not_modified {
            result.app = self.get(bot_id, short_name).cloned();
        }
        result
    }

    /// Number of cached app descriptors.
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_key.len()
    }

    /// Returns `true` if no descriptors are cached.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_key.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Mini App Write Access State Machine (`bot_attach_web_view.cpp:1891-1910`,
// `attach_bot_webview.cpp:3093-3144`)
// ---------------------------------------------------------------------------

/// Step emitted by [`WebViewWriteAccessState`] transitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum WriteAccessStep {
    /// A blocking write-access request is already in progress; reject immediately (`"cancelled"`).
    BusyReject,
    /// Invoke `bots.canSendMessage(bot)` to check if permission is already granted.
    CheckCanSendMessage,
    /// Bot already has write permission; reply `"allowed"` without prompting.
    ReplyAllowed,
    /// Show the user confirmation popup before invoking `bots.allowSendMessage`.
    PromptUserConfirmation,
    /// User confirmed; invoke `bots.allowSendMessage(bot)` and apply returned `Updates`.
    InvokeAllowSendMessage,
    /// User declined or RPC failed; reply `"cancelled"`.
    ReplyCancelled,
}

/// State machine coordinating `web_app_request_write_access` -> `bots.canSendMessage` ->
/// user confirmation -> `bots.allowSendMessage`.
///
/// Port of `Panel::requestWriteAccess` (`attach_bot_webview.cpp:3093-3144`) and
/// `WebViewInstance::botCheckWriteAccess` / `botAllowWriteAccess`
/// (`bot_attach_web_view.cpp:1891-1910`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum WebViewWriteAccessState {
    /// Initial idle state before any write-access check.
    #[default]
    Unknown,
    /// `bots.canSendMessage` RPC is in flight.
    Checking,
    /// Waiting for the user to confirm or cancel the write-access popup.
    PromptingUser,
    /// `bots.allowSendMessage` RPC is in flight.
    Granting,
    /// Write access was confirmed/granted.
    Allowed,
    /// Write access was denied, cancelled, or failed.
    Denied,
}

impl WebViewWriteAccessState {
    /// Returns `true` when an async check, user prompt, or grant RPC is currently in flight
    /// (matching `_inBlockingRequest` in `attach_bot_webview.cpp:3094`).
    #[must_use]
    pub const fn is_in_progress(self) -> bool {
        matches!(self, Self::Checking | Self::PromptingUser | Self::Granting)
    }

    /// Starts a write-access check triggered by `web_app_request_write_access`.
    pub fn begin_request(&mut self) -> WriteAccessStep {
        if self.is_in_progress() {
            return WriteAccessStep::BusyReject;
        }
        *self = Self::Checking;
        WriteAccessStep::CheckCanSendMessage
    }

    /// Processes the `bots.canSendMessage` RPC result (`Ok(true/false)` or `Err`).
    ///
    /// Matches `botCheckWriteAccess` (`bot_attach_web_view.cpp:1891-1899`): RPC failure
    /// falls back to `allowed = false`, proceeding to the confirmation prompt.
    pub fn on_can_send_result(&mut self, can_send: bool) -> WriteAccessStep {
        if can_send {
            *self = Self::Allowed;
            WriteAccessStep::ReplyAllowed
        } else {
            *self = Self::PromptingUser;
            WriteAccessStep::PromptUserConfirmation
        }
    }

    /// Processes the user's answer to the write-access confirmation dialog.
    pub fn on_user_decision(&mut self, allow: bool) -> WriteAccessStep {
        if allow {
            *self = Self::Granting;
            WriteAccessStep::InvokeAllowSendMessage
        } else {
            *self = Self::Denied;
            WriteAccessStep::ReplyCancelled
        }
    }

    /// Processes the `bots.allowSendMessage` RPC completion.
    pub fn on_allow_send_result(&mut self, succeeded: bool) -> WriteAccessStep {
        if succeeded {
            *self = Self::Allowed;
            WriteAccessStep::ReplyAllowed
        } else {
            *self = Self::Denied;
            WriteAccessStep::ReplyCancelled
        }
    }
}

// ---------------------------------------------------------------------------
// messages.checkUrlAuthMatchCode (`boxes/url_auth_box.cpp:911-931`)
// ---------------------------------------------------------------------------

/// Result of verifying an emoji match code via `messages.checkUrlAuthMatchCode`.
///
/// Port of `UrlAuthBox::RequestUrl` (`boxes/url_auth_box.cpp:913-931`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum UrlAuthMatchCodeResult {
    /// Server returned `boolTrue`: remember `match_code` and proceed to the auth confirmation box.
    Matched {
        /// Confirmed emoji match code to forward in `messages.acceptUrlAuth`.
        match_code: String,
    },
    /// Server returned `boolFalse`: show expired/invalid toast and abort.
    MismatchOrExpired,
    /// Server returned `URL_EXPIRED`: show expired toast and abort.
    Expired,
    /// Server returned another RPC error tag (displayed as toast in tdesktop).
    RpcError(String),
}

impl UrlAuthMatchCodeResult {
    /// Maps a `messages.checkUrlAuthMatchCode` RPC outcome into [`UrlAuthMatchCodeResult`].
    #[must_use]
    pub fn from_rpc(match_code: impl Into<String>, rpc_result: Result<bool, &str>) -> Self {
        match rpc_result {
            Ok(true) => Self::Matched {
                match_code: match_code.into(),
            },
            Ok(false) => Self::MismatchOrExpired,
            Err(err) if err.to_ascii_uppercase().contains("URL_EXPIRED") => Self::Expired,
            Err(err) => Self::RpcError(err.to_owned()),
        }
    }

    /// Returns `true` if the match code was accepted by Telegram.
    #[must_use]
    pub const fn is_matched(&self) -> bool {
        matches!(self, Self::Matched { .. })
    }
}

// ---------------------------------------------------------------------------
// Discovery & Chat-Join WebView DTOs
// ---------------------------------------------------------------------------

/// Parsed page from `bots.getPopularAppBots` (`bots.popularAppBots#1991b13b`).
///
/// Port of `AttachWebView::loadPopularAppBots` (`bot_attach_web_view.cpp:2660-2683`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PopularAppBotsPage {
    /// Pagination cursor for the next page, if any (`flags.0?string`).
    pub next_offset: Option<String>,
    /// Bot user ids returned in this page (filtered to `user.bot == true`, matching tdesktop).
    pub bot_ids: Vec<UserId>,
}

/// Parsed response of `bots.getBotRecommendations` (`users.Users` / `users.UsersSlice`).
///
/// Port of `ParseSimilarBots` (`api/api_chat_participants.cpp:252-269, 890-903`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct BotRecommendations {
    /// Recommended similar bot user ids.
    pub bot_ids: Vec<UserId>,
    /// Additional recommendations available beyond this slice (`count - users.len()` for
    /// `users.usersSlice`, `0` for `users.users`).
    pub more_count: i32,
}

/// Parsed response of `bots.getRequestedWebViewButton` (`KeyboardButton`).
///
/// Port of `WebViewInstance::botRequestChat` (`bot_attach_web_view.cpp:2154-2228`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RequestedWebViewButton {
    /// Button asks the user to choose one or more chats/users matching `query`.
    RequestPeer {
        /// Server button id echoed back to `messages.sendBotRequestedPeer`.
        button_id: i32,
        /// Peer selection criteria.
        query: RequestPeerQuery,
    },
    /// Button asks the user to create a managed bot (`requestPeerTypeCreateBot`).
    CreateBot {
        /// Server button id echoed back to `messages.sendBotRequestedPeer`.
        button_id: i32,
        /// Suggested bot name/username.
        request: CreateBotRequest,
    },
    /// Server returned a keyboard button type other than `buttonTypeRequestPeer`
    /// (maps to `"UNSUPPORTED_BUTTON_TYPE"` in `bot_attach_web_view.cpp:2222`).
    Unsupported,
}

/// Parameters for `messages.requestChatJoinWebView#ba9ee679`.
///
/// Port of `WebViewInstance::requestChatJoin` (`bot_attach_web_view.cpp:1369-1387`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatJoinWebViewRequest {
    /// Verification bot user id from `messages.chatInviteJoinResultWebView`.
    pub bot_id: UserId,
    /// Server query id from `messages.chatInviteJoinResultWebView`.
    pub query_id: u64,
    /// Optional compact JSON `theme_params` string.
    pub theme_params_json: Option<String>,
    /// Protocol platform token (defaults to [`MINI_APP_PLATFORM`] = `"tdesktop"`).
    pub platform: String,
}

impl ChatJoinWebViewRequest {
    /// Creates a new [`ChatJoinWebViewRequest`] with [`MINI_APP_PLATFORM`].
    #[must_use]
    pub fn new(bot_id: UserId, query_id: u64, theme_params_json: Option<String>) -> Self {
        Self {
            bot_id,
            query_id,
            theme_params_json,
            platform: MINI_APP_PLATFORM.to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_bot_dispatch_matches_tdesktop_rules() {
        // 1:1 with non-empty start_param -> InvokeStartBot with peer_id = None (InputPeer::Empty)
        let dm = StartBotParams::for_private_chat(UserId(500), "ref_42", 99);
        assert_eq!(
            dm.resolve_dispatch(Some(PeerKind::Bot), "demo_bot"),
            Ok(StartBotDispatch::InvokeStartBot(dm.clone()))
        );

        // Group with non-empty start_param -> InvokeStartBot with group peer
        let grp = StartBotParams::for_group_chat(UserId(500), PeerId(-100123), "grp_tok", 100);
        assert_eq!(
            grp.resolve_dispatch(Some(PeerKind::Group), "demo_bot"),
            Ok(StartBotDispatch::InvokeStartBot(grp))
        );

        // Broadcast channel rejected with USER_BOT
        let ch = StartBotParams::for_group_chat(UserId(500), PeerId(-100999), "tok", 101);
        assert_eq!(
            ch.resolve_dispatch(Some(PeerKind::Broadcast), "demo_bot"),
            Err(StartBotError::BroadcastChannelUnsupported)
        );

        // Empty start_param falls back to plain /start or /start@bot
        let empty_dm = StartBotParams::for_private_chat(UserId(500), "", 0);
        assert_eq!(
            empty_dm.resolve_dispatch(Some(PeerKind::Bot), "demo_bot"),
            Ok(StartBotDispatch::SendPlainStart {
                peer_id: None,
                text: "/start".to_owned(),
            })
        );
        let empty_grp = StartBotParams::for_group_chat(UserId(500), PeerId(-100123), "", 0);
        assert_eq!(
            empty_grp.resolve_dispatch(Some(PeerKind::Group), "@demo_bot"),
            Ok(StartBotDispatch::SendPlainStart {
                peer_id: Some(PeerId(-100123)),
                text: "/start@demo_bot".to_owned(),
            })
        );
    }

    #[test]
    fn bot_app_cache_handles_fresh_and_not_modified() {
        let mut cache = BotAppCache::new();
        assert_eq!(cache.hash_for(UserId(42), "shop"), 0);

        let desc = BotAppDescriptor {
            id: BotAppId(777),
            access_hash: 12345,
            bot_id: UserId(42),
            short_name: "shop".to_owned(),
            title: "Shop App".to_owned(),
            description: "Catalog".to_owned(),
            photo_id: Some(PhotoId(10)),
            document_id: None,
            hash: 998877,
        };
        let first = cache.apply_lookup(
            UserId(42),
            "shop",
            BotAppLookupResult {
                app: Some(desc.clone()),
                not_modified: false,
                inactive: true,
                request_write_access: true,
                has_settings: true,
            },
        );
        assert_eq!(first.app.as_ref(), Some(&desc));
        assert_eq!(cache.hash_for(UserId(42), "shop"), 998877);
        assert_eq!(first.confirmation_flags(false, false), (true, true));

        let second = cache.apply_lookup(
            UserId(42),
            "shop",
            BotAppLookupResult {
                app: None,
                not_modified: true,
                inactive: false,
                request_write_access: false,
                has_settings: false,
            },
        );
        assert_eq!(second.app.as_ref(), Some(&desc));
        assert!(second.not_modified);
        assert_eq!(second.confirmation_flags(true, false), (true, false));
    }

    #[test]
    fn write_access_state_machine_enforces_blocking_and_transitions() {
        let mut st = WebViewWriteAccessState::Unknown;
        assert_eq!(st.begin_request(), WriteAccessStep::CheckCanSendMessage);
        assert_eq!(st, WebViewWriteAccessState::Checking);
        // Concurrent request rejected immediately
        assert_eq!(st.begin_request(), WriteAccessStep::BusyReject);

        // Already allowed short-circuits confirmation
        assert_eq!(st.on_can_send_result(true), WriteAccessStep::ReplyAllowed);
        assert_eq!(st, WebViewWriteAccessState::Allowed);

        // Not allowed prompts user -> confirms -> invokes allowSendMessage -> allowed
        assert_eq!(st.begin_request(), WriteAccessStep::CheckCanSendMessage);
        assert_eq!(
            st.on_can_send_result(false),
            WriteAccessStep::PromptUserConfirmation
        );
        assert_eq!(
            st.on_user_decision(true),
            WriteAccessStep::InvokeAllowSendMessage
        );
        assert_eq!(st.on_allow_send_result(true), WriteAccessStep::ReplyAllowed);
        assert_eq!(st, WebViewWriteAccessState::Allowed);
    }

    #[test]
    fn url_auth_match_code_result_maps_rpc_variants() {
        assert_eq!(
            UrlAuthMatchCodeResult::from_rpc("🚀", Ok(true)),
            UrlAuthMatchCodeResult::Matched {
                match_code: "🚀".to_owned()
            }
        );
        assert_eq!(
            UrlAuthMatchCodeResult::from_rpc("🚀", Ok(false)),
            UrlAuthMatchCodeResult::MismatchOrExpired
        );
        assert_eq!(
            UrlAuthMatchCodeResult::from_rpc("🚀", Err("400: URL_EXPIRED")),
            UrlAuthMatchCodeResult::Expired
        );
        assert_eq!(
            UrlAuthMatchCodeResult::from_rpc("🚀", Err("FLOOD_WAIT_10")),
            UrlAuthMatchCodeResult::RpcError("FLOOD_WAIT_10".to_owned())
        );
    }
}
