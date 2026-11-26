//! Text sends that show the pending message at once (TGB-2931).
//!
//! tdesktop creates the local message on the UI thread and only then sends it
//! (`ApiWrap::sendMessage`, `apiwrap.cpp:4866-4995`): the bubble with the clock is there before
//! any reply arrives, and the server reply later swaps in the server id
//! (`Data::Session::itemIdChanged`). The live backend does the same:
//!
//! 1. The UI prepares the chunks with its own [`MessageSender`], whose local ids start at
//!    [`UI_FIRST_LOCAL_INDEX`] so they never collide with the runtime's own.
//! 2. It stores each chunk's local message in the session and registers its random id (one
//!    short write, no network), so an `updateMessageID` that arrives first still merges into it.
//! 3. It queues the send with [`SessionRuntime::queue_prepared_send`]: the command enters the
//!    runtime's send / edit / delete lane before the call returns, so sends keep their order.
//! 4. A task waits for the reply and records a [`SendResolution`]; the next history drain turns
//!    it into [`HistoryLiveUpdate::LocalMessageSent`] / [`HistoryLiveUpdate::LocalMessageFailed`].

use std::sync::{Arc, Mutex};

use tg_api::convert::entities_from_tl_with_self;
use tg_api::messaging::{MessageSender, PreparedTextChunk};
use tg_api::{PendingMessageState, SendMessageOutcome, SendOptions};
use tg_data::{
    EffectId, FullMsgId as DataFullMsgId, Message, MessageFlags, MsgId as DataMsgId,
    PeerId as DataPeerId, ReplyInfo, Session,
};
use tg_tl::enums as tl_enums;

use tg_app_core::history::backend::{HistoryLiveUpdate, HistoryMessage};

use crate::convert::{from_data_msg, message_to_history_message, now_unix_i32};

/// First client index of the local ids the UI allocates. The runtime's own sender counts from
/// 0; both stay below `MsgId::LOCAL_COUNT`, so every local id has a UI id.
pub const UI_FIRST_LOCAL_INDEX: i32 = 1 << 29;

/// A finished background send, waiting for the next history drain.
#[derive(Debug)]
pub(crate) struct SendResolution {
    /// Chat the message was sent to.
    pub peer: DataPeerId,
    /// Local ids of the chunks, in send order.
    pub local_ids: Vec<DataMsgId>,
    /// The runtime's answer.
    pub outcome: Result<SendMessageOutcome, String>,
    /// Server id of each chunk (`None` when it is not known).
    pub sent_ids: Vec<Option<DataMsgId>>,
    /// Effect the user picked, kept on the sent message when the server does not echo it.
    pub effect_id: Option<u64>,
}

/// What the caller asks for besides the text.
#[derive(Debug, Default)]
pub(crate) struct QueuedSendExtras {
    /// Reply target (same chat).
    pub reply_to_msg_id: Option<DataMsgId>,
    /// Quoted part of the reply target (`text`, UTF-16 offset).
    pub reply_quote: Option<(String, i32)>,
    /// Effect id for the local message and the resolution.
    pub effect_id: Option<u64>,
}

/// Shared state of the optimistic sends of one session (cloned with the `LiveSession`).
#[derive(Clone)]
pub(crate) struct OptimisticSends {
    sender: Arc<Mutex<MessageSender>>,
    resolved: Arc<Mutex<Vec<SendResolution>>>,
}

impl Default for OptimisticSends {
    fn default() -> Self {
        Self {
            sender: Arc::new(Mutex::new(
                MessageSender::default().with_first_local_index(UI_FIRST_LOCAL_INDEX),
            )),
            resolved: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

impl OptimisticSends {
    /// Splits `text` into chunks with fresh local ids and random ids.
    pub(crate) fn prepare(
        &self,
        peer: DataPeerId,
        text: &str,
        entities: &[tl_enums::MessageEntity],
    ) -> Vec<PreparedTextChunk> {
        self.sender
            .lock()
            .map(|mut s| s.prepare_text_chunks(peer, text, entities))
            .unwrap_or_default()
    }

    /// Takes every resolution recorded so far.
    pub(crate) fn take_resolved(&self) -> Vec<SendResolution> {
        self.resolved
            .lock()
            .map(|mut r| std::mem::take(&mut *r))
            .unwrap_or_default()
    }

    /// Queues `chunks` (local messages already stored) and records the outcome when the
    /// runtime answers. Never waits.
    pub(crate) fn queue(
        &self,
        live: &crate::LiveSession,
        peer: DataPeerId,
        chunks: Vec<PreparedTextChunk>,
        reply_to: Option<tl_enums::InputReplyTo>,
        options: SendOptions,
        extras: &QueuedSendExtras,
    ) {
        let local_ids: Vec<DataMsgId> = chunks.iter().map(|c| c.local_id.msg).collect();
        let random_ids: Vec<i64> = chunks.iter().map(|c| c.random_id).collect();
        let runtime = &live.runtime;
        let bridge = live.bridge.state();
        let reply = runtime.queue_prepared_send(peer, chunks, reply_to, options);
        let session = Arc::clone(runtime.session());
        let resolved = Arc::clone(&self.resolved);
        let bridge = Arc::clone(bridge);
        let effect_id = extras.effect_id;
        runtime.tokio_handle().spawn(async move {
            let outcome = reply.await.map_err(|e| e.to_string());
            let sent_ids = match &outcome {
                Ok(outcome) => sent_ids(outcome, &local_ids),
                Err(_) => vec![None; local_ids.len()],
            };
            if let Ok(outcome) = &outcome {
                // A reply without `updateMessageID` still carries the new message: the local
                // copy goes, as `Data::Session::processMessagesIds` never merges it.
                let mut s = session.write().await;
                for (i, (&local, sent)) in local_ids.iter().zip(&sent_ids).enumerate() {
                    let resolved = outcome.messages.iter().any(|p| {
                        p.local_id.msg == local
                            && matches!(p.state, PendingMessageState::Sent { .. })
                    });
                    if !resolved && sent.is_some() {
                        if let Some(&random_id) = random_ids.get(i) {
                            s.unregister_random_id(random_id);
                        }
                        s.delete_messages(peer, &[local]);
                    }
                }
            }
            if outcome.is_err() {
                // The runtime marks the chunks it tried; one that never reached the lane (closed
                // runtime, unknown peer) would otherwise keep its clock forever.
                let mut s = session.write().await;
                for (&local, &random_id) in local_ids.iter().zip(&random_ids) {
                    s.unregister_random_id(random_id);
                    if let Some(msg) = s.message_mut(DataFullMsgId::new(peer, local))
                        && msg.flags.contains(MessageFlags::BEING_SENT)
                    {
                        msg.flags.remove(MessageFlags::BEING_SENT);
                        msg.flags.insert(MessageFlags::SENDING_FAILED);
                    }
                }
            }
            if let Ok(mut r) = resolved.lock() {
                r.push(SendResolution {
                    peer,
                    local_ids,
                    outcome,
                    sent_ids,
                    effect_id,
                });
            }
            bridge.mark_views_changed();
            bridge.notify_ui();
        });
    }
}

/// Server id of each chunk: the id `updateMessageID` resolved, else the new message in that
/// chunk's own reply.
fn sent_ids(outcome: &SendMessageOutcome, local_ids: &[DataMsgId]) -> Vec<Option<DataMsgId>> {
    local_ids
        .iter()
        .enumerate()
        .map(|(i, &local)| {
            outcome
                .messages
                .iter()
                .find(|p| p.local_id.msg == local)
                .and_then(|p| match p.state {
                    PendingMessageState::Sent { server_id } => Some(server_id),
                    _ => None,
                })
                .or_else(|| {
                    outcome
                        .updates
                        .get(i)
                        .and_then(|u| crate::session::extract_message_ids_from_updates(u).pop())
                })
        })
        .collect()
}

/// Stores the local message of each chunk and registers its random id (`ApiWrap::sendMessage`
/// creates the local item before `messages.sendMessage`), then returns the first chunk as the
/// view shows it.
pub(crate) fn store_local_messages(
    session: &mut Session,
    peer: DataPeerId,
    chunks: &[PreparedTextChunk],
    options: &SendOptions,
    extras: &QueuedSendExtras,
) -> Option<HistoryMessage> {
    let self_user_id = session.self_user_id();
    let self_peer_id = session.self_peer_id();
    let now = now_unix_i32();
    for chunk in chunks {
        let mut local = Message::new_text(
            peer,
            chunk.local_id.msg,
            Some(self_peer_id),
            now,
            tg_data::TextWithEntities::new(
                chunk.text.clone(),
                entities_from_tl_with_self(&chunk.entities, self_user_id),
            ),
        );
        local
            .flags
            .insert(MessageFlags::OUTGOING | MessageFlags::BEING_SENT);
        local.flags.set(MessageFlags::SILENT, options.silent);
        local
            .flags
            .set(MessageFlags::NO_FORWARDS, options.noforwards);
        local
            .flags
            .set(MessageFlags::INVERT_MEDIA, options.invert_media);
        local.effect_id = extras.effect_id.map(EffectId);
        local.reply_to = extras.reply_to_msg_id.map(|id| ReplyInfo {
            reply_to_msg_id: Some(id),
            quote: extras.reply_quote.is_some(),
            quote_text: extras
                .reply_quote
                .as_ref()
                .map(|(text, _)| tg_data::TextWithEntities::new(text.clone(), Vec::new())),
            quote_offset: extras.reply_quote.as_ref().map(|(_, offset)| *offset),
            ..ReplyInfo::default()
        });
        session.register_random_id(chunk.random_id, chunk.local_id);
        session.insert_message(local);
    }
    let first = chunks.first()?;
    session
        .message(first.local_id)
        .map(|m| message_to_history_message(session, m))
}

/// Local messages (being sent or failed) of `peer`, oldest first: tdesktop keeps them at the
/// bottom of the history. They are not part of the loaded ranges, so slices add them here.
pub(crate) fn local_messages(session: &Session, peer: DataPeerId) -> Vec<HistoryMessage> {
    session
        .stored_message_ids(peer)
        .take_while(|id| id.is_client())
        .filter_map(|id| session.message(DataFullMsgId::new(peer, id)))
        .filter(|m| {
            m.is_outgoing()
                && m.flags
                    .intersects(MessageFlags::BEING_SENT | MessageFlags::SENDING_FAILED)
        })
        .map(|m| message_to_history_message(session, m))
        .collect()
}

/// History updates for the resolutions of `peer`, built from the session as it is now.
pub(crate) fn resolution_updates(
    session: &Session,
    peer: DataPeerId,
    resolution: &SendResolution,
) -> Vec<HistoryLiveUpdate> {
    if resolution.peer != peer {
        return Vec::new();
    }
    let mut out = Vec::new();
    match &resolution.outcome {
        Ok(_) => {
            for (i, &local) in resolution.local_ids.iter().enumerate() {
                let Some(server_id) = resolution.sent_ids.get(i).copied().flatten() else {
                    continue;
                };
                let Some(mut message) = session
                    .message(DataFullMsgId::new(peer, server_id))
                    .map(|m| message_to_history_message(session, m))
                else {
                    continue;
                };
                if i + 1 == resolution.local_ids.len()
                    && message.extra.effect_id.is_none()
                    && let Some(effect) = resolution.effect_id
                {
                    message.extra.effect_id = Some(effect);
                }
                out.push(HistoryLiveUpdate::LocalMessageSent {
                    local_id: from_data_msg(local),
                    message: Box::new(message),
                });
            }
        }
        Err(error) => {
            let mut error = Some(error.clone());
            let mut gone = Vec::new();
            for &local in &resolution.local_ids {
                let failed = session
                    .message(DataFullMsgId::new(peer, local))
                    .is_some_and(|m| m.flags.contains(MessageFlags::SENDING_FAILED));
                if failed {
                    out.push(HistoryLiveUpdate::LocalMessageFailed {
                        local_id: from_data_msg(local),
                        error: error.take(),
                    });
                } else {
                    // `MESSAGE_EMPTY` destroys the local message (`apiwrap.cpp:4988-4991`).
                    gone.push(from_data_msg(local));
                }
            }
            if !gone.is_empty() {
                out.push(HistoryLiveUpdate::MessagesDeleted(gone));
            }
            if let Some(error) = error {
                // Nothing of the send is left to carry the error: show it on its own.
                out.push(HistoryLiveUpdate::LocalMessageFailed {
                    local_id: from_data_msg(resolution.local_ids.first().copied().unwrap_or(
                        DataMsgId::client_by_index(UI_FIRST_LOCAL_INDEX).unwrap_or(DataMsgId(0)),
                    )),
                    error: Some(error),
                });
            }
        }
    }
    out
}
