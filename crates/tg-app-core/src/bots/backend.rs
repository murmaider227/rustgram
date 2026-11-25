//! Backend trait and view-model types decoupling `tg-app::bots` widgets from MTProto/API.
//!
//! Port origin: `Telegram/SourceFiles/api/api_bot.h`, `api/api_bot.cpp`,
//! `Telegram/SourceFiles/inline_bots/bot_attach_web_view.h`, and
//! `Telegram/SourceFiles/boxes/url_auth_box.cpp`.

use tg_bots::callback::{
    AcceptUrlAuthParams, CallbackAnswerInput, CallbackQueryRequest, UrlAuthResultInput,
    UrlAuthSource,
};
use tg_bots::commands::{BotCommand, BotInfo, BotMenuButton, BotStatus, ScopedBotCommands};
use tg_bots::inline::{GetInlineBotResultsRequest, InlineResult, InlineResultsPage};
use tg_bots::markup::{ReplyMarkupData, RequestPeerCandidate, RequestPeerType};
use tg_bots::mini_app::{AttachMenuStore, AttachMenuToggleState, GameData, GameHighScore};
use tg_bots::protocol::{BotRecommendations, StartBotParams, UrlAuthMatchCodeResult};
use tg_bots::stand_in::{FullMsgId, MsgId, PeerId, PeerKind, UserId};

/// Lightweight peer entry used by Request Peer and Share Game Score choosers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BotPeerCandidate {
    /// Target peer id.
    pub peer_id: PeerId,
    /// Display name of the user, group, or channel.
    pub title: String,
    /// Subtitle (e.g. `@username`, `"42 members"`, `"Bot"`).
    pub subtitle: String,
    /// Coarse peer classification.
    pub peer_kind: PeerKind,
    /// Whether this peer is the current user's Saved Messages chat.
    pub is_self: bool,
    /// Whether the current user can send games into this chat.
    pub can_send_games: bool,
    /// Whether the peer is a forum supergroup.
    pub is_forum: bool,
    /// Request-peer matching criteria metadata.
    pub criteria: RequestPeerCandidate,
}

impl BotPeerCandidate {
    /// Returns a 1-2 character avatar monogram for painting peer circles.
    #[must_use]
    pub fn initials(&self) -> String {
        let mut chars = self
            .title
            .split_whitespace()
            .filter_map(|w| w.chars().next())
            .take(2)
            .map(|c| c.to_ascii_uppercase());
        let first = chars.next().unwrap_or('?');
        match chars.next() {
            Some(second) => format!("{first}{second}"),
            None => first.to_string(),
        }
    }

    /// Returns a human-readable category badge for this peer.
    #[must_use]
    pub const fn category_label(&self) -> &'static str {
        match self.criteria.peer_type {
            RequestPeerType::User => {
                if self.criteria.is_bot {
                    "Bot"
                } else {
                    "User"
                }
            }
            RequestPeerType::Group => {
                if self.is_forum {
                    "Forum"
                } else {
                    "Group"
                }
            }
            RequestPeerType::Broadcast => "Channel",
        }
    }
}

/// Optional rich media preview card rendered inside a bot message bubble.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BotMessageAttachment {
    /// No attachment.
    None,
    /// HTML5 bot game card with high-score leaderboard preview.
    Game {
        /// Game metadata.
        game: GameData,
        /// Top scores to show in the message/share sheet.
        high_scores: Vec<GameHighScore>,
    },
    /// Inline result card sent into the chat.
    InlineSent {
        /// Short kind tag (`"Photo"`, `"GIF"`, `"Article"`, etc.).
        kind_label: String,
        /// Headline text.
        title: String,
        /// Secondary line.
        subtitle: String,
    },
}

/// A message rendered in the bot demo chat canvas (or supplied by `history/`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BotChatMessage {
    /// Full message id.
    pub id: FullMsgId,
    /// Sender display name.
    pub author: String,
    /// `true` for outgoing messages sent by the current user.
    pub outgoing: bool,
    /// Formatted timestamp string (`"14:22"`).
    pub time_label: String,
    /// Message text body.
    pub text: String,
    /// Optional attachment (e.g. a bot game card).
    pub attachment: BotMessageAttachment,
    /// Optional inline keyboard attached beneath this bubble.
    pub inline_markup: Option<ReplyMarkupData>,
    /// Optional reply keyboard defined by this message.
    pub reply_markup: Option<ReplyMarkupData>,
    /// Optional `via @bot` attribution label.
    pub via_bot: Option<String>,
}

/// Complete snapshot of an active bot chat used to initialize bot UI screens/widgets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BotChatSnapshot {
    /// Active chat peer id.
    pub peer_id: PeerId,
    /// Peer kind (`Bot`, `Group`, `User`, `Broadcast`).
    pub peer_kind: PeerKind,
    /// Peer bot-presence status for group command wrapping (`@bot`).
    pub peer_bot_status: BotStatus,
    /// Bot user id.
    pub bot_id: UserId,
    /// Bot display name.
    pub bot_name: String,
    /// Bot username without leading `@`.
    pub bot_username: String,
    /// Whether the bot is verified (affects URL and game confirmation rules).
    pub bot_verified: bool,
    /// Whether the user previously trusted this bot for opening games.
    pub bot_trusted_for_game: bool,
    /// Full `BotInfo` metadata (description, photo id, commands, menu button, privacy URL).
    pub bot_info: BotInfo,
    /// Scoped bot commands registry.
    pub scoped_commands: ScopedBotCommands,
    /// Current account display name (used in URL auth confirmation box).
    pub current_user_name: String,
    /// Current account phone number (fictional placeholder for Share Phone confirmation).
    pub current_user_phone: String,
    /// Messages in the chat.
    pub messages: Vec<BotChatMessage>,
    /// Active reply keyboard `(owning_msg_id, markup)` if one is currently active.
    pub active_reply_keyboard: Option<(FullMsgId, ReplyMarkupData)>,
}

/// Submitted poll payload from the `RequestPoll` flow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmittedPoll {
    /// Poll question text.
    pub question: String,
    /// Answer choices (at least 2).
    pub options: Vec<String>,
    /// Whether this poll is a quiz.
    pub is_quiz: bool,
    /// Correct option index when `is_quiz` is `true`.
    pub correct_option: Option<usize>,
}

/// Decoupled backend trait driving all `tg-app::bots` widgets.
///
/// Production code implements this over `tg-api` / `tg-mtproto`; tests and the standalone
/// `Ctrl+Shift+K` demo use [`crate::bots::mock::MockBotsBackend`].
pub trait BotsBackend: Send {
    /// Loads the active bot chat snapshot (bot info, commands, messages, reply keyboard).
    fn load_bot_chat(&self) -> BotChatSnapshot;

    /// Returns the currently loaded [`AttachMenuStore`].
    fn load_attach_menu(&self) -> AttachMenuStore;

    /// Fetches a single bot's attach-menu metadata via `messages.getAttachMenuBot`
    /// (`inline_bots/bot_attach_web_view.cpp:2769-2787`).
    ///
    /// Default implementation falls back to looking up `bot_id` in [`Self::load_attach_menu`].
    fn load_attach_menu_bot(
        &self,
        bot_id: UserId,
    ) -> Result<tg_bots::mini_app::AttachMenuBot, String> {
        self.load_attach_menu()
            .find(bot_id)
            .cloned()
            .ok_or_else(|| format!("Attach-menu bot {} not found.", bot_id.0))
    }

    /// Lists candidate peers eligible for `RequestPeer` and `ShareGameScore` choosers.
    fn candidate_peers(&self) -> Vec<BotPeerCandidate>;

    /// Executes `messages.getBotCallbackAnswer` (or `ephemeral.getCallbackAnswer`).
    ///
    /// # Errors
    /// Returns a human-readable error message if the RPC fails.
    fn send_callback_query(
        &mut self,
        request: &CallbackQueryRequest,
    ) -> Result<CallbackAnswerInput, String>;

    /// Executes `messages.requestUrlAuth` for an inline login button or standalone URL.
    ///
    /// # Errors
    /// Returns a Telegram RPC error tag (e.g. `"URL_EXPIRED"`) or message on failure.
    fn request_url_auth(&mut self, source: &UrlAuthSource) -> Result<UrlAuthResultInput, String>;

    /// Executes `messages.acceptUrlAuth`.
    ///
    /// # Errors
    /// Returns a human-readable error string on failure.
    fn accept_url_auth(
        &mut self,
        params: &AcceptUrlAuthParams,
    ) -> Result<UrlAuthResultInput, String>;

    /// Executes `messages.declineUrlAuth`.
    ///
    /// # Errors
    /// Returns a human-readable error string on failure.
    fn decline_url_auth(&mut self, url: &str) -> Result<(), String>;

    /// Executes `messages.getInlineBotResults` for a debounced or paginated query.
    ///
    /// # Errors
    /// Returns a human-readable error string on failure.
    fn query_inline_results(
        &mut self,
        request: &GetInlineBotResultsRequest,
    ) -> Result<InlineResultsPage, String>;

    /// Sends a chosen [`InlineResult`] into `peer_id` (`messages.sendInlineBotResult`).
    ///
    /// # Errors
    /// Returns a human-readable error string on failure.
    fn send_inline_result(
        &mut self,
        bot_id: UserId,
        peer_id: PeerId,
        result: &InlineResult,
    ) -> Result<BotChatMessage, String>;

    /// Sends a slash command or reply keyboard button message.
    ///
    /// # Errors
    /// Returns a human-readable error string on failure.
    fn send_bot_command(
        &mut self,
        peer_id: PeerId,
        text: &str,
        reply_to: Option<FullMsgId>,
    ) -> Result<BotChatMessage, String>;

    /// Shares the current user's phone number contact in response to `ButtonKind::RequestPhone`.
    ///
    /// # Errors
    /// Returns a human-readable error string on failure.
    fn share_phone(
        &mut self,
        peer_id: PeerId,
        reply_to: Option<MsgId>,
    ) -> Result<BotChatMessage, String>;

    /// Sends a newly created poll in response to `ButtonKind::RequestPoll`.
    ///
    /// # Errors
    /// Returns a human-readable error string on failure.
    fn send_poll(
        &mut self,
        peer_id: PeerId,
        poll: &SubmittedPoll,
    ) -> Result<BotChatMessage, String>;

    /// Sends selected peers to a bot (`messages.sendBotRequestedPeer`).
    ///
    /// # Errors
    /// Returns a human-readable error string on failure.
    fn send_requested_peers(
        &mut self,
        item_id: FullMsgId,
        button_id: i64,
        selected_peers: &[PeerId],
    ) -> Result<BotChatMessage, String>;

    /// Adds or removes a bot from the user's attachment/side menu (`messages.toggleBotInAttachMenu`).
    ///
    /// # Errors
    /// Returns a human-readable error string on failure.
    fn toggle_attach_menu_bot(
        &mut self,
        bot_id: UserId,
        state: AttachMenuToggleState,
    ) -> Result<AttachMenuStore, String>;

    /// Records acceptance of the Mini Apps disclaimer for `bot_id`.
    fn accept_attach_disclaimer(&mut self, bot_id: UserId);

    /// Shares a game message/score into `target_peer`.
    ///
    /// # Errors
    /// Returns a human-readable error string on failure.
    fn share_game_score(
        &mut self,
        message_id: FullMsgId,
        target_peer: PeerId,
    ) -> Result<String, String>;

    /// Updates the active bot menu button mode (demo helper / live update hook).
    fn set_menu_button(&mut self, button: BotMenuButton);

    /// Resolves a player display name for a [`GameHighScore::user_id`].
    fn resolve_user_name(&self, user_id: UserId) -> String;

    /// Returns the active bot commands for display in the commands popup.
    fn commands_for_chat(&self) -> Vec<BotCommand>;

    /// Loads a [`BotChatSnapshot`] for `peer_id` when that peer is a bot chat.
    fn load_bot_chat_for_peer(&self, peer_id: PeerId) -> Option<BotChatSnapshot> {
        let snap = self.load_bot_chat();
        (snap.peer_id == peer_id).then_some(snap)
    }

    /// Resolves an inline bot username (without leading `@`) to `(bot_id, canonical_username)`.
    fn resolve_inline_bot(&self, username: &str) -> Option<(UserId, String)> {
        let snap = self.load_bot_chat();
        snap.bot_username
            .eq_ignore_ascii_case(username)
            .then_some((snap.bot_id, snap.bot_username))
    }

    /// Returns bot commands available in `peer_id` (bot DM or group with bots).
    fn commands_for_peer(&self, peer_id: PeerId) -> Vec<BotCommand> {
        if self.load_bot_chat().peer_id == peer_id {
            self.commands_for_chat()
        } else {
            Vec::new()
        }
    }

    /// Returns the active reply keyboard `(owning_msg_id, markup)` for `peer_id`, if any.
    fn active_reply_keyboard_for_peer(
        &self,
        peer_id: PeerId,
    ) -> Option<(FullMsgId, ReplyMarkupData)> {
        self.load_bot_chat_for_peer(peer_id)
            .and_then(|snap| snap.active_reply_keyboard)
    }

    /// Starts a bot via `messages.startBot` (for 1:1 `?start=` or group `?startgroup=`),
    /// falling back to plain `"/start"` / `"/start@bot"` only when `start_param` is empty.
    ///
    /// Port of `ApiWrap::sendBotStart` (`Telegram/SourceFiles/apiwrap.cpp:5096-5139`).
    ///
    /// # Errors
    /// Returns a Telegram error tag (such as `"USER_BOT"` for broadcast channels) or message.
    fn start_bot(&mut self, _params: &StartBotParams) -> Result<BotChatMessage, String> {
        Err("start_bot is not supported by this backend".to_owned())
    }

    /// Verifies an OAuth / URL login emoji match code via `messages.checkUrlAuthMatchCode`.
    ///
    /// Port of `UrlAuthBox::RequestUrl` (`Telegram/SourceFiles/boxes/url_auth_box.cpp:913-931`).
    ///
    /// # Errors
    /// Returns a human-readable error string on transport or unexpected RPC failure.
    fn check_url_auth_match_code(
        &mut self,
        _url: &str,
        _match_code: &str,
    ) -> Result<UrlAuthMatchCodeResult, String> {
        Err("check_url_auth_match_code is not supported by this backend".to_owned())
    }

    /// Fetches similar bot recommendations via `bots.getBotRecommendations`.
    ///
    /// Port of `Api::ChatParticipants::loadSimilar` (`api/api_chat_participants.cpp:890-903`).
    ///
    /// # Errors
    /// Returns a human-readable error string on RPC failure.
    fn get_bot_recommendations(&mut self, _bot_id: UserId) -> Result<BotRecommendations, String> {
        Ok(BotRecommendations::default())
    }
}
