//! History slice loading (around/before/after), unread position, and jump-to-date.
//!
//! Port origin: `data/data_search_controller.cpp` (`PrepareHistoryRequest`,
//! `ParseHistoryResult`), `apiwrap.cpp` (`requestHistory`, `resolveJumpToTime`,
//! `requestMessageAfterTime`).

use std::collections::HashSet;

use tg_data::{
    Message, MsgId, MsgRange, PeerId, SERVER_MAX_MSG_ID, SHOW_AT_UNREAD_MSG_ID, Session, TimeId,
};
use tg_tl::{enums as tl_enums, functions as tl_fns};

use crate::convert::{
    apply_tl_users_and_chats, message_from_tl, peer_id_from_input_peer, tl_message_meta,
};
use crate::error::ApiError;
use crate::forum::apply_tl_forum_topic;
use crate::invoker::Invoker;
use crate::types::LoadDirection;

/// Standard history slice size (`kHistoryLimit` in `data_search_controller.cpp`).
pub const HISTORY_LIMIT: i32 = 50;

/// Maximum 32-bit `offset_id` passed to `messages.getHistory` (`0x3FFFFFFF`).
pub const MAX_OFFSET_ID: i32 = 0x3FFF_FFFF;

/// Parsed history slice returned by `HistoryLoader::request_history`.
#[derive(Debug, Clone, PartialEq)]
pub struct HistorySlice {
    /// Message IDs returned in this slice.
    pub message_ids: Vec<MsgId>,
    /// Raw TL message payloads.
    pub messages: Vec<tl_enums::Message>,
    /// Forum topics returned alongside the history slice (`vtopics()` in `data_search_controller.cpp:219`).
    pub topics: Vec<tl_enums::ForumTopic>,
    /// Users returned alongside the history slice.
    pub users: Vec<tl_enums::User>,
    /// Chats/channels returned alongside the history slice.
    pub chats: Vec<tl_enums::Chat>,
    /// Contiguous range confirmed to contain no missing messages.
    pub no_skip_range: MsgRange,
    /// Total message count reported by server (if known).
    pub full_count: Option<i32>,
    /// Channel pts when response was `messages.channelMessages`.
    pub channel_pts: Option<i32>,
}

impl HistorySlice {
    /// Converts and applies this history slice (users, chats, topics, messages, and `no_skip_range`)
    /// into `session` for `peer_id`.
    pub fn apply_to_session(&self, session: &mut Session, peer_id: PeerId) {
        apply_tl_users_and_chats(session, &self.users, &self.chats);
        let self_id = session.self_user_id();
        let domain_msgs: Vec<Message> = self
            .messages
            .iter()
            .filter_map(|m| message_from_tl(m, self_id))
            .collect();
        session.add_history_slice(peer_id, domain_msgs, self.no_skip_range, self.full_count);
        for topic in &self.topics {
            let _ = apply_tl_forum_topic(session, peer_id, topic, 0);
        }
    }
}

/// Result of resolving a jump-to-date/time query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JumpToDateResult {
    /// Target peer ID (may be the migrated legacy chat if timestamp predates migration).
    pub peer: PeerId,
    /// First message ID at or after the requested timestamp, or `SHOW_AT_UNREAD_MSG_ID`.
    pub msg_id: MsgId,
}

/// Builds a `messages.getHistory` TL request with tdesktop's exact `offset_id`, `add_offset`,
/// and `limit` rules (`Api::PrepareHistoryRequest`).
#[must_use]
pub fn prepare_history_request(
    peer: tl_enums::InputPeer,
    message_id: MsgId,
    direction: LoadDirection,
) -> tl_fns::messages::GetHistory {
    let limit = HISTORY_LIMIT;
    let raw_offset_id: i64 = match direction {
        LoadDirection::Before | LoadDirection::Around => message_id.0,
        LoadDirection::After => message_id.0.saturating_add(1),
    };
    let clamped_offset_id = raw_offset_id.clamp(0, i64::from(MAX_OFFSET_ID)) as i32;
    let add_offset = match direction {
        LoadDirection::Before => 0,
        LoadDirection::Around => -limit / 2,
        LoadDirection::After => -limit,
    };
    tl_fns::messages::GetHistory {
        peer,
        offset_id: clamped_offset_id,
        offset_date: 0,
        add_offset,
        limit,
        max_id: 0,
        min_id: 0,
        hash: 0,
    }
}

/// Parses a TL `messages.Messages` response into a `HistorySlice`, reproducing tdesktop's
/// `noSkipRange` calculation (`Api::ParseHistoryResult` / `ParseSearchResult`).
#[must_use]
pub fn parse_history_result(
    message_id: MsgId,
    direction: LoadDirection,
    response: tl_enums::messages::Messages,
) -> HistorySlice {
    let mut no_skip_range = MsgRange::new(message_id, message_id);

    let (messages, topics, users, chats, full_count, channel_pts, is_complete_variant) =
        match response {
            tl_enums::messages::Messages::Messages(m) => {
                let count = m.messages.len() as i32;
                (
                    m.messages,
                    m.topics,
                    m.users,
                    m.chats,
                    Some(count),
                    None,
                    true,
                )
            }
            tl_enums::messages::Messages::Slice(s) => (
                s.messages,
                s.topics,
                s.users,
                s.chats,
                Some(s.count),
                None,
                false,
            ),
            tl_enums::messages::Messages::ChannelMessages(c) => (
                c.messages,
                c.topics,
                c.users,
                c.chats,
                Some(c.count),
                Some(c.pts),
                false,
            ),
            tl_enums::messages::Messages::NotModified(_) => {
                return HistorySlice {
                    message_ids: Vec::new(),
                    messages: Vec::new(),
                    topics: Vec::new(),
                    users: Vec::new(),
                    chats: Vec::new(),
                    no_skip_range,
                    full_count: None,
                    channel_pts: None,
                };
            }
        };

    let mut message_ids = Vec::with_capacity(messages.len());
    let mut min_msg_id: Option<MsgId> = None;
    let mut max_msg_id: Option<MsgId> = None;
    for msg in &messages {
        let msg_id = match msg {
            tl_enums::Message::Message(m) => MsgId(i64::from(m.id)),
            tl_enums::Message::Service(s) => MsgId(i64::from(s.id)),
            tl_enums::Message::Empty(e) => MsgId(i64::from(e.id)),
        };
        message_ids.push(msg_id);
        min_msg_id = Some(min_msg_id.map_or(msg_id, |cur| cur.min(msg_id)));
        max_msg_id = Some(max_msg_id.map_or(msg_id, |cur| cur.max(msg_id)));
        if msg_id < no_skip_range.from {
            no_skip_range.from = msg_id;
        }
        if msg_id > no_skip_range.till {
            no_skip_range.till = msg_id;
        }
    }

    if message_id.is_zero() {
        // When `message_id == 0` (loading newest page from bottom), initializing `no_skip_range`
        // to `(0, 0)` would keep `from == 0` and falsely mark `SparseIdsList::loaded_at_top()`
        // even on a partial `messagesSlice` (`data_search_controller.cpp:211-291`).
        if let (Some(min_id), Some(max_id)) = (min_msg_id, max_msg_id) {
            let from = if is_complete_variant {
                MsgId::ZERO
            } else {
                min_id
            };
            let till = match direction {
                LoadDirection::Before => SERVER_MAX_MSG_ID,
                LoadDirection::Around | LoadDirection::After => max_id,
            };
            no_skip_range = MsgRange::new(from, till);
        } else if is_complete_variant || full_count == Some(0) {
            no_skip_range = MsgRange::new(MsgId::ZERO, SERVER_MAX_MSG_ID);
        }
    } else if message_ids.is_empty() {
        no_skip_range = match direction {
            LoadDirection::Before => MsgRange::new(MsgId::ZERO, no_skip_range.till),
            LoadDirection::Around => MsgRange::new(MsgId::ZERO, SERVER_MAX_MSG_ID),
            LoadDirection::After => MsgRange::new(no_skip_range.from, SERVER_MAX_MSG_ID),
        };
    }

    HistorySlice {
        message_ids,
        messages,
        topics,
        users,
        chats,
        no_skip_range,
        full_count,
        channel_pts,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct HistoryRequestKey {
    peer_id: PeerId,
    around_id: MsgId,
    direction: LoadDirection,
}

/// Deduplicating history slice & jump-to-date loader (`ApiWrap::requestHistory`).
#[derive(Debug, Default)]
pub struct HistoryLoader {
    in_flight: HashSet<HistoryRequestKey>,
}

impl HistoryLoader {
    /// Creates a new `HistoryLoader`.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Requests a slice of history messages (`Before`, `Around`, or `After` `message_id`).
    ///
    /// Deduplicates identical in-flight `(peer, message_id, direction)` requests,
    /// returning `Ok(None)` if already in flight.
    pub async fn request_history<I: Invoker>(
        &mut self,
        invoker: &I,
        peer: tl_enums::InputPeer,
        message_id: MsgId,
        direction: LoadDirection,
    ) -> Result<Option<HistorySlice>, ApiError> {
        let key = HistoryRequestKey {
            peer_id: peer_id_from_input_peer(&peer, None),
            around_id: message_id,
            direction,
        };
        if !self.in_flight.insert(key) {
            return Ok(None);
        }

        let req = prepare_history_request(peer, message_id, direction);
        let result = invoker.messages_get_history(req).await;
        self.in_flight.remove(&key);

        let response = result?;
        Ok(Some(parse_history_result(message_id, direction, response)))
    }

    /// Loads history positioned at the unread boundary (`read_inbox_max_id`).
    ///
    /// If `read_inbox_max_id > 0`, loads `Around(read_inbox_max_id)` so both
    /// context above and unread messages below are fetched; otherwise loads the
    /// latest slice via `Before(MsgId::ZERO)`.
    pub async fn request_unread_position<I: Invoker>(
        &mut self,
        invoker: &I,
        peer: tl_enums::InputPeer,
        read_inbox_max_id: MsgId,
    ) -> Result<Option<HistorySlice>, ApiError> {
        if read_inbox_max_id.0 > 0 {
            self.request_history(invoker, peer, read_inbox_max_id, LoadDirection::Around)
                .await
        } else {
            self.request_history(invoker, peer, MsgId::ZERO, LoadDirection::Before)
                .await
        }
    }

    /// Resolves the first message at or after `when` (unix timestamp seconds)
    /// (`ApiWrap::resolveJumpToHistoryTime` / `requestMessageAfterTime`).
    ///
    /// If `migrated_from` is provided (for a supergroup migrated from a legacy basic group)
    /// and `topic_root_id` is `None`, checks the legacy chat first just like tdesktop.
    pub async fn resolve_jump_to_time<I: Invoker>(
        &self,
        invoker: &I,
        peer: tl_enums::InputPeer,
        migrated_from: Option<tl_enums::InputPeer>,
        topic_root_id: Option<MsgId>,
        when: TimeId,
    ) -> Result<JumpToDateResult, ApiError> {
        if topic_root_id.is_none()
            && let Some(legacy) = migrated_from
        {
            let legacy_id = peer_id_from_input_peer(&legacy, None);
            let found = self
                .request_message_after_time(invoker, legacy, None, when)
                .await?;
            if !found.is_zero() {
                return Ok(JumpToDateResult {
                    peer: legacy_id,
                    msg_id: found,
                });
            }
        }
        let peer_id = peer_id_from_input_peer(&peer, None);
        let found = self
            .request_message_after_time(invoker, peer, topic_root_id, when)
            .await?;
        Ok(JumpToDateResult {
            peer: peer_id,
            msg_id: found,
        })
    }

    async fn request_message_after_time<I: Invoker>(
        &self,
        invoker: &I,
        peer: tl_enums::InputPeer,
        topic_root_id: Option<MsgId>,
        when: TimeId,
    ) -> Result<MsgId, ApiError> {
        // API returns a message with date <= offset_date.
        // Requesting offset_date = when - 1 with add_offset = -1 and limit = 1
        // gives the first message with date >= when.
        let offset_date = when.saturating_sub(1);
        let response = if let Some(root_id) = topic_root_id {
            invoker
                .messages_get_replies(tl_fns::messages::GetReplies {
                    peer,
                    msg_id: root_id.as_i32().unwrap_or(0),
                    offset_id: 0,
                    offset_date,
                    add_offset: -1,
                    limit: 1,
                    max_id: 0,
                    min_id: 0,
                    hash: 0,
                })
                .await?
        } else {
            invoker
                .messages_get_history(tl_fns::messages::GetHistory {
                    peer,
                    offset_id: 0,
                    offset_date,
                    add_offset: -1,
                    limit: 1,
                    max_id: 0,
                    min_id: 0,
                    hash: 0,
                })
                .await?
        };

        let messages = match response {
            tl_enums::messages::Messages::Messages(m) => m.messages,
            tl_enums::messages::Messages::Slice(s) => s.messages,
            tl_enums::messages::Messages::ChannelMessages(c) => c.messages,
            tl_enums::messages::Messages::NotModified(_) => Vec::new(),
        };

        for msg in messages {
            if let Some((_, msg_id, msg_date)) = tl_message_meta(&msg)
                && msg_date >= offset_date
            {
                return Ok(msg_id);
            }
        }
        Ok(SHOW_AT_UNREAD_MSG_ID)
    }
}
