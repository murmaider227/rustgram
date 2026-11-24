//! Port of `data/data_forum.h`, `data/data_forum.cpp`, `data/data_forum_topic.h`,
//! `data/data_forum_topic.cpp`, and `data/data_replies_list.cpp` (model parts).
//!
//! Tracks forum topics per forum peer (channel or bot forum), pagination offsets
//! (`offset_date`, `offset_id`, `offset_topic`), pinned topic ordering, deleted
//! topic tombstones, stale topic ID queues, and per-topic inbox/outbox read state
//! and unread counts.

use std::collections::{BTreeSet, HashMap, HashSet};

use crate::changes::TopicUpdateFlags;
use crate::dialogs::UnreadState;
use crate::history::Draft;
use crate::id::{DocumentId, MsgId, PeerId, TimeId};
use crate::peer::PeerNotifySettings;

/// Root message ID of the General forum topic (`ForumTopic::kGeneralId`).
pub const FORUM_GENERAL_TOPIC_ID: MsgId = MsgId(1);

/// Default forum topic icon palette colors (`Data::ForumTopicColorIds` in `data_forum_topic.cpp:47-57`).
pub const FORUM_TOPIC_COLOR_IDS: [i32; 6] = [
    0x6F_B9_F0, // blue
    0xFF_D6_7E, // yellow
    0xCB_86_DB, // violet
    0x8E_EE_98, // green
    0xFF_93_B2, // rose
    0xFB_6F_5F, // red
];

/// First page limit for `messages.getForumTopics` (`kTopicsFirstLoad` in `data_forum.cpp:41`).
pub const TOPICS_FIRST_LOAD: i32 = 20;

/// Subsequent page limit for `messages.getForumTopics` (`kTopicsPerPage` in `data_forum.cpp:43`).
pub const TOPICS_PER_PAGE: i32 = 500;

/// Maximum stale topic IDs batched into one `messages.getForumTopicsByID`
/// (`kStalePerRequest` in `data_forum.cpp:44`).
pub const TOPICS_STALE_PER_REQUEST: usize = 100;

/// Pagination cursor for `messages.getForumTopics` (`Data::ForumOffsets`).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct ForumOffsets {
    /// Date of the last topic's top message (`0` on first load).
    pub date: TimeId,
    /// Message ID of the last topic's top message (`0` on first load).
    pub id: MsgId,
    /// Root topic ID of the last received topic (`0` on first load).
    pub topic_id: MsgId,
}

/// Domain model for a single forum topic (`Data::ForumTopic`).
#[derive(Clone, Debug, PartialEq)]
pub struct ForumTopicData {
    /// Owning forum peer ID (channel or bot).
    pub peer_id: PeerId,
    /// Topic root message ID (`1` = General).
    pub root_id: MsgId,
    /// Topic title.
    pub title: String,
    /// Topic icon RGB color ID.
    pub icon_color: i32,
    /// Optional custom emoji document ID for the topic icon.
    pub icon_emoji_id: Option<DocumentId>,
    /// Peer who created the topic.
    pub creator_id: PeerId,
    /// Topic creation timestamp (unix seconds).
    pub creation_date: TimeId,
    /// Whether created by the current user (`forumTopic.my`).
    pub my: bool,
    /// Whether the topic is closed (`forumTopic.closed`).
    pub closed: bool,
    /// Whether the topic is pinned (`forumTopic.pinned`).
    pub pinned: bool,
    /// Whether the topic is hidden (General topic only, `forumTopic.hidden`).
    pub hidden: bool,
    /// Whether the topic title is missing/auto-generated (`forumTopic.title_missing`).
    pub title_missing: bool,
    /// Latest known server message ID in this topic (`top_message`).
    pub top_message_id: Option<MsgId>,
    /// Highest server message ID ever observed in this topic (`_lastKnownServerMessageId`).
    pub last_known_server_msg_id: MsgId,
    /// Date of the latest message in this topic (`0` if unknown).
    pub last_message_date: TimeId,
    /// Highest incoming message ID read by the current user (`read_inbox_max_id`).
    pub read_inbox_max_id: MsgId,
    /// Highest outgoing message ID read by others (`read_outbox_max_id`).
    pub read_outbox_max_id: MsgId,
    /// Unread incoming message count (`None` if unknown).
    pub unread_count: Option<i32>,
    /// Unread mentions count in this topic.
    pub unread_mentions_count: i32,
    /// Unread reactions count in this topic.
    pub unread_reactions_count: i32,
    /// Unread poll votes count in this topic.
    pub unread_poll_votes_count: i32,
    /// Per-topic notification settings.
    pub notify: PeerNotifySettings,
    /// Cached muted flag derived from `notify`.
    pub muted: bool,
    /// Cloud draft attached to this topic, if any.
    pub draft: Option<Draft>,
}

impl ForumTopicData {
    /// Creates a new minimally initialized `ForumTopicData` for `(peer_id, root_id)`.
    #[must_use]
    pub fn new(peer_id: PeerId, root_id: MsgId) -> Self {
        Self {
            peer_id,
            root_id,
            title: String::new(),
            icon_color: FORUM_TOPIC_COLOR_IDS[0],
            icon_emoji_id: None,
            creator_id: PeerId::ZERO,
            creation_date: 0,
            my: false,
            closed: false,
            pinned: false,
            hidden: false,
            title_missing: false,
            top_message_id: None,
            last_known_server_msg_id: root_id,
            last_message_date: 0,
            read_inbox_max_id: MsgId::ZERO,
            read_outbox_max_id: MsgId::ZERO,
            unread_count: None,
            unread_mentions_count: 0,
            unread_reactions_count: 0,
            unread_poll_votes_count: 0,
            notify: PeerNotifySettings::default(),
            muted: false,
            draft: None,
        }
    }

    /// Returns `true` if this is the General topic (`root_id == 1`).
    #[inline]
    #[must_use]
    pub const fn is_general(&self) -> bool {
        self.root_id.0 == FORUM_GENERAL_TOPIC_ID.0
    }

    /// Updates `read_inbox_max_id` monotonically and optionally updates `unread_count`
    /// (`RepliesList::setInboxReadTill` in `data_replies_list.cpp`).
    ///
    /// If `unread_count` is `None` and `max_id >= last_known_server_msg_id`, resets
    /// `unread_count` to `Some(0)` (matching `RepliesList::applyInboxReadUpdate`).
    pub fn apply_inbox_read_till(&mut self, max_id: MsgId, unread_count: Option<i32>) {
        if max_id > self.read_inbox_max_id {
            self.read_inbox_max_id = max_id;
        }
        if let Some(count) = unread_count {
            self.unread_count = Some(count.max(0));
        } else if self.last_known_server_msg_id.0 > 0
            && self.read_inbox_max_id >= self.last_known_server_msg_id
        {
            self.unread_count = Some(0);
        } else if self.unread_count.is_some_and(|c| c > 0) && max_id > MsgId::ZERO {
            // Unread count is no longer exact without a loaded replies list; mark unknown.
            self.unread_count = None;
        }
    }

    /// Updates `read_outbox_max_id` monotonically (`RepliesList::setOutboxReadTill`).
    pub fn apply_outbox_read_till(&mut self, max_id: MsgId) {
        if max_id > self.read_outbox_max_id {
            self.read_outbox_max_id = max_id;
        }
    }

    /// Records a newly arrived server message in this topic, advancing `top_message_id` /
    /// `last_known_server_msg_id` and incrementing `unread_count` for unread incoming messages.
    pub fn apply_new_message(&mut self, msg_id: MsgId, date: TimeId, outgoing: bool) {
        if msg_id > self.last_known_server_msg_id {
            self.last_known_server_msg_id = msg_id;
        }
        if self.top_message_id.is_none_or(|cur| msg_id >= cur) {
            self.top_message_id = Some(msg_id);
            if date > 0 {
                self.last_message_date = date;
            }
        }
        if !outgoing
            && msg_id > self.read_inbox_max_id
            && let Some(count) = self.unread_count.as_mut()
        {
            *count = count.saturating_add(1);
        }
    }

    /// Computes this topic's contribution to forum unread counters (`ForumTopic::unreadStateFor`).
    #[must_use]
    pub fn unread_state(&self) -> UnreadState {
        let messages = self.unread_count.unwrap_or(0).max(0);
        let has_unread = messages > 0;
        let has_reactions = self.unread_reactions_count > 0;
        let has_mentions = self.unread_mentions_count > 0;
        let has_polls = self.unread_poll_votes_count > 0;
        UnreadState {
            messages,
            messages_muted: if self.muted { messages } else { 0 },
            chats: i32::from(has_unread),
            chats_muted: i32::from(has_unread && self.muted),
            marks: 0,
            marks_muted: 0,
            reactions: i32::from(has_reactions),
            reactions_muted: i32::from(has_reactions && self.muted),
            mentions: i32::from(has_mentions),
            polls: i32::from(has_polls),
            polls_muted: i32::from(has_polls && self.muted),
            known: self.unread_count.is_some(),
        }
    }
}

/// Per-peer forum topics registry (`Data::Forum` in `data/data_forum.cpp`).
#[derive(Clone, Debug, Default)]
pub struct ForumState {
    /// Owning forum peer ID.
    pub peer_id: PeerId,
    /// Loaded topics keyed by `root_id`.
    pub topics: HashMap<MsgId, ForumTopicData>,
    /// Ordered pinned topic IDs (index 0 = top-most pin).
    pub pinned_order: Vec<MsgId>,
    /// Deleted topic tombstones (`_topicsDeleted`).
    pub deleted_topics: HashSet<MsgId>,
    /// Stale topic IDs queued for `messages.getForumTopicsByID` (`_staleRootIds`).
    pub stale_root_ids: BTreeSet<MsgId>,
    /// Current pagination offset (`_offset`).
    pub offset: ForumOffsets,
    /// Whether all forum topic pages have been loaded (`_topicsList.loaded()`).
    pub all_loaded: bool,
    /// Server `order_by_create_date` hint from `messages.forumTopics`.
    pub order_by_create_date: bool,
}

impl ForumState {
    /// Creates a new `ForumState` for `peer_id`.
    #[must_use]
    pub fn new(peer_id: PeerId) -> Self {
        Self {
            peer_id,
            ..Self::default()
        }
    }

    /// Resets pagination state for a full reload (`Forum::reloadTopics` in `data_forum.cpp:163-173`).
    pub fn begin_reload(&mut self) {
        self.all_loaded = false;
        self.offset = ForumOffsets::default();
        for &root_id in self.topics.keys() {
            self.stale_root_ids.insert(root_id);
        }
    }

    /// Returns a loaded topic by `root_id`.
    #[must_use]
    pub fn topic(&self, root_id: MsgId) -> Option<&ForumTopicData> {
        self.topics.get(&root_id)
    }

    /// Returns a mutable reference to a loaded topic by `root_id`.
    pub fn topic_mut(&mut self, root_id: MsgId) -> Option<&mut ForumTopicData> {
        self.topics.get_mut(&root_id)
    }

    /// Ensures a topic entry exists for `root_id`, clearing any stale/deleted markers.
    pub fn ensure_topic(&mut self, root_id: MsgId) -> &mut ForumTopicData {
        self.deleted_topics.remove(&root_id);
        self.stale_root_ids.remove(&root_id);
        let peer_id = self.peer_id;
        self.topics
            .entry(root_id)
            .or_insert_with(|| ForumTopicData::new(peer_id, root_id))
    }

    /// Marks `root_id` deleted (`Forum::applyTopicDeleted` in `data_forum.cpp:219-266`).
    ///
    /// Matches `data_forum.cpp:441-447`: if `preserve_general` is `true` and `root_id == 1`,
    /// keeps the General topic entry instead of deleting it (required for bot forums).
    pub fn apply_topic_deleted(&mut self, root_id: MsgId, preserve_general: bool) -> bool {
        if preserve_general && root_id == FORUM_GENERAL_TOPIC_ID {
            self.ensure_topic(root_id);
            return false;
        }
        self.deleted_topics.insert(root_id);
        self.stale_root_ids.remove(&root_id);
        self.pinned_order.retain(|&id| id != root_id);
        self.topics.remove(&root_id).is_some()
    }

    /// Sets or clears pinned status for a single loaded topic (`updatePinnedForumTopic`).
    ///
    /// Returns `true` if the topic was loaded and updated, or `false` if unloaded (caller
    /// should queue `messages.getForumTopicsByID` per `api_updates.cpp:2669-2673`).
    pub fn set_topic_pinned(&mut self, root_id: MsgId, pinned: bool) -> bool {
        let Some(topic) = self.topics.get_mut(&root_id) else {
            self.stale_root_ids.insert(root_id);
            return false;
        };
        topic.pinned = pinned;
        if pinned {
            if !self.pinned_order.contains(&root_id) {
                self.pinned_order.insert(0, root_id);
            }
        } else {
            self.pinned_order.retain(|&id| id != root_id);
        }
        true
    }

    /// Applies an authoritative pinned topic order (`updatePinnedForumTopics`).
    ///
    /// Matches `api/api_updates.cpp:2682-2699`: if `order` is `None` or contains any topic ID
    /// not currently loaded in `self.topics`, returns `false` so the caller triggers
    /// `reloadTopics()` instead of applying a partial pinned list.
    pub fn apply_pinned_order(&mut self, order: Option<&[MsgId]>) -> bool {
        let Some(ids) = order else {
            return false;
        };
        if ids.iter().any(|id| !self.topics.contains_key(id)) {
            return false;
        }
        let pinned_set: HashSet<MsgId> = ids.iter().copied().collect();
        for (&root_id, topic) in &mut self.topics {
            topic.pinned = pinned_set.contains(&root_id);
        }
        self.pinned_order = ids.to_vec();
        true
    }

    /// Applies an inline `updateEditForumTopic` patch onto a loaded topic, returning the
    /// changed [`TopicUpdateFlags`] (or `None` if the topic is not loaded).
    pub fn apply_topic_edit(
        &mut self,
        root_id: MsgId,
        title: Option<String>,
        icon_emoji_id: Option<i64>,
        closed: Option<bool>,
        hidden: Option<bool>,
    ) -> Option<TopicUpdateFlags> {
        let topic = self.topics.get_mut(&root_id)?;
        let mut flags = TopicUpdateFlags::empty();
        if let Some(new_title) = title
            && topic.title != new_title
        {
            topic.title = new_title;
            flags |= TopicUpdateFlags::TITLE;
        }
        if let Some(new_icon) = icon_emoji_id {
            let mapped = (new_icon != 0).then_some(DocumentId(new_icon as u64));
            if topic.icon_emoji_id != mapped {
                topic.icon_emoji_id = mapped;
                flags |= TopicUpdateFlags::ICON_ID;
            }
        }
        if let Some(new_closed) = closed
            && topic.closed != new_closed
        {
            topic.closed = new_closed;
            flags |= TopicUpdateFlags::CLOSED;
        }
        if let Some(new_hidden) = hidden {
            topic.hidden = new_hidden;
        }
        Some(flags)
    }

    /// Pops up to [`TOPICS_STALE_PER_REQUEST`] (100) stale topic IDs for
    /// `messages.getForumTopicsByID` (`Forum::requestSomeStale` in `data_forum.cpp:454-475`).
    pub fn take_stale_batch(&mut self) -> Vec<MsgId> {
        let mut out = Vec::with_capacity(self.stale_root_ids.len().min(TOPICS_STALE_PER_REQUEST));
        while out.len() < TOPICS_STALE_PER_REQUEST {
            let Some(id) = self.stale_root_ids.pop_first() else {
                break;
            };
            out.push(id);
        }
        out
    }

    /// Returns topic root IDs sorted in display order: pinned topics first in `pinned_order`,
    /// then unpinned topics descending by last message date (or creation date if
    /// `order_by_create_date` is set), tie-breaking by descending `root_id`.
    #[must_use]
    pub fn sorted_topic_ids(&self) -> Vec<MsgId> {
        let mut ids: Vec<MsgId> = self.topics.keys().copied().collect();
        let pin_pos = |id: MsgId| self.pinned_order.iter().position(|&p| p == id);
        ids.sort_by(|&a, &b| {
            let ta = &self.topics[&a];
            let tb = &self.topics[&b];
            match (pin_pos(a), pin_pos(b)) {
                (Some(pa), Some(pb)) => pa.cmp(&pb),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => {
                    let key_a = if self.order_by_create_date {
                        ta.creation_date
                    } else {
                        ta.last_message_date.max(ta.creation_date)
                    };
                    let key_b = if self.order_by_create_date {
                        tb.creation_date
                    } else {
                        tb.last_message_date.max(tb.creation_date)
                    };
                    key_b.cmp(&key_a).then_with(|| b.cmp(&a))
                }
            }
        });
        ids
    }

    /// Aggregates [`UnreadState`] across all loaded topics in this forum.
    #[must_use]
    pub fn total_unread_state(&self) -> UnreadState {
        let mut total = UnreadState {
            known: true,
            ..UnreadState::default()
        };
        for topic in self.topics.values() {
            let st = topic.unread_state();
            total += st;
            if !st.known {
                total.known = false;
            }
        }
        total
    }
}
