//! Live implementation of [`BotsBackend`] and bot MTProto RPC helpers on [`LiveSession`].
//!
//! Port origin: `Telegram/SourceFiles/api/api_bot.cpp`,
//! `Telegram/SourceFiles/boxes/url_auth_box.cpp`,
//! `Telegram/SourceFiles/inline_bots/inline_results_widget.cpp`,
//! `Telegram/SourceFiles/inline_bots/bot_attach_web_view.cpp`, and
//! `Telegram/SourceFiles/data/data_user.cpp`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

use tg_api::RpcClient;
use tg_api::convert::{
    apply_tl_user_full, apply_tl_users_and_chats, input_peer_for, input_user_for,
};
use tg_bots::callback::{
    AcceptUrlAuthParams, CallbackAnswerInput, CallbackQueryRequest, UrlAuthResultInput,
    UrlAuthSource,
};
use tg_bots::commands::{
    BotCommand, BotCommandScope, BotInfo, BotMenuButton, BotStatus, ScopedBotCommands,
};
use tg_bots::inline::{GetInlineBotResultsRequest, InlineResult, InlineResultsPage};
use tg_bots::markup::{ReplyMarkupData, RequestPeerCandidate, RequestPeerType};
use tg_bots::mini_app::{
    AttachMenuStore, AttachMenuToggleState, GameHighScore, SetGameScoreRequest,
};
use tg_bots::protocol::{
    BotRecommendations, StartBotDispatch, StartBotParams, UrlAuthMatchCodeResult,
};
use tg_bots::stand_in::{
    FullMsgId as BotFullMsgId, MsgId as BotMsgId, PeerId as BotPeerId, PeerKind as BotPeerKind,
    UserId as BotUserId,
};
use tg_bots::tl_convert::{
    attach_menu_bots_from_tl, bot_info_from_data, bot_info_update_from_tl, bot_menu_button_from_tl,
    bot_recommendations_from_tl, bot_results_from_tl, callback_answer_from_tl,
    chat_admin_rights_from_data, game_data_from_data, game_high_scores_from_tl,
    reply_markup_from_data, url_auth_result_from_tl,
};
use tg_crypto::srp::{PasswordKdfAlgoModPow, compute_check};
use tg_data::{
    FullMsgId as DataFullMsgId, LoadedStatus, Message, MessageMedia, MsgId as DataMsgId, Peer,
    PeerId as DataPeerId, Session, UserId as DataUserId,
};
use tg_tl::{enums as tl_enums, functions as tl_fns, types as tl_types};
use tokio::sync::RwLock;
use tracing::{debug, warn};

use crate::convert::{
    format_short_time, history_messages_ordered, known_peer_ids, now_unix_i32, peer_display_title,
    peer_status_text,
};
use crate::media::{block_on_runtime, with_rpc_timeout};
use crate::session::LiveSession;
use tg_app_core::bots::backend::{
    BotChatMessage, BotChatSnapshot, BotMessageAttachment, BotPeerCandidate, BotsBackend,
    SubmittedPoll,
};

const BOTS_RPC_TIMEOUT: Duration = Duration::from_secs(10);

struct LiveBotsState {
    active_bot_peer: Option<BotPeerId>,
    attach_menu: AttachMenuStore,
    attach_menu_loaded: bool,
    menu_button_overrides: HashMap<u64, BotMenuButton>,
    pending_callback_password: Option<String>,
    next_random_id: u64,
}

impl LiveBotsState {
    fn new() -> Self {
        Self {
            active_bot_peer: None,
            attach_menu: AttachMenuStore::new(),
            attach_menu_loaded: false,
            menu_button_overrides: HashMap::new(),
            pending_callback_password: None,
            next_random_id: 1,
        }
    }

    fn alloc_random_id(&mut self) -> i64 {
        let now = now_unix_i32() as u64;
        self.next_random_id = self.next_random_id.wrapping_add(1);
        ((now << 24) ^ self.next_random_id ^ 0xB075_4242_1337) as i64
    }
}

type BotsStateRegistry = Mutex<Vec<(Weak<RwLock<Session>>, Arc<Mutex<LiveBotsState>>)>>;

fn bots_state_registry() -> &'static BotsStateRegistry {
    static REGISTRY: OnceLock<BotsStateRegistry> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(Vec::new()))
}

fn bots_state_for_session(session: &Arc<RwLock<Session>>) -> Arc<Mutex<LiveBotsState>> {
    let key = Arc::downgrade(session);
    let mut guard = bots_state_registry()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    guard.retain(|(weak, _)| weak.strong_count() > 0);
    if let Some((_, state)) = guard.iter().find(|(weak, _)| Weak::ptr_eq(weak, &key)) {
        return Arc::clone(state);
    }
    let state = Arc::new(Mutex::new(LiveBotsState::new()));
    guard.push((key, Arc::clone(&state)));
    state
}

pub(crate) fn mark_attach_menu_bot_removed(session: &Arc<RwLock<Session>>, bot_id: u64) {
    let state = bots_state_for_session(session);
    if let Ok(mut guard) = state.lock()
        && let Some(bot) = guard
            .attach_menu
            .bots
            .iter_mut()
            .find(|b| b.bot_id.0 == bot_id)
    {
        bot.inactive = true;
    }
}

pub(crate) fn attach_menu_flags_for_bot(
    session: &Arc<RwLock<Session>>,
    bot_id: u64,
) -> (bool, bool) {
    let state = bots_state_for_session(session);
    if let Ok(guard) = state.lock()
        && let Some(bot) = guard.attach_menu.find(BotUserId(bot_id))
    {
        return (!bot.inactive && bot.in_attach_menu, bot.in_side_menu);
    }
    (false, false)
}

pub(crate) fn upsert_attach_menu_bot(
    session: &Arc<RwLock<Session>>,
    bot: tg_bots::mini_app::AttachMenuBot,
) {
    let state = bots_state_for_session(session);
    if let Ok(mut guard) = state.lock() {
        guard.attach_menu.upsert(bot);
    }
}

#[cfg(test)]
pub(crate) fn cached_attach_menu_bot(
    session: &Arc<RwLock<Session>>,
    bot_id: BotUserId,
) -> Option<tg_bots::mini_app::AttachMenuBot> {
    let state = bots_state_for_session(session);
    state
        .lock()
        .ok()
        .and_then(|guard| guard.attach_menu.find(bot_id).cloned())
}

impl LiveSession {
    fn bots_read_session<R: Send>(&self, f: impl FnOnce(&Session) -> R + Send) -> R {
        let session_lock = self.runtime().session();
        if let Ok(guard) = session_lock.try_read() {
            return f(&guard);
        }
        let session_arc = Arc::clone(session_lock);
        block_on_runtime(self.runtime().tokio_handle(), async move {
            let guard = session_arc.read().await;
            f(&guard)
        })
    }

    fn bots_write_session<R: Send>(&self, f: impl FnOnce(&mut Session) -> R + Send) -> R {
        let session_lock = self.runtime().session();
        if let Ok(mut guard) = session_lock.try_write() {
            return f(&mut guard);
        }
        let session_arc = Arc::clone(session_lock);
        block_on_runtime(self.runtime().tokio_handle(), async move {
            let mut guard = session_arc.write().await;
            f(&mut guard)
        })
    }

    fn bots_invoke_rpc<R>(&self, req: R) -> Result<R::Return, String>
    where
        R: tg_tl::RemoteCall + Send + Sync + 'static,
        R::Return: Send + 'static,
    {
        let rpc = self.runtime().rpc().clone();
        block_on_runtime(self.runtime().tokio_handle(), async move {
            with_rpc_timeout(BOTS_RPC_TIMEOUT, async move { rpc.invoke(&req).await }).await
        })
    }

    fn bots_input_peer(&self, peer_id: BotPeerId) -> Result<tl_enums::InputPeer, String> {
        let peer = self.bots_read_session(|s| input_peer_for(s, DataPeerId(peer_id.0)));
        if matches!(peer, tl_enums::InputPeer::Empty) {
            Err(format!("Peer {} is not resolved in session.", peer_id.0))
        } else {
            Ok(peer)
        }
    }

    fn bots_input_user(&self, user_id: BotUserId) -> Result<tl_enums::InputUser, String> {
        let user = self.bots_read_session(|s| input_user_for(s, DataUserId(user_id.0)));
        if matches!(user, tl_enums::InputUser::Empty) {
            Err(format!("User {} is not resolved in session.", user_id.0))
        } else {
            Ok(user)
        }
    }

    fn bots_next_random_id(&self) -> i64 {
        let state = bots_state_for_session(self.runtime().session());
        state
            .lock()
            .map(|mut s| s.alloc_random_id())
            .unwrap_or_else(|_| now_unix_i32() as i64)
    }

    fn bots_apply_updates(&self, updates: &tl_enums::Updates) -> Vec<DataMsgId> {
        self.apply_updates_to_session(updates)
    }

    /// Sets the active bot chat peer used by [`BotsBackend::load_bot_chat`].
    pub fn set_active_bot_peer(&self, peer_id: BotPeerId) {
        let state = bots_state_for_session(self.runtime().session());
        if let Ok(mut guard) = state.lock() {
            guard.active_bot_peer = Some(peer_id);
        }
    }

    /// Stages a 2FA cloud password to be consumed by the next
    /// `CallbackQueryRequest { requires_password: true, .. }` sent via
    /// [`BotsBackend::send_callback_query`].
    pub fn set_pending_callback_password(&self, password: Option<String>) {
        let state = bots_state_for_session(self.runtime().session());
        if let Ok(mut guard) = state.lock() {
            guard.pending_callback_password = password;
        }
    }

    /// Fetches `account.getPassword` and computes Telegram's SRP-6a
    /// `InputCheckPasswordSRP` via [`tg_crypto::srp::compute_check`].
    ///
    /// Port of `Api::CloudPassword` + `SendBotCallbackDataWithPassword` in `api/api_bot.cpp`.
    pub fn compute_srp_password_check(
        &self,
        password: &str,
    ) -> Result<tl_enums::InputCheckPasswordSRP, String> {
        let tl_enums::account::Password::Password(pwd) =
            self.bots_invoke_rpc(tl_fns::account::GetPassword {})?;
        let Some(tl_enums::PasswordKdfAlgo::SHA256SHA256PBKDF2HMACSHA512iter100000SHA256ModPow(
            algo_tl,
        )) = pwd.current_algo
        else {
            return Err("Account does not have a supported SRP 2FA password algorithm.".to_owned());
        };
        let Some(srp_b) = pwd.srp_b else {
            return Err("Server response missing srp_B for SRP check.".to_owned());
        };
        let Some(srp_id) = pwd.srp_id else {
            return Err("Server response missing srp_id for SRP check.".to_owned());
        };
        let algo = PasswordKdfAlgoModPow {
            salt1: algo_tl.salt1,
            salt2: algo_tl.salt2,
            g: algo_tl.g,
            p: algo_tl.p,
        };
        let check = compute_check(&algo, &srp_b, srp_id, password.as_bytes())
            .map_err(|e| format!("SRP computation failed: {e}"))?;
        Ok(tl_enums::InputCheckPasswordSRP::InputCheckPasswordSRP(
            tl_types::InputCheckPasswordSRP {
                srp_id: check.srp_id,
                a: check.a.to_vec(),
                m1: check.m1.to_vec(),
            },
        ))
    }

    /// Executes `messages.getBotCallbackAnswer` with a 2FA SRP password proof computed
    /// via `account.getPassword` and `tg-crypto`.
    pub fn send_callback_query_with_password(
        &self,
        request: &CallbackQueryRequest,
        password: &str,
    ) -> Result<CallbackAnswerInput, String> {
        let peer = self.bots_input_peer(request.peer_id)?;
        let srp = self.compute_srp_password_check(password)?;
        let resp = self.bots_invoke_rpc(tl_fns::messages::GetBotCallbackAnswer {
            game: request.is_game,
            peer,
            msg_id: request.msg_id.0,
            data: request.data.clone(),
            password: Some(srp),
        })?;
        Ok(callback_answer_from_tl(&resp))
    }

    /// Fetches `users.getFullUser` for a bot, applies `UserFull` (including `UserFull.bot_info`)
    /// to `tg_data::Session`, and returns the updated [`BotInfo`].
    pub fn fetch_bot_full_info(&self, bot_id: BotUserId) -> Result<BotInfo, String> {
        let id = self.bots_input_user(bot_id)?;
        let tl_enums::users::UserFull::UserFull(resp) =
            self.bots_invoke_rpc(tl_fns::users::GetFullUser { id })?;
        self.bots_write_session(|session| {
            apply_tl_users_and_chats(session, &resp.users, &resp.chats);
            apply_tl_user_full(session, &resp.full_user);
        });
        let tl_enums::UserFull::UserFull(full) = &resp.full_user;
        let mut info = self
            .bots_read_session(|s| {
                s.user(DataUserId(bot_id.0))
                    .and_then(|u| u.bot_info.as_ref())
                    .map(bot_info_from_data)
            })
            .unwrap_or_default();
        if let Some(ref tl_bi) = full.bot_info {
            let update = bot_info_update_from_tl(tl_bi);
            info.apply_update(update);
        }
        self.bridge_state().notify_ui();
        Ok(info)
    }

    /// Queries `bots.getBotMenuButton` for `bot_id`, updates `tg_data::Session` and local cache,
    /// and returns the resolved [`BotMenuButton`].
    pub fn fetch_bot_menu_button(&self, bot_id: BotUserId) -> Result<BotMenuButton, String> {
        let user_id = self.bots_input_user(bot_id)?;
        let tl_btn = self.bots_invoke_rpc(tl_fns::bots::GetBotMenuButton { user_id })?;
        let button = bot_menu_button_from_tl(&tl_btn);
        let state = bots_state_for_session(self.runtime().session());
        if let Ok(mut guard) = state.lock() {
            guard.menu_button_overrides.insert(bot_id.0, button.clone());
        }
        self.bots_write_session(|session| {
            if let Some(user) = session.user(DataUserId(bot_id.0)).cloned() {
                let mut updated = user;
                if let Some(ref mut bi) = updated.bot_info {
                    match &button {
                        BotMenuButton::WebApp { text, url } => {
                            bi.menu_button_text = text.clone();
                            bi.menu_button_url = url.clone();
                        }
                        BotMenuButton::Default | BotMenuButton::Commands => {
                            bi.menu_button_text.clear();
                            bi.menu_button_url.clear();
                        }
                    }
                }
                session.upsert_user(updated, tg_data::PeerUpdateFlags::IS_BOT);
            }
        });
        self.bridge_state().notify_ui();
        Ok(button)
    }

    /// Fetches `messages.getAttachMenuBots` using the cached hash, applies users into `Session`,
    /// updates the cached [`AttachMenuStore`], and returns it.
    pub fn fetch_attach_menu_bots(&self) -> Result<AttachMenuStore, String> {
        let state = bots_state_for_session(self.runtime().session());
        let hash = state.lock().map(|s| s.attach_menu.bots_hash).unwrap_or(0);
        let resp = self.bots_invoke_rpc(tl_fns::messages::GetAttachMenuBots { hash })?;
        if let tl_enums::AttachMenuBots::AttachMenuBots(ref full) = resp {
            self.bots_write_session(|session| {
                apply_tl_users_and_chats(session, &full.users, &[]);
            });
        }
        let store = if let Ok(mut guard) = state.lock() {
            guard.attach_menu_loaded = true;
            if let Some((new_hash, bots)) = attach_menu_bots_from_tl(&resp) {
                guard.attach_menu.update_bots(new_hash, bots);
            }
            guard.attach_menu.clone()
        } else {
            AttachMenuStore::new()
        };
        self.bridge_state().notify_ui();
        Ok(store)
    }

    /// Executes `messages.setGameScore` and applies returned `Updates` to `Session`.
    pub fn set_game_score(&self, req: &SetGameScoreRequest) -> Result<(), String> {
        let peer = self.bots_input_peer(req.peer_id)?;
        let user_id = self.bots_input_user(req.user_id)?;
        let updates = self.bots_invoke_rpc(tl_fns::messages::SetGameScore {
            edit_message: req.edit_message,
            force: req.force,
            peer,
            id: req.msg_id.0,
            user_id,
            score: req.score,
        })?;
        let _ = self.bots_apply_updates(&updates);
        Ok(())
    }

    /// Executes `messages.getGameHighScores`, applies returned `users` into `Session`,
    /// and returns the leaderboard entries.
    pub fn get_game_high_scores(
        &self,
        peer_id: BotPeerId,
        msg_id: BotMsgId,
        user_id: BotUserId,
    ) -> Result<Vec<GameHighScore>, String> {
        let peer = self.bots_input_peer(peer_id)?;
        let tl_user = self.bots_input_user(user_id)?;
        let resp = self.bots_invoke_rpc(tl_fns::messages::GetGameHighScores {
            peer,
            id: msg_id.0,
            user_id: tl_user,
        })?;
        let tl_enums::messages::HighScores::HighScores(ref hs) = resp;
        self.bots_write_session(|session| {
            apply_tl_users_and_chats(session, &hs.users, &[]);
        });
        Ok(game_high_scores_from_tl(&resp))
    }

    /// Whether `peer` is a bot whose `users.getFullUser` (with `bot_info`) is not loaded yet.
    pub(crate) fn bot_full_info_needed(&self, peer: DataPeerId) -> bool {
        let Some(uid) = peer.to_user() else {
            return false;
        };
        self.bots_read_session(|s| {
            s.user(uid).is_some_and(|u| {
                u.is_bot()
                    && (u.common.loaded_status < LoadedStatus::Full
                        || u.bot_info.as_ref().is_none_or(|b| !b.inited))
            })
        })
    }

    /// Loads the bot's full info if [`Self::bot_full_info_needed`] (used by
    /// [`crate::open_chat_prefetch`]).
    pub(crate) fn load_bot_full_info_now(&self, peer: DataPeerId) {
        let Some(uid) = peer.to_user() else {
            return;
        };
        if self.bot_full_info_needed(peer)
            && let Err(err) = self.fetch_bot_full_info(BotUserId(uid.0))
        {
            debug!(bot = uid.0, error = %err, "users.getFullUser for bot failed");
        }
    }

    fn ensure_bot_full_loaded(&self, bot_id: BotUserId) {
        let peer = DataPeerId::from_user(DataUserId(bot_id.0));
        if !crate::open_chat_prefetch::started_in_background(
            self,
            peer,
            crate::open_chat_prefetch::OpenChatRequest::BotInfo,
        ) {
            self.load_bot_full_info_now(peer);
        }
    }

    fn build_bot_chat_snapshot(&self, target_peer: DataPeerId) -> Option<BotChatSnapshot> {
        if let Some(uid) = target_peer.to_user() {
            self.ensure_bot_full_loaded(BotUserId(uid.0));
        }

        let state = bots_state_for_session(self.runtime().session());
        let menu_overrides = state
            .lock()
            .map(|s| s.menu_button_overrides.clone())
            .unwrap_or_default();

        self.bots_read_session(|session| {
            let peer = session.peer(target_peer)?;
            let self_peer_id = session.self_peer_id();
            let (current_user_name, current_user_phone) = session
                .peer(self_peer_id)
                .map(|p| {
                    let phone = p.as_user().map(|u| u.phone.clone()).unwrap_or_default();
                    (p.name().to_owned(), phone)
                })
                .unwrap_or_default();

            let (
                peer_kind,
                peer_bot_status,
                bot_id,
                bot_name,
                bot_username,
                bot_verified,
                mut bot_info,
            ) = match peer {
                Peer::User(u) => {
                    let kind = if u.is_bot() {
                        BotPeerKind::Bot
                    } else {
                        BotPeerKind::User
                    };
                    let info = u
                        .bot_info
                        .as_ref()
                        .map(bot_info_from_data)
                        .unwrap_or_default();
                    (
                        kind,
                        BotStatus::NoBots,
                        BotUserId(u.user_id().0),
                        u.common.name.clone(),
                        u.usernames.username().to_owned(),
                        u.is_verified(),
                        info,
                    )
                }
                Peer::Chat(c) => (
                    BotPeerKind::Group,
                    match c.bot_status {
                        tg_data::BotStatus::NoBots => BotStatus::NoBots,
                        tg_data::BotStatus::Unknown => BotStatus::Unknown,
                        tg_data::BotStatus::HasBots => BotStatus::HasBots,
                    },
                    BotUserId(0),
                    c.common.name.clone(),
                    String::new(),
                    false,
                    BotInfo::default(),
                ),
                Peer::Channel(ch) => (
                    if ch.is_broadcast() {
                        BotPeerKind::Broadcast
                    } else {
                        BotPeerKind::Group
                    },
                    BotStatus::Unknown,
                    BotUserId(0),
                    ch.common.name.clone(),
                    ch.usernames.username().to_owned(),
                    ch.is_verified(),
                    BotInfo::default(),
                ),
            };

            if let Some(override_btn) = menu_overrides.get(&bot_id.0) {
                bot_info.menu_button = override_btn.clone();
            }

            let mut scoped_commands = ScopedBotCommands::new();
            if !bot_info.commands.is_empty() {
                scoped_commands.set(BotCommandScope::Default, "", bot_info.commands.clone());
            }

            let history_msgs = history_messages_ordered(session, target_peer);
            let messages: Vec<BotChatMessage> = history_msgs
                .iter()
                .map(|m| data_message_to_bot_chat_message(session, m))
                .collect();
            let active_reply_keyboard = find_active_reply_keyboard(&history_msgs);

            Some(BotChatSnapshot {
                peer_id: BotPeerId(target_peer.0),
                peer_kind,
                peer_bot_status,
                bot_id,
                bot_name,
                bot_username,
                bot_verified,
                bot_trusted_for_game: bot_verified,
                bot_info,
                scoped_commands,
                current_user_name,
                current_user_phone,
                messages,
                active_reply_keyboard,
            })
        })
    }

    fn resolve_latest_sent_bot_message(
        &self,
        peer_id: BotPeerId,
        new_ids: &[DataMsgId],
        fallback_text: String,
    ) -> BotChatMessage {
        let data_peer = DataPeerId(peer_id.0);
        if let Some(&mid) = new_ids.last()
            && let Some(msg) = self.bots_read_session(|s| {
                s.message(DataFullMsgId::new(data_peer, mid))
                    .map(|m| data_message_to_bot_chat_message(s, m))
            })
        {
            return msg;
        }
        let msg_id = new_ids
            .last()
            .and_then(|m| m.as_i32())
            .unwrap_or_else(now_unix_i32);
        BotChatMessage {
            id: BotFullMsgId::new(peer_id, BotMsgId(msg_id)),
            author: "You".to_owned(),
            outgoing: true,
            time_label: format_short_time(now_unix_i32()),
            text: fallback_text,
            attachment: BotMessageAttachment::None,
            inline_markup: None,
            reply_markup: None,
            via_bot: None,
        }
    }
}

fn data_message_to_bot_chat_message(session: &Session, msg: &Message) -> BotChatMessage {
    let author = msg
        .from_id
        .map(|pid| {
            if pid == session.self_peer_id() {
                "You".to_owned()
            } else {
                peer_display_title(session, pid)
            }
        })
        .unwrap_or_else(|| peer_display_title(session, msg.peer_id));
    let text = msg.text().map(|t| t.text.clone()).unwrap_or_default();
    let attachment = match msg.media() {
        Some(MessageMedia::Game(g)) => BotMessageAttachment::Game {
            game: game_data_from_data(g),
            high_scores: Vec::new(),
        },
        _ => BotMessageAttachment::None,
    };
    let parsed_markup = msg.reply_markup.as_ref().map(reply_markup_from_data);
    let (inline_markup, reply_markup) = match parsed_markup {
        Some(m) if m.is_inline() => (Some(m), None),
        Some(m) => (None, Some(m)),
        None => (None, None),
    };
    let via_bot = msg.via_bot_id.and_then(|uid| {
        session
            .user(uid)
            .map(|u| u.usernames.username())
            .filter(|u| !u.is_empty())
            .map(|u| format!("@{u}"))
    });
    BotChatMessage {
        id: BotFullMsgId::new(
            BotPeerId(msg.peer_id.0),
            BotMsgId(msg.id.as_i32().unwrap_or(0)),
        ),
        author,
        outgoing: msg.is_outgoing(),
        time_label: format_short_time(msg.date),
        text,
        attachment,
        inline_markup,
        reply_markup,
        via_bot,
    }
}

fn find_active_reply_keyboard(messages: &[&Message]) -> Option<(BotFullMsgId, ReplyMarkupData)> {
    for msg in messages.iter().rev() {
        let Some(ref raw) = msg.reply_markup else {
            continue;
        };
        let converted = reply_markup_from_data(raw);
        if converted.is_inline() {
            continue;
        }
        if converted.is_remove_keyboard() {
            return None;
        }
        if !converted.rows.is_empty() || converted.is_force_reply() {
            return Some((
                BotFullMsgId::new(
                    BotPeerId(msg.peer_id.0),
                    BotMsgId(msg.id.as_i32().unwrap_or(0)),
                ),
                converted,
            ));
        }
    }
    None
}

impl BotsBackend for LiveSession {
    fn load_bot_chat(&self) -> BotChatSnapshot {
        let preferred = bots_state_for_session(self.runtime().session())
            .lock()
            .ok()
            .and_then(|s| s.active_bot_peer);
        if let Some(pid) = preferred
            && let Some(snap) = self.build_bot_chat_snapshot(DataPeerId(pid.0))
        {
            return snap;
        }
        let first_bot = self.bots_read_session(|session| {
            known_peer_ids(session).into_iter().find(|&pid| {
                session
                    .peer(pid)
                    .and_then(Peer::as_user)
                    .is_some_and(tg_data::User::is_bot)
            })
        });
        if let Some(pid) = first_bot
            && let Some(snap) = self.build_bot_chat_snapshot(pid)
        {
            return snap;
        }
        let self_pid = self.bots_read_session(Session::self_peer_id);
        self.build_bot_chat_snapshot(self_pid)
            .unwrap_or_else(|| BotChatSnapshot {
                peer_id: BotPeerId(self_pid.0),
                peer_kind: BotPeerKind::User,
                peer_bot_status: BotStatus::NoBots,
                bot_id: BotUserId(0),
                bot_name: String::new(),
                bot_username: String::new(),
                bot_verified: false,
                bot_trusted_for_game: false,
                bot_info: BotInfo::default(),
                scoped_commands: ScopedBotCommands::new(),
                current_user_name: String::new(),
                current_user_phone: String::new(),
                messages: Vec::new(),
                active_reply_keyboard: None,
            })
    }

    fn load_attach_menu(&self) -> AttachMenuStore {
        let state = bots_state_for_session(self.runtime().session());
        let already_loaded = state.lock().map(|s| s.attach_menu_loaded).unwrap_or(false);
        if !already_loaded {
            match self.fetch_attach_menu_bots() {
                Ok(store) => return store,
                Err(err) => {
                    debug!(error = %err, "messages.getAttachMenuBots failed");
                    if let Ok(mut guard) = state.lock() {
                        guard.attach_menu_loaded = true;
                    }
                }
            }
        }
        state
            .lock()
            .map(|s| s.attach_menu.clone())
            .unwrap_or_default()
    }

    fn load_attach_menu_bot(
        &self,
        bot_id: BotUserId,
    ) -> Result<tg_bots::mini_app::AttachMenuBot, String> {
        crate::live_extras::get_attach_menu_bot(self, bot_id)
    }

    fn candidate_peers(&self) -> Vec<BotPeerCandidate> {
        let now = now_unix_i32();
        self.bots_read_session(|session| {
            let self_id = session.self_peer_id();
            let send_ctx = session.send_context();
            known_peer_ids(session)
                .into_iter()
                .filter_map(|pid| {
                    let peer = session.peer(pid)?;
                    let title = peer_display_title(session, pid);
                    let (subtitle, _) = peer_status_text(peer, now);
                    let is_self = pid == self_id;
                    let can_send_games = peer.can_write(send_ctx);
                    let (peer_kind, is_forum, criteria) = match peer {
                        Peer::User(u) => (
                            if u.is_bot() {
                                BotPeerKind::Bot
                            } else {
                                BotPeerKind::User
                            },
                            false,
                            RequestPeerCandidate {
                                peer_type: RequestPeerType::User,
                                is_bot: u.is_bot(),
                                is_premium: u.is_premium(),
                                is_forum: false,
                                has_username: !u.usernames.username().is_empty(),
                                am_creator: false,
                                is_bot_participant: false,
                                my_rights: tg_bots::stand_in::ChatAdminRights::empty(),
                                bot_rights: tg_bots::stand_in::ChatAdminRights::empty(),
                            },
                        ),
                        Peer::Chat(c) => (
                            BotPeerKind::Group,
                            false,
                            RequestPeerCandidate {
                                peer_type: RequestPeerType::Group,
                                is_bot: false,
                                is_premium: false,
                                is_forum: false,
                                has_username: false,
                                am_creator: c.am_creator(),
                                is_bot_participant: c.bot_status == tg_data::BotStatus::HasBots,
                                my_rights: chat_admin_rights_from_data(c.admin_rights),
                                bot_rights: tg_bots::stand_in::ChatAdminRights::empty(),
                            },
                        ),
                        Peer::Channel(ch) => {
                            let is_broadcast = ch.is_broadcast();
                            let forum = ch.is_forum();
                            (
                                if is_broadcast {
                                    BotPeerKind::Broadcast
                                } else {
                                    BotPeerKind::Group
                                },
                                forum,
                                RequestPeerCandidate {
                                    peer_type: if is_broadcast {
                                        RequestPeerType::Broadcast
                                    } else {
                                        RequestPeerType::Group
                                    },
                                    is_bot: false,
                                    is_premium: false,
                                    is_forum: forum,
                                    has_username: !ch.usernames.username().is_empty(),
                                    am_creator: ch.am_creator(),
                                    is_bot_participant: false,
                                    my_rights: chat_admin_rights_from_data(ch.admin_rights),
                                    bot_rights: tg_bots::stand_in::ChatAdminRights::empty(),
                                },
                            )
                        }
                    };
                    Some(BotPeerCandidate {
                        peer_id: BotPeerId(pid.0),
                        title,
                        subtitle,
                        peer_kind,
                        is_self,
                        can_send_games,
                        is_forum,
                        criteria,
                    })
                })
                .collect()
        })
    }

    fn send_callback_query(
        &mut self,
        request: &CallbackQueryRequest,
    ) -> Result<CallbackAnswerInput, String> {
        let peer = self.bots_input_peer(request.peer_id)?;
        if let Some(ephemeral_id) = request.ephemeral_id {
            // Port of `SendBotCallbackData` (`api/api_bot.cpp:173-181`): only set `f_data`
            // on `ephemeral.getCallbackAnswer` when payload bytes are non-empty.
            let data = request.data.clone().filter(|d| !d.is_empty());
            let resp = self.bots_invoke_rpc(tl_fns::ephemeral::GetCallbackAnswer {
                peer,
                id: ephemeral_id,
                data,
            })?;
            return Ok(callback_answer_from_tl(&resp));
        }

        let password = if request.requires_password {
            let staged = bots_state_for_session(self.runtime().session())
                .lock()
                .ok()
                .and_then(|mut s| s.pending_callback_password.take());
            if let Some(pwd) = staged {
                Some(self.compute_srp_password_check(&pwd)?)
            } else {
                Some(tl_enums::InputCheckPasswordSRP::InputCheckPasswordEmpty)
            }
        } else {
            None
        };

        // Port of `SendBotCallbackData` (`api/api_bot.cpp:80-86`): `f_game` and `f_data`
        // are mutually exclusive; game callbacks must omit `data`.
        let data = if request.is_game {
            None
        } else {
            request.data.clone()
        };
        let resp = self.bots_invoke_rpc(tl_fns::messages::GetBotCallbackAnswer {
            game: request.is_game,
            peer,
            msg_id: request.msg_id.0,
            data,
            password,
        })?;
        Ok(callback_answer_from_tl(&resp))
    }

    fn request_url_auth(&mut self, source: &UrlAuthSource) -> Result<UrlAuthResultInput, String> {
        let req = match source {
            UrlAuthSource::Button {
                peer_id,
                msg_id,
                button_id,
                ..
            } => tl_fns::messages::RequestUrlAuth {
                peer: Some(self.bots_input_peer(*peer_id)?),
                msg_id: Some(msg_id.0),
                button_id: Some(*button_id as i32),
                url: None,
                in_app_origin: None,
            },
            UrlAuthSource::Url { url } => tl_fns::messages::RequestUrlAuth {
                peer: None,
                msg_id: None,
                button_id: None,
                url: Some(url.clone()),
                in_app_origin: None,
            },
        };
        let resp = self.bots_invoke_rpc(req)?;
        if let tl_enums::UrlAuthResult::Request(ref r) = resp {
            self.bots_write_session(|session| {
                apply_tl_users_and_chats(session, std::slice::from_ref(&r.bot), &[]);
            });
        }
        Ok(url_auth_result_from_tl(&resp))
    }

    fn accept_url_auth(
        &mut self,
        params: &AcceptUrlAuthParams,
    ) -> Result<UrlAuthResultInput, String> {
        let (peer, msg_id, button_id, url) = match &params.source {
            UrlAuthSource::Button {
                peer_id,
                msg_id,
                button_id,
                ..
            } => (
                Some(self.bots_input_peer(*peer_id)?),
                Some(msg_id.0),
                Some(*button_id as i32),
                None,
            ),
            UrlAuthSource::Url { url } => (None, None, None, Some(url.clone())),
        };
        // Port of `UrlAuthBox::RequestButton` / `RequestUrl` (`boxes/url_auth_box.cpp:398,522`):
        // only set `f_match_code` when the match code string is non-empty.
        let match_code = params.match_code.clone().filter(|s| !s.is_empty());
        let resp = self.bots_invoke_rpc(tl_fns::messages::AcceptUrlAuth {
            write_allowed: params.write_allowed,
            share_phone_number: params.share_phone_number,
            peer,
            msg_id,
            button_id,
            url,
            match_code,
            tonconnect_session: None,
        })?;
        Ok(url_auth_result_from_tl(&resp))
    }

    fn decline_url_auth(&mut self, url: &str) -> Result<(), String> {
        let _ = self.bots_invoke_rpc(tl_fns::messages::DeclineUrlAuth {
            url: url.to_owned(),
        })?;
        Ok(())
    }

    fn query_inline_results(
        &mut self,
        request: &GetInlineBotResultsRequest,
    ) -> Result<InlineResultsPage, String> {
        let bot = self.bots_input_user(request.bot_id)?;
        let peer = self.bots_input_peer(request.peer_id)?;
        let resp = self.bots_invoke_rpc(tl_fns::messages::GetInlineBotResults {
            bot,
            peer,
            geo_point: None,
            query: request.query.clone(),
            offset: request.offset.clone(),
        })?;
        let tl_enums::messages::BotResults::BotResults(ref br) = resp;
        self.bots_write_session(|session| {
            apply_tl_users_and_chats(session, &br.users, &[]);
        });
        Ok(bot_results_from_tl(&request.query, &resp))
    }

    fn send_inline_result(
        &mut self,
        _bot_id: BotUserId,
        peer_id: BotPeerId,
        result: &InlineResult,
    ) -> Result<BotChatMessage, String> {
        let peer = self.bots_input_peer(peer_id)?;
        let random_id = self.bots_next_random_id();
        let updates = self.bots_invoke_rpc(tl_fns::messages::SendInlineBotResult {
            silent: false,
            background: false,
            clear_draft: true,
            hide_via: false,
            peer,
            reply_to: None,
            random_id,
            query_id: result.query_id as i64,
            id: result.id.clone(),
            schedule_date: None,
            send_as: None,
            quick_reply_shortcut: None,
            allow_paid_stars: None,
        })?;
        let ids = self.bots_apply_updates(&updates);
        let fallback = if result.title.is_empty() {
            format!("Inline result {}", result.id)
        } else {
            result.title.clone()
        };
        Ok(self.resolve_latest_sent_bot_message(peer_id, &ids, fallback))
    }

    fn send_bot_command(
        &mut self,
        peer_id: BotPeerId,
        text: &str,
        reply_to: Option<BotFullMsgId>,
    ) -> Result<BotChatMessage, String> {
        let peer = self.bots_input_peer(peer_id)?;
        let random_id = self.bots_next_random_id();
        let tl_reply = reply_to.map(|r| {
            tl_enums::InputReplyTo::Message(tl_types::InputReplyToMessage {
                reply_to_msg_id: r.msg.0,
                top_msg_id: None,
                reply_to_peer_id: None,
                quote_text: None,
                quote_entities: None,
                quote_offset: None,
                monoforum_peer_id: None,
                todo_item_id: None,
                poll_option: None,
                quote_rich: None,
            })
        });
        let updates = self.bots_invoke_rpc(tl_fns::messages::SendMessage {
            no_webpage: false,
            silent: false,
            background: false,
            clear_draft: true,
            noforwards: false,
            update_stickersets_order: false,
            invert_media: false,
            allow_paid_floodskip: false,
            peer,
            reply_to: tl_reply,
            message: text.to_owned(),
            random_id,
            reply_markup: None,
            entities: None,
            schedule_date: None,
            schedule_repeat_period: None,
            send_as: None,
            quick_reply_shortcut: None,
            effect: None,
            allow_paid_stars: None,
            suggested_post: None,
            rich_message: None,
        })?;
        let ids = self.bots_apply_updates(&updates);
        Ok(self.resolve_latest_sent_bot_message(peer_id, &ids, text.to_owned()))
    }

    fn share_phone(
        &mut self,
        peer_id: BotPeerId,
        reply_to: Option<BotMsgId>,
    ) -> Result<BotChatMessage, String> {
        let peer = self.bots_input_peer(peer_id)?;
        let (phone_number, first_name, last_name) = self.bots_read_session(|s| {
            s.user(s.self_user_id())
                .map(|u| (u.phone.clone(), u.first_name.clone(), u.last_name.clone()))
                .unwrap_or_default()
        });
        let random_id = self.bots_next_random_id();
        let tl_reply = reply_to.map(|mid| {
            tl_enums::InputReplyTo::Message(tl_types::InputReplyToMessage {
                reply_to_msg_id: mid.0,
                top_msg_id: None,
                reply_to_peer_id: None,
                quote_text: None,
                quote_entities: None,
                quote_offset: None,
                monoforum_peer_id: None,
                todo_item_id: None,
                poll_option: None,
                quote_rich: None,
            })
        });
        let updates = self.bots_invoke_rpc(tl_fns::messages::SendMedia {
            silent: false,
            background: false,
            clear_draft: false,
            noforwards: false,
            update_stickersets_order: false,
            invert_media: false,
            allow_paid_floodskip: false,
            peer,
            reply_to: tl_reply,
            media: tl_enums::InputMedia::Contact(tl_types::InputMediaContact {
                phone_number: phone_number.clone(),
                first_name: first_name.clone(),
                last_name,
                vcard: String::new(),
            }),
            message: String::new(),
            random_id,
            reply_markup: None,
            entities: None,
            schedule_date: None,
            schedule_repeat_period: None,
            send_as: None,
            quick_reply_shortcut: None,
            effect: None,
            allow_paid_stars: None,
            suggested_post: None,
        })?;
        let ids = self.bots_apply_updates(&updates);
        Ok(self.resolve_latest_sent_bot_message(
            peer_id,
            &ids,
            format!("Shared contact: {first_name} ({phone_number})"),
        ))
    }

    fn send_poll(
        &mut self,
        peer_id: BotPeerId,
        poll: &SubmittedPoll,
    ) -> Result<BotChatMessage, String> {
        if poll.question.trim().is_empty() || poll.options.len() < 2 {
            return Err("Poll requires a question and at least two options.".to_owned());
        }
        let peer = self.bots_input_peer(peer_id)?;
        let random_id = self.bots_next_random_id();
        let answers: Vec<tl_enums::PollAnswer> = poll
            .options
            .iter()
            .enumerate()
            .map(|(idx, text)| {
                tl_enums::PollAnswer::PollAnswer(tl_types::PollAnswer {
                    text: tl_enums::TextWithEntities::TextWithEntities(
                        tl_types::TextWithEntities {
                            text: text.clone(),
                            entities: Vec::new(),
                        },
                    ),
                    option: vec![idx as u8],
                    media: None,
                    added_by: None,
                    date: None,
                })
            })
            .collect();
        let correct_answers = poll.correct_option.map(|idx| vec![idx as i32]);
        let tl_poll = tl_enums::Poll::Poll(tl_types::Poll {
            id: random_id,
            closed: false,
            public_voters: false,
            multiple_choice: false,
            quiz: poll.is_quiz,
            open_answers: false,
            revoting_disabled: false,
            shuffle_answers: false,
            hide_results_until_close: false,
            creator: true,
            subscribers_only: false,
            question: tl_enums::TextWithEntities::TextWithEntities(tl_types::TextWithEntities {
                text: poll.question.trim().to_owned(),
                entities: Vec::new(),
            }),
            answers,
            close_period: None,
            close_date: None,
            countries_iso2: None,
            hash: 0,
        });
        let updates = self.bots_invoke_rpc(tl_fns::messages::SendMedia {
            silent: false,
            background: false,
            clear_draft: false,
            noforwards: false,
            update_stickersets_order: false,
            invert_media: false,
            allow_paid_floodskip: false,
            peer,
            reply_to: None,
            media: tl_enums::InputMedia::Poll(tl_types::InputMediaPoll {
                poll: tl_poll,
                correct_answers,
                attached_media: None,
                solution: None,
                solution_entities: None,
                solution_media: None,
            }),
            message: String::new(),
            random_id,
            reply_markup: None,
            entities: None,
            schedule_date: None,
            schedule_repeat_period: None,
            send_as: None,
            quick_reply_shortcut: None,
            effect: None,
            allow_paid_stars: None,
            suggested_post: None,
        })?;
        let ids = self.bots_apply_updates(&updates);
        Ok(self.resolve_latest_sent_bot_message(peer_id, &ids, poll.question.trim().to_owned()))
    }

    fn send_requested_peers(
        &mut self,
        item_id: BotFullMsgId,
        button_id: i64,
        selected_peers: &[BotPeerId],
    ) -> Result<BotChatMessage, String> {
        if selected_peers.is_empty() {
            return Err("Select at least one peer.".to_owned());
        }
        let peer = self.bots_input_peer(item_id.peer)?;
        let mut requested_peers = Vec::with_capacity(selected_peers.len());
        for &pid in selected_peers {
            requested_peers.push(self.bots_input_peer(pid)?);
        }
        let updates = self.bots_invoke_rpc(tl_fns::messages::SendBotRequestedPeer {
            peer,
            msg_id: Some(item_id.msg.0),
            webapp_req_id: None,
            button_id: button_id as i32,
            requested_peers,
        })?;
        let ids = self.bots_apply_updates(&updates);
        Ok(self.resolve_latest_sent_bot_message(
            item_id.peer,
            &ids,
            format!("Shared {} peer(s)", selected_peers.len()),
        ))
    }

    fn toggle_attach_menu_bot(
        &mut self,
        bot_id: BotUserId,
        state: AttachMenuToggleState,
    ) -> Result<AttachMenuStore, String> {
        let bot = self.bots_input_user(bot_id)?;
        let enabled = !matches!(state, AttachMenuToggleState::Removed);
        let write_allowed = matches!(state, AttachMenuToggleState::AllowedToWrite);
        let _ = self.bots_invoke_rpc(tl_fns::messages::ToggleBotInAttachMenu {
            write_allowed,
            bot,
            enabled,
        })?;
        let bots_state = bots_state_for_session(self.runtime().session());
        let needs_single_fetch = if let Ok(mut guard) = bots_state.lock() {
            let found = if let Some(entry) = guard
                .attach_menu
                .bots
                .iter_mut()
                .find(|b| b.bot_id == bot_id)
            {
                entry.inactive = !enabled;
                true
            } else {
                false
            };
            guard.attach_menu_loaded = true;
            enabled && !found
        } else {
            false
        };
        if needs_single_fetch {
            let _ = crate::live_extras::get_attach_menu_bot(self, bot_id);
        }
        if let Ok(guard) = bots_state.lock() {
            return Ok(guard.attach_menu.clone());
        }
        Ok(AttachMenuStore::new())
    }

    fn accept_attach_disclaimer(&mut self, bot_id: BotUserId) {
        let state = bots_state_for_session(self.runtime().session());
        if let Ok(mut guard) = state.lock() {
            guard.attach_menu.mark_disclaimer_accepted(bot_id);
        }
    }

    fn share_game_score(
        &mut self,
        message_id: BotFullMsgId,
        target_peer: BotPeerId,
    ) -> Result<String, String> {
        let from_peer = self.bots_input_peer(message_id.peer)?;
        let to_peer = self.bots_input_peer(target_peer)?;
        let random_id = self.bots_next_random_id();
        let updates = self.bots_invoke_rpc(tl_fns::messages::ForwardMessages {
            silent: false,
            background: false,
            with_my_score: true,
            drop_author: false,
            drop_media_captions: false,
            noforwards: false,
            allow_paid_floodskip: false,
            from_ephemeral: false,
            from_peer,
            id: vec![message_id.msg.0],
            random_id: vec![random_id],
            to_peer,
            top_msg_id: None,
            reply_to: None,
            schedule_date: None,
            schedule_repeat_period: None,
            send_as: None,
            quick_reply_shortcut: None,
            effect: None,
            video_timestamp: None,
            allow_paid_stars: None,
            suggested_post: None,
        })?;
        let _ = self.bots_apply_updates(&updates);
        let title = self.bots_read_session(|s| peer_display_title(s, DataPeerId(target_peer.0)));
        Ok(format!("Shared game score with {title}"))
    }

    fn set_menu_button(&mut self, button: BotMenuButton) {
        let state = bots_state_for_session(self.runtime().session());
        if let Ok(mut guard) = state.lock()
            && let Some(pid) = guard.active_bot_peer
            && let Some(uid) = DataPeerId(pid.0).to_user()
        {
            guard.menu_button_overrides.insert(uid.0, button);
        }
        self.bridge_state().notify_ui();
    }

    fn resolve_user_name(&self, user_id: BotUserId) -> String {
        self.bots_read_session(|s| {
            peer_display_title(s, DataPeerId::from_user(DataUserId(user_id.0)))
        })
    }

    fn commands_for_chat(&self) -> Vec<BotCommand> {
        let snap = self.load_bot_chat();
        snap.bot_info.commands
    }

    fn load_bot_chat_for_peer(&self, peer_id: BotPeerId) -> Option<BotChatSnapshot> {
        self.build_bot_chat_snapshot(DataPeerId(peer_id.0))
    }

    fn resolve_inline_bot(&self, username: &str) -> Option<(BotUserId, String)> {
        let needle = username.trim().trim_start_matches('@');
        if needle.is_empty() {
            return None;
        }
        let local = self.bots_read_session(|session| {
            for pid in known_peer_ids(session) {
                if let Some(user) = session.peer(pid).and_then(Peer::as_user)
                    && user.is_bot()
                    && user.usernames.username().eq_ignore_ascii_case(needle)
                {
                    return Some((
                        BotUserId(user.user_id().0),
                        user.usernames.username().to_owned(),
                    ));
                }
            }
            None
        });
        if local.is_some() {
            return local;
        }
        match self.bots_invoke_rpc(tl_fns::contacts::ResolveUsername {
            username: needle.to_owned(),
            referer: None,
        }) {
            Ok(tl_enums::contacts::ResolvedPeer::ResolvedPeer(rp)) => {
                self.bots_write_session(|session| {
                    apply_tl_users_and_chats(session, &rp.users, &rp.chats);
                });
                let resolved_pid = tg_api::convert::peer_id_from_tl(&rp.peer);
                self.bots_read_session(|session| {
                    session
                        .peer(resolved_pid)
                        .and_then(Peer::as_user)
                        .filter(|u| u.is_bot())
                        .map(|u| (BotUserId(u.user_id().0), u.usernames.username().to_owned()))
                })
            }
            Err(err) => {
                warn!(username = needle, error = %err, "contacts.resolveUsername for inline bot failed");
                None
            }
        }
    }

    fn commands_for_peer(&self, peer_id: BotPeerId) -> Vec<BotCommand> {
        let data_peer = DataPeerId(peer_id.0);
        if let Some(uid) = data_peer.to_user() {
            self.ensure_bot_full_loaded(BotUserId(uid.0));
        }
        self.bots_read_session(|session| {
            session
                .peer(data_peer)
                .and_then(Peer::as_user)
                .and_then(|u| u.bot_info.as_ref())
                .map(|bi| {
                    bi.commands
                        .iter()
                        .map(|c| BotCommand::new(c.command.clone(), c.description.clone()))
                        .collect()
                })
                .unwrap_or_default()
        })
    }

    fn active_reply_keyboard_for_peer(
        &self,
        peer_id: BotPeerId,
    ) -> Option<(BotFullMsgId, ReplyMarkupData)> {
        self.bots_read_session(|session| {
            let msgs = history_messages_ordered(session, DataPeerId(peer_id.0));
            find_active_reply_keyboard(&msgs)
        })
    }

    fn start_bot(&mut self, params: &StartBotParams) -> Result<BotChatMessage, String> {
        // Port of `ApiWrap::sendBotStart` (`Telegram/SourceFiles/apiwrap.cpp:5096-5139`).
        let (target_peer_kind, bot_username) = self.bots_read_session(|session| {
            let kind = params.peer_id.and_then(|pid| {
                session.peer(DataPeerId(pid.0)).map(|peer| match peer {
                    Peer::User(u) if u.is_bot() => BotPeerKind::Bot,
                    Peer::User(_) => BotPeerKind::User,
                    Peer::Chat(_) => BotPeerKind::Group,
                    Peer::Channel(ch) if ch.is_broadcast() => BotPeerKind::Broadcast,
                    Peer::Channel(_) => BotPeerKind::Group,
                })
            });
            let uname = session
                .user(DataUserId(params.bot_id.0))
                .map(|u| u.usernames.username().to_owned())
                .unwrap_or_default();
            (kind, uname)
        });

        let bot_dm_peer = BotPeerId(DataPeerId::from_user(DataUserId(params.bot_id.0)).0);
        match params
            .resolve_dispatch(target_peer_kind, &bot_username)
            .map_err(|e| e.to_string())?
        {
            StartBotDispatch::SendPlainStart { peer_id, text } => {
                let target = peer_id.unwrap_or(bot_dm_peer);
                self.send_bot_command(target, &text, None)
            }
            StartBotDispatch::InvokeStartBot(start_params) => {
                let bot = self.bots_input_user(start_params.bot_id)?;
                // `apiwrap.cpp:5128`: for 1:1 `?start=` (chat == nullptr), tdesktop passes
                // `MTP_inputPeerEmpty()`; for group `?startgroup=`, it passes `chat->input()`.
                let peer = match start_params.peer_id {
                    Some(chat_pid) => self.bots_input_peer(chat_pid)?,
                    None => tl_enums::InputPeer::Empty,
                };
                let random_id = if start_params.random_id != 0 {
                    start_params.random_id
                } else {
                    self.bots_next_random_id()
                };
                if start_params.peer_id.is_none() {
                    // `apiwrap.cpp:5123-5125`: clear cached `start_token` on 1:1 bot start.
                    self.bots_write_session(|session| {
                        if let Some(mut user) =
                            session.user(DataUserId(start_params.bot_id.0)).cloned()
                            && let Some(ref mut bi) = user.bot_info
                            && !bi.start_token.is_empty()
                        {
                            bi.start_token.clear();
                            session.upsert_user(user, tg_data::PeerUpdateFlags::IS_BOT);
                        }
                    });
                }
                let req = start_params.to_tl_request(bot, peer, random_id);
                let updates = self.bots_invoke_rpc(req)?;
                let ids = self.bots_apply_updates(&updates);
                let target = start_params.peer_id.unwrap_or(bot_dm_peer);
                Ok(self.resolve_latest_sent_bot_message(target, &ids, "/start".to_owned()))
            }
        }
    }

    fn check_url_auth_match_code(
        &mut self,
        url: &str,
        match_code: &str,
    ) -> Result<UrlAuthMatchCodeResult, String> {
        // Port of `UrlAuthBox::RequestUrl` (`Telegram/SourceFiles/boxes/url_auth_box.cpp:913-931`).
        match self.bots_invoke_rpc(tl_fns::messages::CheckUrlAuthMatchCode {
            url: url.to_owned(),
            match_code: match_code.to_owned(),
        }) {
            Ok(matched) => Ok(UrlAuthMatchCodeResult::from_rpc(match_code, Ok(matched))),
            Err(err) => match UrlAuthMatchCodeResult::from_rpc(match_code, Err(&err)) {
                UrlAuthMatchCodeResult::Expired => Ok(UrlAuthMatchCodeResult::Expired),
                _ => Err(err),
            },
        }
    }

    fn get_bot_recommendations(&mut self, bot_id: BotUserId) -> Result<BotRecommendations, String> {
        // Port of `Api::ChatParticipants::loadSimilar` (`api/api_chat_participants.cpp:890-903`).
        let bot = self.bots_input_user(bot_id)?;
        let resp = self.bots_invoke_rpc(tl_fns::bots::GetBotRecommendations { bot })?;
        let tl_users = match &resp {
            tl_enums::users::Users::Users(u) => u.users.as_slice(),
            tl_enums::users::Users::Slice(s) => s.users.as_slice(),
        };
        if !tl_users.is_empty() {
            self.bots_write_session(|session| {
                apply_tl_users_and_chats(session, tl_users, &[]);
            });
            self.bridge_state().notify_ui();
        }
        Ok(bot_recommendations_from_tl(&resp))
    }
}
