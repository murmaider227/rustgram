//! Live Layer-233 rich message RPCs (`messages.sendMessage` / `messages.editMessage` /
//! `messages.getRichMessage`) and per-session rich-message store for [`LiveSession`].
//!
//! Port origin: `Telegram/SourceFiles/api/api_sending.cpp` (`SendRichMessage`),
//! `Telegram/SourceFiles/api/api_editing.cpp` (`EditRichMessage`), and
//! `Telegram/SourceFiles/iv/iv_rich_page.cpp`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, Weak};

use tg_api::convert::apply_tl_users_and_chats;
use tg_data::{
    FullMsgId as DataFullMsgId, Message, MessageContent, MessageFlags, MsgId as DataMsgId, Session,
    TextWithEntities as DataText,
};
use tg_iv::rich_page::{RichPage, flatten_rich_page_summary};
use tg_text::TextWithEntities;
use tg_tl::{enums as tl_enums, functions as tl_fns, types as tl_types};
use tokio::sync::RwLock;

use crate::convert::{
    from_data_msg, from_data_peer, message_to_history_message, now_unix_i32, peer_display_title,
    to_data_msg, to_data_peer,
};
use crate::session::LiveSession;
use tg_app_core::core_types::dto::rich_message::{RichMessagePayload, payload_from_tl_enum};
use tg_app_core::history::backend::{HistoryMessage, MessageExtra};
use tg_app_core::stand_in::types::{MsgId, PeerId};

// ============================================================================
// Per-Session Rich Message Cache (`Weak<RwLock<Session>>` keyed, matching live/webview.rs)
// ============================================================================

#[derive(Default)]
struct RichMessageStore {
    by_msg: HashMap<(PeerId, MsgId), RichMessagePayload>,
    next_random_id: u64,
}

impl RichMessageStore {
    fn alloc_random_id(&mut self) -> i64 {
        let now = now_unix_i32() as u64;
        self.next_random_id = self.next_random_id.wrapping_add(1);
        ((now << 24) ^ self.next_random_id ^ 0x5249_4348_2026) as i64
    }
}

type StoreRegistry = Mutex<Vec<(Weak<RwLock<Session>>, Arc<Mutex<RichMessageStore>>)>>;

fn store_registry() -> &'static StoreRegistry {
    static REGISTRY: OnceLock<StoreRegistry> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(Vec::new()))
}

fn store_for_session(session: &Arc<RwLock<Session>>) -> Arc<Mutex<RichMessageStore>> {
    let key = Arc::downgrade(session);
    let mut guard = store_registry().lock().unwrap_or_else(|e| e.into_inner());
    guard.retain(|(weak, _)| weak.strong_count() > 0);
    if let Some((_, store)) = guard.iter().find(|(weak, _)| Weak::ptr_eq(weak, &key)) {
        return Arc::clone(store);
    }
    let store = Arc::new(Mutex::new(RichMessageStore::default()));
    guard.push((key, Arc::clone(&store)));
    store
}

/// Records a [`RichMessagePayload`] for `(peer_id, msg_id)` in `session`'s live cache.
pub fn cache_rich_message(
    session: &Arc<RwLock<Session>>,
    peer_id: PeerId,
    msg_id: MsgId,
    payload: RichMessagePayload,
) {
    let store = store_for_session(session);
    if let Ok(mut guard) = store.lock() {
        guard.by_msg.insert((peer_id, msg_id), payload);
    }
}

/// Looks up a cached [`RichMessagePayload`] for `(peer_id, msg_id)`.
#[must_use]
pub fn get_cached_rich_message(
    session: &Arc<RwLock<Session>>,
    peer_id: PeerId,
    msg_id: MsgId,
) -> Option<RichMessagePayload> {
    let store = store_for_session(session);
    store
        .lock()
        .ok()
        .and_then(|g| g.by_msg.get(&(peer_id, msg_id)).cloned())
}

/// Attaches cached [`RichMessagePayload`]s onto a slice of [`HistoryMessage`]s.
pub fn attach_cached_rich_messages(
    session: &Arc<RwLock<Session>>,
    messages: &mut [HistoryMessage],
) {
    let store = store_for_session(session);
    let Ok(guard) = store.lock() else {
        return;
    };
    if guard.by_msg.is_empty() {
        return;
    }
    for msg in messages {
        if msg.extra.rich_message.is_none()
            && let Some(payload) = guard.by_msg.get(&(msg.peer_id, msg.id))
        {
            msg.extra.rich_message = Some(payload.clone());
        }
    }
}

/// Scans an incoming TL [`tl_enums::Updates`] batch and caches any `Message::rich_message`
/// payloads via [`tg_iv::rich_page::rich_page_from_tl_rich_message`].
pub fn record_rich_messages_from_updates(
    session: &Arc<RwLock<Session>>,
    updates: &tl_enums::Updates,
    self_uid: i64,
) {
    match updates {
        tl_enums::Updates::Updates(u) => {
            for upd in &u.updates {
                record_rich_from_single_update(session, upd, self_uid);
            }
        }
        tl_enums::Updates::Combined(u) => {
            for upd in &u.updates {
                record_rich_from_single_update(session, upd, self_uid);
            }
        }
        _ => {}
    }
}

fn record_rich_from_single_update(
    session: &Arc<RwLock<Session>>,
    upd: &tl_enums::Update,
    self_uid: i64,
) {
    let tl_msg = match upd {
        tl_enums::Update::NewMessage(u) => &u.message,
        tl_enums::Update::NewChannelMessage(u) => &u.message,
        tl_enums::Update::EditMessage(u) => &u.message,
        tl_enums::Update::EditChannelMessage(u) => &u.message,
        _ => return,
    };
    record_rich_from_tl_message(session, tl_msg, self_uid);
}

fn record_rich_from_tl_message(
    session: &Arc<RwLock<Session>>,
    tl_msg: &tl_enums::Message,
    self_uid: i64,
) {
    let tl_enums::Message::Message(m) = tl_msg else {
        return;
    };
    let Some(ref rm) = m.rich_message else {
        return;
    };
    let peer_id = peer_id_from_tl_peer(&m.peer_id, m.out, m.from_id.as_ref(), self_uid);
    let payload = payload_from_tl_enum(rm);
    cache_rich_message(session, peer_id, MsgId(m.id), payload);
}

fn peer_id_from_tl_peer(
    peer: &tl_enums::Peer,
    is_out: bool,
    from_id: Option<&tl_enums::Peer>,
    self_uid: i64,
) -> PeerId {
    match peer {
        tl_enums::Peer::User(u) => {
            if !is_out
                && u.user_id == self_uid
                && let Some(tl_enums::Peer::User(fu)) = from_id
            {
                PeerId::from_user(fu.user_id as u64)
            } else {
                PeerId::from_user(u.user_id as u64)
            }
        }
        tl_enums::Peer::Chat(c) => PeerId::from_chat(c.chat_id as u64),
        tl_enums::Peer::Channel(ch) => PeerId::from_channel(ch.channel_id as u64),
    }
}

fn make_input_reply(reply_to: MsgId) -> tl_enums::InputReplyTo {
    tl_enums::InputReplyTo::Message(tl_types::InputReplyToMessage {
        quote_rich: None,
        reply_to_msg_id: reply_to.0,
        top_msg_id: None,
        reply_to_peer_id: None,
        quote_text: None,
        quote_entities: None,
        quote_offset: None,
        monoforum_peer_id: None,
        todo_item_id: None,
        poll_option: None,
    })
}

/// Sends a Layer-233 rich message via `messages.sendMessage` with `rich_message: Some(...)`.
pub fn send_rich_message_live(
    live: &mut LiveSession,
    peer_id: PeerId,
    input: tl_types::InputRichMessage,
    page: RichPage,
    reply_to: Option<MsgId>,
    silent: bool,
) -> Result<HistoryMessage, String> {
    let Some(input_peer) = live.input_peer(peer_id) else {
        return Err("Peer not found in session.".to_owned());
    };
    let session_arc = Arc::clone(live.runtime().session());
    let random_id = store_for_session(&session_arc)
        .lock()
        .map_err(|_| "Rich message store lock poisoned".to_owned())?
        .alloc_random_id();

    let payload = RichMessagePayload::from_page_and_input(page.clone(), &input);
    let summary_text = flatten_rich_page_summary(&page, true).text;

    let req = tl_fns::messages::SendMessage {
        no_webpage: false,
        silent,
        background: false,
        clear_draft: true,
        noforwards: false,
        update_stickersets_order: false,
        invert_media: false,
        allow_paid_floodskip: false,
        peer: input_peer,
        reply_to: reply_to.map(make_input_reply),
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
        rich_message: Some(tl_enums::InputRichMessage::InputRichMessage(input)),
    };

    let updates = live.invoke_rpc(req)?;
    let new_ids = live.apply_updates_to_session(&updates);

    let data_peer = to_data_peer(peer_id);
    let assigned_msg_id = new_ids
        .last()
        .copied()
        .unwrap_or_else(|| DataMsgId(i64::from((random_id.abs() as i32).max(1))));
    let ui_msg_id = from_data_msg(assigned_msg_id);

    // Ensure the rich payload is cached even if the server returned UpdateShortSentMessage
    // without echoing the full `RichMessage` object back.
    if get_cached_rich_message(&session_arc, peer_id, ui_msg_id).is_none() {
        cache_rich_message(&session_arc, peer_id, ui_msg_id, payload.clone());
    }
    let final_payload =
        get_cached_rich_message(&session_arc, peer_id, ui_msg_id).unwrap_or(payload);

    // Ensure a local `tg_data::Message` exists in the session history with summary text.
    live.write_session(|s| {
        let full = DataFullMsgId::new(data_peer, assigned_msg_id);
        if s.message(full).is_none() {
            let self_pid = s.self_peer_id();
            let mut m = Message::new_text(
                data_peer,
                assigned_msg_id,
                Some(self_pid),
                now_unix_i32(),
                DataText::plain(summary_text.clone()),
            );
            m.flags
                .insert(MessageFlags::OUTGOING | MessageFlags::HISTORY_ENTRY);
            s.insert_message(m);
        }
    });

    let mut result = live.read_session(|s| {
        s.message(DataFullMsgId::new(data_peer, assigned_msg_id))
            .map(|m| message_to_history_message(s, m))
            .unwrap_or_else(|| {
                let self_pid = s.self_peer_id();
                HistoryMessage {
                    id: ui_msg_id,
                    peer_id,
                    from_id: from_data_peer(self_pid),
                    sender_name: peer_display_title(s, self_pid),
                    date_unix: now_unix_i32(),
                    outgoing: true,
                    is_post: false,
                    text: TextWithEntities::simple(summary_text.clone()),
                    media: None,
                    service: None,
                    reply: None,
                    forward: None,
                    reactions: Vec::new(),
                    views: None,
                    edited: false,
                    is_read: false,
                    is_sending: false,
                    is_pinned: false,
                    extra: MessageExtra::default(),
                }
            })
    });
    if result.text.text.is_empty() {
        result.text = TextWithEntities::simple(summary_text);
    }
    result.extra.rich_message = Some(final_payload);
    live.bridge_state().notify_ui();
    Ok(result)
}

/// Edits an existing Layer-233 rich message via `messages.editMessage` with `rich_message: Some(...)`.
pub fn edit_rich_message_live(
    live: &mut LiveSession,
    peer_id: PeerId,
    msg_id: MsgId,
    input: tl_types::InputRichMessage,
    page: RichPage,
) -> Result<HistoryMessage, String> {
    let Some(input_peer) = live.input_peer(peer_id) else {
        return Err("Peer not found in session.".to_owned());
    };
    let session_arc = Arc::clone(live.runtime().session());
    let payload = RichMessagePayload::from_page_and_input(page.clone(), &input);
    let summary_text = flatten_rich_page_summary(&page, true).text;

    let req = tl_fns::messages::EditMessage {
        no_webpage: false,
        invert_media: false,
        peer: input_peer,
        id: msg_id.0,
        message: None,
        media: None,
        reply_markup: None,
        entities: None,
        schedule_date: None,
        schedule_repeat_period: None,
        quick_reply_shortcut_id: None,
        rich_message: Some(tl_enums::InputRichMessage::InputRichMessage(input)),
    };

    if let Some(updates) = live.edit_message_via_runtime(req)? {
        live.apply_updates_side_caches(&updates);
    }

    cache_rich_message(&session_arc, peer_id, msg_id, payload.clone());

    let data_peer = to_data_peer(peer_id);
    let data_msg = to_data_msg(msg_id);
    let edit_ts = now_unix_i32();
    live.write_session(|s| {
        if let Some(m) = s.message_mut(DataFullMsgId::new(data_peer, data_msg)) {
            if let MessageContent::Regular {
                text: ref mut t, ..
            } = m.content
            {
                *t = DataText::plain(summary_text.clone());
            }
            m.edit_date = Some(edit_ts);
        }
    });

    let mut result = live
        .read_session(|s| {
            s.message(DataFullMsgId::new(data_peer, data_msg))
                .map(|m| message_to_history_message(s, m))
        })
        .ok_or_else(|| "Edited rich message not found in session.".to_owned())?;
    result.text = TextWithEntities::simple(summary_text);
    result.edited = true;
    result.extra.rich_message = Some(payload);
    live.bridge_state().notify_ui();
    Ok(result)
}

/// Fetches the full Layer-233 `RichMessage` for `(peer_id, msg_id)` via `messages.getRichMessage`
/// and caches its decoded [`RichMessagePayload`].
pub fn fetch_rich_message_live(
    live: &LiveSession,
    peer_id: PeerId,
    msg_id: MsgId,
) -> Result<Option<RichMessagePayload>, String> {
    let Some(input_peer) = live.input_peer(peer_id) else {
        return Err("Peer not found in session.".to_owned());
    };
    let req = tl_fns::messages::GetRichMessage {
        peer: input_peer,
        id: msg_id.0,
    };
    let resp = live.invoke_rpc(req)?;
    // Apply accompanying `users` and `chats` to `Session` so peers referenced inside the hydrated
    // rich message resolve locally (`Telegram/SourceFiles/iv/iv_rich_page.cpp`).
    let (messages, users, chats, channel_pts) = match resp {
        tl_enums::messages::Messages::Messages(m) => (m.messages, m.users, m.chats, None),
        tl_enums::messages::Messages::Slice(m) => (m.messages, m.users, m.chats, None),
        tl_enums::messages::Messages::ChannelMessages(m) => {
            (m.messages, m.users, m.chats, (m.pts > 0).then_some(m.pts))
        }
        tl_enums::messages::Messages::NotModified(_) => (Vec::new(), Vec::new(), Vec::new(), None),
    };
    if let Some(pts) = channel_pts
        && let Some(ch_id) = crate::convert::to_data_peer(peer_id).to_channel()
    {
        let _ = live.apply_channel_pts(ch_id, pts, 0);
    }
    if !users.is_empty() || !chats.is_empty() {
        live.write_session(|s| {
            apply_tl_users_and_chats(s, &users, &chats);
        });
    }
    let session_arc = Arc::clone(live.runtime().session());
    let self_uid = live.read_session(|s| s.self_user_id().0 as i64);
    for msg in &messages {
        record_rich_from_tl_message(&session_arc, msg, self_uid);
    }
    Ok(get_cached_rich_message(&session_arc, peer_id, msg_id))
}
