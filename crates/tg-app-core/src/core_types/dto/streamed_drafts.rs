//! Pure Layer-233 streamed bot draft state machine (`HistoryStreamedDrafts`).

use std::collections::{BTreeMap, HashSet};

use tg_iv::rich_page::flatten_rich_page_summary;
use tg_text::TextWithEntities;
use tg_tl::{enums as tl_enums, types as tl_types};

use crate::core_types::dto::rich_message::{RichMessagePayload, payload_from_tl_enum};
use crate::history::backend::{HistoryMessage, MessageExtra};
use crate::stand_in::types::{MsgId, PeerId};

/// Timeout before an un-updated streamed draft is automatically cleared (`kClearTimeout = 30s`).
pub const STREAMED_DRAFT_CLEAR_TIMEOUT_MS: u64 = 30_000;

/// Synthetic root topic id for the General topic / main timeline (`Data::ForumTopic::kGeneralId = 1`).
pub const GENERAL_TOPIC_ROOT_ID: i32 = 1;

/// Height of the floating "Stop generating" bar rendered above the composer while a stoppable
/// bot draft is actively streaming.
pub const STOP_STREAMED_DRAFT_BAR_HEIGHT: f32 = 34.0;

/// Streamed draft payload kind (`HistoryStreamedDrafts::DraftKind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamedDraftKind {
    /// Plain/formatted text streamed via `sendMessageTextDraftAction`.
    Text,
    /// Structured Layer-233 rich document streamed via `sendMessageRichMessageDraftAction`.
    Rich,
}

/// Active streamed bot draft stored in a peer's history (`HistoryStreamedDrafts::Draft`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamedDraft {
    /// Synthetic negative local message id allocated for rendering in the message list.
    pub local_msg_id: MsgId,
    /// Protocol `random_id` identifying this draft stream.
    pub random_id: i64,
    /// Peer history owning this draft.
    pub peer_id: PeerId,
    /// Normalized thread root id (defaults to [`GENERAL_TOPIC_ROOT_ID`]).
    pub root_id: MsgId,
    /// Raw `top_msg_id` to echo back in `messages.setTyping(sendMessageStopDraftAction)`.
    pub top_msg_id: Option<MsgId>,
    /// Sender peer id (usually the bot user id).
    pub from_id: PeerId,
    /// Unix timestamp (seconds) from the first draft update.
    pub date_unix: i32,
    /// Monotonic local update timestamp in milliseconds.
    pub updated_ms: u64,
    /// Text vs Rich draft kind.
    pub kind: StreamedDraftKind,
    /// Whether the user can request stopping generation (`can_stop`).
    pub can_stop: bool,
    /// Whether the draft bubble remains visible after stopping (`keep_on_stop`).
    pub keep_on_stop: bool,
    /// Plain-text match key (without trailing loading indicator) used for `adopt_incoming`.
    pub match_text: String,
    /// Rendered text (including streaming indicator when appropriate).
    pub text: TextWithEntities,
    /// Optional Layer-233 rich message payload.
    pub rich_message: Option<RichMessagePayload>,
}

/// Result returned when an incoming server message matches a streamed draft
/// (`HistoryStreamedDrafts::adoptIncoming`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdoptedStreamedDraft {
    /// Protocol `random_id` of the matched draft.
    pub random_id: i64,
    /// Previous synthetic local message id that was replaced or cleared.
    pub local_msg_id: MsgId,
    /// Final server message id from the incoming `MTPDmessage`.
    pub real_msg_id: MsgId,
    /// `true` when `can_adopt_in_place` succeeded (smooth transition); `false` if the draft was
    /// simply cleared because the final message cannot be adopted in-place.
    pub adopted_in_place: bool,
}

/// Extracted primary and secondary thread keys from an incoming `MTPDmessage` (`ThreadRootIds`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThreadKeys {
    /// Primary thread root id (`reply_to_top_id`, or `reply_to_msg_id` if `forum_topic`, else `1`).
    pub primary: i32,
    /// Secondary fallback key (`reply_to_msg_id` when not cross-peer/scheduled/ephemeral and != primary).
    pub secondary: i32,
}

/// Computes the common Unicode scalar prefix length between `a` and `b` (`CommonPrefixLength`).
#[must_use]
pub fn common_prefix_len(a: &str, b: &str) -> usize {
    a.chars()
        .zip(b.chars())
        .take_while(|(ca, cb)| ca == cb)
        .count()
}

/// Normalizes a raw `top_msg_id` into a non-zero thread root id (`GENERAL_TOPIC_ROOT_ID` for 0/None).
#[must_use]
pub const fn normalize_root_id(root_id: Option<MsgId>) -> MsgId {
    match root_id {
        Some(id) if id.0 != 0 => id,
        _ => MsgId(GENERAL_TOPIC_ROOT_ID),
    }
}

/// Extracts `(primary, secondary)` thread root keys from a TL `Message` (`ThreadRootIds`).
#[must_use]
pub fn thread_root_ids_from_tl(msg: &tl_types::Message) -> ThreadKeys {
    let mut primary = 0i32;
    let mut secondary = 0i32;
    if let Some(tl_enums::MessageReplyHeader::MessageReplyHeader(h)) = msg.reply_to.as_ref() {
        let reply_to_msg_id = h.reply_to_msg_id.unwrap_or(0);
        primary = h.reply_to_top_id.unwrap_or(0);
        if h.reply_to_peer_id.is_none() && !h.reply_to_scheduled && !h.reply_to_ephemeral {
            secondary = reply_to_msg_id;
        }
        if primary == 0 && h.forum_topic {
            primary = reply_to_msg_id;
        }
    }
    if primary == 0 {
        primary = GENERAL_TOPIC_ROOT_ID;
    }
    if secondary == primary {
        secondary = 0;
    }
    ThreadKeys { primary, secondary }
}

/// Returns `true` if `msg` can adopt an existing streamed draft bubble in-place (`CanAdoptInPlace`).
#[must_use]
pub fn can_adopt_in_place(msg: &tl_types::Message) -> bool {
    if msg.post
        || msg.fwd_from.is_some()
        || msg.grouped_id.is_some()
        || msg.saved_peer_id.is_some()
        || msg.suggested_post.is_some()
    {
        return false;
    }
    match msg.media.as_ref() {
        None | Some(tl_enums::MessageMedia::Empty) => true,
        Some(tl_enums::MessageMedia::Photo(p)) => p.ttl_seconds.is_none(),
        Some(tl_enums::MessageMedia::Document(d)) => d.ttl_seconds.is_none(),
        Some(_) => true,
    }
}

/// Per-peer manager of Layer-233 streamed bot drafts (`HistoryStreamedDrafts`).
#[derive(Debug, Clone)]
pub struct HistoryStreamedDrafts {
    peer_id: PeerId,
    drafts: BTreeMap<i64, StreamedDraft>,
    stopped_random_ids: HashSet<i64>,
    next_local_id: i32,
}

impl HistoryStreamedDrafts {
    /// Creates an empty streamed drafts container for `peer_id`.
    #[must_use]
    pub fn new(peer_id: PeerId) -> Self {
        Self {
            peer_id,
            drafts: BTreeMap::new(),
            stopped_random_ids: HashSet::new(),
            next_local_id: -100_000,
        }
    }

    /// Returns `true` if no active streamed drafts remain.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.drafts.is_empty()
    }

    /// Returns the count of active streamed drafts.
    #[must_use]
    pub fn len(&self) -> usize {
        self.drafts.len()
    }

    /// Returns an iterator over all active streamed drafts.
    pub fn iter(&self) -> impl Iterator<Item = &StreamedDraft> {
        self.drafts.values()
    }

    /// Looks up an active draft by its protocol `random_id`.
    #[must_use]
    pub fn get(&self, random_id: i64) -> Option<&StreamedDraft> {
        self.drafts.get(&random_id)
    }

    /// Applies an incoming `sendMessageTextDraftAction` update.
    ///
    /// Returns `true` if visible draft state changed.
    #[allow(clippy::too_many_arguments)]
    pub fn apply_text_draft(
        &mut self,
        top_msg_id: Option<MsgId>,
        from_id: PeerId,
        when: i32,
        random_id: i64,
        mut text: TextWithEntities,
        can_stop: bool,
        keep_on_stop: bool,
        now_ms: u64,
    ) -> bool {
        let match_text = text.text.clone();
        if !text.text.ends_with(" ▍") {
            text.text.push_str(" ▍");
        }
        self.apply_prepared(
            top_msg_id,
            from_id,
            when,
            random_id,
            StreamedDraftKind::Text,
            text,
            None,
            match_text,
            can_stop,
            keep_on_stop,
            now_ms,
        )
    }

    /// Applies an incoming `sendMessageRichMessageDraftAction` update.
    ///
    /// Returns `true` if visible draft state changed.
    #[allow(clippy::too_many_arguments)]
    pub fn apply_rich_draft(
        &mut self,
        top_msg_id: Option<MsgId>,
        from_id: PeerId,
        when: i32,
        random_id: i64,
        payload: RichMessagePayload,
        can_stop: bool,
        keep_on_stop: bool,
        now_ms: u64,
    ) -> bool {
        let summary = flatten_rich_page_summary(&payload.page, false);
        let match_text = summary.text.clone();
        let text = if summary.text.is_empty() {
            TextWithEntities::simple("▍")
        } else {
            TextWithEntities::simple(summary.text)
        };
        self.apply_prepared(
            top_msg_id,
            from_id,
            when,
            random_id,
            StreamedDraftKind::Rich,
            text,
            Some(payload),
            match_text,
            can_stop,
            keep_on_stop,
            now_ms,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_prepared(
        &mut self,
        top_msg_id: Option<MsgId>,
        from_id: PeerId,
        when: i32,
        random_id: i64,
        kind: StreamedDraftKind,
        text: TextWithEntities,
        rich_message: Option<RichMessagePayload>,
        match_text: String,
        can_stop: bool,
        keep_on_stop: bool,
        now_ms: u64,
    ) -> bool {
        let root_id = normalize_root_id(top_msg_id);
        let clean_top = top_msg_id.filter(|id| id.0 != 0);
        if when == 0 {
            return self.clear_by_random_id(random_id);
        }
        if self.stopped_random_ids.contains(&random_id) {
            return false;
        }
        if let Some(existing) = self.drafts.get_mut(&random_id) {
            existing.kind = kind;
            existing.text = text;
            existing.rich_message = rich_message;
            existing.match_text = match_text;
            existing.updated_ms = now_ms;
            existing.can_stop = can_stop;
            existing.keep_on_stop = keep_on_stop;
            return true;
        }
        if let Some(prev_id) = self.previous_random_id(root_id, from_id) {
            self.drafts.remove(&prev_id);
        }
        let local_msg_id = MsgId(self.next_local_id);
        self.next_local_id -= 1;
        self.drafts.insert(
            random_id,
            StreamedDraft {
                local_msg_id,
                random_id,
                peer_id: self.peer_id,
                root_id,
                top_msg_id: clean_top,
                from_id,
                date_unix: when,
                updated_ms: now_ms,
                kind,
                can_stop,
                keep_on_stop,
                match_text,
                text,
                rich_message,
            },
        );
        true
    }

    fn previous_random_id(&self, root_id: MsgId, from_id: PeerId) -> Option<i64> {
        self.drafts
            .iter()
            .find(|(_, d)| d.root_id == root_id && d.from_id == from_id)
            .map(|(rid, _)| *rid)
    }

    /// Returns the most recently updated stoppable draft `(random_id, top_msg_id)` for `root_id`.
    #[must_use]
    pub fn stoppable_for(&self, root_id: Option<MsgId>) -> Option<(i64, Option<MsgId>)> {
        let target_root = normalize_root_id(root_id);
        self.drafts
            .values()
            .filter(|d| d.can_stop && d.root_id == target_root)
            .max_by_key(|d| d.updated_ms)
            .map(|d| (d.random_id, d.top_msg_id))
    }

    /// Marks `random_id` stopped (`HistoryStreamedDrafts::applyStop`).
    ///
    /// Removes the draft immediately unless `keep_on_stop` is set, in which case `can_stop` is
    /// cleared and the streamed content stays visible until adopted or expired.
    pub fn apply_stop(&mut self, random_id: i64) -> bool {
        let Some(draft) = self.drafts.get_mut(&random_id) else {
            return false;
        };
        self.stopped_random_ids.insert(random_id);
        if !draft.keep_on_stop {
            self.drafts.remove(&random_id);
            true
        } else if draft.can_stop {
            draft.can_stop = false;
            true
        } else {
            false
        }
    }

    /// Removes a draft by protocol `random_id`.
    pub fn clear_by_random_id(&mut self, random_id: i64) -> bool {
        self.drafts.remove(&random_id).is_some()
    }

    /// Removes a draft by its synthetic local message id (`applyItemRemoved`).
    pub fn apply_item_removed(&mut self, local_msg_id: MsgId) -> bool {
        let key = self
            .drafts
            .iter()
            .find(|(_, d)| d.local_msg_id == local_msg_id)
            .map(|(rid, _)| *rid);
        if let Some(rid) = key {
            self.drafts.remove(&rid);
            true
        } else {
            false
        }
    }

    /// Attempts to match and adopt (or clear) a streamed draft when an incoming server `Message`
    /// arrives (`HistoryStreamedDrafts::adoptIncoming`).
    pub fn adopt_incoming(
        &mut self,
        msg: &tl_types::Message,
        from_id: PeerId,
        already_exists: bool,
    ) -> Option<AdoptedStreamedDraft> {
        if self.drafts.is_empty() || msg.out {
            return None;
        }
        let keys = thread_root_ids_from_tl(msg);
        let (incoming_kind, incoming_text) = if let Some(ref rm) = msg.rich_message {
            let payload = payload_from_tl_enum(rm);
            (
                StreamedDraftKind::Rich,
                flatten_rich_page_summary(&payload.page, false).text,
            )
        } else {
            (StreamedDraftKind::Text, msg.message.clone())
        };

        let mut best_rid: Option<i64> = None;
        for raw_root in [keys.primary, keys.secondary] {
            if raw_root == 0 {
                break;
            }
            let root_id = MsgId(raw_root);
            if incoming_kind == StreamedDraftKind::Rich && incoming_text.is_empty() {
                best_rid = self
                    .drafts
                    .iter()
                    .filter(|(_, d)| {
                        d.root_id == root_id
                            && d.from_id == from_id
                            && d.kind == StreamedDraftKind::Rich
                    })
                    .max_by_key(|(_, d)| d.updated_ms)
                    .map(|(rid, _)| *rid);
            } else {
                let mut best_same_kind: Option<(i64, usize)> = None;
                let mut best_other_kind: Option<(i64, usize)> = None;
                let mut newest_same_thread_rich: Option<(i64, u64)> = None;
                for (&rid, draft) in &self.drafts {
                    if draft.root_id != root_id || draft.from_id != from_id {
                        continue;
                    }
                    if incoming_kind == StreamedDraftKind::Rich
                        && draft.kind == StreamedDraftKind::Rich
                        && newest_same_thread_rich
                            .is_none_or(|(_, prev_u)| draft.updated_ms > prev_u)
                    {
                        newest_same_thread_rich = Some((rid, draft.updated_ms));
                    }
                    let prefix = common_prefix_len(&draft.match_text, &incoming_text);
                    if prefix == 0 {
                        continue;
                    }
                    if draft.kind == incoming_kind {
                        if best_same_kind.is_none_or(|(_, p)| prefix > p) {
                            best_same_kind = Some((rid, prefix));
                        }
                    } else if best_other_kind.is_none_or(|(_, p)| prefix > p) {
                        best_other_kind = Some((rid, prefix));
                    }
                }
                best_rid = best_same_kind
                    .map(|(rid, _)| rid)
                    .or_else(|| best_other_kind.map(|(rid, _)| rid))
                    .or_else(|| newest_same_thread_rich.map(|(rid, _)| rid));
            }
            if best_rid.is_some() {
                break;
            }
        }

        let rid = best_rid?;
        let draft = self.drafts.remove(&rid)?;
        let adopted_in_place = can_adopt_in_place(msg) && !already_exists;
        Some(AdoptedStreamedDraft {
            random_id: rid,
            local_msg_id: draft.local_msg_id,
            real_msg_id: MsgId(msg.id),
            adopted_in_place,
        })
    }

    /// Expires all drafts not updated for at least [`STREAMED_DRAFT_CLEAR_TIMEOUT_MS`].
    ///
    /// Returns the synthetic local message ids of expired drafts.
    pub fn expire_stale(&mut self, now_ms: u64) -> Vec<MsgId> {
        let mut expired = Vec::new();
        self.drafts.retain(|_, draft| {
            if now_ms.saturating_sub(draft.updated_ms) >= STREAMED_DRAFT_CLEAR_TIMEOUT_MS {
                expired.push(draft.local_msg_id);
                false
            } else {
                true
            }
        });
        expired
    }

    /// Converts active drafts (optionally filtered by `root_id`) into synthetic [`HistoryMessage`]s
    /// sorted by `date_unix` / `updated_ms` for rendering at the bottom of the chat.
    #[must_use]
    pub fn to_history_messages<F>(
        &self,
        root_id: Option<MsgId>,
        mut sender_name_for: F,
    ) -> Vec<HistoryMessage>
    where
        F: FnMut(PeerId) -> String,
    {
        let filter_root = root_id.map(|r| normalize_root_id(Some(r)));
        let mut drafts: Vec<&StreamedDraft> = self
            .drafts
            .values()
            .filter(|d| filter_root.is_none_or(|fr| d.root_id == fr))
            .collect();
        drafts.sort_by_key(|d| (d.date_unix, d.updated_ms));
        drafts
            .into_iter()
            .map(|d| HistoryMessage {
                id: d.local_msg_id,
                peer_id: d.peer_id,
                from_id: d.from_id,
                sender_name: sender_name_for(d.from_id),
                date_unix: d.date_unix,
                outgoing: false,
                is_post: false,
                text: d.text.clone(),
                media: None,
                service: None,
                reply: None,
                forward: None,
                reactions: Vec::new(),
                views: None,
                edited: false,
                is_read: false,
                is_sending: true,
                is_pinned: false,
                extra: MessageExtra {
                    thread_id: d.top_msg_id,
                    rich_message: d.rich_message.clone(),
                    ..MessageExtra::default()
                },
            })
            .collect()
    }
}
