//! Message sending, editing, deleting, forwarding, inbox read throttling,
//! typing actions, and cloud draft saving.
//!
//! Port origin: `apiwrap.cpp` (`sendMessage`, `sendMessageFail`, `forwardMessages`,
//! `saveDraftToCloudDelayed`, `saveDraftsToCloud`, `isQuitPrevent`),
//! `api/api_editing.cpp` (`EditTextMessage`), `api/api_send_progress.cpp`,
//! `data/data_histories.cpp` (`readInboxTill`, `sendReadRequests`, `deleteMessages`,
//! `deleteAllMessages`).

use std::collections::HashMap;
use std::time::{Duration, Instant};

use rand::Rng;
use tg_data::{FullMsgId, MsgId, PeerId, TimeId};
use tg_tl::{enums as tl_enums, functions as tl_fns, types as tl_types};

use crate::convert::{input_peer_to_input_channel, peer_id_from_input_peer};
use crate::error::{ApiError, SendMessageErrorKind, map_send_message_error};
use crate::invoker::Invoker;
use crate::rpc::InvocationError;
use crate::types::{ForwardOptions, SendOptions};

/// Default maximum message length in Unicode scalar values before splitting (`4096`).
pub const DEFAULT_MESSAGE_LENGTH_LIMIT: usize = 4096;

/// Maximum `messages.deleteHistory` retry iterations (`kDeleteHistoryRetryLimit`).
pub const DELETE_HISTORY_RETRY_LIMIT: usize = 8;

/// Delay before flushing non-forced inbox read requests (`Data::Histories::kReadRequestTimeout`).
pub const READ_REQUEST_TIMEOUT: Duration = Duration::from_secs(3);

/// Auto-cancel timeout for typing actions (`kCancelTypingActionTimeout`).
pub const CANCEL_TYPING_ACTION_TIMEOUT: Duration = Duration::from_secs(5);

/// Minimum interval between repeated speaking updates (`kSendMySpeakingInterval`).
pub const SEND_MY_SPEAKING_INTERVAL: Duration = Duration::from_secs(3);

/// Minimum interval between repeated typing/upload updates (`kSendMyTypingInterval`).
pub const SEND_MY_TYPING_INTERVAL: Duration = Duration::from_secs(5);

/// Skip sending typing updates to users offline for more than this many seconds
/// (`kSendTypingsToOfflineFor`).
pub const SEND_TYPINGS_TO_OFFLINE_FOR_SECS: i32 = 30;

/// Debounce delay before saving local drafts to the cloud (`kSaveCloudDraftTimeout`).
pub const SAVE_CLOUD_DRAFT_TIMEOUT: Duration = Duration::from_secs(1);

// ---------------------------------------------------------------------------
// Message Sending, Editing, Deleting & Forwarding
// ---------------------------------------------------------------------------

/// Lifecycle status of a locally created outgoing message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingMessageState {
    /// Sent to server; waiting for `updateMessageID` / `updateShortSentMessage`.
    Sending,
    /// Server confirmed and assigned `server_id`.
    Sent {
        /// Assigned server message ID.
        server_id: MsgId,
    },
    /// Send failed with a mapped error.
    Failed {
        /// Mapped failure category.
        error: SendMessageErrorKind,
    },
    /// Destroyed locally (e.g. `MESSAGE_EMPTY`).
    Destroyed,
}

/// Local pending message tracked between creation and server acknowledgement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingMessage {
    /// Local client-side message ID (`MsgId::is_client() == true`).
    pub local_id: FullMsgId,
    /// Unique 64-bit random ID sent to Telegram.
    pub random_id: i64,
    /// Message text body.
    pub text: String,
    /// Current delivery state.
    pub state: PendingMessageState,
}

/// Outcome of sending one message (possibly split into multiple chunks if > limit).
#[derive(Debug, Clone, PartialEq)]
pub struct SendMessageOutcome {
    /// Local pending messages created for each sent chunk.
    pub messages: Vec<PendingMessage>,
    /// Raw TL `Updates` responses returned by `messages.sendMessage`.
    pub updates: Vec<tl_enums::Updates>,
}

/// Prepared local text chunk with allocated `local_id`, unique 64-bit `random_id`,
/// trimmed text, and UTF-16-aligned TL entities ready for `messages.sendMessage`.
#[derive(Debug, Clone, PartialEq)]
pub struct PreparedTextChunk {
    /// Local client-side message ID.
    pub local_id: FullMsgId,
    /// Unique 64-bit random ID sent to Telegram.
    pub random_id: i64,
    /// Trimmed chunk text.
    pub text: String,
    /// UTF-16 entities sliced and shifted for this chunk.
    pub entities: Vec<tl_enums::MessageEntity>,
}

/// Outcome of `MessageSender::send_media` (`messages.sendMedia`).
#[derive(Debug, Clone, PartialEq)]
pub struct SendMediaOutcome {
    /// Local pending message tracked for this media send.
    pub message: PendingMessage,
    /// Raw TL `Updates` response returned by `messages.sendMedia`.
    pub updates: tl_enums::Updates,
}

/// Single media item inside an album passed to `MessageSender::send_multi_media`
/// (`messages.sendMultiMedia`, `apiwrap.cpp:5565-5850`).
#[derive(Debug, Clone, PartialEq)]
pub struct AlbumMediaItem {
    /// Uploaded or referenced `InputMedia`.
    pub media: tl_enums::InputMedia,
    /// Optional caption text for this album item.
    pub caption: String,
    /// Optional UTF-16 TL entities for `caption`.
    pub entities: Vec<tl_enums::MessageEntity>,
}

/// Outcome of `MessageSender::send_multi_media` (`messages.sendMultiMedia`).
#[derive(Debug, Clone, PartialEq)]
pub struct SendMultiMediaOutcome {
    /// Local 64-bit album `grouped_id` shared by all pending messages in the album.
    pub grouped_id: u64,
    /// Local pending messages created for each item in the album (each with a unique `random_id`).
    pub messages: Vec<PendingMessage>,
    /// Raw TL `Updates` response returned by `messages.sendMultiMedia`.
    pub updates: tl_enums::Updates,
}

/// Source message item to forward (`forwardMessages` input).
#[derive(Debug, Clone, PartialEq)]
pub struct ForwardItem {
    /// Source peer where the message lives.
    pub from_peer: tl_enums::InputPeer,
    /// Server message ID in `from_peer`.
    pub msg_id: MsgId,
}

/// Outcome of `MessageSender::forward_messages`.
#[derive(Debug, Clone, PartialEq)]
pub struct ForwardOutcome {
    /// Local pending message if a single `PreserveInfo` message was forwarded.
    pub local_message: Option<PendingMessage>,
    /// Server TL `Updates` responses (one per contiguous source-peer batch).
    pub updates: Vec<tl_enums::Updates>,
}

/// Result of `MessageSender::delete_history`.
#[derive(Debug, Clone, PartialEq)]
pub enum DeleteHistoryOutcome {
    /// Channel history deleted via `channels.deleteHistory`.
    Channel(Box<tl_enums::Updates>),
    /// Non-channel history deleted via one or more `messages.deleteHistory` pages.
    Affected(Vec<tl_types::messages::AffectedHistory>),
}

/// Source of 64-bit random IDs for outgoing messages (injectable for deterministic tests).
pub trait RandomIdSource: Send + Sync {
    /// Generates a non-zero 64-bit random ID.
    fn next_random_id(&mut self) -> i64;
}

/// Default OS-seeded random ID generator.
#[derive(Debug, Default)]
pub struct OsRandomIdSource;

impl RandomIdSource for OsRandomIdSource {
    fn next_random_id(&mut self) -> i64 {
        let mut rng = rand::thread_rng();
        loop {
            let val: i64 = rng.r#gen();
            if val != 0 {
                return val;
            }
        }
    }
}

/// Manages outgoing message sending, pending `random_id` -> `server_id` mapping,
/// editing, deleting, and forwarding.
#[derive(Debug)]
pub struct MessageSender<R: RandomIdSource = OsRandomIdSource> {
    rng: R,
    next_local_index: i32,
    message_length_limit: usize,
    random_to_local: HashMap<i64, FullMsgId>,
    pending: HashMap<FullMsgId, PendingMessage>,
}

impl Default for MessageSender<OsRandomIdSource> {
    fn default() -> Self {
        Self::new(OsRandomIdSource)
    }
}

impl<R: RandomIdSource> MessageSender<R> {
    /// Creates a new `MessageSender` with the given random ID source.
    #[must_use]
    pub fn new(rng: R) -> Self {
        Self {
            rng,
            next_local_index: 0,
            message_length_limit: DEFAULT_MESSAGE_LENGTH_LIMIT,
            random_to_local: HashMap::new(),
            pending: HashMap::new(),
        }
    }

    /// Starts allocating local message ids at client index `index`, so two senders that both
    /// create local messages for one session never hand out the same id.
    #[must_use]
    pub fn with_first_local_index(mut self, index: i32) -> Self {
        self.next_local_index = index.max(0);
        self
    }

    /// Tracks chunks another sender prepared (the UI allocates them so the pending message is
    /// shown before the send is queued): `finish_prepared_chunk` and
    /// `resolve_update_message_id` then resolve them like chunks prepared here.
    pub fn adopt_prepared_chunks(&mut self, chunks: &[PreparedTextChunk]) {
        for chunk in chunks {
            self.random_to_local.insert(chunk.random_id, chunk.local_id);
            self.pending.insert(
                chunk.local_id,
                PendingMessage {
                    local_id: chunk.local_id,
                    random_id: chunk.random_id,
                    text: chunk.text.clone(),
                    state: PendingMessageState::Sending,
                },
            );
        }
    }

    /// Sets the maximum message length in characters before splitting into multiple messages.
    pub fn set_message_length_limit(&mut self, limit: usize) {
        self.message_length_limit = limit.max(1);
    }

    /// Allocates the next client-side message ID (`nextLocalMessageId`, `MsgId::is_client() == true`).
    pub fn next_local_msg_id(&mut self) -> MsgId {
        let idx = self.next_local_index;
        self.next_local_index = self.next_local_index.saturating_add(1);
        MsgId::client_by_index(idx).unwrap_or(tg_data::START_CLIENT_MSG_ID)
    }

    /// Looks up a local pending message by `FullMsgId`.
    #[must_use]
    pub fn pending_message(&self, id: FullMsgId) -> Option<&PendingMessage> {
        self.pending.get(&id)
    }

    /// Looks up the local `FullMsgId` associated with an in-flight `random_id`.
    #[must_use]
    pub fn message_id_by_random_id(&self, random_id: i64) -> Option<FullMsgId> {
        self.random_to_local.get(&random_id).copied()
    }

    /// Resolves an `updateMessageID` (or `updateShortSentMessage`) mapping `random_id -> server_id`.
    ///
    /// Marks the corresponding local pending message as `Sent { server_id }` and
    /// unregisters `random_id`. Returns the resolved local `FullMsgId` if found.
    pub fn resolve_update_message_id(
        &mut self,
        random_id: i64,
        server_id: MsgId,
    ) -> Option<FullMsgId> {
        let local_id = self.random_to_local.remove(&random_id)?;
        if let Some(entry) = self.pending.get_mut(&local_id) {
            entry.state = PendingMessageState::Sent { server_id };
        }
        Some(local_id)
    }

    /// Scans a TL `Updates` returned by `sendMessage` / `forwardMessages` and resolves
    /// any `Update::MessageID` or `Updates::UpdateShortSentMessage` entries.
    pub fn apply_sent_updates(&mut self, batch: &tl_enums::Updates, sent_random_id: Option<i64>) {
        match batch {
            tl_enums::Updates::UpdateShortSentMessage(s) => {
                if let Some(rand_id) = sent_random_id {
                    self.resolve_update_message_id(rand_id, MsgId(i64::from(s.id)));
                }
            }
            tl_enums::Updates::UpdateShort(s) => {
                if let tl_enums::Update::MessageID(m) = &s.update {
                    self.resolve_update_message_id(m.random_id, MsgId(i64::from(m.id)));
                }
            }
            tl_enums::Updates::Updates(u) => {
                for item in &u.updates {
                    if let tl_enums::Update::MessageID(m) = item {
                        self.resolve_update_message_id(m.random_id, MsgId(i64::from(m.id)));
                    }
                }
            }
            tl_enums::Updates::Combined(c) => {
                for item in &c.updates {
                    if let tl_enums::Update::MessageID(m) = item {
                        self.resolve_update_message_id(m.random_id, MsgId(i64::from(m.id)));
                    }
                }
            }
            _ => {}
        }
    }

    /// Splits `text` and UTF-16 `entities` into chunks of at most `message_length_limit`
    /// Unicode scalar values and allocates a local pending message and unique 64-bit
    /// `random_id` for each chunk (`apiwrap.cpp:4842-4918`, `ApiWrap::sendMessage`).
    pub fn prepare_text_chunks(
        &mut self,
        peer_id: PeerId,
        text: &str,
        entities: &[tl_enums::MessageEntity],
    ) -> Vec<PreparedTextChunk> {
        let chunks = split_message_chunks_with_entities(text, entities, self.message_length_limit);
        let mut prepared = Vec::with_capacity(chunks.len());
        for (chunk_text, chunk_entities) in chunks {
            let local_msg_id = self.next_local_msg_id();
            let local_id = FullMsgId::new(peer_id, local_msg_id);
            let random_id = self.rng.next_random_id();

            let pending = PendingMessage {
                local_id,
                random_id,
                text: chunk_text.clone(),
                state: PendingMessageState::Sending,
            };
            self.random_to_local.insert(random_id, local_id);
            self.pending.insert(local_id, pending);
            prepared.push(PreparedTextChunk {
                local_id,
                random_id,
                text: chunk_text,
                entities: chunk_entities,
            });
        }
        prepared
    }

    /// Sends a single previously prepared text chunk via `messages.sendMessage`.
    pub async fn send_prepared_chunk<I: Invoker>(
        &mut self,
        invoker: &I,
        peer: tl_enums::InputPeer,
        chunk: &PreparedTextChunk,
        reply_to: Option<tl_enums::InputReplyTo>,
        options: &SendOptions,
    ) -> Result<(PendingMessage, tl_enums::Updates), SendMessageErrorKind> {
        let req = Self::prepared_chunk_request(peer, chunk, reply_to, options);
        let result = invoker.messages_send_message(req).await;
        self.finish_prepared_chunk(chunk, result)
    }

    /// Builds the `messages.sendMessage` request for a prepared chunk, for callers that run the
    /// request themselves and then call [`Self::finish_prepared_chunk`].
    #[must_use]
    pub fn prepared_chunk_request(
        peer: tl_enums::InputPeer,
        chunk: &PreparedTextChunk,
        reply_to: Option<tl_enums::InputReplyTo>,
        options: &SendOptions,
    ) -> tl_fns::messages::SendMessage {
        let chunk_entities = (!chunk.entities.is_empty()).then(|| chunk.entities.clone());
        tl_fns::messages::SendMessage {
            no_webpage: options.no_webpage,
            silent: options.silent,
            background: false,
            clear_draft: options.clear_draft,
            noforwards: options.noforwards,
            update_stickersets_order: false,
            invert_media: options.invert_media,
            allow_paid_floodskip: false,
            peer,
            reply_to,
            message: chunk.text.clone(),
            random_id: chunk.random_id,
            reply_markup: None,
            entities: chunk_entities,
            schedule_date: options.schedule_date,
            schedule_repeat_period: options.schedule_repeat_period,
            send_as: options.send_as.clone(),
            quick_reply_shortcut: None,
            effect: options.effect_id,
            allow_paid_stars: options.allow_paid_stars,
            suggested_post: None,
            rich_message: None,
        }
    }

    /// Records the outcome of a prepared chunk's `messages.sendMessage`: resolves the server id
    /// on success, marks the pending message failed (or destroyed on `MESSAGE_EMPTY`) on error.
    pub fn finish_prepared_chunk(
        &mut self,
        chunk: &PreparedTextChunk,
        result: Result<tl_enums::Updates, InvocationError>,
    ) -> Result<(PendingMessage, tl_enums::Updates), SendMessageErrorKind> {
        match result {
            Ok(batch) => {
                self.apply_sent_updates(&batch, Some(chunk.random_id));
                let entry = self
                    .pending
                    .get(&chunk.local_id)
                    .cloned()
                    .unwrap_or(PendingMessage {
                        local_id: chunk.local_id,
                        random_id: chunk.random_id,
                        text: chunk.text.clone(),
                        state: PendingMessageState::Sending,
                    });
                Ok((entry, batch))
            }
            Err(err) => {
                let mapped = map_send_message_error(&err);
                self.random_to_local.remove(&chunk.random_id);
                if let Some(entry) = self.pending.get_mut(&chunk.local_id) {
                    entry.state = if mapped == SendMessageErrorKind::MessageEmpty {
                        PendingMessageState::Destroyed
                    } else {
                        PendingMessageState::Failed {
                            error: mapped.clone(),
                        }
                    };
                }
                Err(mapped)
            }
        }
    }

    /// Sends a text message, splitting at `message_length_limit` if necessary
    /// (`ApiWrap::sendMessage`, `apiwrap.cpp:4842-4995`).
    ///
    /// Creates a local pending message for each chunk before invoking `messages.sendMessage`,
    /// preserves UTF-16 entity offsets and `reply_to` across split chunks, resolves
    /// `updateMessageID` / `updateShortSentMessage` on success, and marks failed
    /// (or destroys on `MESSAGE_EMPTY`) on error.
    pub async fn send_message<I: Invoker>(
        &mut self,
        invoker: &I,
        peer: tl_enums::InputPeer,
        text: &str,
        entities: Vec<tl_enums::MessageEntity>,
        reply_to: Option<tl_enums::InputReplyTo>,
        options: SendOptions,
    ) -> Result<SendMessageOutcome, SendMessageErrorKind> {
        let peer_id = peer_id_from_input_peer(&peer, None);
        let prepared = self.prepare_text_chunks(peer_id, text, &entities);
        let mut out_messages = Vec::with_capacity(prepared.len());
        let mut out_updates = Vec::with_capacity(prepared.len());

        for chunk in &prepared {
            // Preserve `reply_to` on every split chunk (`apiwrap.cpp:4866-4870`).
            let (entry, batch) = self
                .send_prepared_chunk(invoker, peer.clone(), chunk, reply_to.clone(), &options)
                .await?;
            out_messages.push(entry);
            out_updates.push(batch);
        }

        Ok(SendMessageOutcome {
            messages: out_messages,
            updates: out_updates,
        })
    }

    /// Sends a single media message via `messages.sendMedia`
    /// (`api/api_sending.cpp:250-350`, `Api::SendExistingMedia`).
    #[allow(clippy::too_many_arguments)]
    pub async fn send_media<I: Invoker>(
        &mut self,
        invoker: &I,
        peer: tl_enums::InputPeer,
        media: tl_enums::InputMedia,
        caption: &str,
        entities: Vec<tl_enums::MessageEntity>,
        reply_to: Option<tl_enums::InputReplyTo>,
        options: SendOptions,
    ) -> Result<SendMediaOutcome, SendMessageErrorKind> {
        let peer_id = peer_id_from_input_peer(&peer, None);
        let local_msg_id = self.next_local_msg_id();
        let local_id = FullMsgId::new(peer_id, local_msg_id);
        let random_id = self.rng.next_random_id();

        let pending = PendingMessage {
            local_id,
            random_id,
            text: caption.to_owned(),
            state: PendingMessageState::Sending,
        };
        self.random_to_local.insert(random_id, local_id);
        self.pending.insert(local_id, pending);

        let req = tl_fns::messages::SendMedia {
            silent: options.silent,
            background: false,
            clear_draft: options.clear_draft,
            noforwards: options.noforwards,
            update_stickersets_order: false,
            invert_media: options.invert_media,
            allow_paid_floodskip: false,
            peer,
            reply_to,
            media,
            message: caption.to_owned(),
            random_id,
            reply_markup: None,
            entities: (!entities.is_empty()).then_some(entities),
            schedule_date: options.schedule_date,
            schedule_repeat_period: options.schedule_repeat_period,
            send_as: options.send_as,
            quick_reply_shortcut: None,
            effect: options.effect_id,
            allow_paid_stars: options.allow_paid_stars,
            suggested_post: None,
        };

        match invoker.messages_send_media(req).await {
            Ok(batch) => {
                self.apply_sent_updates(&batch, Some(random_id));
                let entry = self
                    .pending
                    .get(&local_id)
                    .cloned()
                    .unwrap_or(PendingMessage {
                        local_id,
                        random_id,
                        text: caption.to_owned(),
                        state: PendingMessageState::Sending,
                    });
                Ok(SendMediaOutcome {
                    message: entry,
                    updates: batch,
                })
            }
            Err(err) => {
                let mapped = map_send_message_error(&err);
                self.random_to_local.remove(&random_id);
                if let Some(entry) = self.pending.get_mut(&local_id) {
                    entry.state = if mapped == SendMessageErrorKind::MessageEmpty {
                        PendingMessageState::Destroyed
                    } else {
                        PendingMessageState::Failed {
                            error: mapped.clone(),
                        }
                    };
                }
                Err(mapped)
            }
        }
    }

    /// Sends an album of media items via `messages.sendMultiMedia`, assigning a unique
    /// 64-bit `random_id` per item and a shared local 64-bit `grouped_id`
    /// (`apiwrap.cpp:5565-5850`, `ApiWrap::sendAlbumIfReady`).
    pub async fn send_multi_media<I: Invoker>(
        &mut self,
        invoker: &I,
        peer: tl_enums::InputPeer,
        items: Vec<AlbumMediaItem>,
        reply_to: Option<tl_enums::InputReplyTo>,
        options: SendOptions,
    ) -> Result<SendMultiMediaOutcome, SendMessageErrorKind> {
        let peer_id = peer_id_from_input_peer(&peer, None);
        let grouped_id = self.rng.next_random_id() as u64;
        let mut multi_media = Vec::with_capacity(items.len());
        let mut tracked: Vec<(i64, FullMsgId)> = Vec::with_capacity(items.len());

        for item in items {
            let local_msg_id = self.next_local_msg_id();
            let local_id = FullMsgId::new(peer_id, local_msg_id);
            let random_id = self.rng.next_random_id();

            let pending = PendingMessage {
                local_id,
                random_id,
                text: item.caption.clone(),
                state: PendingMessageState::Sending,
            };
            self.random_to_local.insert(random_id, local_id);
            self.pending.insert(local_id, pending);
            tracked.push((random_id, local_id));

            multi_media.push(tl_enums::InputSingleMedia::InputSingleMedia(
                tl_types::InputSingleMedia {
                    media: item.media,
                    random_id,
                    message: item.caption,
                    entities: (!item.entities.is_empty()).then_some(item.entities),
                },
            ));
        }

        let req = tl_fns::messages::SendMultiMedia {
            silent: options.silent,
            background: false,
            clear_draft: options.clear_draft,
            noforwards: options.noforwards,
            update_stickersets_order: false,
            invert_media: options.invert_media,
            allow_paid_floodskip: false,
            peer,
            reply_to,
            multi_media,
            schedule_date: options.schedule_date,
            send_as: options.send_as,
            quick_reply_shortcut: None,
            effect: options.effect_id,
            allow_paid_stars: options.allow_paid_stars,
        };

        match invoker.messages_send_multi_media(req).await {
            Ok(batch) => {
                self.apply_sent_updates(&batch, None);
                let messages = tracked
                    .iter()
                    .filter_map(|(_, local_id)| self.pending.get(local_id).cloned())
                    .collect();
                Ok(SendMultiMediaOutcome {
                    grouped_id,
                    messages,
                    updates: batch,
                })
            }
            Err(err) => {
                let mapped = map_send_message_error(&err);
                for (rand_id, local_id) in tracked {
                    self.random_to_local.remove(&rand_id);
                    if let Some(entry) = self.pending.get_mut(&local_id) {
                        entry.state = if mapped == SendMessageErrorKind::MessageEmpty {
                            PendingMessageState::Destroyed
                        } else {
                            PendingMessageState::Failed {
                                error: mapped.clone(),
                            }
                        };
                    }
                }
                Err(mapped)
            }
        }
    }

    /// Edits an existing message (`Api::EditTextMessage` in `api/api_editing.cpp`).
    pub async fn edit_message<I: Invoker>(
        &self,
        invoker: &I,
        req: tl_fns::messages::EditMessage,
    ) -> Result<tl_enums::Updates, ApiError> {
        let updates = invoker.messages_edit_message(req).await?;
        Ok(updates)
    }

    /// Deletes specific messages (`Data::Histories::deleteMessages`).
    ///
    /// Routes channel peers to `channels.deleteMessages` and non-channels to
    /// `messages.deleteMessages` (passing `revoke`).
    pub async fn delete_messages<I: Invoker>(
        &self,
        invoker: &I,
        peer: tl_enums::InputPeer,
        ids: Vec<MsgId>,
        revoke: bool,
    ) -> Result<tl_types::messages::AffectedMessages, ApiError> {
        if ids.is_empty() {
            return Ok(tl_types::messages::AffectedMessages {
                pts: 0,
                pts_count: 0,
            });
        }
        let id: Vec<i32> = ids.into_iter().filter_map(MsgId::as_i32).collect();
        let affected = if let Some(channel) = input_peer_to_input_channel(&peer) {
            invoker
                .channels_delete_messages(tl_fns::channels::DeleteMessages { channel, id })
                .await?
        } else {
            invoker
                .messages_delete_messages(tl_fns::messages::DeleteMessages { revoke, id })
                .await?
        };
        Ok(affected)
    }

    /// Clears or deletes a peer's history (`Data::Histories::deleteAllMessages`).
    ///
    /// For channels calls `channels.deleteHistory`; for users/chats loops
    /// `messages.deleteHistory` while `offset > 0` up to `DELETE_HISTORY_RETRY_LIMIT`.
    pub async fn delete_history<I: Invoker>(
        &self,
        invoker: &I,
        req: tl_fns::messages::DeleteHistory,
    ) -> Result<DeleteHistoryOutcome, ApiError> {
        if let Some(channel) = input_peer_to_input_channel(&req.peer) {
            let updates = invoker
                .channels_delete_history(tl_fns::channels::DeleteHistory {
                    for_everyone: req.revoke,
                    channel,
                    max_id: req.max_id,
                })
                .await?;
            return Ok(DeleteHistoryOutcome::Channel(Box::new(updates)));
        }

        let mut pages = Vec::new();
        for _ in 0..DELETE_HISTORY_RETRY_LIMIT {
            let affected = invoker.messages_delete_history(req.clone()).await?;
            let more = affected.offset > 0;
            pages.push(affected);
            if !more {
                break;
            }
        }
        Ok(DeleteHistoryOutcome::Affected(pages))
    }

    /// Forwards messages, batching consecutive items sharing the same source peer
    /// (`ApiWrap::forwardMessages`).
    pub async fn forward_messages<I: Invoker>(
        &mut self,
        invoker: &I,
        items: &[ForwardItem],
        to_peer: tl_enums::InputPeer,
        top_msg_id: Option<MsgId>,
        forward_options: ForwardOptions,
        send_options: SendOptions,
    ) -> Result<ForwardOutcome, SendMessageErrorKind> {
        if items.is_empty() {
            return Ok(ForwardOutcome {
                local_message: None,
                updates: Vec::new(),
            });
        }

        let create_local = items.len() == 1 && forward_options == ForwardOptions::PreserveInfo;
        let to_peer_id = peer_id_from_input_peer(&to_peer, None);
        let mut local_message_id = None;
        let mut updates = Vec::new();

        let (drop_author, drop_media_captions) = match forward_options {
            ForwardOptions::PreserveInfo => (false, false),
            ForwardOptions::NoSenderNames => (true, false),
            ForwardOptions::NoNamesAndCaptions => (true, true),
        };

        let mut batch_start = 0;
        while batch_start < items.len() {
            let from_peer = &items[batch_start].from_peer;
            let mut batch_end = batch_start + 1;
            while batch_end < items.len() && items[batch_end].from_peer == *from_peer {
                batch_end += 1;
            }

            let slice = &items[batch_start..batch_end];
            let mut ids = Vec::with_capacity(slice.len());
            let mut random_ids = Vec::with_capacity(slice.len());
            let mut batch_local_pairs = Vec::new();

            for item in slice {
                let random_id = self.rng.next_random_id();
                ids.push(item.msg_id.as_i32().unwrap_or(0));
                random_ids.push(random_id);
                if create_local {
                    let local_id = FullMsgId::new(to_peer_id, self.next_local_msg_id());
                    let entry = PendingMessage {
                        local_id,
                        random_id,
                        text: String::new(),
                        state: PendingMessageState::Sending,
                    };
                    self.random_to_local.insert(random_id, local_id);
                    self.pending.insert(local_id, entry);
                    local_message_id = Some(local_id);
                    batch_local_pairs.push((random_id, local_id));
                }
            }

            // Match `ApiWrap::forwardMessages` (`apiwrap.cpp:4035-4047`):
            // omit `top_msg_id` for the General topic (`kGeneralId = MsgId(1)`) and pass
            // `send_options.noforwards`.
            let effective_top_msg_id = top_msg_id
                .filter(|&id| id != MsgId(1))
                .and_then(MsgId::as_i32);
            let req = tl_fns::messages::ForwardMessages {
                silent: send_options.silent,
                background: false,
                with_my_score: false,
                drop_author,
                drop_media_captions,
                noforwards: send_options.noforwards,
                allow_paid_floodskip: false,
                from_ephemeral: false,
                from_peer: from_peer.clone(),
                id: ids,
                random_id: random_ids,
                to_peer: to_peer.clone(),
                top_msg_id: effective_top_msg_id,
                reply_to: None,
                schedule_date: send_options.schedule_date,
                schedule_repeat_period: send_options.schedule_repeat_period,
                send_as: send_options.send_as.clone(),
                quick_reply_shortcut: None,
                effect: send_options.effect_id,
                video_timestamp: None,
                allow_paid_stars: send_options.allow_paid_stars,
                suggested_post: None,
            };

            match invoker.messages_forward_messages(req).await {
                Ok(batch) => {
                    self.apply_sent_updates(&batch, None);
                    updates.push(batch);
                }
                Err(err) => {
                    let mapped = map_send_message_error(&err);
                    for (rand_id, local_id) in batch_local_pairs {
                        self.random_to_local.remove(&rand_id);
                        if let Some(entry) = self.pending.get_mut(&local_id) {
                            entry.state = PendingMessageState::Failed {
                                error: mapped.clone(),
                            };
                        }
                    }
                    return Err(mapped);
                }
            }

            batch_start = batch_end;
        }

        let local_message = local_message_id.and_then(|id| self.pending.get(&id).cloned());
        Ok(ForwardOutcome {
            local_message,
            updates,
        })
    }
}

/// Returns `(offset, length)` in UTF-16 code units for any TL `MessageEntity`.
const fn tl_entity_span(entity: &tl_enums::MessageEntity) -> (i32, i32) {
    match entity {
        tl_enums::MessageEntity::Unknown(e) => (e.offset, e.length),
        tl_enums::MessageEntity::Mention(e) => (e.offset, e.length),
        tl_enums::MessageEntity::Hashtag(e) => (e.offset, e.length),
        tl_enums::MessageEntity::BotCommand(e) => (e.offset, e.length),
        tl_enums::MessageEntity::Url(e) => (e.offset, e.length),
        tl_enums::MessageEntity::Email(e) => (e.offset, e.length),
        tl_enums::MessageEntity::Bold(e) => (e.offset, e.length),
        tl_enums::MessageEntity::Italic(e) => (e.offset, e.length),
        tl_enums::MessageEntity::Code(e) => (e.offset, e.length),
        tl_enums::MessageEntity::Pre(e) => (e.offset, e.length),
        tl_enums::MessageEntity::TextUrl(e) => (e.offset, e.length),
        tl_enums::MessageEntity::MentionName(e) => (e.offset, e.length),
        tl_enums::MessageEntity::InputMessageEntityMentionName(e) => (e.offset, e.length),
        tl_enums::MessageEntity::Phone(e) => (e.offset, e.length),
        tl_enums::MessageEntity::Cashtag(e) => (e.offset, e.length),
        tl_enums::MessageEntity::Underline(e) => (e.offset, e.length),
        tl_enums::MessageEntity::Strike(e) => (e.offset, e.length),
        tl_enums::MessageEntity::BankCard(e) => (e.offset, e.length),
        tl_enums::MessageEntity::Spoiler(e) => (e.offset, e.length),
        tl_enums::MessageEntity::CustomEmoji(e) => (e.offset, e.length),
        tl_enums::MessageEntity::Blockquote(e) => (e.offset, e.length),
        tl_enums::MessageEntity::FormattedDate(e) => (e.offset, e.length),
        tl_enums::MessageEntity::DiffInsert(e) => (e.offset, e.length),
        tl_enums::MessageEntity::DiffReplace(e) => (e.offset, e.length),
        tl_enums::MessageEntity::DiffDelete(e) => (e.offset, e.length),
        tl_enums::MessageEntity::TonAddress(e) => (e.offset, e.length),
    }
}

/// Clones `entity` with updated UTF-16 `offset` and `length`.
fn tl_entity_with_span(
    entity: &tl_enums::MessageEntity,
    offset: i32,
    length: i32,
) -> tl_enums::MessageEntity {
    let mut cloned = entity.clone();
    match &mut cloned {
        tl_enums::MessageEntity::Unknown(e) => {
            e.offset = offset;
            e.length = length;
        }
        tl_enums::MessageEntity::Mention(e) => {
            e.offset = offset;
            e.length = length;
        }
        tl_enums::MessageEntity::Hashtag(e) => {
            e.offset = offset;
            e.length = length;
        }
        tl_enums::MessageEntity::BotCommand(e) => {
            e.offset = offset;
            e.length = length;
        }
        tl_enums::MessageEntity::Url(e) => {
            e.offset = offset;
            e.length = length;
        }
        tl_enums::MessageEntity::Email(e) => {
            e.offset = offset;
            e.length = length;
        }
        tl_enums::MessageEntity::Bold(e) => {
            e.offset = offset;
            e.length = length;
        }
        tl_enums::MessageEntity::Italic(e) => {
            e.offset = offset;
            e.length = length;
        }
        tl_enums::MessageEntity::Code(e) => {
            e.offset = offset;
            e.length = length;
        }
        tl_enums::MessageEntity::Pre(e) => {
            e.offset = offset;
            e.length = length;
        }
        tl_enums::MessageEntity::TextUrl(e) => {
            e.offset = offset;
            e.length = length;
        }
        tl_enums::MessageEntity::MentionName(e) => {
            e.offset = offset;
            e.length = length;
        }
        tl_enums::MessageEntity::InputMessageEntityMentionName(e) => {
            e.offset = offset;
            e.length = length;
        }
        tl_enums::MessageEntity::Phone(e) => {
            e.offset = offset;
            e.length = length;
        }
        tl_enums::MessageEntity::Cashtag(e) => {
            e.offset = offset;
            e.length = length;
        }
        tl_enums::MessageEntity::Underline(e) => {
            e.offset = offset;
            e.length = length;
        }
        tl_enums::MessageEntity::Strike(e) => {
            e.offset = offset;
            e.length = length;
        }
        tl_enums::MessageEntity::BankCard(e) => {
            e.offset = offset;
            e.length = length;
        }
        tl_enums::MessageEntity::Spoiler(e) => {
            e.offset = offset;
            e.length = length;
        }
        tl_enums::MessageEntity::CustomEmoji(e) => {
            e.offset = offset;
            e.length = length;
        }
        tl_enums::MessageEntity::Blockquote(e) => {
            e.offset = offset;
            e.length = length;
        }
        tl_enums::MessageEntity::FormattedDate(e) => {
            e.offset = offset;
            e.length = length;
        }
        tl_enums::MessageEntity::DiffInsert(e) => {
            e.offset = offset;
            e.length = length;
        }
        tl_enums::MessageEntity::DiffReplace(e) => {
            e.offset = offset;
            e.length = length;
        }
        tl_enums::MessageEntity::DiffDelete(e) => {
            e.offset = offset;
            e.length = length;
        }
        tl_enums::MessageEntity::TonAddress(e) => {
            e.offset = offset;
            e.length = length;
        }
    }
    cloned
}

/// Slices and shifts `entities` (in UTF-16 code units) for the subrange `[start_u16, end_u16)`.
fn slice_tl_entities(
    entities: &[tl_enums::MessageEntity],
    start_u16: i32,
    end_u16: i32,
) -> Vec<tl_enums::MessageEntity> {
    let mut out = Vec::new();
    for entity in entities {
        let (off, len) = tl_entity_span(entity);
        if len <= 0 {
            continue;
        }
        let ent_end = off.saturating_add(len);
        let clipped_start = off.max(start_u16);
        let clipped_end = ent_end.min(end_u16);
        if clipped_end > clipped_start {
            out.push(tl_entity_with_span(
                entity,
                clipped_start - start_u16,
                clipped_end - clipped_start,
            ));
        }
    }
    out
}

/// Splits `text` and its UTF-16 `entities` into trimmed chunks of at most `max_chars` Unicode
/// scalar values, adjusting entity offsets and lengths for every chunk (`apiwrap.cpp:4842-4918`,
/// `ApiWrap::sendMessage` + `TextUtilities::CutPart`).
fn split_message_chunks_with_entities(
    text: &str,
    entities: &[tl_enums::MessageEntity],
    max_chars: usize,
) -> Vec<(String, Vec<tl_enums::MessageEntity>)> {
    let lead_bytes = text
        .char_indices()
        .find(|(_, c)| !c.is_whitespace())
        .map_or(text.len(), |(idx, _)| idx);
    if lead_bytes == text.len() {
        return vec![(String::new(), Vec::new())];
    }
    let trail_bytes = text
        .char_indices()
        .rev()
        .find(|(_, c)| !c.is_whitespace())
        .map_or(lead_bytes, |(idx, c)| idx + c.len_utf8());

    let lead_u16 = text[..lead_bytes].encode_utf16().count() as i32;
    let trimmed = &text[lead_bytes..trail_bytes];
    let trimmed_u16 = trimmed.encode_utf16().count() as i32;
    let base_entities = slice_tl_entities(entities, lead_u16, lead_u16 + trimmed_u16);

    let limit = max_chars.max(1);
    let mut chunks = Vec::new();
    let mut chunk_start_byte = 0usize;
    let mut chunk_start_u16 = 0i32;
    let mut cur_u16 = 0i32;
    let mut char_count = 0usize;

    let push_slice =
        |slice: &str,
         slice_base_u16: i32,
         chunks: &mut Vec<(String, Vec<tl_enums::MessageEntity>)>| {
            let inner_lead = slice
                .char_indices()
                .find(|(_, c)| !c.is_whitespace())
                .map_or(slice.len(), |(idx, _)| idx);
            if inner_lead == slice.len() {
                return;
            }
            let inner_trail = slice
                .char_indices()
                .rev()
                .find(|(_, c)| !c.is_whitespace())
                .map_or(inner_lead, |(idx, c)| idx + c.len_utf8());
            let inner_lead_u16 = slice[..inner_lead].encode_utf16().count() as i32;
            let piece = &slice[inner_lead..inner_trail];
            let piece_u16 = piece.encode_utf16().count() as i32;
            let piece_start_u16 = slice_base_u16 + inner_lead_u16;
            let piece_entities =
                slice_tl_entities(&base_entities, piece_start_u16, piece_start_u16 + piece_u16);
            chunks.push((piece.to_owned(), piece_entities));
        };

    for (byte_idx, ch) in trimmed.char_indices() {
        cur_u16 += ch.len_utf16() as i32;
        char_count += 1;
        if char_count == limit {
            let end_byte = byte_idx + ch.len_utf8();
            push_slice(
                &trimmed[chunk_start_byte..end_byte],
                chunk_start_u16,
                &mut chunks,
            );
            chunk_start_byte = end_byte;
            chunk_start_u16 = cur_u16;
            char_count = 0;
        }
    }
    if chunk_start_byte < trimmed.len() {
        push_slice(&trimmed[chunk_start_byte..], chunk_start_u16, &mut chunks);
    }
    if chunks.is_empty() {
        chunks.push((String::new(), Vec::new()));
    }
    chunks
}

// ---------------------------------------------------------------------------
// Inbox Read History Throttling (`Data::Histories::readInboxTill`)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct ReadPeerState {
    peer: tl_enums::InputPeer,
    will_read_till: MsgId,
    sent_read_till: MsgId,
    will_read_when: Option<Instant>,
}

/// Result of flushing a single peer's inbox read request.
#[derive(Debug, Clone, PartialEq)]
pub struct FlushedRead {
    /// Peer whose inbox was marked read.
    pub peer_id: PeerId,
    /// `max_id` sent to server.
    pub max_id: MsgId,
    /// `AffectedMessages` for non-channels (`None` for channels).
    pub affected: Option<tl_types::messages::AffectedMessages>,
}

/// Throttled inbox read manager matching `Data::Histories::readInboxTill` and
/// `sendReadRequests` (`kReadRequestTimeout = 3000ms`).
#[derive(Debug, Default)]
pub struct ReadHistoryManager {
    states: HashMap<PeerId, ReadPeerState>,
}

impl ReadHistoryManager {
    /// Creates a new `ReadHistoryManager`.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Schedules an inbox read up to `till_id` for `peer`.
    ///
    /// Returns `true` if a request is due immediately (`flush_due` should be called).
    pub fn read_inbox_till(
        &mut self,
        peer: tl_enums::InputPeer,
        till_id: MsgId,
        still_unread: Option<i32>,
        force: bool,
        now: Instant,
    ) -> bool {
        if !till_id.is_server() {
            return false;
        }
        let peer_id = peer_id_from_input_peer(&peer, None);
        let entry = self.states.entry(peer_id).or_insert_with(|| ReadPeerState {
            peer: peer.clone(),
            will_read_till: MsgId::ZERO,
            sent_read_till: MsgId::ZERO,
            will_read_when: None,
        });
        entry.peer = peer;

        if entry.sent_read_till >= till_id {
            return false;
        }
        if entry.will_read_till >= till_id {
            if force {
                entry.will_read_when = Some(now);
                return true;
            }
            return false;
        }

        entry.will_read_till = till_id;
        let immediate = force || still_unread.is_none() || still_unread == Some(0);
        if immediate {
            entry.will_read_when = Some(now);
            true
        } else {
            if entry.will_read_when.is_none() {
                entry.will_read_when = Some(now + READ_REQUEST_TIMEOUT);
            }
            false
        }
    }

    /// Returns the earliest scheduled read deadline across all peers, if any.
    #[must_use]
    pub fn next_deadline(&self) -> Option<Instant> {
        self.states
            .values()
            .filter(|s| s.will_read_till.0 > 0)
            .filter_map(|s| s.will_read_when)
            .min()
    }

    /// Flushes all pending inbox read requests whose deadline is `<= now`.
    ///
    /// Restores `sent_read_till` on RPC failure so failed reads are retriable
    /// (`data/data_histories.cpp:763-770`, `Histories::sendReadRequests`).
    pub async fn flush_due<I: Invoker>(
        &mut self,
        invoker: &I,
        now: Instant,
    ) -> Result<Vec<FlushedRead>, ApiError> {
        let mut flushed = Vec::new();
        for due in self.take_due(now) {
            match due.send(invoker).await {
                Ok(read) => flushed.push(read),
                Err(err) => {
                    self.restore_failed(&due);
                    return Err(ApiError::Invocation(err));
                }
            }
        }
        Ok(flushed)
    }

    /// The bookkeeping half of [`Self::flush_due`]: marks every read due at `now` as sent and
    /// returns the requests to send. A failed request goes back through [`Self::restore_failed`].
    pub fn take_due(&mut self, now: Instant) -> Vec<DueRead> {
        let mut due = Vec::new();
        for (&peer_id, state) in &mut self.states {
            if state.will_read_till > state.sent_read_till
                && let Some(when) = state.will_read_when
                && when <= now
            {
                let prev_sent = state.sent_read_till;
                let till = state.will_read_till;
                state.sent_read_till = till;
                state.will_read_till = MsgId::ZERO;
                state.will_read_when = None;
                due.push(DueRead {
                    peer_id,
                    peer: state.peer.clone(),
                    max_id: till,
                    prev_sent,
                });
            }
        }
        due
    }

    /// Restores `sent_read_till` after `due` failed so the read is retried
    /// (`data/data_histories.cpp:763-770`, `Histories::sendReadRequests`).
    pub fn restore_failed(&mut self, due: &DueRead) {
        if let Some(state) = self.states.get_mut(&due.peer_id)
            && state.sent_read_till == due.max_id
        {
            state.sent_read_till = due.prev_sent;
            if state.will_read_till < due.max_id {
                state.will_read_till = due.max_id;
            }
        }
    }
}

/// One inbox read request taken by [`ReadHistoryManager::take_due`].
#[derive(Debug, Clone, PartialEq)]
pub struct DueRead {
    /// Peer whose inbox is read.
    pub peer_id: PeerId,
    /// Input peer for the request.
    pub peer: tl_enums::InputPeer,
    /// `max_id` to send.
    pub max_id: MsgId,
    /// `sent_read_till` before this request, restored on failure.
    pub prev_sent: MsgId,
}

impl DueRead {
    /// Sends `messages.readHistory` (or `channels.readHistory` for channels).
    pub async fn send<I: Invoker>(&self, invoker: &I) -> Result<FlushedRead, InvocationError> {
        let max_id = self.max_id.as_i32().unwrap_or(0);
        let affected = if let Some(channel) = input_peer_to_input_channel(&self.peer) {
            invoker
                .channels_read_history(tl_fns::channels::ReadHistory { channel, max_id })
                .await
                .map(|_| None)?
        } else {
            invoker
                .messages_read_history(tl_fns::messages::ReadHistory {
                    peer: self.peer.clone(),
                    max_id,
                })
                .await
                .map(Some)?
        };
        Ok(FlushedRead {
            peer_id: self.peer_id,
            max_id: self.max_id,
            affected,
        })
    }
}

// ---------------------------------------------------------------------------
// Typing / Send Progress Manager (`Api::SendProgressManager`)
// ---------------------------------------------------------------------------

/// Metadata about a target peer used to decide whether typing notifications should be sent
/// (`SendProgressManager::update` and `skipRequest`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TypingPeerInfo {
    /// Target peer kind/ID.
    pub peer_id: PeerId,
    /// `true` if Saved Messages (self).
    pub is_self: bool,
    /// `true` if a broadcast channel (not a megagroup).
    pub is_broadcast_channel: bool,
    /// `true` if a bot user.
    pub is_bot: bool,
    /// `true` if official Telegram support bot.
    pub is_support: bool,
    /// `true` if user has `UserStatusRecently`.
    pub lastseen_recently: bool,
    /// Unix timestamp until which the user is considered online (0 if unknown/hidden).
    pub online_till: TimeId,
}

impl TypingPeerInfo {
    /// Helper for an online non-bot user.
    #[must_use]
    pub const fn online_user(peer_id: PeerId, online_till: TimeId) -> Self {
        Self {
            peer_id,
            is_self: false,
            is_broadcast_channel: false,
            is_bot: false,
            is_support: false,
            lastseen_recently: false,
            online_till,
        }
    }

    /// Helper for a group or megagroup chat.
    #[must_use]
    pub const fn group(peer_id: PeerId) -> Self {
        Self {
            peer_id,
            is_self: false,
            is_broadcast_channel: false,
            is_bot: false,
            is_support: false,
            lastseen_recently: true,
            online_till: 0,
        }
    }
}

/// Discriminant for throttling typing/upload progress keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SendProgressKind {
    /// Typing text.
    Typing,
    /// Recording video.
    RecordVideo,
    /// Uploading video.
    UploadVideo,
    /// Recording voice message.
    RecordVoice,
    /// Uploading voice message.
    UploadVoice,
    /// Recording round video.
    RecordRound,
    /// Uploading round video.
    UploadRound,
    /// Uploading photo.
    UploadPhoto,
    /// Uploading file/document.
    UploadFile,
    /// Choosing geo location.
    ChooseLocation,
    /// Choosing contact.
    ChooseContact,
    /// Playing game.
    PlayGame,
    /// Speaking in group call.
    Speaking,
    /// Choosing sticker.
    ChooseSticker,
}

impl SendProgressKind {
    fn to_tl_action(self, progress: i32) -> tl_enums::SendMessageAction {
        match self {
            Self::Typing => tl_enums::SendMessageAction::SendMessageTypingAction,
            Self::RecordVideo => tl_enums::SendMessageAction::SendMessageRecordVideoAction,
            Self::UploadVideo => tl_enums::SendMessageAction::SendMessageUploadVideoAction(
                tl_types::SendMessageUploadVideoAction { progress },
            ),
            Self::RecordVoice => tl_enums::SendMessageAction::SendMessageRecordAudioAction,
            Self::UploadVoice => tl_enums::SendMessageAction::SendMessageUploadAudioAction(
                tl_types::SendMessageUploadAudioAction { progress },
            ),
            Self::RecordRound => tl_enums::SendMessageAction::SendMessageRecordRoundAction,
            Self::UploadRound => tl_enums::SendMessageAction::SendMessageUploadRoundAction(
                tl_types::SendMessageUploadRoundAction { progress },
            ),
            Self::UploadPhoto => tl_enums::SendMessageAction::SendMessageUploadPhotoAction(
                tl_types::SendMessageUploadPhotoAction { progress },
            ),
            Self::UploadFile => tl_enums::SendMessageAction::SendMessageUploadDocumentAction(
                tl_types::SendMessageUploadDocumentAction { progress },
            ),
            Self::ChooseLocation => tl_enums::SendMessageAction::SendMessageGeoLocationAction,
            Self::ChooseContact => tl_enums::SendMessageAction::SendMessageChooseContactAction,
            Self::PlayGame => tl_enums::SendMessageAction::SendMessageGamePlayAction,
            Self::Speaking => tl_enums::SendMessageAction::SpeakingInGroupCallAction,
            Self::ChooseSticker => tl_enums::SendMessageAction::SendMessageChooseStickerAction,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct ProgressKey {
    peer_id: PeerId,
    top_msg_id: Option<MsgId>,
    kind: SendProgressKind,
}

/// Parameters for a single `SendProgressManager::update` call.
#[derive(Debug, Clone, PartialEq)]
pub struct SendProgressUpdate {
    /// Target conversation input peer.
    pub peer: tl_enums::InputPeer,
    /// Eligibility metadata for the target peer.
    pub info: TypingPeerInfo,
    /// Optional forum topic root ID.
    pub top_msg_id: Option<MsgId>,
    /// Typing/upload action kind.
    pub kind: SendProgressKind,
    /// `>= 0` to send/refresh (`0` for typing, `0..=100` for uploads), `< 0` to cancel.
    pub progress: i32,
}

/// Throttled typing and upload-progress manager (`Api::SendProgressManager`).
#[derive(Debug, Default)]
pub struct SendProgressManager {
    updated_until: HashMap<ProgressKey, Instant>,
    stop_typing_deadline: Option<(PeerId, Instant)>,
}

impl SendProgressManager {
    /// Creates a new `SendProgressManager`.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Updates a typing/upload action (`SendProgressManager::update`).
    ///
    /// Returns `Ok(true)` if `messages.setTyping` was actually sent over the network,
    /// or `Ok(false)` if skipped due to throttling or peer eligibility rules.
    pub async fn update<I: Invoker>(
        &mut self,
        invoker: &I,
        update: SendProgressUpdate,
        now: Instant,
        now_unix: TimeId,
    ) -> Result<bool, ApiError> {
        let (peer_id, kind) = (update.info.peer_id, update.kind);
        let Some(req) = self.prepare(update, now, now_unix) else {
            return Ok(false);
        };
        invoker.messages_set_typing(req).await?;
        self.finish_sent(peer_id, kind, now);
        Ok(true)
    }

    /// The decision half of [`Self::update`]: applies the throttle and peer rules and returns
    /// the `messages.setTyping` request to send, or `None` when nothing is sent. After a
    /// successful request the caller reports it with [`Self::finish_sent`].
    pub fn prepare(
        &mut self,
        update: SendProgressUpdate,
        now: Instant,
        now_unix: TimeId,
    ) -> Option<tl_fns::messages::SetTyping> {
        let SendProgressUpdate {
            peer,
            info,
            top_msg_id,
            kind,
            progress,
        } = update;
        if info.is_self || (info.is_broadcast_channel && kind != SendProgressKind::Speaking) {
            return None;
        }
        let doing = progress >= 0;
        let key = ProgressKey {
            peer_id: info.peer_id,
            top_msg_id,
            kind,
        };
        if !self.check_throttle(key, doing, now) || !doing || Self::should_skip_peer(info, now_unix)
        {
            return None;
        }
        Some(tl_fns::messages::SetTyping {
            peer,
            top_msg_id: top_msg_id.and_then(MsgId::as_i32),
            action: kind.to_tl_action(progress),
        })
    }

    /// Records a successful `messages.setTyping` prepared by [`Self::prepare`] at `now`.
    pub fn finish_sent(&mut self, peer_id: PeerId, kind: SendProgressKind, now: Instant) {
        if kind == SendProgressKind::Typing {
            self.stop_typing_deadline = Some((peer_id, now + CANCEL_TYPING_ACTION_TIMEOUT));
        }
    }

    /// Explicitly cancels active typing state for `peer_id` (`SendProgressManager::cancelTyping`).
    pub fn cancel_typing(&mut self, peer_id: PeerId, top_msg_id: Option<MsgId>) {
        self.updated_until.remove(&ProgressKey {
            peer_id,
            top_msg_id,
            kind: SendProgressKind::Typing,
        });
        if let Some((active_peer, _)) = self.stop_typing_deadline
            && active_peer == peer_id
        {
            self.stop_typing_deadline = None;
        }
    }

    /// Returns the active auto-cancel typing deadline, if any (`5000ms` after last typing send).
    #[must_use]
    pub const fn typing_cancel_deadline(&self) -> Option<(PeerId, Instant)> {
        self.stop_typing_deadline
    }

    fn check_throttle(&mut self, key: ProgressKey, doing: bool, now: Instant) -> bool {
        if doing {
            let send_each = if key.kind == SendProgressKind::Speaking {
                SEND_MY_SPEAKING_INTERVAL
            } else {
                SEND_MY_TYPING_INTERVAL
            };
            match self.updated_until.get_mut(&key) {
                None => {
                    self.updated_until.insert(key, now + send_each * 2);
                    true
                }
                Some(stored) if *stored > now + send_each => false,
                Some(stored) => {
                    *stored = now + send_each * 2;
                    true
                }
            }
        } else {
            match self.updated_until.get(&key) {
                None => false,
                Some(&stored) if stored <= now => false,
                Some(_) => {
                    self.updated_until.remove(&key);
                    true
                }
            }
        }
    }

    fn should_skip_peer(info: TypingPeerInfo, now_unix: TimeId) -> bool {
        if !info.peer_id.is_user() || info.peer_id.is_zero() {
            return false;
        }
        if info.is_self {
            return true;
        }
        if info.is_bot && !info.is_support {
            return true;
        }
        if info.lastseen_recently {
            return false;
        }
        let recently = now_unix.saturating_sub(SEND_TYPINGS_TO_OFFLINE_FOR_SECS);
        if info.online_till > 0 {
            info.online_till < recently
        } else {
            true
        }
    }
}

// ---------------------------------------------------------------------------
// Cloud Draft Save Manager (`ApiWrap::saveDraftToCloudDelayed`)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct DraftSlotKey {
    peer_id: PeerId,
    topic_root_id: Option<MsgId>,
}

#[derive(Debug, Clone)]
struct DraftEntry {
    request: tl_fns::messages::SaveDraft,
    save_at: Instant,
    in_flight: bool,
    changed_while_saving: bool,
}

/// Debounced cloud draft saver (`kSaveCloudDraftTimeout = 1000ms`) with quit prevention.
#[derive(Debug, Default)]
pub struct DraftManager {
    entries: HashMap<DraftSlotKey, DraftEntry>,
}

impl DraftManager {
    /// Creates a new `DraftManager`.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Schedules a cloud draft save with tdesktop's 1-second delay (`saveDraftToCloudDelayed`).
    ///
    /// If a save is already in flight for this peer/topic, marks `changed_while_saving`
    /// so the latest draft is automatically re-scheduled upon completion.
    pub fn schedule_save(
        &mut self,
        topic_root_id: Option<MsgId>,
        request: tl_fns::messages::SaveDraft,
        now: Instant,
    ) {
        let key = DraftSlotKey {
            peer_id: peer_id_from_input_peer(&request.peer, None),
            topic_root_id,
        };
        match self.entries.get_mut(&key) {
            Some(entry) => {
                entry.request = request;
                if entry.in_flight {
                    entry.changed_while_saving = true;
                } else {
                    entry.save_at = now + SAVE_CLOUD_DRAFT_TIMEOUT;
                }
            }
            None => {
                self.entries.insert(
                    key,
                    DraftEntry {
                        request,
                        save_at: now + SAVE_CLOUD_DRAFT_TIMEOUT,
                        in_flight: false,
                        changed_while_saving: false,
                    },
                );
            }
        }
    }

    /// Returns `true` if there are unsaved or in-flight drafts blocking app quit
    /// (`ApiWrap::isQuitPrevent`).
    #[must_use]
    pub fn is_quit_prevent(&self) -> bool {
        !self.entries.is_empty()
    }

    /// Returns the earliest pending draft save deadline.
    #[must_use]
    pub fn next_deadline(&self) -> Option<Instant> {
        self.entries
            .values()
            .filter(|e| !e.in_flight)
            .map(|e| e.save_at)
            .min()
    }

    /// Flushes all pending drafts whose debounce timer has expired (`<= now`).
    pub async fn flush_due<I: Invoker>(
        &mut self,
        invoker: &I,
        now: Instant,
    ) -> Result<usize, ApiError> {
        self.flush_matching(invoker, now, false).await
    }

    /// Immediately flushes all pending drafts regardless of deadline (used on quit / offline).
    pub async fn flush_all<I: Invoker>(
        &mut self,
        invoker: &I,
        now: Instant,
    ) -> Result<usize, ApiError> {
        self.flush_matching(invoker, now, true).await
    }

    async fn flush_matching<I: Invoker>(
        &mut self,
        invoker: &I,
        now: Instant,
        force_all: bool,
    ) -> Result<usize, ApiError> {
        let mut to_send = Vec::new();
        for (&key, entry) in &mut self.entries {
            if !entry.in_flight && (force_all || entry.save_at <= now) {
                entry.in_flight = true;
                to_send.push((key, entry.request.clone()));
            }
        }

        let mut saved_count = 0;
        for (key, request) in to_send {
            let res = invoker.messages_save_draft(request).await;
            if let Some(entry) = self.entries.get_mut(&key) {
                entry.in_flight = false;
                if entry.changed_while_saving {
                    entry.changed_while_saving = false;
                    entry.save_at = now + SAVE_CLOUD_DRAFT_TIMEOUT;
                } else {
                    self.entries.remove(&key);
                }
            }
            res?;
            saved_count += 1;
        }
        Ok(saved_count)
    }
}
