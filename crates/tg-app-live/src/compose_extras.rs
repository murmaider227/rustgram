//! Live compose extras for [`LiveSession`]: Send-As peers (`channels.getSendAs`,
//! `messages.saveDefaultSendAs`), slow mode (`ChannelFull.slowmode_seconds` /
//! `slowmode_next_send_date` and send blocking), link previews (`messages.getWebPagePreview`
//! and `InputMediaWebPage` media size/placement options), translation (`messages.translateText`,
//! `messages.togglePeerTranslations`, "Do not translate" languages), reactions
//! (`messages.getAvailableReactions` cached by hash, multi-reaction toggle,
//! `messages.getMessageReactionsList`, `messages.setDefaultReaction`,
//! `messages.getSavedReactionTags`), and polls (`messages.getPollVotes`, retract vote via
//! empty `messages.sendVote`, stop poll via `messages.editMessage` with `closed = true`).
//!
//! Port origin:
//! - `Telegram/SourceFiles/main/session/send_as_peers.cpp`
//! - `Telegram/SourceFiles/history/history_widget.cpp` (slowmode countdown & send blocking)
//! - `Telegram/SourceFiles/history/view/controls/history_view_webpage_processor.cpp`
//! - `Telegram/SourceFiles/history/view/history_view_translate_bar.cpp`
//! - `Telegram/SourceFiles/api/api_who_reacted.cpp`
//! - `Telegram/SourceFiles/data/data_message_reactions.cpp`
//! - `Telegram/SourceFiles/api/api_polls.cpp`

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

use tg_api::convert::{
    apply_tl_chat_full, apply_tl_users_and_chats, input_channel_for, input_peer_for,
    peer_id_from_tl,
};
use tg_api::{PendingMessageState, RpcClient, SendOptions};
use tg_data::{
    DocumentId, FullMsgId as DataFullMsgId, MessageContent, MessageMedia, MsgId as DataMsgId, Peer,
    PeerId as DataPeerId, ReactionId, Session,
};
use tg_tl::{RemoteCall, enums as tl_enums, functions as tl_fns, types as tl_types};
use tracing::debug;

use crate::bridge::LiveBridgeState;
use crate::convert::{
    avatar_rgb_for_color_index, from_data_peer, message_to_history_message, now_unix_i32,
    now_unix_i64, peer_display_title, peer_primary_username, to_data_msg, to_data_peer,
    ui_text_to_tl_entities,
};
use crate::media::{block_on_runtime, download_file_bytes, with_rpc_timeout};
use crate::media_cache::decompress_tgs_to_lottie_json;
use crate::session::LiveSession;
use tg_app_core::dialogs::backend::SearchTagChip;
use tg_app_core::history::backend::{
    AvailableMessageEffect, AvailableReactionItem, HistoryMessage, MessageReactionsPage,
    PollVoterEntry, PollVotesPage, ReactionCounter, ReactionPeerEntry, SavedReactionTagItem,
    SendMessageExtended,
};
use tg_app_core::history::sections_model::{
    LinkPreviewData, SendAsPeer, SlowModeState, effect_name_and_accent,
};
use tg_app_core::stand_in::types::{MsgId, PeerId};

/// Default timeout for compose-extras RPC invocations.
const COMPOSE_EXTRAS_RPC_TIMEOUT: Duration = Duration::from_secs(10);

/// Cache TTL for `channels.getSendAs` matching tdesktop `kRequestEach = 30s` (`send_as_peers.cpp:20`).
const SEND_AS_CACHE_TTL_SECS: i64 = 30;

/// Maximum simultaneous reactions per message for standard vs Telegram Premium users.
const MAX_REACTIONS_STANDARD: usize = 1;
const MAX_REACTIONS_PREMIUM: usize = 3;

/// Per-`LiveSession` cache for compose extras state (kept out of `LiveSession`'s struct definition
/// so parallel work packages touching `live/session.rs` merge cleanly).
#[derive(Debug)]
struct ComposeExtrasLiveState {
    // Send-as (`channels.getSendAs`, `messages.saveDefaultSendAs`)
    send_as_cache: HashMap<PeerId, (i64, Vec<SendAsPeer>)>,
    default_send_as: HashMap<PeerId, PeerId>,

    // Slow mode & full-channel metadata (`channels.getFullChannel`)
    channel_full_loaded: HashSet<PeerId>,
    slow_mode: HashMap<PeerId, SlowModeState>,

    // Link previews (`messages.getWebPagePreview`)
    link_preview_cache: HashMap<String, Option<LinkPreviewData>>,

    // Translation (`messages.translateText`, `messages.togglePeerTranslations`)
    translated_messages: HashMap<(PeerId, MsgId), (String, String)>,
    peer_translations_disabled: HashMap<PeerId, bool>,
    do_not_translate_languages: Vec<String>,
    target_translation_language: String,
    // Platform translation (`CoreSettings::use_platform_translation` + `Platform::CreateTranslateProvider`)
    use_platform_translation: bool,
    platform_translator: crate::wp40_polish::PlatformTranslatorSlot,

    // Reactions (`messages.getAvailableReactions`, `messages.getTopReactions`, `setDefaultReaction`, `getSavedReactionTags`)
    available_reactions_hash: i32,
    available_reactions_loaded: bool,
    available_reactions: Vec<AvailableReactionItem>,
    reactions_sync: tg_api::cached_lists::ReactionsSync,
    top_reactions_loaded: bool,
    default_reaction: String,
    saved_tags_cache: HashMap<Option<PeerId>, (i64, Vec<SavedReactionTagItem>)>,

    // Message effects (`messages.getAvailableEffects`)
    available_effects_hash: i32,
    available_effects_loaded: bool,
    available_effects: Vec<AvailableMessageEffect>,
    effect_documents: HashMap<i64, tl_types::Document>,
    effect_lottie_cache: HashMap<u64, Option<String>>,

    // Monotonic random id counter for direct `messages.sendMedia` calls
    next_random_id: u64,
}

impl Default for ComposeExtrasLiveState {
    fn default() -> Self {
        Self {
            send_as_cache: HashMap::new(),
            default_send_as: HashMap::new(),
            channel_full_loaded: HashSet::new(),
            slow_mode: HashMap::new(),
            link_preview_cache: HashMap::new(),
            translated_messages: HashMap::new(),
            peer_translations_disabled: HashMap::new(),
            do_not_translate_languages: vec!["en".to_owned()],
            target_translation_language: "en".to_owned(),
            use_platform_translation: false,
            platform_translator: crate::wp40_polish::PlatformTranslatorSlot::system(),
            available_reactions_hash: 0,
            available_reactions_loaded: false,
            available_reactions: Vec::new(),
            reactions_sync: tg_api::cached_lists::ReactionsSync::default(),
            top_reactions_loaded: false,
            default_reaction: "👍".to_owned(),
            saved_tags_cache: HashMap::new(),
            available_effects_hash: 0,
            available_effects_loaded: false,
            available_effects: Vec::new(),
            effect_documents: HashMap::new(),
            effect_lottie_cache: HashMap::new(),
            next_random_id: 1,
        }
    }
}

impl ComposeExtrasLiveState {
    fn next_random_id(&mut self) -> i64 {
        let now = now_unix_i32() as u64;
        self.next_random_id = self.next_random_id.wrapping_add(1);
        ((now << 24) ^ self.next_random_id ^ 0x6C5A_9001_CAFE) as i64
    }
}

struct ComposeExtrasStateEntry {
    owner: Weak<LiveBridgeState>,
    state: ComposeExtrasLiveState,
}

fn compose_extras_registry() -> &'static Mutex<HashMap<usize, ComposeExtrasStateEntry>> {
    static REGISTRY: OnceLock<Mutex<HashMap<usize, ComposeExtrasStateEntry>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn with_compose_extras<R>(
    live: &LiveSession,
    f: impl FnOnce(&mut ComposeExtrasLiveState) -> R,
) -> R
where
{
    let key = Arc::as_ptr(live.bridge_state()) as usize;
    let mut map = compose_extras_registry()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    map.retain(|_, entry| entry.owner.strong_count() > 0);
    let entry = map.entry(key).or_insert_with(|| ComposeExtrasStateEntry {
        owner: Arc::downgrade(live.bridge_state()),
        state: ComposeExtrasLiveState::default(),
    });
    f(&mut entry.state)
}

// ---------------------------------------------------------------------------
// Internal RPC / Session helpers
// ---------------------------------------------------------------------------

fn read_session<R>(live: &LiveSession, f: impl FnOnce(&Session) -> R + Send) -> R
where
    R: Send,
{
    let lock = live.runtime().session();
    if let Ok(guard) = lock.try_read() {
        return f(&guard);
    }
    let arc = Arc::clone(lock);
    block_on_runtime(live.runtime().tokio_handle(), async move {
        let guard = arc.read().await;
        f(&guard)
    })
}

fn write_session<R>(live: &LiveSession, f: impl FnOnce(&mut Session) -> R + Send) -> R
where
    R: Send,
{
    let lock = live.runtime().session();
    if let Ok(mut guard) = lock.try_write() {
        return f(&mut guard);
    }
    let arc = Arc::clone(lock);
    block_on_runtime(live.runtime().tokio_handle(), async move {
        let mut guard = arc.write().await;
        f(&mut guard)
    })
}

fn invoke_rpc<R>(live: &LiveSession, req: R) -> Result<R::Return, String>
where
    R: RemoteCall + Send + Sync + 'static,
    R::Return: Send + 'static,
{
    let rpc = live.runtime().rpc().clone();
    block_on_runtime(live.runtime().tokio_handle(), async move {
        with_rpc_timeout(
            COMPOSE_EXTRAS_RPC_TIMEOUT,
            async move { rpc.invoke(&req).await },
        )
        .await
    })
}

fn input_peer(live: &LiveSession, peer_id: PeerId) -> Option<tl_enums::InputPeer> {
    let data_peer = to_data_peer(peer_id);
    let peer = read_session(live, |s| input_peer_for(s, data_peer));
    if matches!(peer, tl_enums::InputPeer::Empty) {
        None
    } else {
        Some(peer)
    }
}

fn input_channel(live: &LiveSession, peer_id: PeerId) -> Option<tl_enums::InputChannel> {
    let data_peer = to_data_peer(peer_id);
    read_session(live, |s| {
        let cid = data_peer.to_channel()?;
        let ch = input_channel_for(s, cid);
        if matches!(ch, tl_enums::InputChannel::Empty) {
            None
        } else {
            Some(ch)
        }
    })
}

struct AppliedUpdates {
    new_ids: Vec<DataMsgId>,
    reaction_updates: HashSet<DataFullMsgId>,
}

fn apply_updates(live: &LiveSession, updates: &tl_enums::Updates) -> AppliedUpdates {
    let new_ids = live.apply_updates_to_session(updates);
    let mut reaction_updates = HashSet::new();
    let mut poll_updates: Vec<&tl_types::UpdateMessagePoll> = Vec::new();
    let list: &[tl_enums::Update] = match updates {
        tl_enums::Updates::Updates(u) => &u.updates,
        tl_enums::Updates::Combined(u) => &u.updates,
        tl_enums::Updates::UpdateShort(s) => std::slice::from_ref(&s.update),
        _ => &[],
    };
    for upd in list {
        match upd {
            tl_enums::Update::MessageReactions(u) => {
                reaction_updates.insert(DataFullMsgId::new(
                    peer_id_from_tl(&u.peer),
                    DataMsgId(i64::from(u.msg_id)),
                ));
            }
            tl_enums::Update::MessagePoll(u) => {
                poll_updates.push(u);
            }
            _ => {}
        }
    }
    if !poll_updates.is_empty() {
        write_session(live, |session| {
            for poll_upd in poll_updates {
                apply_poll_update(session, poll_upd);
            }
        });
    }
    AppliedUpdates {
        new_ids,
        reaction_updates,
    }
}

fn apply_poll_update(session: &mut Session, upd: &tl_types::UpdateMessagePoll) {
    let target_peer = upd.peer.as_ref().map(peer_id_from_tl);
    let target_msg = upd.msg_id.map(|id| DataMsgId(i64::from(id)));
    let (Some(peer_id), Some(msg_id)) = (target_peer, target_msg) else {
        return;
    };
    let Some(msg) = session.message_mut(DataFullMsgId::new(peer_id, msg_id)) else {
        return;
    };
    let MessageContent::Regular {
        media: MessageMedia::Poll(ref mut poll),
        ..
    } = msg.content
    else {
        return;
    };
    if let Some(tl_enums::Poll::Poll(tl_p)) = &upd.poll {
        poll.closed = tl_p.closed;
        poll.public_voters = tl_p.public_voters;
        poll.multiple_choice = tl_p.multiple_choice;
        poll.quiz = tl_p.quiz;
    }
    let tl_enums::PollResults::PollResults(res) = &upd.results;
    if let Some(total) = res.total_voters {
        poll.results.total_voters = Some(total);
    }
    if let Some(ref voters_list) = res.results {
        poll.results.results = voters_list
            .iter()
            .map(|v| {
                let tl_enums::PollAnswerVoters::PollAnswerVoters(av) = v;
                tg_data::PollAnswerVoters {
                    option: av.option.clone(),
                    voters: av.voters.unwrap_or(0),
                    chosen: av.chosen,
                    correct: av.correct,
                }
            })
            .collect();
    }
}

fn make_input_reply_to(msg_id: MsgId, quote_text: Option<String>) -> tl_enums::InputReplyTo {
    tl_enums::InputReplyTo::Message(tl_types::InputReplyToMessage {
        quote_rich: None,
        reply_to_msg_id: msg_id.0,
        top_msg_id: None,
        reply_to_peer_id: None,
        quote_text,
        quote_entities: None,
        quote_offset: None,
        monoforum_peer_id: None,
        todo_item_id: None,
        poll_option: None,
    })
}

/// Parses `SLOWMODE_WAIT_X` or `SendMessageErrorKind::SlowmodeWait` (`"Slowmode enabled; wait X seconds."`)
/// out of an RPC error string, returning `Some(seconds)`.
#[must_use]
pub fn parse_slowmode_wait_error(err: &str) -> Option<u32> {
    if let Some(idx) = err.find("SLOWMODE_WAIT_") {
        let rest = &err[idx + "SLOWMODE_WAIT_".len()..];
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(secs) = digits.parse::<u32>() {
            return Some(secs);
        }
    }
    let lower = err.to_ascii_lowercase();
    if let Some(idx) = lower.find("slowmode") {
        let after = &lower[idx + "slowmode".len()..];
        let digits: String = after
            .chars()
            .skip_while(|c| !c.is_ascii_digit())
            .take_while(|c| c.is_ascii_digit())
            .collect();
        if let Ok(secs) = digits.parse::<u32>() {
            return Some(secs);
        }
    }
    None
}

/// Converts a UI reaction key (`"👍"`, `"custom:<doc_id>"`, `"⭐"`) into a TL [`tl_enums::Reaction`].
#[must_use]
pub fn reaction_str_to_tl(emoji: &str) -> tl_enums::Reaction {
    if let Some(id_str) = emoji.strip_prefix("custom:")
        && let Ok(doc_id) = id_str.parse::<i64>()
    {
        return tl_enums::Reaction::CustomEmoji(tl_types::ReactionCustomEmoji {
            document_id: doc_id,
        });
    }
    if emoji == "⭐" {
        return tl_enums::Reaction::Paid;
    }
    tl_enums::Reaction::Emoji(tl_types::ReactionEmoji {
        emoticon: emoji.to_owned(),
    })
}

/// Converts a UI reaction key into `tg_data::ReactionId`.
#[must_use]
pub fn reaction_str_to_data(emoji: &str) -> ReactionId {
    if let Some(id_str) = emoji.strip_prefix("custom:")
        && let Ok(doc_id) = id_str.parse::<u64>()
    {
        return ReactionId::Custom(DocumentId(doc_id));
    }
    if emoji == "⭐" {
        return ReactionId::Paid;
    }
    ReactionId::Emoji(emoji.to_owned())
}

/// Converts a `tg_data::ReactionId` into a TL [`tl_enums::Reaction`].
#[must_use]
fn reaction_data_to_tl(id: &ReactionId) -> tl_enums::Reaction {
    match id {
        ReactionId::Emoji(e) => tl_enums::Reaction::Emoji(tl_types::ReactionEmoji {
            emoticon: e.clone(),
        }),
        ReactionId::Custom(doc) => tl_enums::Reaction::CustomEmoji(tl_types::ReactionCustomEmoji {
            document_id: doc.0 as i64,
        }),
        ReactionId::Paid => tl_enums::Reaction::Paid,
    }
}

/// Formats a TL [`tl_enums::Reaction`] as a UI display string.
#[must_use]
fn tl_reaction_to_display(r: &tl_enums::Reaction) -> String {
    match r {
        tl_enums::Reaction::Emoji(e) => e.emoticon.clone(),
        tl_enums::Reaction::CustomEmoji(c) => format!("custom:{}", c.document_id),
        tl_enums::Reaction::Paid => "⭐".to_owned(),
        tl_enums::Reaction::Empty => String::new(),
    }
}

fn peer_color_index(session: &Session, peer_id: DataPeerId) -> u8 {
    match session.peer(peer_id) {
        Some(Peer::User(u)) => u.common.color_index,
        Some(Peer::Chat(c)) => c.common.color_index,
        Some(Peer::Channel(c)) => c.common.color_index,
        None => (peer_id.bare() % 7) as u8,
    }
}

// ---------------------------------------------------------------------------
// LiveSession Compose Extras implementation
// ---------------------------------------------------------------------------

impl LiveSession {
    // =========================================================================
    // 1. ChannelFull + Slow Mode (`channels.getFullChannel`)
    // =========================================================================

    /// Lazily loads `channels.getFullChannel` once per channel peer to populate
    /// `slowmode_seconds`, `slowmode_next_send_date`, `default_send_as`, and
    /// `translations_disabled`.
    pub(crate) fn ensure_channel_full_loaded(&self, peer_id: PeerId) {
        let data_peer = to_data_peer(peer_id);
        if !data_peer.is_channel() {
            return;
        }
        let already_loaded =
            with_compose_extras(self, |st| st.channel_full_loaded.contains(&peer_id));
        if already_loaded {
            return;
        }
        let Some(channel) = input_channel(self, peer_id) else {
            return;
        };
        with_compose_extras(self, |st| {
            st.channel_full_loaded.insert(peer_id);
        });
        match invoke_rpc(self, tl_fns::channels::GetFullChannel { channel }) {
            Ok(tl_enums::messages::ChatFull::ChatFull(full)) => {
                self.apply_channel_full_response(peer_id, &full);
            }
            Err(err) => {
                debug!(peer = peer_id.0, error = %err, "channels.getFullChannel failed");
            }
        }
    }

    /// Applies a `messages.ChatFull` response into `Session` and compose-extras state
    /// (`slowmode_seconds`, `slowmode_next_send_date`, `default_send_as`, `translations_disabled`).
    pub fn apply_channel_full_response(
        &self,
        peer_id: PeerId,
        full: &tl_types::messages::ChatFull,
    ) {
        write_session(self, |session| {
            apply_tl_users_and_chats(session, &full.users, &full.chats);
            apply_tl_chat_full(session, &full.full_chat);
        });
        if let tl_enums::ChatFull::ChannelFull(cf) = &full.full_chat {
            let slow_secs = cf.slowmode_seconds.unwrap_or(0).max(0) as u32;
            let next_send = cf.slowmode_next_send_date.map_or(0, i64::from);
            let default_sa = cf
                .default_send_as
                .as_ref()
                .map(|p| from_data_peer(peer_id_from_tl(p)));
            let tr_disabled = cf.translations_disabled;

            with_compose_extras(self, |st| {
                st.channel_full_loaded.insert(peer_id);
                st.slow_mode.insert(
                    peer_id,
                    SlowModeState {
                        seconds: slow_secs,
                        next_send_unix: next_send,
                    },
                );
                if let Some(sa) = default_sa {
                    st.default_send_as.entry(peer_id).or_insert(sa);
                }
                st.peer_translations_disabled
                    .entry(peer_id)
                    .or_insert(tr_disabled);
            });
            self.bridge_state().notify_ui();
        }
    }

    /// Returns slow mode configuration and active cooldown for `peer_id`.
    ///
    /// Mirrors tdesktop `PeerData::slowmodeApplied()` / `slowmodeSecondsLeft()`:
    /// channel creators and admins bypass slow mode unless the server explicitly returned a
    /// future `slowmode_next_send_date`.
    pub(crate) fn slow_mode_state_live(&self, peer_id: PeerId) -> SlowModeState {
        let data_peer = to_data_peer(peer_id);
        if !data_peer.is_channel() {
            return SlowModeState::default();
        }
        self.ensure_channel_full_loaded(peer_id);

        let now = now_unix_i32();
        let (is_privileged, data_secs, data_left) = read_session(self, |s| {
            let Some(Peer::Channel(ch)) = s.peer(data_peer) else {
                return (false, 0u32, 0i32);
            };
            let privileged = ch.am_creator() || ch.has_admin_rights();
            let secs = ch.slowmode_seconds().max(0) as u32;
            let left = ch.slowmode_seconds_left(now);
            (privileged, secs, left)
        });

        let cached = with_compose_extras(self, |st| st.slow_mode.get(&peer_id).copied());
        let mut state = cached.unwrap_or_else(|| SlowModeState {
            seconds: data_secs,
            next_send_unix: if data_left > 0 {
                i64::from(now) + i64::from(data_left)
            } else {
                0
            },
        });
        if state.seconds == 0 && data_secs > 0 {
            state.seconds = data_secs;
        }
        if is_privileged {
            // Admins/creators are exempt from slow mode per `Channel::slowmode_applied()`.
            return SlowModeState::default();
        }
        state
    }

    /// Records a sent message timestamp against `peer_id`'s slow mode timer.
    pub(crate) fn record_slow_mode_send_live(&self, peer_id: PeerId, now_unix: i64) {
        let state = self.slow_mode_state_live(peer_id);
        if state.seconds == 0 {
            return;
        }
        let next = now_unix.saturating_add(i64::from(state.seconds));
        with_compose_extras(self, |st| {
            let entry = st.slow_mode.entry(peer_id).or_insert(state);
            entry.seconds = state.seconds;
            if entry.next_send_unix < next {
                entry.next_send_unix = next;
            }
        });
        let data_peer = to_data_peer(peer_id);
        write_session(self, |s| {
            if let Some(cid) = data_peer.to_channel()
                && let Some(mut ch) = s.channel(cid).cloned()
            {
                ch.grow_slowmode_last_message(now_unix as i32);
                s.upsert_channel(ch, tg_data::PeerUpdateFlags::SLOWMODE);
            }
        });
        self.bridge_state().notify_ui();
    }

    pub(crate) fn apply_slowmode_wait_from_error(&self, peer_id: PeerId, err: &str) {
        if let Some(wait_secs) = parse_slowmode_wait_error(err) {
            let now = now_unix_i64();
            let next = now.saturating_add(i64::from(wait_secs));
            with_compose_extras(self, |st| {
                let entry = st.slow_mode.entry(peer_id).or_default();
                if entry.seconds == 0 {
                    entry.seconds = wait_secs;
                }
                entry.next_send_unix = next;
            });
            self.bridge_state().notify_ui();
        }
    }

    // =========================================================================
    // 2. Send-As (`channels.getSendAs`, `messages.saveDefaultSendAs`)
    // =========================================================================

    /// Fetches available "Send As" peers for `peer_id` via `channels.getSendAs` (cached 30s).
    pub(crate) fn send_as_peers_live(&self, peer_id: PeerId) -> Vec<SendAsPeer> {
        let data_peer = to_data_peer(peer_id);
        if !data_peer.is_channel() {
            return Vec::new();
        }
        self.ensure_channel_full_loaded(peer_id);

        let now = now_unix_i64();
        if let Some(cached) = with_compose_extras(self, |st| {
            st.send_as_cache
                .get(&peer_id)
                .filter(|(ts, _)| now.saturating_sub(*ts) < SEND_AS_CACHE_TTL_SECS)
                .map(|(_, list)| list.clone())
        }) {
            return cached;
        }

        let Some(peer) = input_peer(self, peer_id) else {
            return Vec::new();
        };
        let req = tl_fns::channels::GetSendAs {
            for_paid_reactions: false,
            for_live_stories: false,
            peer,
        };
        let list = match invoke_rpc(self, req) {
            Ok(tl_enums::channels::SendAsPeers::SendAsPeers(resp)) => {
                write_session(self, |session| {
                    apply_tl_users_and_chats(session, &resp.users, &resp.chats);
                });
                read_session(self, |session| {
                    resp.peers
                        .iter()
                        .map(|sp| {
                            let tl_enums::SendAsPeer::SendAsPeer(item) = sp;
                            let pid = peer_id_from_tl(&item.peer);
                            let title = peer_display_title(session, pid);
                            let is_channel = pid.is_channel();
                            let subtitle = match session.peer(pid) {
                                Some(Peer::Channel(ch)) => {
                                    let count = ch.members_count.max(0);
                                    if item.premium_required {
                                        format!("Channel · {count} subscribers · Premium")
                                    } else if count > 0 {
                                        format!("Channel · {count} subscribers")
                                    } else {
                                        "Channel".to_owned()
                                    }
                                }
                                Some(Peer::User(_)) => {
                                    if item.premium_required {
                                        "Personal account · Premium".to_owned()
                                    } else {
                                        "Personal account".to_owned()
                                    }
                                }
                                _ => "Group".to_owned(),
                            };
                            let color_idx = peer_color_index(session, pid);
                            SendAsPeer {
                                peer_id: from_data_peer(pid),
                                title,
                                subtitle,
                                is_channel,
                                avatar_rgb: avatar_rgb_for_color_index(color_idx),
                            }
                        })
                        .collect::<Vec<_>>()
                })
            }
            Err(err) => {
                debug!(peer = peer_id.0, error = %err, "channels.getSendAs failed");
                Vec::new()
            }
        };

        with_compose_extras(self, |st| {
            st.send_as_cache.insert(peer_id, (now, list.clone()));
        });
        list
    }

    /// Returns the persisted default "Send As" peer for `peer_id`.
    pub(crate) fn default_send_as_peer_live(&self, peer_id: PeerId) -> Option<PeerId> {
        self.ensure_channel_full_loaded(peer_id);
        with_compose_extras(self, |st| st.default_send_as.get(&peer_id).copied())
    }

    /// Saves the default "Send As" peer via `messages.saveDefaultSendAs`.
    pub(crate) fn save_default_send_as_live(
        &self,
        peer_id: PeerId,
        send_as: PeerId,
    ) -> Result<(), String> {
        let Some(peer) = input_peer(self, peer_id) else {
            return Err("Chat peer not found in session.".to_owned());
        };
        let Some(send_as_input) = input_peer(self, send_as) else {
            return Err("Send-as peer not found in session.".to_owned());
        };
        let _ = invoke_rpc(
            self,
            tl_fns::messages::SaveDefaultSendAs {
                peer,
                send_as: send_as_input,
            },
        )?;
        with_compose_extras(self, |st| {
            st.default_send_as.insert(peer_id, send_as);
        });
        self.bridge_state().notify_ui();
        Ok(())
    }

    // =========================================================================
    // 3. Link Preview (`messages.getWebPagePreview`) & Extended Send
    // =========================================================================

    /// Resolves a link preview via `messages.getWebPagePreview` (cached per URL).
    pub(crate) fn resolve_link_preview_live(&self, url: &str) -> Option<LinkPreviewData> {
        let trimmed = url.trim();
        if trimmed.is_empty() {
            return None;
        }
        if let Some(cached) =
            with_compose_extras(self, |st| st.link_preview_cache.get(trimmed).cloned())
        {
            return cached;
        }
        let req = tl_fns::messages::GetWebPagePreview {
            message: trimmed.to_owned(),
            entities: None,
        };
        let resolved = match invoke_rpc(self, req) {
            Ok(tl_enums::messages::WebPagePreview::WebPagePreview(preview)) => {
                write_session(self, |session| {
                    apply_tl_users_and_chats(session, &preview.users, &preview.chats);
                });
                match preview.media {
                    tl_enums::MessageMedia::WebPage(wp_media) => match wp_media.webpage {
                        tl_enums::WebPage::WebPage(wp) => {
                            let site_name = wp
                                .site_name
                                .clone()
                                .filter(|s| !s.is_empty())
                                .unwrap_or_else(|| wp.display_url.clone());
                            let title = wp
                                .title
                                .clone()
                                .filter(|s| !s.is_empty())
                                .unwrap_or_else(|| wp.url.clone());
                            let description = wp.description.clone().unwrap_or_default();
                            let large_media = wp_media.force_large_media
                                || (!wp_media.force_small_media && wp.has_large_media);
                            Some(LinkPreviewData {
                                url: if wp.url.is_empty() {
                                    trimmed.to_owned()
                                } else {
                                    wp.url
                                },
                                site_name,
                                title,
                                description,
                                thumb_rgb: Some(avatar_rgb_for_color_index(
                                    (wp.id.unsigned_abs() % 7) as u8,
                                )),
                                large_media,
                                above_text: false,
                            })
                        }
                        _ => None,
                    },
                    _ => None,
                }
            }
            Err(err) => {
                debug!(url = trimmed, error = %err, "messages.getWebPagePreview failed");
                None
            }
        };

        with_compose_extras(self, |st| {
            st.link_preview_cache
                .insert(trimmed.to_owned(), resolved.clone());
        });
        resolved
    }

    /// Extended message send supporting slow mode blocking, `send_as`, `effect_id`,
    /// `no_webpage`, `invert_media` (above/below text), and `InputMediaWebPage`
    /// (`force_large_media` / `force_small_media`).
    pub(crate) fn send_message_extended_live(
        &mut self,
        peer_id: PeerId,
        request: SendMessageExtended,
    ) -> Result<HistoryMessage, String> {
        if request.media.is_some() {
            return crate::upload::send_history_media_extended(self, peer_id, request);
        }

        // Slow mode send blocking (tdesktop `history_widget.cpp:6083`: scheduled ignores slowmode).
        let now = now_unix_i64();
        if request.schedule_date.is_none() {
            let sm = self.slow_mode_state_live(peer_id);
            let remaining = sm.remaining_secs(now);
            if remaining > 0 {
                return Err(format!(
                    "Slow mode is enabled. Wait {} before sending another message.",
                    SlowModeState::format_countdown(remaining)
                ));
            }
        }

        let data_peer = to_data_peer(peer_id);
        let entities = read_session(self, |s| ui_text_to_tl_entities(&request.text, s));
        let reply_to = request.reply_to.map(|mid| {
            make_input_reply_to(mid, request.reply_quote.as_ref().map(|q| q.text.clone()))
        });
        let send_as_tl = request.send_as.and_then(|sa| input_peer(self, sa));
        let invert_media = !request.no_webpage
            && request
                .link_preview
                .as_ref()
                .is_some_and(|lp| lp.above_text);

        // When the user attached an explicit link preview (with large/small media or above/below),
        // use `messages.sendMedia` with `InputMediaWebPage` per Telegram Layer 233.
        if !request.no_webpage
            && let Some(ref lp) = request.link_preview
        {
            let Some(peer) = input_peer(self, peer_id) else {
                return Err("Peer not found in session.".to_owned());
            };
            let random_id = with_compose_extras(self, ComposeExtrasLiveState::next_random_id);
            let req = tl_fns::messages::SendMedia {
                silent: request.silent,
                background: false,
                clear_draft: true,
                noforwards: false,
                update_stickersets_order: false,
                invert_media,
                allow_paid_floodskip: false,
                peer,
                reply_to,
                media: tl_enums::InputMedia::WebPage(tl_types::InputMediaWebPage {
                    force_large_media: lp.large_media,
                    force_small_media: !lp.large_media,
                    optional: true,
                    url: lp.url.clone(),
                }),
                message: request.text.text.clone(),
                random_id,
                reply_markup: None,
                entities: (!entities.is_empty()).then_some(entities),
                schedule_date: request.schedule_date,
                schedule_repeat_period: None,
                send_as: send_as_tl,
                quick_reply_shortcut: None,
                effect: request.effect_id.map(|id| id as i64),
                allow_paid_stars: None,
                suggested_post: None,
            };
            let updates = match invoke_rpc(self, req) {
                Ok(u) => u,
                Err(err) => {
                    self.apply_slowmode_wait_from_error(peer_id, &err);
                    return Err(err);
                }
            };
            let applied = apply_updates(self, &updates);
            if request.schedule_date.is_none() {
                self.record_slow_mode_send_live(peer_id, now);
            }
            let msg_id = applied.new_ids.last().copied().unwrap_or(DataMsgId(0));
            return read_session(self, |s| {
                s.message(DataFullMsgId::new(data_peer, msg_id))
                    .map(|m| message_to_history_message(s, m))
            })
            .ok_or_else(|| "sent message was not stored in session".to_owned());
        }

        let options = SendOptions {
            silent: request.silent,
            no_webpage: request.no_webpage,
            invert_media,
            schedule_date: request.schedule_date,
            send_as: send_as_tl,
            effect_id: request.effect_id.map(|id| id as i64),
            ..SendOptions::default()
        };
        if request.schedule_date.is_none() {
            // Shown at once, sent in the background (TGB-2931); a scheduled message never
            // appears in the chat, so it still waits for the server below.
            let extras = crate::optimistic_send::QueuedSendExtras {
                reply_to_msg_id: request.reply_to.map(crate::convert::to_data_msg),
                reply_quote: request
                    .reply_quote
                    .as_ref()
                    .map(|q| (q.text.clone(), q.offset_utf16)),
                effect_id: request.effect_id,
            };
            let sent = self.send_text_optimistic(
                data_peer,
                &request.text.text,
                &entities,
                reply_to,
                options,
                &extras,
            )?;
            // The slow-mode wait starts with the send (`HistoryWidget::showSlowmodeError` also
            // refuses while a message is still sending).
            self.record_slow_mode_send_live(peer_id, now);
            return Ok(sent);
        }
        let rt = Arc::clone(self.runtime());
        let text_owned = request.text.text.clone();
        let outcome = match block_on_runtime(self.runtime().tokio_handle(), async move {
            with_rpc_timeout(
                COMPOSE_EXTRAS_RPC_TIMEOUT,
                rt.send_message_with_entities(data_peer, text_owned, entities, reply_to, options),
            )
            .await
        }) {
            Ok(out) => out,
            Err(err) => {
                self.apply_slowmode_wait_from_error(peer_id, &err);
                return Err(err);
            }
        };
        if request.schedule_date.is_none() {
            self.record_slow_mode_send_live(peer_id, now);
        }
        // `SessionRuntime::send_message_with_entities` already routed `outcome.updates`
        // through `UpdatesHandler`; only collect IDs and refresh UI side-caches here.
        let mut applied_ids = Vec::new();
        for upd in &outcome.updates {
            applied_ids.extend(crate::session::extract_message_ids_from_updates(upd));
            self.apply_updates_side_caches(upd);
        }
        let msg_id = outcome
            .messages
            .last()
            .and_then(|pending| match pending.state {
                PendingMessageState::Sent { server_id } => Some(server_id),
                _ => None,
            })
            .or_else(|| applied_ids.last().copied())
            .or_else(|| outcome.messages.last().map(|p| p.local_id.msg))
            .unwrap_or(DataMsgId(0));
        let mut msg = read_session(self, |s| {
            s.message(DataFullMsgId::new(data_peer, msg_id))
                .map(|m| message_to_history_message(s, m))
        });
        if let Some(ref mut m) = msg
            && m.extra.effect_id.is_none()
            && let Some(eff_id) = request.effect_id
        {
            m.extra.effect_id = Some(eff_id);
        }
        self.bridge_state().notify_ui();
        msg.ok_or_else(|| "sent message was not stored in session".to_owned())
    }

    // =========================================================================
    // 4. Translation (`messages.translateText`, `messages.togglePeerTranslations`)
    // =========================================================================

    /// Translates a single message via `messages.translateText`.
    pub(crate) fn translate_message_live(
        &self,
        peer_id: PeerId,
        msg_id: MsgId,
        target_lang: &str,
    ) -> Result<String, String> {
        let Some(peer) = input_peer(self, peer_id) else {
            return Err("Peer not found in session.".to_owned());
        };
        let lang = target_lang.trim();
        if lang.is_empty() {
            return Err("Target language must not be empty.".to_owned());
        }
        if let Some(translated) = self.platform_translate_message(peer_id, msg_id, lang) {
            with_compose_extras(self, |st| {
                st.translated_messages
                    .insert((peer_id, msg_id), (lang.to_owned(), translated.clone()));
            });
            self.bridge_state().notify_ui();
            return Ok(translated);
        }
        let req = tl_fns::messages::TranslateText {
            peer: Some(peer),
            id: Some(vec![msg_id.0]),
            text: None,
            to_lang: lang.to_owned(),
            tone: None,
        };
        let tl_enums::messages::TranslatedText::TranslateResult(res) = invoke_rpc(self, req)?;
        let translated = res
            .result
            .into_iter()
            .next()
            .map(|tl_enums::TextWithEntities::TextWithEntities(t)| t.text)
            .ok_or_else(|| "Server returned an empty translation result.".to_owned())?;
        with_compose_extras(self, |st| {
            st.translated_messages
                .insert((peer_id, msg_id), (lang.to_owned(), translated.clone()));
        });
        self.bridge_state().notify_ui();
        Ok(translated)
    }

    /// Batch-translates multiple messages in `peer_id` via a single `messages.translateText` RPC.
    pub(crate) fn translate_messages_batch_live(
        &self,
        peer_id: PeerId,
        msg_ids: &[MsgId],
        target_lang: &str,
    ) -> Result<Vec<(MsgId, String)>, String> {
        if msg_ids.is_empty() {
            return Ok(Vec::new());
        }
        let Some(peer) = input_peer(self, peer_id) else {
            return Err("Peer not found in session.".to_owned());
        };
        let lang = target_lang.trim();
        if lang.is_empty() {
            return Err("Target language must not be empty.".to_owned());
        }
        let mut pairs = Vec::with_capacity(msg_ids.len());
        let mut remote_ids = Vec::with_capacity(msg_ids.len());
        for &mid in msg_ids {
            match self.platform_translate_message(peer_id, mid, lang) {
                Some(text) => pairs.push((mid, text)),
                None => remote_ids.push(mid),
            }
        }
        if !remote_ids.is_empty() {
            let req = tl_fns::messages::TranslateText {
                peer: Some(peer),
                id: Some(remote_ids.iter().map(|m| m.0).collect()),
                text: None,
                to_lang: lang.to_owned(),
                tone: None,
            };
            let tl_enums::messages::TranslatedText::TranslateResult(res) = invoke_rpc(self, req)?;
            for (&mid, entry) in remote_ids.iter().zip(res.result) {
                let tl_enums::TextWithEntities::TextWithEntities(t) = entry;
                pairs.push((mid, t.text));
            }
        }
        with_compose_extras(self, |st| {
            for (mid, text) in &pairs {
                st.translated_messages
                    .insert((peer_id, *mid), (lang.to_owned(), text.clone()));
            }
        });
        self.bridge_state().notify_ui();
        Ok(pairs)
    }

    /// Enables or disables the platform translation provider
    /// (`CoreSettings::use_platform_translation`, pushed by the live host).
    pub fn set_platform_translation_live(&self, enabled: bool) {
        with_compose_extras(self, |st| st.use_platform_translation = enabled);
    }

    /// Replaces the platform translation provider (`None` = no provider on this host).
    pub fn set_platform_translator_live(
        &self,
        provider: Option<Arc<dyn tg_platform::translate_win::TranslateProvider>>,
    ) {
        with_compose_extras(self, |st| {
            st.platform_translator = crate::wp40_polish::PlatformTranslatorSlot::new(provider);
        });
    }

    /// Translates one cached message with the platform provider when it is enabled and supports
    /// `lang` (`Ui::CreateTranslateProvider` in `translate_provider.cpp`); `None` means the caller
    /// should use `messages.translateText`.
    fn platform_translate_message(
        &self,
        peer_id: PeerId,
        msg_id: MsgId,
        lang: &str,
    ) -> Option<String> {
        let (enabled, provider) = with_compose_extras(self, |st| {
            (st.use_platform_translation, st.platform_translator.get())
        });
        let provider = provider.filter(|_| enabled)?;
        let data_peer = to_data_peer(peer_id);
        let text = read_session(self, |s| {
            s.message(DataFullMsgId::new(
                data_peer,
                DataMsgId(i64::from(msg_id.0)),
            ))
            .and_then(|m| m.text().map(|t| t.text.clone()))
        })?;
        crate::wp40_polish::translate_with_platform_only(provider.as_ref(), &text, lang)
    }

    /// Clears a cached message translation ("Show Original").
    pub(crate) fn clear_message_translation_live(&self, peer_id: PeerId, msg_id: MsgId) {
        with_compose_extras(self, |st| {
            st.translated_messages.remove(&(peer_id, msg_id));
        });
        self.bridge_state().notify_ui();
    }

    /// Decorates a slice of [`HistoryMessage`] items with any cached translations.
    pub(crate) fn decorate_cached_translations(
        &self,
        peer_id: PeerId,
        messages: &mut [HistoryMessage],
    ) {
        with_compose_extras(self, |st| {
            if st.translated_messages.is_empty() {
                return;
            }
            for msg in messages.iter_mut() {
                if let Some(tr) = st.translated_messages.get(&(peer_id, msg.id)) {
                    msg.extra.translated_text = Some(tr.clone());
                }
            }
        });
    }

    /// Returns `true` if peer translations are disabled/hidden for `peer_id`.
    pub(crate) fn peer_translations_disabled_live(&self, peer_id: PeerId) -> bool {
        if to_data_peer(peer_id).is_channel() {
            self.ensure_channel_full_loaded(peer_id);
        }
        with_compose_extras(self, |st| {
            st.peer_translations_disabled
                .get(&peer_id)
                .copied()
                .unwrap_or(false)
        })
    }

    /// Calls `messages.togglePeerTranslations` and updates cached state.
    pub(crate) fn toggle_peer_translations_live(
        &self,
        peer_id: PeerId,
        disabled: bool,
    ) -> Result<(), String> {
        let Some(peer) = input_peer(self, peer_id) else {
            return Err("Peer not found in session.".to_owned());
        };
        let _ = invoke_rpc(
            self,
            tl_fns::messages::TogglePeerTranslations { disabled, peer },
        )?;
        with_compose_extras(self, |st| {
            st.peer_translations_disabled.insert(peer_id, disabled);
        });
        self.bridge_state().notify_ui();
        Ok(())
    }

    /// Returns the user's "Do not translate" language list.
    pub(crate) fn do_not_translate_languages_live(&self) -> Vec<String> {
        with_compose_extras(self, |st| st.do_not_translate_languages.clone())
    }

    /// Replaces the user's "Do not translate" language list (deduplicated lowercase codes).
    pub(crate) fn set_do_not_translate_languages_live(&self, langs: Vec<String>) {
        let mut unique = Vec::new();
        for l in langs {
            let norm = l.trim().to_ascii_lowercase();
            if !norm.is_empty() && !unique.contains(&norm) {
                unique.push(norm);
            }
        }
        with_compose_extras(self, |st| {
            st.do_not_translate_languages = unique;
        });
    }

    /// Returns the preferred target translation language code.
    pub(crate) fn target_translation_language_live(&self) -> String {
        with_compose_extras(self, |st| st.target_translation_language.clone())
    }

    /// Sets the preferred target translation language code.
    pub(crate) fn set_target_translation_language_live(&self, lang: String) {
        let norm = lang.trim().to_ascii_lowercase();
        if !norm.is_empty() {
            with_compose_extras(self, |st| {
                st.target_translation_language = norm;
            });
        }
    }

    // =========================================================================
    // 5. Reactions (available catalog, multi-reaction, who-reacted, default, saved tags)
    // =========================================================================

    /// Fetches available reactions via `messages.getAvailableReactions`, cached by hash.
    pub(crate) fn available_reactions_live(&self) -> Vec<AvailableReactionItem> {
        let already = with_compose_extras(self, |st| {
            st.available_reactions_loaded
                .then(|| st.available_reactions.clone())
        });
        if let Some(list) = already {
            return list;
        }
        self.refresh_available_reactions_live()
    }

    /// Explicitly refreshes available reactions sending the cached `hash` (`NotModified` preserves cache).
    pub fn refresh_available_reactions_live(&self) -> Vec<AvailableReactionItem> {
        let current_hash = with_compose_extras(self, |st| st.available_reactions_hash);
        match invoke_rpc(
            self,
            tl_fns::messages::GetAvailableReactions { hash: current_hash },
        ) {
            Ok(tl_enums::messages::AvailableReactions::NotModified) => {
                with_compose_extras(self, |st| {
                    st.available_reactions_loaded = true;
                    st.available_reactions.clone()
                })
            }
            Ok(tl_enums::messages::AvailableReactions::AvailableReactions(data)) => {
                let items: Vec<AvailableReactionItem> = data
                    .reactions
                    .into_iter()
                    .map(|tl_enums::AvailableReaction::AvailableReaction(r)| {
                        AvailableReactionItem {
                            emoji: r.reaction,
                            title: r.title,
                            is_premium: r.premium,
                            inactive: r.inactive,
                        }
                    })
                    .filter(|r| !r.inactive)
                    .collect();
                with_compose_extras(self, |st| {
                    st.available_reactions_hash = data.hash;
                    st.available_reactions_loaded = true;
                    st.available_reactions = items.clone();
                });
                items
            }
            Err(err) => {
                debug!(error = %err, "messages.getAvailableReactions failed");
                with_compose_extras(self, |st| st.available_reactions.clone())
            }
        }
    }

    /// Returns cached top reaction emojis from `messages.getTopReactions` (fetching once on first
    /// call via `tg_api::cached_lists::ReactionsSync::refresh_top`, matching `Data::Reactions`).
    pub(crate) fn top_reactions_live(&self) -> Vec<String> {
        let already = with_compose_extras(self, |st| {
            st.top_reactions_loaded
                .then(|| top_reaction_strings(&st.reactions_sync.top, true))
        });
        if let Some(list) = already {
            return list;
        }
        self.refresh_top_reactions_live(true)
    }

    /// Explicitly refreshes top reactions via `tg_api::cached_lists::ReactionsSync::refresh_top`
    /// (`messages.getTopReactions(limit = 14, hash)`), preserving cache on `NotModified` and
    /// resetting `top_hash = 0` on RPC error (`data_message_reactions.cpp:972-991`).
    pub fn refresh_top_reactions_live(&self, force: bool) -> Vec<String> {
        let mut sync_snapshot = with_compose_extras(self, |st| st.reactions_sync.clone());
        let now_ms = (now_unix_i32().max(0) as u64).saturating_mul(1_000);
        let rpc = self.runtime().rpc().clone();
        let res = block_on_runtime(self.runtime().tokio_handle(), async move {
            let outcome = with_rpc_timeout(
                COMPOSE_EXTRAS_RPC_TIMEOUT,
                sync_snapshot.refresh_top(&rpc, now_ms, force),
            )
            .await;
            (sync_snapshot, outcome)
        });
        match res {
            (updated_sync, Ok(_)) => {
                let items = top_reaction_strings(&updated_sync.top, true);
                with_compose_extras(self, |st| {
                    st.reactions_sync = updated_sync;
                    st.top_reactions_loaded = true;
                });
                items
            }
            (updated_sync, Err(err)) => {
                debug!(error = %err, "messages.getTopReactions failed");
                with_compose_extras(self, |st| {
                    // Preserve `top_hash = 0` reset performed by `ReactionsSync::refresh_top`.
                    st.reactions_sync.top_hash = updated_sync.top_hash;
                    top_reaction_strings(&st.reactions_sync.top, true)
                })
            }
        }
    }

    /// Returns the ordered emoji list for the message context-menu reaction strip following
    /// tdesktop's `Data::LookupPossibleReactions` (`data_message_reactions.cpp:180-351`):
    /// user's favorite/default reaction rotated to front, followed by `messages.getTopReactions`,
    /// followed by remaining active `messages.getAvailableReactions`, filtered by `peer_id`'s
    /// allowed reactions policy and account Premium status.
    pub(crate) fn reaction_strip_emojis_live(&self, peer_id: Option<PeerId>) -> Vec<String> {
        let top = self.top_reactions_live();
        let available = self.available_reactions_live();
        let fav = self.default_reaction_live();

        let (is_premium, peer_allowed) = read_session(self, |s| {
            let premium = s
                .peer(s.self_peer_id())
                .and_then(|p| p.as_user())
                .is_some_and(|u| u.is_premium());
            let allowed = peer_id.and_then(|pid| {
                let dp = to_data_peer(pid);
                match s.peer(dp)? {
                    tg_data::Peer::Chat(c)
                        if c.common.loaded_status >= tg_data::LoadedStatus::Full =>
                    {
                        Some(c.allowed_reactions.clone())
                    }
                    tg_data::Peer::Channel(ch)
                        if ch.common.loaded_status >= tg_data::LoadedStatus::Full =>
                    {
                        Some(ch.allowed_reactions.clone())
                    }
                    _ => None,
                }
            });
            (premium, allowed)
        });

        let allowed_filter = |emoji: &str| -> bool {
            let is_custom = emoji.starts_with("custom:");
            if is_custom && !is_premium {
                return false;
            }
            match &peer_allowed {
                None => !is_custom,
                Some(allowed) => match allowed.kind {
                    tg_data::AllowedReactionsType::All => true,
                    tg_data::AllowedReactionsType::Default => !is_custom,
                    tg_data::AllowedReactionsType::Some => {
                        let target = reaction_str_to_data(emoji);
                        allowed.some.contains(&target)
                    }
                },
            }
        };

        if let Some(ref allowed) = peer_allowed
            && allowed.kind == tg_data::AllowedReactionsType::Some
            && allowed.some.is_empty()
        {
            return Vec::new();
        }

        let mut ordered = Vec::new();
        let mut push_unique = |emoji: String| {
            if !emoji.is_empty() && allowed_filter(&emoji) && !ordered.contains(&emoji) {
                ordered.push(emoji);
            }
        };

        for emoji in top {
            // Skip premium-only standard reactions for non-premium accounts when known in `available`.
            let blocked_by_premium = !is_premium
                && available
                    .iter()
                    .find(|a| a.emoji == emoji)
                    .is_some_and(|a| a.is_premium);
            if !blocked_by_premium {
                push_unique(emoji);
            }
        }
        for item in &available {
            if !item.inactive && (is_premium || !item.is_premium) {
                push_unique(item.emoji.clone());
            }
        }
        if let Some(ref allowed) = peer_allowed
            && allowed.kind == tg_data::AllowedReactionsType::Some
        {
            for id in &allowed.some {
                if let Some(emoji) = data_reaction_to_strip_string(id, is_premium) {
                    push_unique(emoji);
                }
            }
        }

        // Rotate favorite/default reaction to the front if present (`data_message_reactions.cpp:317`).
        if let Some(pos) = ordered.iter().position(|e| e == &fav)
            && pos > 0
        {
            let item = ordered.remove(pos);
            ordered.insert(0, item);
        }

        ordered
    }

    /// Toggles `emoji` reaction on `msg_id` while supporting multiple reactions per message
    /// (up to 3 simultaneous reactions for Telegram Premium accounts, 1 for standard accounts).
    pub(crate) fn toggle_reaction_live(
        &self,
        peer_id: PeerId,
        msg_id: MsgId,
        emoji: &str,
    ) -> Result<Vec<ReactionCounter>, String> {
        let Some(peer) = input_peer(self, peer_id) else {
            return Err("Peer not found.".to_owned());
        };
        let data_peer = to_data_peer(peer_id);
        let data_msg = to_data_msg(msg_id);
        let target_id = reaction_str_to_data(emoji);

        let (is_premium, mut chosen_ordered) = read_session(self, |s| {
            let premium = s
                .peer(s.self_peer_id())
                .and_then(|p| p.as_user())
                .is_some_and(|u| u.is_premium());
            let mut chosen: Vec<(i32, ReactionId)> = s
                .message(DataFullMsgId::new(data_peer, data_msg))
                .and_then(|m| m.reactions.as_ref())
                .map(|r| {
                    r.results
                        .iter()
                        .filter_map(|c| c.chosen_order.map(|ord| (ord, c.id.clone())))
                        .collect()
                })
                .unwrap_or_default();
            chosen.sort_by_key(|(ord, _)| *ord);
            (
                premium,
                chosen.into_iter().map(|(_, id)| id).collect::<Vec<_>>(),
            )
        });

        let max_reactions = if is_premium {
            MAX_REACTIONS_PREMIUM
        } else {
            MAX_REACTIONS_STANDARD
        };

        let mut removed_ids = Vec::new();
        let mut added_id = None;
        if let Some(pos) = chosen_ordered.iter().position(|id| id == &target_id) {
            removed_ids.push(chosen_ordered.remove(pos));
        } else {
            while chosen_ordered.len() >= max_reactions && !chosen_ordered.is_empty() {
                removed_ids.push(chosen_ordered.remove(0));
            }
            chosen_ordered.push(target_id.clone());
            added_id = Some(target_id);
        }

        let tl_reactions: Vec<tl_enums::Reaction> =
            chosen_ordered.iter().map(reaction_data_to_tl).collect();
        let req = tl_fns::messages::SendReaction {
            big: false,
            add_to_recent: true,
            peer,
            msg_id: msg_id.0,
            reaction: Some(tl_reactions),
        };
        let updates = invoke_rpc(self, req)?;
        let applied = apply_updates(self, &updates);
        let full_id = DataFullMsgId::new(data_peer, data_msg);

        if !applied.reaction_updates.contains(&full_id) {
            write_session(self, |s| {
                if let Some(m) = s.message_mut(full_id) {
                    let reactions = m.reactions.get_or_insert_with(Default::default);
                    for rem in &removed_ids {
                        if let Some(existing) = reactions.results.iter_mut().find(|c| &c.id == rem)
                        {
                            existing.chosen_order = None;
                            existing.count = existing.count.saturating_sub(1);
                        }
                    }
                    if let Some(ref added) = added_id {
                        if let Some(existing) =
                            reactions.results.iter_mut().find(|c| &c.id == added)
                        {
                            existing.count = existing.count.saturating_add(1);
                        } else {
                            reactions.results.push(tg_data::MessageReactionCount {
                                id: added.clone(),
                                count: 1,
                                chosen_order: Some(0),
                            });
                        }
                    }
                    // Reassign sequential `chosen_order` matching `chosen_ordered`.
                    for c in &mut reactions.results {
                        c.chosen_order = chosen_ordered
                            .iter()
                            .position(|id| id == &c.id)
                            .map(|idx| idx as i32);
                    }
                    reactions.results.retain(|c| c.count > 0);
                }
            });
            self.bridge_state().notify_ui();
        }

        Ok(read_session(self, |s| {
            s.message(DataFullMsgId::new(data_peer, data_msg))
                .map(|m| message_to_history_message(s, m).reactions)
                .unwrap_or_default()
        }))
    }

    /// Loads who reacted to `msg_id` via `messages.getMessageReactionsList`.
    pub(crate) fn message_reactions_list_live(
        &self,
        peer_id: PeerId,
        msg_id: MsgId,
        reaction_filter: Option<&str>,
        offset: Option<&str>,
        limit: usize,
    ) -> Result<MessageReactionsPage, String> {
        let Some(peer) = input_peer(self, peer_id) else {
            return Err("Peer not found in session.".to_owned());
        };
        let req = tl_fns::messages::GetMessageReactionsList {
            peer,
            id: msg_id.0,
            reaction: reaction_filter.map(reaction_str_to_tl),
            offset: offset.map(str::to_owned),
            limit: limit.clamp(1, 100) as i32,
        };
        let tl_enums::messages::MessageReactionsList::MessageReactionsList(list) =
            invoke_rpc(self, req)?;
        write_session(self, |session| {
            apply_tl_users_and_chats(session, &list.users, &list.chats);
        });
        let entries = read_session(self, |session| {
            list.reactions
                .iter()
                .map(|tl_enums::MessagePeerReaction::MessagePeerReaction(r)| {
                    let pid = peer_id_from_tl(&r.peer_id);
                    ReactionPeerEntry {
                        peer_id: from_data_peer(pid),
                        name: peer_display_title(session, pid),
                        username: session.peer(pid).and_then(peer_primary_username),
                        emoji: tl_reaction_to_display(&r.reaction),
                        date_unix: r.date,
                        is_my: r.my,
                        color_index: peer_color_index(session, pid),
                    }
                })
                .collect()
        });
        Ok(MessageReactionsPage {
            total_count: list.count.max(0) as u32,
            entries,
            next_offset: list.next_offset,
        })
    }

    /// Returns the cached default quick reaction emoji.
    pub(crate) fn default_reaction_live(&self) -> String {
        with_compose_extras(self, |st| st.default_reaction.clone())
    }

    /// Sets the user's default quick reaction via `messages.setDefaultReaction`.
    pub(crate) fn set_default_reaction_live(&self, emoji: &str) -> Result<(), String> {
        let trimmed = emoji.trim();
        if trimmed.is_empty() {
            return Err("Default reaction cannot be empty.".to_owned());
        }
        let _ = invoke_rpc(
            self,
            tl_fns::messages::SetDefaultReaction {
                reaction: reaction_str_to_tl(trimmed),
            },
        )?;
        with_compose_extras(self, |st| {
            st.default_reaction = trimmed.to_owned();
        });
        self.bridge_state().notify_ui();
        Ok(())
    }

    /// Fetches Saved Messages reaction tags via `messages.getSavedReactionTags` (cached by hash).
    pub(crate) fn saved_reaction_tags_live(
        &self,
        peer_id: Option<PeerId>,
    ) -> Vec<SavedReactionTagItem> {
        // Delegate to `tg_api::saved::get_saved_reaction_tags` (`Reactions::requestMyTags`,
        // `data/data_message_reactions.cpp:1053-1077`), ensuring the cached hash is reset to `0`
        // on RPC error so the next fetch does not reuse a stale hash.
        let data_sublist = peer_id.map(to_data_peer);
        let rpc = self.runtime().rpc().clone();
        let session_arc = Arc::clone(self.runtime().session());
        let res = block_on_runtime(self.runtime().tokio_handle(), async move {
            tokio::time::timeout(COMPOSE_EXTRAS_RPC_TIMEOUT, async move {
                let mut session = session_arc.write().await;
                tg_api::saved::get_saved_reaction_tags(&rpc, &mut session, data_sublist).await
            })
            .await
        });

        match res {
            Ok(Ok(_modified)) => {
                let (hash, items): (i64, Vec<SavedReactionTagItem>) = read_session(self, |s| {
                    let store = s.saved_reaction_tags();
                    let hash = store.hash_for(data_sublist);
                    let items = store
                        .tags_for(data_sublist)
                        .iter()
                        .map(|t| {
                            let emoji = match &t.reaction {
                                ReactionId::Emoji(e) => e.clone(),
                                ReactionId::Custom(doc) => format!("custom:{}", doc.0),
                                ReactionId::Paid => "⭐".to_owned(),
                            };
                            SavedReactionTagItem {
                                emoji,
                                title: t.title.clone(),
                                count: t.count.max(0) as u32,
                            }
                        })
                        .collect();
                    (hash, items)
                });
                with_compose_extras(self, |st| {
                    st.saved_tags_cache.insert(peer_id, (hash, items.clone()));
                });
                items
            }
            Ok(Err(err)) => {
                debug!(error = %err, "messages.getSavedReactionTags failed");
                with_compose_extras(self, |st| {
                    if let Some((hash, tags)) = st.saved_tags_cache.get_mut(&peer_id) {
                        *hash = 0;
                        tags.clone()
                    } else {
                        Vec::new()
                    }
                })
            }
            Err(_) => {
                debug!("messages.getSavedReactionTags timed out");
                write_session(self, |s| {
                    s.saved_reaction_tags_mut()
                        .reset_hash_on_error(data_sublist);
                });
                with_compose_extras(self, |st| {
                    if let Some((hash, tags)) = st.saved_tags_cache.get_mut(&peer_id) {
                        *hash = 0;
                        tags.clone()
                    } else {
                        Vec::new()
                    }
                })
            }
        }
    }

    /// Converts `messages.getSavedReactionTags` into [`SearchTagChip`]s for `DialogsBackend::search_tags`.
    pub(crate) fn search_tags_live(&self) -> Vec<SearchTagChip> {
        self.saved_reaction_tags_live(None)
            .into_iter()
            .map(|tag| {
                let label = match tag.title {
                    Some(ref title) if !title.is_empty() => format!("{} {}", tag.emoji, title),
                    _ => tag.emoji.clone(),
                };
                SearchTagChip {
                    id: tag.emoji,
                    label,
                    count: tag.count,
                }
            })
            .collect()
    }

    // =========================================================================
    // 6. Polls (`messages.getPollVotes`, retract vote, stop poll)
    // =========================================================================

    /// Fetches voters for a non-anonymous poll via `messages.getPollVotes`.
    pub(crate) fn poll_votes_live(
        &self,
        peer_id: PeerId,
        msg_id: MsgId,
        option_index: Option<usize>,
        offset: Option<&str>,
        limit: usize,
    ) -> Result<PollVotesPage, String> {
        let Some(peer) = input_peer(self, peer_id) else {
            return Err("Peer not found in session.".to_owned());
        };
        let data_peer = to_data_peer(peer_id);
        let data_msg = to_data_msg(msg_id);
        let answer_options: Vec<Vec<u8>> = read_session(self, |s| {
            s.message(DataFullMsgId::new(data_peer, data_msg))
                .and_then(|m| match m.media() {
                    Some(MessageMedia::Poll(p)) => {
                        Some(p.answers.iter().map(|a| a.option.clone()).collect())
                    }
                    _ => None,
                })
                .unwrap_or_default()
        });
        let requested_option = match option_index {
            Some(idx) => Some(
                answer_options
                    .get(idx)
                    .cloned()
                    .ok_or_else(|| format!("Poll option index {idx} out of range."))?,
            ),
            None => None,
        };
        let req = tl_fns::messages::GetPollVotes {
            peer,
            id: msg_id.0,
            option: requested_option.clone(),
            offset: offset.map(str::to_owned),
            limit: limit.clamp(1, 100) as i32,
        };
        let tl_enums::messages::VotesList::VotesList(list) = invoke_rpc(self, req)?;
        write_session(self, |session| {
            apply_tl_users_and_chats(session, &list.users, &list.chats);
        });
        let voters = read_session(self, |session| {
            list.votes
                .iter()
                .map(|vote| {
                    let (pid, chosen_bytes, date) = match vote {
                        tl_enums::MessagePeerVote::MessagePeerVote(v) => {
                            (peer_id_from_tl(&v.peer), vec![v.option.clone()], v.date)
                        }
                        tl_enums::MessagePeerVote::InputOption(v) => (
                            peer_id_from_tl(&v.peer),
                            requested_option.clone().into_iter().collect(),
                            v.date,
                        ),
                        tl_enums::MessagePeerVote::Multiple(v) => {
                            (peer_id_from_tl(&v.peer), v.options.clone(), v.date)
                        }
                    };
                    let option_indices: Vec<usize> = chosen_bytes
                        .iter()
                        .filter_map(|opt| answer_options.iter().position(|a| a == opt))
                        .collect();
                    PollVoterEntry {
                        peer_id: from_data_peer(pid),
                        name: peer_display_title(session, pid),
                        username: session.peer(pid).and_then(peer_primary_username),
                        option_indices,
                        date_unix: date,
                        color_index: peer_color_index(session, pid),
                    }
                })
                .collect()
        });
        Ok(PollVotesPage {
            total_count: list.count.max(0) as u32,
            voters,
            next_offset: list.next_offset,
        })
    }

    /// Retracts the current user's vote via `messages.sendVote` with an empty `options` vector.
    pub(crate) fn retract_poll_vote_live(
        &self,
        peer_id: PeerId,
        msg_id: MsgId,
    ) -> Result<(), String> {
        let Some(peer) = input_peer(self, peer_id) else {
            return Err("Peer not found in session.".to_owned());
        };
        let data_peer = to_data_peer(peer_id);
        let data_msg = to_data_msg(msg_id);
        let check = read_session(self, |s| {
            let msg = s.message(DataFullMsgId::new(data_peer, data_msg))?;
            match msg.media() {
                Some(MessageMedia::Poll(p)) => Some((p.closed, p.quiz)),
                _ => None,
            }
        });
        if let Some((closed, quiz)) = check {
            if closed {
                return Err("Cannot retract vote on a closed poll.".to_owned());
            }
            if quiz {
                return Err("Cannot retract vote in quiz mode.".to_owned());
            }
        }
        let req = tl_fns::messages::SendVote {
            peer,
            msg_id: msg_id.0,
            options: Vec::new(),
        };
        let updates = invoke_rpc(self, req)?;
        let _ = apply_updates(self, &updates);

        // Optimistically clear local chosen flags in case the update did not include a full poll.
        write_session(self, |s| {
            if let Some(msg) = s.message_mut(DataFullMsgId::new(data_peer, data_msg))
                && let MessageContent::Regular {
                    media: MessageMedia::Poll(ref mut poll),
                    ..
                } = msg.content
            {
                let mut had_any = false;
                for res in &mut poll.results.results {
                    if res.chosen {
                        res.chosen = false;
                        res.voters = res.voters.saturating_sub(1);
                        had_any = true;
                    }
                }
                if had_any && let Some(ref mut total) = poll.results.total_voters {
                    *total = total.saturating_sub(1);
                }
            }
        });
        self.bridge_state().notify_ui();
        Ok(())
    }

    /// Stops/closes a poll via `messages.editMessage` with `InputMediaPoll { poll.closed = true }`.
    ///
    /// Direct port of `Api::Polls::close` (`api/api_polls.cpp:430-459`).
    pub(crate) fn stop_poll_live(&self, peer_id: PeerId, msg_id: MsgId) -> Result<(), String> {
        let Some(peer) = input_peer(self, peer_id) else {
            return Err("Peer not found in session.".to_owned());
        };
        let data_peer = to_data_peer(peer_id);
        let data_msg = to_data_msg(msg_id);
        let poll_data = read_session(self, |s| {
            s.message(DataFullMsgId::new(data_peer, data_msg))
                .and_then(|m| match m.media() {
                    Some(MessageMedia::Poll(p)) => Some(p.clone()),
                    _ => None,
                })
        })
        .ok_or_else(|| "Poll message not found in session.".to_owned())?;

        if poll_data.closed {
            return Ok(());
        }

        let answers: Vec<tl_enums::PollAnswer> = poll_data
            .answers
            .iter()
            .map(|ans| {
                tl_enums::PollAnswer::PollAnswer(tl_types::PollAnswer {
                    text: tl_enums::TextWithEntities::TextWithEntities(
                        tl_types::TextWithEntities {
                            text: ans.text.text.clone(),
                            entities: Vec::new(),
                        },
                    ),
                    option: ans.option.clone(),
                    media: None,
                    added_by: None,
                    date: None,
                })
            })
            .collect();

        let closed_poll = tl_enums::Poll::Poll(tl_types::Poll {
            id: poll_data.id.0 as i64,
            closed: true,
            public_voters: poll_data.public_voters,
            multiple_choice: poll_data.multiple_choice,
            quiz: poll_data.quiz,
            open_answers: false,
            revoting_disabled: false,
            shuffle_answers: false,
            hide_results_until_close: false,
            creator: true,
            subscribers_only: false,
            question: tl_enums::TextWithEntities::TextWithEntities(tl_types::TextWithEntities {
                text: poll_data.question.text.clone(),
                entities: Vec::new(),
            }),
            answers,
            close_period: poll_data.close_period,
            close_date: poll_data.close_date,
            countries_iso2: None,
            hash: 0,
        });

        let req = tl_fns::messages::EditMessage {
            no_webpage: false,
            invert_media: false,
            peer,
            id: msg_id.0,
            message: None,
            media: Some(tl_enums::InputMedia::Poll(tl_types::InputMediaPoll {
                poll: closed_poll,
                correct_answers: None,
                attached_media: None,
                solution: None,
                solution_entities: None,
                solution_media: None,
            })),
            reply_markup: None,
            entities: None,
            schedule_date: None,
            schedule_repeat_period: None,
            quick_reply_shortcut_id: None,
            rich_message: None,
        };
        if let Some(updates) = self.edit_message_via_runtime(req)? {
            self.apply_updates_side_caches(&updates);
            write_session(self, |session| {
                let mut inspect = |upd: &tl_enums::Update| {
                    if let tl_enums::Update::MessagePoll(u) = upd {
                        apply_poll_update(session, u);
                    }
                };
                match &updates {
                    tl_enums::Updates::Updates(u) => u.updates.iter().for_each(&mut inspect),
                    tl_enums::Updates::Combined(u) => u.updates.iter().for_each(&mut inspect),
                    tl_enums::Updates::UpdateShort(s) => inspect(&s.update),
                    _ => {}
                }
            });
        }

        write_session(self, |s| {
            if let Some(msg) = s.message_mut(DataFullMsgId::new(data_peer, data_msg))
                && let MessageContent::Regular {
                    media: MessageMedia::Poll(ref mut poll),
                    ..
                } = msg.content
            {
                poll.closed = true;
            }
        });
        self.bridge_state().notify_ui();
        Ok(())
    }

    // =========================================================================
    // 7. Live Message Effects (`messages.getAvailableEffects`)
    // =========================================================================

    /// Fetches (and hash-caches) the available message effects catalog via
    /// `messages.getAvailableEffects`, mirroring `Data::Reactions::requestEffects`
    /// (`data_message_reactions.cpp:1099-1241`).
    pub fn refresh_available_effects(&self) -> Result<Vec<AvailableMessageEffect>, String> {
        let hash = with_compose_extras(self, |st| st.available_effects_hash);
        let resp = invoke_rpc(self, tl_fns::messages::GetAvailableEffects { hash })?;
        match resp {
            tl_enums::messages::AvailableEffects::NotModified => {
                Ok(with_compose_extras(self, |st| {
                    st.available_effects_loaded = true;
                    st.available_effects.clone()
                }))
            }
            tl_enums::messages::AvailableEffects::AvailableEffects(data) => {
                let mut docs = HashMap::new();
                for doc_enum in data.documents {
                    if let tl_enums::Document::Document(doc) = doc_enum {
                        docs.insert(doc.id, doc);
                    }
                }
                let items: Vec<AvailableMessageEffect> = data
                    .effects
                    .into_iter()
                    .map(|tl_enums::AvailableEffect::AvailableEffect(eff)| {
                        let id = eff.id as u64;
                        let (name, accent_rgb) = effect_name_and_accent(id, &eff.emoticon);
                        AvailableMessageEffect {
                            id,
                            emoji: eff.emoticon,
                            name,
                            premium_required: eff.premium_required,
                            static_icon_id: eff.static_icon_id,
                            effect_sticker_id: eff.effect_sticker_id,
                            effect_animation_id: eff.effect_animation_id,
                            lottie_json: None,
                            accent_rgb,
                        }
                    })
                    .collect();
                with_compose_extras(self, |st| {
                    st.available_effects_hash = data.hash;
                    st.available_effects_loaded = true;
                    st.available_effects = items.clone();
                    st.effect_documents = docs;
                    st.effect_lottie_cache.clear();
                });
                self.bridge_state().notify_ui();
                Ok(items)
            }
        }
    }

    /// Returns cached available message effects, querying `messages.getAvailableEffects` on
    /// first access.
    pub(crate) fn available_effects_live(&self) -> Vec<AvailableMessageEffect> {
        let cached = with_compose_extras(self, |st| {
            st.available_effects_loaded
                .then(|| st.available_effects.clone())
        });
        if let Some(list) = cached {
            return list;
        }
        match self.refresh_available_effects() {
            Ok(list) => list,
            Err(err) => {
                debug!(error = %err, "messages.getAvailableEffects failed");
                with_compose_extras(self, |st| st.available_effects.clone())
            }
        }
    }

    /// Downloads and decompresses the `.tgs` / Lottie JSON animation attached to `effect_id`
    /// (`effect_animation_id` or `effect_sticker_id` from `messages.getAvailableEffects`).
    pub(crate) fn effect_lottie_json_live(&self, effect_id: u64) -> Option<String> {
        if let Some(cached) =
            with_compose_extras(self, |st| st.effect_lottie_cache.get(&effect_id).cloned())
        {
            return cached;
        }
        let loaded = with_compose_extras(self, |st| st.available_effects_loaded);
        if !loaded {
            let _ = self.available_effects_live();
        }
        let doc = with_compose_extras(self, |st| {
            let eff = st.available_effects.iter().find(|e| e.id == effect_id)?;
            eff.effect_animation_id
                .and_then(|id| st.effect_documents.get(&id).cloned())
                .or_else(|| st.effect_documents.get(&eff.effect_sticker_id).cloned())
        })?;

        let location = tl_enums::InputFileLocation::InputDocumentFileLocation(
            tl_types::InputDocumentFileLocation {
                id: doc.id,
                access_hash: doc.access_hash,
                file_reference: doc.file_reference.clone(),
                thumb_size: String::new(),
            },
        );
        let downloaded = download_file_bytes(
            self.runtime().rpc(),
            self.runtime().tokio_handle(),
            doc.dc_id,
            location,
            (doc.size > 0).then_some(doc.size as u64),
        );
        let parsed = match downloaded {
            Ok(bytes) => decompress_tgs_to_lottie_json(&bytes).ok().or_else(|| {
                String::from_utf8(bytes)
                    .ok()
                    .filter(|s| s.trim_start().starts_with('{'))
            }),
            Err(err) => {
                debug!(effect_id, error = %err, "failed to download effect animation document");
                None
            }
        };
        with_compose_extras(self, |st| {
            st.effect_lottie_cache.insert(effect_id, parsed.clone());
        });
        parsed
    }
}

fn data_reaction_to_strip_string(id: &ReactionId, allow_custom: bool) -> Option<String> {
    match id {
        ReactionId::Emoji(e) if !e.is_empty() => Some(e.clone()),
        ReactionId::Custom(doc_id) if allow_custom && !doc_id.is_zero() => {
            Some(format!("custom:{}", doc_id.0))
        }
        ReactionId::Paid | ReactionId::Emoji(_) | ReactionId::Custom(_) => None,
    }
}

fn top_reaction_strings(list: &[ReactionId], allow_custom: bool) -> Vec<String> {
    list.iter()
        .filter_map(|id| data_reaction_to_strip_string(id, allow_custom))
        .collect()
}
