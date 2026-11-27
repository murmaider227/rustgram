//! Live Layer-233 history extras (`Layer233ExtrasBackend` for [`LiveSession`]):
//! - Checklists / To-Do lists (`messages.sendMedia(inputMediaTodo)`, `messages.editMessage`,
//!   `messages.toggleTodoCompleted`, `messages.appendTodoList`)
//! - Sponsored messages (`messages.getSponsoredMessages`, `messages.viewSponsoredMessage`,
//!   `messages.clickSponsoredMessage`, `messages.reportSponsoredMessage`,
//!   `account.toggleSponsoredMessages`)
//! - Fact checks (`messages.getFactCheck` batched, `messages.editFactCheck`,
//!   `messages.deleteFactCheck`)
//! - Suggested posts & channel direct messages / monoforum (`messages.getSavedDialogs`,
//!   `messages.getSavedHistory`, `messages.readSavedHistory`, `messages.sendMessage` with
//!   `inputReplyToMonoForum` + `suggestedPost`, `messages.toggleSuggestedPostApproval`)
//!
//! Port origin:
//! - `Telegram/SourceFiles/api/api_todo_lists.cpp`
//! - `Telegram/SourceFiles/data/components/sponsored_messages.cpp`
//! - `Telegram/SourceFiles/data/components/factchecks.cpp`
//! - `Telegram/SourceFiles/api/api_suggest_post.cpp`
//! - `Telegram/SourceFiles/data/data_saved_sublist.cpp`

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

use tg_api::RpcClient;
use tg_api::convert::{
    apply_tl_users_and_chats, entities_from_tl_for_text, input_peer_for, message_from_tl,
};
use tg_data::{
    FullMsgId as DataFullMsgId, MessageContent, MessageMedia, Peer, PeerId as DataPeerId, Session,
    TodoCompletionData, TodoItemData,
};
use tg_text::TextWithEntities;
use tg_tl::{RemoteCall, enums as tl_enums, functions as tl_fns, types as tl_types};

use crate::bridge::LiveBridgeState;
use crate::convert::{
    avatar_rgb_for_color_index, from_data_peer, message_to_history_message, now_unix_i32,
    peer_display_title, to_data_msg, to_data_peer, ui_text_to_data_text, ui_text_to_tl_entities,
};
use crate::media::{block_on_runtime, with_rpc_timeout};
use crate::session::LiveSession;
use tg_app_core::core_types::dto::factcheck::{FactCheckBatcher, FactCheckData};
use tg_app_core::core_types::dto::monoforum::{
    MonoforumSublist, SuggestApprovalDecision, SuggestOptions, SuggestedCurrency,
    SuggestedPostData, SuggestedPrice,
};
use tg_app_core::core_types::dto::sponsored::{
    SponsoredMessageItem, SponsoredMessagesState, SponsoredPeerList, SponsoredPlacementMode,
    SponsoredReportOption, SponsoredReportOutcome,
};
use tg_app_core::core_types::dto::todo::{CreateTodoListRequest, TodoItemInput, TodoToggleBatcher};
use tg_app_core::history::backend::{HistoryMessage, Layer233ExtrasBackend};
use tg_app_core::stand_in::types::{FullMsgId, MsgId, PeerId};

/// Default timeout for synchronous Layer-233 RPC invocations.
const LAYER233_RPC_TIMEOUT: Duration = Duration::from_secs(10);

/// Per-session cached state for WP-15 Layer-233 features so clones of [`LiveSession`] share
/// state without expanding `LiveSession`'s struct layout.
#[derive(Default)]
struct Layer233LiveState {
    todo_batcher: TodoToggleBatcher,
    sponsored: SponsoredMessagesState,
    factchecks: HashMap<(PeerId, MsgId), FactCheckData>,
    factcheck_batcher: FactCheckBatcher,
    can_edit_factcheck: bool,
    monoforum_sublists: HashMap<PeerId, Vec<MonoforumSublist>>,
    monoforum_messages: HashMap<(PeerId, PeerId), Vec<HistoryMessage>>,
    suggested_posts: HashMap<(PeerId, MsgId), SuggestedPostData>,
    random_seq: u64,
}

impl Layer233LiveState {
    fn next_random_id(&mut self) -> i64 {
        let now = now_unix_i32() as u64;
        self.random_seq = self.random_seq.wrapping_add(1);
        ((now << 24) ^ self.random_seq ^ 0x4C32_3333_7711) as i64
    }
}

struct Layer233StateEntry {
    owner: Weak<LiveBridgeState>,
    state: Layer233LiveState,
}

fn layer233_state_registry() -> &'static Mutex<HashMap<usize, Layer233StateEntry>> {
    static REGISTRY: OnceLock<Mutex<HashMap<usize, Layer233StateEntry>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn with_state<R>(live: &LiveSession, f: impl FnOnce(&mut Layer233LiveState) -> R) -> R
where
{
    let key = Arc::as_ptr(live.bridge_state()) as usize;
    let mut map = layer233_state_registry()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    map.retain(|_, entry| entry.owner.strong_count() > 0);
    let entry = map.entry(key).or_insert_with(|| Layer233StateEntry {
        owner: Arc::downgrade(live.bridge_state()),
        state: Layer233LiveState::default(),
    });
    f(&mut entry.state)
}

// -----------------------------------------------------------------------------
// Internal RPC & Session helpers
// -----------------------------------------------------------------------------

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
        with_rpc_timeout(LAYER233_RPC_TIMEOUT, async move { rpc.invoke(&req).await }).await
    })
}

fn resolve_input_peer(live: &LiveSession, peer_id: PeerId) -> Option<tl_enums::InputPeer> {
    let data_peer = to_data_peer(peer_id);
    read_session(live, |s| {
        let ip = input_peer_for(s, data_peer);
        (!matches!(ip, tl_enums::InputPeer::Empty)).then_some(ip)
    })
}

fn self_is_premium(live: &LiveSession) -> bool {
    read_session(live, |s| {
        s.peer(s.self_peer_id())
            .and_then(Peer::as_user)
            .is_some_and(tg_data::User::is_premium)
    })
}

fn apply_updates_to_session(
    live: &LiveSession,
    updates: &tl_enums::Updates,
) -> Vec<(PeerId, MsgId)> {
    let _ = live.apply_updates_to_session(updates);
    extract_layer233_extras_from_updates(live, updates)
}

fn extract_layer233_extras_from_updates(
    live: &LiveSession,
    updates: &tl_enums::Updates,
) -> Vec<(PeerId, MsgId)> {
    let mut touched = Vec::new();
    let mut extracted_suggestions: Vec<(PeerId, MsgId, SuggestedPostData)> = Vec::new();
    let mut extracted_factchecks: Vec<(PeerId, MsgId, FactCheckData)> = Vec::new();

    let mut handle_tl_msg = |tl_msg: &tl_enums::Message| {
        if let tl_enums::Message::Message(m) = tl_msg {
            let dp = data_peer_from_tl(&m.peer_id);
            let ui_peer = from_data_peer(dp);
            let ui_msg = MsgId(m.id);
            if let Some(ref sp) = m.suggested_post {
                extracted_suggestions.push((
                    ui_peer,
                    ui_msg,
                    suggested_post_from_tl(
                        sp,
                        m.paid_suggested_post_stars,
                        m.paid_suggested_post_ton,
                    ),
                ));
            }
            if let Some(ref fc) = m.factcheck {
                extracted_factchecks.push((ui_peer, ui_msg, factcheck_from_tl(fc)));
            }
            touched.push((ui_peer, ui_msg));
        } else if let tl_enums::Message::Service(s) = tl_msg {
            let dp = data_peer_from_tl(&s.peer_id);
            touched.push((from_data_peer(dp), MsgId(s.id)));
        }
    };

    let list: &[tl_enums::Update] = match updates {
        tl_enums::Updates::Updates(u) => &u.updates,
        tl_enums::Updates::Combined(u) => &u.updates,
        tl_enums::Updates::UpdateShort(s) => std::slice::from_ref(&s.update),
        _ => &[],
    };
    for upd in list {
        match upd {
            tl_enums::Update::NewMessage(nm) => handle_tl_msg(&nm.message),
            tl_enums::Update::NewChannelMessage(nm) => handle_tl_msg(&nm.message),
            tl_enums::Update::EditMessage(em) => handle_tl_msg(&em.message),
            tl_enums::Update::EditChannelMessage(em) => handle_tl_msg(&em.message),
            _ => {}
        }
    }

    if !extracted_suggestions.is_empty() || !extracted_factchecks.is_empty() {
        with_state(live, |st| {
            for (p, m, sp) in extracted_suggestions {
                st.suggested_posts.insert((p, m), sp);
            }
            for (p, m, fc) in extracted_factchecks {
                st.factchecks.insert((p, m), fc);
            }
        });
    }
    live.bridge_state().notify_ui();
    touched
}

// -----------------------------------------------------------------------------
// TL conversion helpers
// -----------------------------------------------------------------------------

fn data_peer_from_tl(peer: &tl_enums::Peer) -> DataPeerId {
    match peer {
        tl_enums::Peer::User(u) => DataPeerId::from_user(tg_data::UserId::from_tl(u.user_id)),
        tl_enums::Peer::Chat(c) => DataPeerId::from_chat(tg_data::ChatId::from_tl(c.chat_id)),
        tl_enums::Peer::Channel(ch) => {
            DataPeerId::from_channel(tg_data::ChannelId::from_tl(ch.channel_id))
        }
    }
}

fn ui_text_to_tl_text_with_entities(
    text: &TextWithEntities,
    session: &Session,
) -> tl_enums::TextWithEntities {
    tl_enums::TextWithEntities::TextWithEntities(tl_types::TextWithEntities {
        text: text.text.clone(),
        entities: ui_text_to_tl_entities(text, session),
    })
}

fn tl_text_with_entities_to_ui(text: &tl_enums::TextWithEntities) -> TextWithEntities {
    let tl_enums::TextWithEntities::TextWithEntities(inner) = text;
    let data_entities =
        entities_from_tl_for_text(&inner.text, &inner.entities, tg_data::UserId::ZERO);
    let data_text = tg_data::TextWithEntities {
        text: inner.text.clone(),
        entities: data_entities,
    };
    crate::convert::data_text_to_ui_text(&data_text)
}

fn todo_items_to_tl(items: &[TodoItemInput], session: &Session) -> Vec<tl_enums::TodoItem> {
    items
        .iter()
        .enumerate()
        .map(|(idx, it)| {
            let id = if it.id > 0 { it.id } else { (idx + 1) as i32 };
            tl_enums::TodoItem::TodoItem(tl_types::TodoItem {
                id,
                title: ui_text_to_tl_text_with_entities(&it.text, session),
            })
        })
        .collect()
}

fn todo_request_to_input_media(
    req: &CreateTodoListRequest,
    session: &Session,
) -> tl_enums::InputMedia {
    tl_enums::InputMedia::Todo(tl_types::InputMediaTodo {
        todo: tl_enums::TodoList::TodoList(tl_types::TodoList {
            others_can_append: req.others_can_append,
            others_can_complete: req.others_can_complete,
            title: ui_text_to_tl_text_with_entities(&req.title, session),
            list: todo_items_to_tl(&req.items, session),
        }),
    })
}

fn factcheck_from_tl(fc: &tl_enums::FactCheck) -> FactCheckData {
    let tl_enums::FactCheck::FactCheck(inner) = fc;
    FactCheckData {
        need_check: inner.need_check,
        country: inner.country.clone(),
        text: inner.text.as_ref().map(tl_text_with_entities_to_ui),
        hash: inner.hash,
    }
}

fn stars_amount_from_tl(amount: &tl_enums::StarsAmount) -> SuggestedPrice {
    match amount {
        tl_enums::StarsAmount::StarsAmount(s) => SuggestedPrice {
            currency: SuggestedCurrency::Stars,
            whole: s.amount,
            nanos: s.nanos,
        },
        tl_enums::StarsAmount::StarsTonAmount(t) => SuggestedPrice::ton_nanos(t.amount),
    }
}

fn stars_amount_to_tl(price: &SuggestedPrice) -> tl_enums::StarsAmount {
    match price.currency {
        SuggestedCurrency::Stars => tl_enums::StarsAmount::StarsAmount(tl_types::StarsAmount {
            amount: price.whole,
            nanos: price.nanos,
        }),
        SuggestedCurrency::Ton => tl_enums::StarsAmount::StarsTonAmount(tl_types::StarsTonAmount {
            amount: price.total_nanos(),
        }),
    }
}

fn suggested_post_from_tl(
    sp: &tl_enums::SuggestedPost,
    paid_stars: bool,
    paid_ton: bool,
) -> SuggestedPostData {
    let tl_enums::SuggestedPost::SuggestedPost(inner) = sp;
    SuggestedPostData {
        price: inner.price.as_ref().map(stars_amount_from_tl),
        schedule_date: inner.schedule_date,
        accepted: inner.accepted,
        rejected: inner.rejected,
        paid_stars,
        paid_ton,
    }
}

fn suggest_options_to_tl(opts: &SuggestOptions) -> tl_enums::SuggestedPost {
    tl_enums::SuggestedPost::SuggestedPost(tl_types::SuggestedPost {
        accepted: false,
        rejected: false,
        price: opts
            .price
            .as_ref()
            .filter(|p| !p.is_free())
            .map(stars_amount_to_tl),
        schedule_date: opts.schedule_date,
    })
}

fn sponsored_message_from_tl(msg: &tl_enums::SponsoredMessage) -> SponsoredMessageItem {
    let tl_enums::SponsoredMessage::SponsoredMessage(d) = msg;
    let entities = d
        .entities
        .as_deref()
        .map(|e| entities_from_tl_for_text(&d.message, e, tg_data::UserId::ZERO))
        .unwrap_or_default();
    let ui_text = crate::convert::data_text_to_ui_text(&tg_data::TextWithEntities {
        text: d.message.clone(),
        entities,
    });
    let color_index = d
        .color
        .as_ref()
        .and_then(|c| match c {
            tl_enums::PeerColor::PeerColor(pc) => pc.color.map(|idx| (idx.max(0) % 7) as u8),
            _ => None,
        })
        .unwrap_or(0);
    let is_link_internal = d.url.starts_with("https://t.me/")
        || d.url.starts_with("tg://")
        || d.url.starts_with("t.me/");
    SponsoredMessageItem {
        random_id: d.random_id.clone(),
        url: d.url.clone(),
        title: d.title.clone(),
        text: ui_text,
        button_text: d.button_text.clone(),
        sponsor_info: d.sponsor_info.clone(),
        additional_info: d.additional_info.clone(),
        recommended: d.recommended,
        can_report: d.can_report,
        color_index,
        min_display_duration_ms: d.min_display_duration.unwrap_or(0).max(0) as u64 * 1_000,
        max_display_duration_ms: d.max_display_duration.unwrap_or(0).max(0) as u64 * 1_000,
        is_link_internal,
        viewed: false,
    }
}

// -----------------------------------------------------------------------------
// Additional LiveSession helper methods for batching & app-config flags
// -----------------------------------------------------------------------------

impl LiveSession {
    /// Configures whether this account may edit fact-checks (`appConfig.can_edit_factcheck`).
    pub fn set_can_edit_factcheck_config(&self, allowed: bool) {
        with_state(self, |st| st.can_edit_factcheck = allowed);
    }

    /// Queues a debounced checklist task toggle (`kSendTogglesDelay = 3s`).
    pub fn queue_todo_toggle(
        &self,
        peer_id: PeerId,
        msg_id: MsgId,
        task_id: i32,
        completed: bool,
        now_ms: u64,
    ) {
        with_state(self, |st| {
            st.todo_batcher.toggle(
                FullMsgId {
                    peer: peer_id,
                    msg: msg_id,
                },
                task_id,
                completed,
                now_ms,
            );
        });
    }

    /// Flushes all ready batched checklist completion toggles via `messages.toggleTodoCompleted`.
    pub fn flush_todo_toggles(&mut self, now_ms: u64, force: bool) -> Result<usize, String> {
        let batches = with_state(self, |st| st.todo_batcher.take_ready(now_ms, force));
        let mut sent = 0usize;
        for (full_id, completed, incompleted) in batches {
            let res =
                self.toggle_todo_completed(full_id.peer, full_id.msg, &completed, &incompleted);
            with_state(self, |st| st.todo_batcher.mark_finished(full_id));
            res?;
            sent += 1;
        }
        Ok(sent)
    }

    /// Enqueues a message for batched `messages.getFactCheck` retrieval.
    pub fn enqueue_factcheck(&self, peer_id: PeerId, msg_id: MsgId, now_ms: u64) -> bool {
        with_state(self, |st| {
            st.factcheck_batcher.enqueue(
                FullMsgId {
                    peer: peer_id,
                    msg: msg_id,
                },
                now_ms,
            )
        })
    }

    /// Flushes the next ready single-peer `messages.getFactCheck` batch.
    pub fn flush_factcheck_batch(&mut self, now_ms: u64, force: bool) -> Result<usize, String> {
        let Some((peer_id, msg_ids)) = with_state(self, |st| {
            st.factcheck_batcher.take_ready_batch(now_ms, force)
        }) else {
            return Ok(0);
        };
        match self.get_factchecks(peer_id, &msg_ids) {
            Ok(pairs) => {
                let list: Vec<FactCheckData> = pairs.into_iter().map(|(_, fc)| fc).collect();
                let count = list.len();
                let resolved = with_state(self, |st| st.factcheck_batcher.finish_batch(Ok(list)));
                with_state(self, |st| {
                    for (fid, fc) in resolved {
                        st.factchecks.insert((fid.peer, fid.msg), fc);
                    }
                });
                Ok(count)
            }
            Err(err) => {
                let _ = with_state(self, |st| st.factcheck_batcher.finish_batch(Err(())));
                Err(err)
            }
        }
    }
}

// -----------------------------------------------------------------------------
// Layer233ExtrasBackend implementation for LiveSession
// -----------------------------------------------------------------------------

impl Layer233ExtrasBackend for LiveSession {
    fn create_todo_list(
        &mut self,
        peer_id: PeerId,
        request: CreateTodoListRequest,
    ) -> Result<HistoryMessage, String> {
        let input_peer =
            resolve_input_peer(self, peer_id).ok_or_else(|| "Peer not found.".to_owned())?;
        let random_id = with_state(self, Layer233LiveState::next_random_id);
        let media = read_session(self, |s| todo_request_to_input_media(&request, s));
        let req = tl_fns::messages::SendMedia {
            silent: false,
            background: false,
            clear_draft: false,
            noforwards: false,
            update_stickersets_order: false,
            invert_media: false,
            allow_paid_floodskip: false,
            peer: input_peer,
            reply_to: None,
            media,
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
        };
        let updates = invoke_rpc(self, req)?;
        let touched = apply_updates_to_session(self, &updates);
        if let Some((tp, tm)) = touched.last().copied()
            && let Some(msg) = read_session(self, |s| {
                s.message(DataFullMsgId::new(to_data_peer(tp), to_data_msg(tm)))
                    .map(|m| message_to_history_message(s, m))
            })
        {
            return Ok(msg);
        }
        Err("Checklist created but server returned no message object.".to_owned())
    }

    fn edit_todo_list(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        request: CreateTodoListRequest,
    ) -> Result<HistoryMessage, String> {
        let input_peer =
            resolve_input_peer(self, peer_id).ok_or_else(|| "Peer not found.".to_owned())?;
        let media = read_session(self, |s| todo_request_to_input_media(&request, s));
        let req = tl_fns::messages::EditMessage {
            no_webpage: false,
            invert_media: false,
            peer: input_peer,
            id: msg_id.0,
            message: None,
            media: Some(media),
            reply_markup: None,
            entities: None,
            schedule_date: None,
            schedule_repeat_period: None,
            quick_reply_shortcut_id: None,
            rich_message: None,
        };
        if let Some(updates) = self.edit_message_via_runtime(req)? {
            self.apply_updates_side_caches(&updates);
            let _ = extract_layer233_extras_from_updates(self, &updates);
        }
        read_session(self, |s| {
            s.message(DataFullMsgId::new(
                to_data_peer(peer_id),
                to_data_msg(msg_id),
            ))
            .map(|m| message_to_history_message(s, m))
        })
        .ok_or_else(|| "Edited checklist message not found in session.".to_owned())
    }

    fn toggle_todo_completed(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        completed: &[i32],
        incompleted: &[i32],
    ) -> Result<Option<HistoryMessage>, String> {
        let input_peer =
            resolve_input_peer(self, peer_id).ok_or_else(|| "Peer not found.".to_owned())?;
        let req = tl_fns::messages::ToggleTodoCompleted {
            peer: input_peer,
            msg_id: msg_id.0,
            completed: completed.to_vec(),
            incompleted: incompleted.to_vec(),
        };
        let updates = invoke_rpc(self, req)?;
        let _ = apply_updates_to_session(self, &updates);

        // Apply completion delta locally to cached session message so UI updates immediately.
        let now = now_unix_i32();
        let data_peer = to_data_peer(peer_id);
        let data_msg = to_data_msg(msg_id);
        let updated = write_session(self, |s| {
            let self_peer = s.self_peer_id();
            if let Some(msg) = s.message_mut(DataFullMsgId::new(data_peer, data_msg))
                && let MessageContent::Regular {
                    media: MessageMedia::TodoList(todo),
                    ..
                } = &mut msg.content
            {
                for &cid in completed {
                    if !todo.completions.iter().any(|c| c.id == cid) {
                        todo.completions.push(TodoCompletionData {
                            id: cid,
                            completed_by: self_peer,
                            date: now,
                        });
                    }
                }
                todo.completions.retain(|c| !incompleted.contains(&c.id));
            }
            s.message(DataFullMsgId::new(data_peer, data_msg))
                .map(|m| message_to_history_message(s, m))
        });
        self.bridge_state().notify_ui();
        Ok(updated)
    }

    fn append_todo_list(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        items: Vec<TodoItemInput>,
    ) -> Result<Option<HistoryMessage>, String> {
        if items.is_empty() {
            return Ok(None);
        }
        let input_peer =
            resolve_input_peer(self, peer_id).ok_or_else(|| "Peer not found.".to_owned())?;
        let list = read_session(self, |s| todo_items_to_tl(&items, s));
        let req = tl_fns::messages::AppendTodoList {
            peer: input_peer,
            msg_id: msg_id.0,
            list,
        };
        let updates = invoke_rpc(self, req)?;
        let _ = apply_updates_to_session(self, &updates);

        let data_peer = to_data_peer(peer_id);
        let data_msg = to_data_msg(msg_id);
        let updated = write_session(self, |s| {
            if let Some(msg) = s.message_mut(DataFullMsgId::new(data_peer, data_msg))
                && let MessageContent::Regular {
                    media: MessageMedia::TodoList(todo),
                    ..
                } = &mut msg.content
            {
                for item in &items {
                    if !todo.items.iter().any(|it| it.id == item.id) {
                        todo.items.push(TodoItemData {
                            id: item.id,
                            title: ui_text_to_data_text(&item.text),
                        });
                    }
                }
            }
            s.message(DataFullMsgId::new(data_peer, data_msg))
                .map(|m| message_to_history_message(s, m))
        });
        self.bridge_state().notify_ui();
        Ok(updated)
    }

    fn is_sponsored_hidden(&self) -> bool {
        let is_prem = self_is_premium(self);
        with_state(self, |st| {
            st.sponsored.is_premium = is_prem;
            st.sponsored.ads_suppressed()
        })
    }

    fn set_sponsored_hidden(&mut self, hide_ads: bool) -> Result<(), String> {
        let is_prem = self_is_premium(self);
        let _ = invoke_rpc(
            self,
            tl_fns::account::ToggleSponsoredMessages { enabled: !hide_ads },
        )?;
        with_state(self, |st| {
            st.sponsored.set_premium_hide_ads(is_prem, hide_ads);
        });
        self.bridge_state().notify_ui();
        Ok(())
    }

    fn get_sponsored_messages(
        &mut self,
        peer_id: PeerId,
        msg_id: Option<MsgId>,
    ) -> Result<Vec<SponsoredMessageItem>, String> {
        let is_prem = self_is_premium(self);
        let now_ms = (now_unix_i32().max(0) as u64) * 1_000;
        let suppressed_or_cached = with_state(self, |st| {
            st.sponsored.is_premium = is_prem;
            if st.sponsored.ads_suppressed() {
                return Some(Vec::new());
            }
            if !st.sponsored.should_request(peer_id, now_ms) {
                return Some(st.sponsored.list_for_peer(peer_id).to_vec());
            }
            None
        });
        if let Some(cached) = suppressed_or_cached {
            return Ok(cached);
        }

        let input_peer =
            resolve_input_peer(self, peer_id).ok_or_else(|| "Peer not found.".to_owned())?;
        let resp = invoke_rpc(
            self,
            tl_fns::messages::GetSponsoredMessages {
                peer: input_peer,
                msg_id: msg_id.map(|m| m.0),
            },
        )?;

        let peer_is_channel = read_session(self, |s| {
            s.peer(to_data_peer(peer_id))
                .and_then(Peer::as_channel)
                .is_some()
        });

        let list = match resp {
            tl_enums::messages::SponsoredMessages::SponsoredMessages(data) => {
                write_session(self, |s| {
                    apply_tl_users_and_chats(s, &data.users, &data.chats);
                });
                let mode = if data.posts_between.is_some() {
                    SponsoredPlacementMode::InjectToMiddle
                } else if peer_is_channel {
                    SponsoredPlacementMode::AppendToEnd
                } else {
                    SponsoredPlacementMode::AppendToTopBar
                };
                let entries: Vec<SponsoredMessageItem> = data
                    .messages
                    .iter()
                    .map(sponsored_message_from_tl)
                    .collect();
                SponsoredPeerList {
                    posts_between: data.posts_between,
                    start_delay_ms: data.start_delay.unwrap_or(0).max(0) as u64 * 1_000,
                    between_delay_ms: data.between_delay.unwrap_or(0).max(0) as u64 * 1_000,
                    mode,
                    entries,
                    received_ms: now_ms,
                    injected_count: 0,
                    showed_all: false,
                }
            }
            tl_enums::messages::SponsoredMessages::Empty => SponsoredPeerList::default(),
        };

        let out = list.entries.clone();
        with_state(self, |st| {
            st.sponsored.store_list(peer_id, list, now_ms);
        });
        Ok(out)
    }

    fn view_sponsored_message(&mut self, random_id: &[u8]) -> Result<(), String> {
        let now_ms = (now_unix_i32().max(0) as u64) * 1_000;
        let should_send = with_state(self, |st| st.sponsored.record_view(random_id, now_ms));
        if !should_send {
            return Ok(());
        }
        let _ = invoke_rpc(
            self,
            tl_fns::messages::ViewSponsoredMessage {
                random_id: random_id.to_vec(),
            },
        )?;
        Ok(())
    }

    fn click_sponsored_message(
        &mut self,
        random_id: &[u8],
        is_media: bool,
        is_fullscreen: bool,
    ) -> Result<(), String> {
        let _ = invoke_rpc(
            self,
            tl_fns::messages::ClickSponsoredMessage {
                media: is_media,
                fullscreen: is_fullscreen,
                random_id: random_id.to_vec(),
            },
        )?;
        Ok(())
    }

    fn report_sponsored_message(
        &mut self,
        random_id: &[u8],
        option: &[u8],
    ) -> Result<SponsoredReportOutcome, String> {
        if option == b"-1" {
            with_state(self, |st| {
                st.sponsored.remove_by_random_id(random_id);
            });
            return Ok(SponsoredReportOutcome::Silence);
        }

        match invoke_rpc(
            self,
            tl_fns::messages::ReportSponsoredMessage {
                random_id: random_id.to_vec(),
                option: option.to_vec(),
            },
        ) {
            Ok(tl_enums::channels::SponsoredMessageReportResult::ChooseOption(choose)) => {
                let options = choose
                    .options
                    .into_iter()
                    .map(
                        |tl_enums::SponsoredMessageReportOption::SponsoredMessageReportOption(
                            o,
                        )| {
                            SponsoredReportOption {
                                option: o.option,
                                text: o.text,
                            }
                        },
                    )
                    .collect();
                Ok(SponsoredReportOutcome::ChooseOption {
                    title: choose.title,
                    options,
                })
            }
            Ok(tl_enums::channels::SponsoredMessageReportResult::AdsHidden) => {
                with_state(self, |st| st.sponsored.clear());
                Ok(SponsoredReportOutcome::AdsHidden)
            }
            Ok(tl_enums::channels::SponsoredMessageReportResult::Reported) => {
                with_state(self, |st| {
                    st.sponsored.remove_by_random_id(random_id);
                });
                if option == b"1" {
                    Ok(SponsoredReportOutcome::Silence)
                } else {
                    Ok(SponsoredReportOutcome::Reported)
                }
            }
            Err(err) => {
                if err.contains("PREMIUM_ACCOUNT_REQUIRED") {
                    Ok(SponsoredReportOutcome::PremiumRequired)
                } else {
                    Ok(SponsoredReportOutcome::Error(err))
                }
            }
        }
    }

    fn cached_factcheck(&self, peer_id: PeerId, msg_id: MsgId) -> Option<FactCheckData> {
        with_state(self, |st| st.factchecks.get(&(peer_id, msg_id)).cloned())
            .filter(|fc| !fc.is_empty())
    }

    fn can_edit_factcheck_config(&self) -> bool {
        with_state(self, |st| st.can_edit_factcheck)
    }

    fn get_factchecks(
        &mut self,
        peer_id: PeerId,
        msg_ids: &[MsgId],
    ) -> Result<Vec<(MsgId, FactCheckData)>, String> {
        if msg_ids.is_empty() {
            return Ok(Vec::new());
        }
        let input_peer =
            resolve_input_peer(self, peer_id).ok_or_else(|| "Peer not found.".to_owned())?;
        let raw_ids: Vec<i32> = msg_ids.iter().map(|m| m.0).collect();
        let list = invoke_rpc(
            self,
            tl_fns::messages::GetFactCheck {
                peer: input_peer,
                msg_id: raw_ids,
            },
        )?;
        let mut pairs = Vec::with_capacity(msg_ids.len());
        for (idx, &mid) in msg_ids.iter().enumerate() {
            let fc = list.get(idx).map(factcheck_from_tl).unwrap_or_default();
            pairs.push((mid, fc));
        }
        with_state(self, |st| {
            for (mid, fc) in &pairs {
                st.factchecks.insert((peer_id, *mid), fc.clone());
            }
        });
        self.bridge_state().notify_ui();
        Ok(pairs)
    }

    fn edit_factcheck(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        text: TextWithEntities,
    ) -> Result<Option<FactCheckData>, String> {
        let input_peer =
            resolve_input_peer(self, peer_id).ok_or_else(|| "Peer not found.".to_owned())?;
        if text.text.trim().is_empty() {
            let updates = invoke_rpc(
                self,
                tl_fns::messages::DeleteFactCheck {
                    peer: input_peer,
                    msg_id: msg_id.0,
                },
            )?;
            let _ = apply_updates_to_session(self, &updates);
            with_state(self, |st| {
                st.factchecks.remove(&(peer_id, msg_id));
            });
            self.bridge_state().notify_ui();
            return Ok(None);
        }

        let tl_text = read_session(self, |s| ui_text_to_tl_text_with_entities(&text, s));
        let updates = invoke_rpc(
            self,
            tl_fns::messages::EditFactCheck {
                peer: input_peer,
                msg_id: msg_id.0,
                text: tl_text,
            },
        )?;
        let _ = apply_updates_to_session(self, &updates);
        let fc = FactCheckData {
            need_check: false,
            country: None,
            text: Some(text),
            hash: 0,
        };
        with_state(self, |st| {
            st.factchecks.insert((peer_id, msg_id), fc.clone());
        });
        self.bridge_state().notify_ui();
        Ok(Some(fc))
    }

    fn monoforum_sublists(&mut self, monoforum_peer_id: PeerId) -> Vec<MonoforumSublist> {
        if let Some(cached) = with_state(self, |st| {
            st.monoforum_sublists.get(&monoforum_peer_id).cloned()
        }) {
            return cached;
        }
        let Some(input_peer) = resolve_input_peer(self, monoforum_peer_id) else {
            return Vec::new();
        };
        let req = tl_fns::messages::GetSavedDialogs {
            // Match `SavedMessages::sendLoadMore` (`data/data_saved_messages.cpp:311`):
            // always set `exclude_pinned = true`.
            exclude_pinned: true,
            parent_peer: Some(input_peer),
            offset_date: 0,
            offset_id: 0,
            offset_peer: tl_enums::InputPeer::Empty,
            limit: 100,
            hash: 0,
        };
        let Ok(resp) = invoke_rpc(self, req) else {
            return Vec::new();
        };
        let (dialogs, messages, chats, users) = match resp {
            tl_enums::messages::SavedDialogs::SavedDialogs(d) => {
                (d.dialogs, d.messages, d.chats, d.users)
            }
            tl_enums::messages::SavedDialogs::Slice(d) => (d.dialogs, d.messages, d.chats, d.users),
            tl_enums::messages::SavedDialogs::NotModified(_) => return Vec::new(),
        };

        let mut msg_previews: HashMap<i32, (String, i32)> = HashMap::new();
        write_session(self, |s| {
            apply_tl_users_and_chats(s, &users, &chats);
            let self_uid = s.self_user_id();
            for tl_msg in &messages {
                if let Some(m) = message_from_tl(tl_msg, self_uid) {
                    let preview = tg_session::runtime::format_message_preview(&m);
                    msg_previews.insert(m.id.0 as i32, (preview, m.date));
                    s.insert_message(m);
                }
            }
        });

        let sublists: Vec<MonoforumSublist> = read_session(self, |s| {
            dialogs
                .iter()
                .map(|dlg| match dlg {
                    tl_enums::SavedDialog::MonoForumDialog(mf) => {
                        let sub_dp = data_peer_from_tl(&mf.peer);
                        let title = peer_display_title(s, sub_dp);
                        let color_idx = s.peer(sub_dp).map_or(0, |p| p.common().color_index);
                        let (last_preview, last_date_unix) = msg_previews
                            .get(&mf.top_message)
                            .cloned()
                            .unwrap_or_else(|| (String::new(), 0));
                        let draft_text = mf.draft.as_ref().and_then(|d| match d {
                            tl_enums::DraftMessage::DraftMessage(dm) => Some(dm.message.clone()),
                            tl_enums::DraftMessage::Empty(_) => None,
                        });
                        MonoforumSublist {
                            parent_peer_id: monoforum_peer_id,
                            sublist_peer_id: from_data_peer(sub_dp),
                            title,
                            avatar_rgb: avatar_rgb_for_color_index(color_idx),
                            top_message_id: MsgId(mf.top_message),
                            last_preview,
                            last_date_unix,
                            read_inbox_max_id: MsgId(mf.read_inbox_max_id),
                            read_outbox_max_id: MsgId(mf.read_outbox_max_id),
                            unread_count: mf.unread_count.max(0) as u32,
                            unread_reactions_count: mf.unread_reactions_count.max(0) as u32,
                            unread_mark: mf.unread_mark,
                            nopaid_messages_exception: mf.nopaid_messages_exception,
                            draft_text,
                        }
                    }
                    tl_enums::SavedDialog::SavedDialog(sd) => {
                        let sub_dp = data_peer_from_tl(&sd.peer);
                        let title = peer_display_title(s, sub_dp);
                        let color_idx = s.peer(sub_dp).map_or(0, |p| p.common().color_index);
                        let (last_preview, last_date_unix) = msg_previews
                            .get(&sd.top_message)
                            .cloned()
                            .unwrap_or_else(|| (String::new(), 0));
                        MonoforumSublist {
                            parent_peer_id: monoforum_peer_id,
                            sublist_peer_id: from_data_peer(sub_dp),
                            title,
                            avatar_rgb: avatar_rgb_for_color_index(color_idx),
                            top_message_id: MsgId(sd.top_message),
                            last_preview,
                            last_date_unix,
                            read_inbox_max_id: MsgId(0),
                            read_outbox_max_id: MsgId(0),
                            unread_count: 0,
                            unread_reactions_count: 0,
                            unread_mark: false,
                            nopaid_messages_exception: false,
                            draft_text: None,
                        }
                    }
                })
                .collect()
        });

        with_state(self, |st| {
            st.monoforum_sublists
                .insert(monoforum_peer_id, sublists.clone());
        });
        sublists
    }

    fn monoforum_sublist_messages(
        &mut self,
        monoforum_peer_id: PeerId,
        sublist_peer_id: PeerId,
    ) -> Vec<HistoryMessage> {
        if let Some(cached) = with_state(self, |st| {
            st.monoforum_messages
                .get(&(monoforum_peer_id, sublist_peer_id))
                .cloned()
        }) {
            return cached;
        }
        let (Some(parent_ip), Some(sub_ip)) = (
            resolve_input_peer(self, monoforum_peer_id),
            resolve_input_peer(self, sublist_peer_id),
        ) else {
            return Vec::new();
        };
        let req = tl_fns::messages::GetSavedHistory {
            parent_peer: Some(parent_ip),
            peer: sub_ip,
            offset_id: 0,
            offset_date: 0,
            add_offset: 0,
            limit: 50,
            max_id: 0,
            min_id: 0,
            hash: 0,
        };
        let Ok(resp) = invoke_rpc(self, req) else {
            return Vec::new();
        };
        let (tl_msgs, chats, users, channel_pts) = match resp {
            tl_enums::messages::Messages::Messages(m) => (m.messages, m.chats, m.users, None),
            tl_enums::messages::Messages::Slice(m) => (m.messages, m.chats, m.users, None),
            tl_enums::messages::Messages::ChannelMessages(m) => {
                (m.messages, m.chats, m.users, (m.pts > 0).then_some(m.pts))
            }
            tl_enums::messages::Messages::NotModified(_) => return Vec::new(),
        };
        if let Some(pts) = channel_pts
            && let Some(ch_id) = to_data_peer(monoforum_peer_id).to_channel()
        {
            let _ = self.apply_channel_pts(ch_id, pts, 0);
        }

        let mut suggestions = Vec::new();
        let mut msg_ids = Vec::new();
        write_session(self, |s| {
            apply_tl_users_and_chats(s, &users, &chats);
            let self_uid = s.self_user_id();
            for tl_msg in &tl_msgs {
                if let tl_enums::Message::Message(m) = tl_msg
                    && let Some(ref sp) = m.suggested_post
                {
                    suggestions.push((
                        MsgId(m.id),
                        suggested_post_from_tl(
                            sp,
                            m.paid_suggested_post_stars,
                            m.paid_suggested_post_ton,
                        ),
                    ));
                }
                if let Some(m) = message_from_tl(tl_msg, self_uid) {
                    msg_ids.push((m.peer_id, m.id));
                    s.insert_message(m);
                }
            }
        });

        let mut ui_msgs: Vec<HistoryMessage> = read_session(self, |s| {
            msg_ids
                .iter()
                .filter_map(|&(dp, mid)| {
                    s.message(DataFullMsgId::new(dp, mid))
                        .map(|m| message_to_history_message(s, m))
                })
                .collect()
        });
        ui_msgs.sort_by_key(|m| m.id.0);

        with_state(self, |st| {
            for (mid, sp) in suggestions {
                st.suggested_posts.insert((monoforum_peer_id, mid), sp);
            }
            st.monoforum_messages
                .insert((monoforum_peer_id, sublist_peer_id), ui_msgs.clone());
        });
        ui_msgs
    }

    fn mark_monoforum_sublist_read(
        &mut self,
        monoforum_peer_id: PeerId,
        sublist_peer_id: PeerId,
        max_id: MsgId,
    ) -> Result<(), String> {
        let parent_ip = resolve_input_peer(self, monoforum_peer_id)
            .ok_or_else(|| "Monoforum peer not found.".to_owned())?;
        let sub_ip = resolve_input_peer(self, sublist_peer_id)
            .ok_or_else(|| "Sublist peer not found.".to_owned())?;
        let _ = invoke_rpc(
            self,
            tl_fns::messages::ReadSavedHistory {
                parent_peer: parent_ip,
                peer: sub_ip,
                max_id: max_id.0,
            },
        )?;
        with_state(self, |st| {
            if let Some(list) = st.monoforum_sublists.get_mut(&monoforum_peer_id)
                && let Some(sub) = list
                    .iter_mut()
                    .find(|s| s.sublist_peer_id == sublist_peer_id)
            {
                sub.apply_read_till(max_id, false);
            }
        });
        Ok(())
    }

    fn send_monoforum_message(
        &mut self,
        monoforum_peer_id: PeerId,
        sublist_peer_id: PeerId,
        text: TextWithEntities,
        suggest: Option<SuggestOptions>,
    ) -> Result<HistoryMessage, String> {
        let parent_ip = resolve_input_peer(self, monoforum_peer_id)
            .ok_or_else(|| "Monoforum peer not found.".to_owned())?;
        let sub_ip = resolve_input_peer(self, sublist_peer_id)
            .ok_or_else(|| "Sublist peer not found.".to_owned())?;
        let random_id = with_state(self, Layer233LiveState::next_random_id);
        let entities = read_session(self, |s| ui_text_to_tl_entities(&text, s));
        let req = tl_fns::messages::SendMessage {
            no_webpage: false,
            silent: false,
            background: false,
            clear_draft: true,
            noforwards: false,
            update_stickersets_order: false,
            invert_media: false,
            allow_paid_floodskip: false,
            peer: parent_ip,
            reply_to: Some(tl_enums::InputReplyTo::MonoForum(
                tl_types::InputReplyToMonoForum {
                    monoforum_peer_id: sub_ip,
                },
            )),
            message: text.text.clone(),
            random_id,
            reply_markup: None,
            entities: (!entities.is_empty()).then_some(entities),
            schedule_date: None,
            schedule_repeat_period: None,
            send_as: None,
            quick_reply_shortcut: None,
            effect: None,
            allow_paid_stars: None,
            suggested_post: suggest.as_ref().map(suggest_options_to_tl),
            rich_message: None,
        };
        let updates = invoke_rpc(self, req)?;
        let touched = apply_updates_to_session(self, &updates);
        if let Some((tp, tm)) = touched.last().copied()
            && let Some(msg) = read_session(self, |s| {
                s.message(DataFullMsgId::new(to_data_peer(tp), to_data_msg(tm)))
                    .map(|m| message_to_history_message(s, m))
            })
        {
            with_state(self, |st| {
                if let Some(ref opts) = suggest {
                    st.suggested_posts
                        .entry((monoforum_peer_id, msg.id))
                        .or_insert(SuggestedPostData {
                            price: opts.price,
                            schedule_date: opts.schedule_date,
                            accepted: false,
                            rejected: false,
                            paid_stars: false,
                            paid_ton: false,
                        });
                }
                st.monoforum_messages
                    .entry((monoforum_peer_id, sublist_peer_id))
                    .or_default()
                    .push(msg.clone());
            });
            return Ok(msg);
        }
        Err("Monoforum message sent but server returned no message object.".to_owned())
    }

    fn suggested_post_for(&self, peer_id: PeerId, msg_id: MsgId) -> Option<SuggestedPostData> {
        with_state(self, |st| {
            st.suggested_posts.get(&(peer_id, msg_id)).cloned()
        })
    }

    fn toggle_suggested_post_approval(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        decision: SuggestApprovalDecision,
    ) -> Result<(), String> {
        let input_peer =
            resolve_input_peer(self, peer_id).ok_or_else(|| "Peer not found.".to_owned())?;
        let (reject, schedule_date, reject_comment) = match &decision {
            SuggestApprovalDecision::Approve { schedule_date } => (false, *schedule_date, None),
            SuggestApprovalDecision::Reject { comment } => (true, None, comment.clone()),
        };
        let req = tl_fns::messages::ToggleSuggestedPostApproval {
            reject,
            peer: input_peer,
            msg_id: msg_id.0,
            schedule_date,
            reject_comment,
        };
        let updates = invoke_rpc(self, req)?;
        let _ = apply_updates_to_session(self, &updates);
        with_state(self, |st| {
            let entry = st.suggested_posts.entry((peer_id, msg_id)).or_default();
            match decision {
                SuggestApprovalDecision::Approve { schedule_date } => {
                    entry.accepted = true;
                    entry.rejected = false;
                    if schedule_date.is_some() {
                        entry.schedule_date = schedule_date;
                    }
                }
                SuggestApprovalDecision::Reject { .. } => {
                    entry.rejected = true;
                    entry.accepted = false;
                }
            }
        });
        self.bridge_state().notify_ui();
        Ok(())
    }
}

#[cfg(test)]
#[path = "tests_entities_cut.rs"]
#[cfg(test)]
mod tests_entities_cut;
