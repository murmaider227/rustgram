//! Reply headers whose target message is not in the loaded history.
//!
//! Port of the reply resolution path in Telegram Desktop 7.4.5:
//! `HistoryMessageReply::updateData` (`history/history_item_components.cpp`) finds the target in
//! `Data::Session`; when it is missing, `Api::RequestMessageDataIfNeeded` calls
//! `ApiWrap::requestMessageData` (`apiwrap.cpp`), which queues the id per message box (one queue
//! per channel, one for the common box) and `resolveMessageDatas` sends one
//! `channels.getMessages` / `messages.getMessages` per box. When the answer arrives every
//! dependent item is repainted (`Data::Session::requestItemRepaint`); a target that comes back as
//! `messageEmpty` (or not at all) is shown as "Deleted message". Replies to stories are resolved
//! the same way through `stories.getStoriesByID` (`Data::Stories::resolve`), one request per
//! story owner.
//!
//! Fetched targets are kept in a per-session cache here, not inserted into `tg_data::Session`:
//! `Session::insert_message` would mark the range up to the newest message as loaded history.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock, PoisonError, Weak};
use std::time::Duration;

use tg_api::convert::{
    apply_tl_users_and_chats, input_channel_for, input_peer_for, message_from_tl,
};
use tg_api::{InvocationError, RpcClient};
use tg_app_core::history::backend::HistoryMessage;
use tg_data::{
    FullMsgId, FullStoryId, Message, MsgId as DataMsgId, PeerId as DataPeerId, Session, StoryId,
};
use tg_tl::{enums as tl_enums, functions as tl_fns, types as tl_types};
use tracing::debug;

use crate::bridge::LiveBridgeState;
use crate::convert::{
    ReplyTargetState, StoryReplyState, reply_header_for, story_reply_header, to_data_msg,
    to_data_peer,
};
use crate::session::LiveSession;

/// Delay before a queued batch is sent, so targets queued by several conversions in the same
/// frame share one request (`_messageDataResolveDelayed` + `afterDelay(kSmallDelayMs)`).
const RESOLVE_DELAY: Duration = Duration::from_millis(5);
/// Upper bound on one `getMessages` request (the server rejects larger id vectors).
const MAX_IDS_PER_REQUEST: usize = 100;
/// Cached resolved / deleted targets kept per session before the cache is trimmed.
const MAX_CACHED_TARGETS: usize = 4096;
/// Timeout of one batched request.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// A reply target: a message or a story.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
enum Target {
    Message(FullMsgId),
    Story(FullStoryId),
}

enum TargetEntry {
    /// Queued or in flight.
    Pending,
    /// Fetched from the server.
    Loaded(Box<Message>),
    /// A story that exists, with its caption and its photo or video.
    Story {
        caption: String,
        media: Option<Box<tg_data::MessageMedia>>,
    },
    /// `messageEmpty` / `storyItemDeleted`, missing from the answer, or not accessible.
    Deleted,
}

#[derive(Default)]
struct ReplyTargetsStore {
    targets: HashMap<Target, TargetEntry>,
    queued: Vec<Target>,
    flush_scheduled: bool,
    listener_started: bool,
    /// Target -> messages whose reply header shows it (`registerDependentMessage`).
    dependents: HashMap<Target, HashSet<FullMsgId>>,
    /// Flushes whose repaints are recorded (tests wait for it instead of for time).
    #[cfg(test)]
    flushes_done: u64,
}

impl ReplyTargetsStore {
    fn trim(&mut self) {
        if self.targets.len() <= MAX_CACHED_TARGETS {
            return;
        }
        self.targets
            .retain(|_, entry| matches!(entry, TargetEntry::Pending));
        let pending: HashSet<Target> = self.targets.keys().copied().collect();
        self.dependents.retain(|target, _| pending.contains(target));
    }
}

struct RegistryEntry {
    owner: Weak<LiveBridgeState>,
    store: Arc<Mutex<ReplyTargetsStore>>,
}

fn registry() -> &'static Mutex<HashMap<usize, RegistryEntry>> {
    static REGISTRY: OnceLock<Mutex<HashMap<usize, RegistryEntry>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn store_for(bridge: &Arc<LiveBridgeState>) -> Arc<Mutex<ReplyTargetsStore>> {
    let key = Arc::as_ptr(bridge) as usize;
    let owner = Arc::downgrade(bridge);
    let mut map = registry().lock().unwrap_or_else(PoisonError::into_inner);
    map.retain(|_, entry| entry.owner.strong_count() > 0);
    if let Some(entry) = map.get(&key)
        && entry.owner.ptr_eq(&owner)
    {
        return Arc::clone(&entry.store);
    }
    let store = Arc::new(Mutex::new(ReplyTargetsStore::default()));
    map.insert(
        key,
        RegistryEntry {
            owner,
            store: Arc::clone(&store),
        },
    );
    store
}

/// Resolves the reply headers of `messages` whose target is not in the loaded history.
///
/// Targets found in the session or in this cache get their real header; unknown targets show
/// "Loading..." and are queued for one batched `getMessages` per message box. When the answer
/// arrives, every dependent message is reported as edited so the open history repaints it.
pub fn attach_reply_targets(live: &LiveSession, messages: &mut [HistoryMessage]) {
    if !messages.iter().any(|m| m.reply.is_some()) {
        return;
    }
    let store = store_for(live.bridge_state());
    ensure_updates_listener(live, &store);
    let queued_any = live.read_session(|session| {
        let mut guard = store.lock().unwrap_or_else(PoisonError::into_inner);
        let mut queued_any = false;
        for hm in messages.iter_mut() {
            if hm.reply.is_none() {
                continue;
            }
            let holder_id = FullMsgId::new(to_data_peer(hm.peer_id), to_data_msg(hm.id));
            let Some(holder) = session.message(holder_id) else {
                continue;
            };
            let Some(reply) = holder.reply_to.as_ref() else {
                continue;
            };
            let target = match (
                reply.target_full_msg_id(holder.peer_id),
                reply.reply_to_story,
            ) {
                (Some(id), _) => {
                    if let Some(found) = session.message(id) {
                        hm.reply =
                            reply_header_for(session, holder, ReplyTargetState::Found(found));
                        continue;
                    }
                    Target::Message(id)
                }
                (None, Some(story)) => Target::Story(story),
                (None, None) => continue,
            };
            guard
                .dependents
                .entry(target)
                .or_default()
                .insert(holder_id);
            if let std::collections::hash_map::Entry::Vacant(slot) = guard.targets.entry(target) {
                slot.insert(TargetEntry::Pending);
                guard.queued.push(target);
                queued_any = true;
            }
            let entry = guard.targets.get(&target);
            hm.reply = match target {
                Target::Message(_) => {
                    let state = match entry {
                        Some(TargetEntry::Loaded(found)) => ReplyTargetState::Found(found),
                        Some(TargetEntry::Deleted | TargetEntry::Story { .. }) => {
                            ReplyTargetState::Deleted
                        }
                        Some(TargetEntry::Pending) | None => ReplyTargetState::Loading,
                    };
                    reply_header_for(session, holder, state)
                }
                Target::Story(story) => {
                    let state = match entry {
                        Some(TargetEntry::Story { caption, media }) => StoryReplyState::Found {
                            caption,
                            media: media.as_deref(),
                        },
                        Some(TargetEntry::Deleted | TargetEntry::Loaded(_)) => {
                            StoryReplyState::Deleted
                        }
                        Some(TargetEntry::Pending) | None => StoryReplyState::Loading,
                    };
                    Some(story_reply_header(session, holder, story, state))
                }
            };
        }
        let schedule = queued_any && !guard.flush_scheduled;
        if schedule {
            guard.flush_scheduled = true;
        }
        schedule
    });
    if queued_any {
        schedule_flush(live, store);
    }
}

/// Thumbnail the reply header of `msg_id` shows (`HistoryBackend::reply_preview_thumb`): the
/// replied-to message is looked up in the session and in this cache, a story in this cache.
pub(crate) fn reply_preview_thumb(
    live: &LiveSession,
    peer_id: tg_app_core::stand_in::types::PeerId,
    msg_id: tg_app_core::stand_in::types::MsgId,
) -> tg_app_core::info::backend::SharedMediaThumb {
    use crate::reply_preview::{OwnedReplyImage, ReplyTargets, reply_image_to_load};
    use tg_app_core::info::backend::SharedMediaThumb;
    let holder_id = FullMsgId::new(to_data_peer(peer_id), to_data_msg(msg_id));
    let store = store_for(live.bridge_state());
    let image = live.read_session(|session| {
        let holder = session.message(holder_id)?;
        let reply = holder.reply_to.as_ref()?;
        let guard = store.lock().unwrap_or_else(PoisonError::into_inner);
        let message = reply.target_full_msg_id(holder.peer_id).and_then(|id| {
            session
                .message(id)
                .or_else(|| match guard.targets.get(&Target::Message(id)) {
                    Some(TargetEntry::Loaded(found)) => Some(&**found),
                    _ => None,
                })
        });
        let story =
            reply
                .reply_to_story
                .and_then(|id| match guard.targets.get(&Target::Story(id)) {
                    Some(TargetEntry::Story { media, .. }) => media.as_deref(),
                    _ => None,
                });
        reply_image_to_load(holder, ReplyTargets { message, story })
    });
    let cache = live.media_cache();
    let (rpc, handle, bridge) = (
        live.runtime().rpc(),
        live.runtime().tokio_handle(),
        live.bridge_state(),
    );
    match image {
        None => SharedMediaThumb::Unavailable,
        Some(OwnedReplyImage::Photo(photo)) => {
            cache.shared_media_thumb_for_photo(&photo, rpc, handle, bridge)
        }
        Some(OwnedReplyImage::Document(doc)) => {
            cache.shared_media_thumb_for_document(&doc, rpc, handle, bridge)
        }
        Some(OwnedReplyImage::Web(web)) => crate::web_thumb::fetch(live, &web),
    }
}

/// Keeps cached targets current: a deleted target turns into "Deleted message" and an edited one
/// shows its new text (`HistoryMessageReply::itemRemoved`, `Data::Session::requestItemTextRefresh`
/// for dependent items). Only targets kept in this cache are handled here; targets in the loaded
/// history are already re-read from the session.
fn ensure_updates_listener(live: &LiveSession, store: &Arc<Mutex<ReplyTargetsStore>>) {
    {
        let mut guard = store.lock().unwrap_or_else(PoisonError::into_inner);
        if guard.listener_started {
            return;
        }
        guard.listener_started = true;
    }
    let handle = live.runtime().tokio_handle();
    let updates_rx = {
        let _guard = handle.enter();
        live.runtime().rpc().subscribe_updates()
    };
    let Some(mut rx) = updates_rx else {
        return;
    };
    let store_weak = Arc::downgrade(store);
    let bridge_weak = Arc::downgrade(live.bridge_state());
    let session_weak = Arc::downgrade(live.runtime().session());
    handle.spawn(async move {
        loop {
            let updates = match rx.recv().await {
                Ok(updates) => updates,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    debug!(skipped, "reply targets updates listener lagged");
                    continue;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            };
            let (Some(store), Some(session)) = (store_weak.upgrade(), session_weak.upgrade())
            else {
                break;
            };
            let self_id = session.read().await.self_user_id();
            let repaint = apply_updates_to_cache(&store, &updates, self_id);
            if !repaint.is_empty()
                && let Some(bridge) = bridge_weak.upgrade()
            {
                for holder in repaint {
                    bridge.record_message_edited(holder.peer, holder.msg);
                }
                bridge.notify_ui();
            }
        }
    });
}

fn apply_updates_to_cache(
    store: &Mutex<ReplyTargetsStore>,
    updates: &tl_enums::Updates,
    self_id: tg_data::UserId,
) -> HashSet<FullMsgId> {
    let list: Vec<&tl_enums::Update> = match updates {
        tl_enums::Updates::Updates(x) => x.updates.iter().collect(),
        tl_enums::Updates::Combined(x) => x.updates.iter().collect(),
        tl_enums::Updates::UpdateShort(x) => vec![&x.update],
        _ => Vec::new(),
    };
    let mut guard = store.lock().unwrap_or_else(PoisonError::into_inner);
    let mut changed: Vec<Target> = Vec::new();
    for update in list {
        match update {
            tl_enums::Update::DeleteMessages(u) => {
                let ids: HashSet<i64> = u.messages.iter().map(|&id| i64::from(id)).collect();
                for (target, entry) in &mut guard.targets {
                    if let Target::Message(id) = target
                        && !id.peer.is_channel()
                        && ids.contains(&id.msg.0)
                        && matches!(entry, TargetEntry::Loaded(_))
                    {
                        *entry = TargetEntry::Deleted;
                        changed.push(*target);
                    }
                }
            }
            tl_enums::Update::DeleteChannelMessages(u) => {
                let Ok(channel) = u64::try_from(u.channel_id) else {
                    continue;
                };
                let peer = DataPeerId::from_channel(tg_data::ChannelId(channel));
                for &id in &u.messages {
                    let target = Target::Message(FullMsgId::new(peer, DataMsgId(i64::from(id))));
                    if let Some(entry @ TargetEntry::Loaded(_)) = guard.targets.get_mut(&target) {
                        *entry = TargetEntry::Deleted;
                        changed.push(target);
                    }
                }
            }
            tl_enums::Update::EditMessage(tl_types::UpdateEditMessage { message, .. })
            | tl_enums::Update::EditChannelMessage(tl_types::UpdateEditChannelMessage {
                message,
                ..
            }) => {
                let Some(edited) = message_from_tl(message, self_id) else {
                    continue;
                };
                let target = Target::Message(edited.full_id());
                if let Some(entry @ TargetEntry::Loaded(_)) = guard.targets.get_mut(&target) {
                    *entry = TargetEntry::Loaded(Box::new(edited));
                    changed.push(target);
                }
            }
            _ => {}
        }
    }
    changed
        .iter()
        .filter_map(|t| guard.dependents.get(t))
        .flat_map(|deps| deps.iter().copied())
        .collect()
}

fn schedule_flush(live: &LiveSession, store: Arc<Mutex<ReplyTargetsStore>>) {
    let rpc = live.runtime().rpc().clone();
    let session = Arc::clone(live.runtime().session());
    let bridge = Arc::downgrade(live.bridge_state());
    live.runtime().tokio_handle().spawn(async move {
        tokio::time::sleep(RESOLVE_DELAY).await;
        let queued = {
            let mut guard = store.lock().unwrap_or_else(PoisonError::into_inner);
            guard.flush_scheduled = false;
            std::mem::take(&mut guard.queued)
        };
        let batches = {
            let guard = session.read().await;
            group_by_box(&guard, &queued)
        };
        let mut resolved: Vec<(Target, Option<TargetEntry>)> = Vec::new();
        for batch in batches {
            resolved.extend(resolve_batch(&rpc, &session, batch).await);
        }
        let mut repaint: HashSet<FullMsgId> = HashSet::new();
        {
            let mut guard = store.lock().unwrap_or_else(PoisonError::into_inner);
            for (target, entry) in resolved {
                match entry {
                    Some(entry) => {
                        guard.targets.insert(target, entry);
                        if let Some(deps) = guard.dependents.get(&target) {
                            repaint.extend(deps.iter().copied());
                        }
                    }
                    // Transient failure: forget it so the next conversion asks again.
                    None => {
                        guard.targets.remove(&target);
                    }
                }
            }
            guard.trim();
        }
        if let Some(bridge) = bridge.upgrade()
            && !repaint.is_empty()
        {
            for holder in repaint {
                bridge.record_message_edited(holder.peer, holder.msg);
            }
            bridge.notify_ui();
        }
        #[cfg(test)]
        {
            store
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .flushes_done += 1;
        }
    });
}

/// How many reply target requests of `live` have finished and recorded their repaints.
#[cfg(test)]
pub(crate) fn flushes_done(live: &LiveSession) -> u64 {
    store_for(live.bridge_state())
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .flushes_done
}

/// One request with the targets it answers: a channel box (`channels.getMessages`), the common
/// box (`messages.getMessages`), or one story owner (`stories.getStoriesByID`).
enum Batch {
    Messages {
        channel: Option<tl_enums::InputChannel>,
        targets: Vec<FullMsgId>,
    },
    Stories {
        peer: tl_enums::InputPeer,
        targets: Vec<FullStoryId>,
    },
}

fn push_grouped<K: PartialEq, T>(groups: &mut Vec<(K, Vec<T>)>, key: K, item: T) {
    match groups.iter_mut().find(|(k, _)| *k == key) {
        Some((_, list)) => list.push(item),
        None => groups.push((key, vec![item])),
    }
}

fn group_by_box(session: &Session, queued: &[Target]) -> Vec<Batch> {
    let mut common: Vec<FullMsgId> = Vec::new();
    let mut channels: Vec<(DataPeerId, Vec<FullMsgId>)> = Vec::new();
    let mut stories: Vec<(DataPeerId, Vec<FullStoryId>)> = Vec::new();
    for &target in queued {
        match target {
            Target::Message(id) if id.peer.is_channel() => push_grouped(&mut channels, id.peer, id),
            Target::Message(id) => common.push(id),
            Target::Story(id) => push_grouped(&mut stories, id.peer, id),
        }
    }
    let mut out = Vec::new();
    for chunk in common.chunks(MAX_IDS_PER_REQUEST) {
        out.push(Batch::Messages {
            channel: None,
            targets: chunk.to_vec(),
        });
    }
    for (peer, list) in channels {
        let input = peer
            .to_channel()
            .map_or(tl_enums::InputChannel::Empty, |id| {
                input_channel_for(session, id)
            });
        for chunk in list.chunks(MAX_IDS_PER_REQUEST) {
            out.push(Batch::Messages {
                channel: Some(input.clone()),
                targets: chunk.to_vec(),
            });
        }
    }
    for (peer, list) in stories {
        let input = input_peer_for(session, peer);
        for chunk in list.chunks(MAX_IDS_PER_REQUEST) {
            out.push(Batch::Stories {
                peer: input.clone(),
                targets: chunk.to_vec(),
            });
        }
    }
    out
}

fn input_ids(targets: &[FullMsgId]) -> Vec<tl_enums::InputMessage> {
    let mut seen = HashSet::new();
    targets
        .iter()
        .filter(|t| seen.insert(t.msg))
        .filter_map(|t| i32::try_from(t.msg.0).ok())
        .map(|id| tl_enums::InputMessage::ID(tl_types::InputMessageID { id }))
        .collect()
}

/// Marks every target of a failed request: `Some(Deleted)` when the server refused it,
/// `None` (ask again later) when it did not complete.
fn failed<T: Copy>(
    targets: &[T],
    wrap: fn(T) -> Target,
    refused: bool,
) -> Vec<(Target, Option<TargetEntry>)> {
    targets
        .iter()
        .map(|&t| (wrap(t), refused.then_some(TargetEntry::Deleted)))
        .collect()
}

/// `Some(true)` when the server refused the request (CHANNEL_PRIVATE, MESSAGE_IDS_EMPTY, ...),
/// `Some(false)` when it did not complete (connection lost, timeout).
fn failure_kind<T>(
    answer: &Result<Result<T, InvocationError>, tokio::time::error::Elapsed>,
) -> Option<bool> {
    match answer {
        Ok(Ok(_)) => None,
        Ok(Err(InvocationError::Rpc(err))) => {
            debug!(?err, "reply target request failed");
            Some(true)
        }
        Ok(Err(err)) => {
            debug!(%err, "reply target request did not complete");
            Some(false)
        }
        Err(_) => Some(false),
    }
}

async fn resolve_batch<C: RpcClient<Error = InvocationError>>(
    rpc: &C,
    session: &Arc<tokio::sync::RwLock<Session>>,
    batch: Batch,
) -> Vec<(Target, Option<TargetEntry>)> {
    match batch {
        Batch::Messages { channel, targets } => {
            resolve_messages(rpc, session, channel, targets).await
        }
        Batch::Stories { peer, targets } => resolve_stories(rpc, session, peer, targets).await,
    }
}

async fn resolve_stories<C: RpcClient<Error = InvocationError>>(
    rpc: &C,
    session: &Arc<tokio::sync::RwLock<Session>>,
    peer: tl_enums::InputPeer,
    targets: Vec<FullStoryId>,
) -> Vec<(Target, Option<TargetEntry>)> {
    if matches!(peer, tl_enums::InputPeer::Empty) {
        return failed(&targets, Target::Story, true);
    }
    let mut ids: Vec<StoryId> = targets.iter().map(|t| t.story).collect();
    ids.dedup();
    debug!(count = ids.len(), "requesting story reply targets");
    let req = tl_fns::stories::GetStoriesByID { peer, id: ids };
    let answer = tokio::time::timeout(REQUEST_TIMEOUT, rpc.invoke(&req)).await;
    if let Some(refused) = failure_kind(&answer) {
        return failed(&targets, Target::Story, refused);
    }
    let Ok(Ok(tl_enums::stories::Stories::Stories(answer))) = answer else {
        return failed(&targets, Target::Story, false);
    };
    {
        let mut guard = session.write().await;
        apply_tl_users_and_chats(&mut guard, &answer.users, &answer.chats);
    }
    type FoundStory = (String, Option<Box<tg_data::MessageMedia>>);
    let mut found: HashMap<StoryId, Option<FoundStory>> = HashMap::new();
    for item in &answer.stories {
        match item {
            tl_enums::StoryItem::StoryItem(story) => {
                let media = tg_api::convert::message_media_from_tl(&story.media);
                found.insert(
                    story.id,
                    Some((
                        story.caption.clone().unwrap_or_default(),
                        Some(Box::new(media)),
                    )),
                );
            }
            tl_enums::StoryItem::Skipped(story) => {
                found.insert(story.id, Some((String::new(), None)));
            }
            tl_enums::StoryItem::Deleted(story) => {
                found.insert(story.id, None);
            }
        }
    }
    targets
        .into_iter()
        .map(|t| {
            let entry = match found.get(&t.story) {
                Some(Some((caption, media))) => TargetEntry::Story {
                    caption: caption.clone(),
                    media: media.clone(),
                },
                _ => TargetEntry::Deleted,
            };
            (Target::Story(t), Some(entry))
        })
        .collect()
}

async fn resolve_messages<C: RpcClient<Error = InvocationError>>(
    rpc: &C,
    session: &Arc<tokio::sync::RwLock<Session>>,
    channel: Option<tl_enums::InputChannel>,
    targets: Vec<FullMsgId>,
) -> Vec<(Target, Option<TargetEntry>)> {
    let id = input_ids(&targets);
    debug!(
        count = id.len(),
        channel = channel.is_some(),
        "requesting reply targets"
    );
    let answer = match channel {
        Some(tl_enums::InputChannel::Empty) => {
            // No access hash for the channel: tdesktop cannot ask either and shows the reply
            // as unavailable.
            return failed(&targets, Target::Message, true);
        }
        Some(channel) => {
            let req = tl_fns::channels::GetMessages { channel, id };
            tokio::time::timeout(REQUEST_TIMEOUT, rpc.invoke(&req)).await
        }
        None => {
            let req = tl_fns::messages::GetMessages { id };
            tokio::time::timeout(REQUEST_TIMEOUT, rpc.invoke(&req)).await
        }
    };
    if let Some(refused) = failure_kind(&answer) {
        return failed(&targets, Target::Message, refused);
    }
    let Ok(Ok(answer)) = answer else {
        return failed(&targets, Target::Message, false);
    };
    let (messages, users, chats) = match answer {
        tl_enums::messages::Messages::Messages(m) => (m.messages, m.users, m.chats),
        tl_enums::messages::Messages::Slice(m) => (m.messages, m.users, m.chats),
        tl_enums::messages::Messages::ChannelMessages(m) => (m.messages, m.users, m.chats),
        tl_enums::messages::Messages::NotModified(_) => (Vec::new(), Vec::new(), Vec::new()),
    };
    let fetched: HashMap<DataMsgId, Message> = {
        let mut guard = session.write().await;
        apply_tl_users_and_chats(&mut guard, &users, &chats);
        let self_id = guard.self_user_id();
        messages
            .iter()
            .filter_map(|raw| message_from_tl(raw, self_id))
            .map(|m| (m.id, m))
            .collect()
    };
    targets
        .into_iter()
        .map(|target| {
            let entry = match fetched.get(&target.msg) {
                Some(m) if !matches!(m.content, tg_data::MessageContent::Empty) => {
                    TargetEntry::Loaded(Box::new(m.clone()))
                }
                _ => TargetEntry::Deleted,
            };
            (Target::Message(target), Some(entry))
        })
        .collect()
}
