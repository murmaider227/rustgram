//! Port of `history/history.h`, `history/history.cpp` (model parts),
//! `storage/storage_sparse_ids_list.h`, `storage/storage_sparse_ids_list.cpp`,
//! and `data/data_drafts.h`.
//!
//! Implements per-peer message histories, contiguous loaded message ID ranges
//! (`SparseIdsList`), inbox/outbox read state, unread counters/marks/mentions/
//! reactions/poll votes, and local/cloud drafts with tdesktop's exact `DraftKey`
//! serialization.

use std::collections::{BTreeMap, BTreeSet};

use crate::dialogs::{
    BadgeSettings, BadgesState, CountInBadge, IncludeInBadge, UnreadState, badges_for_unread,
};
use crate::id::{
    ARCHIVE_FOLDER_ID, BusinessShortcutId, FilterId, FolderId, MsgId, MsgRange, PeerId,
    SERVER_MAX_MSG_ID, TimeId, UserId, WebPageId,
};
use crate::message::{Message, ReplyInfo};
use crate::text::TextWithEntities;

/// Contiguous loaded slice of server message IDs (`Storage::SparseIdsList::Slice`).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IdSlice {
    /// Loaded message IDs inside `range`.
    pub messages: BTreeSet<MsgId>,
    /// Inclusive-start, inclusive/sentinel-end range with no gaps between loaded IDs.
    pub range: MsgRange,
}

impl IdSlice {
    /// Creates a new slice from `messages` and `range`.
    pub fn new(messages: BTreeSet<MsgId>, range: MsgRange) -> Self {
        Self { messages, range }
    }

    /// Merges another overlapping or adjacent range and its messages into `self`.
    fn merge_iter<I>(&mut self, more_messages: I, more_range: MsgRange)
    where
        I: IntoIterator<Item = MsgId>,
    {
        self.messages.extend(more_messages);
        self.range = MsgRange::new(
            self.range.from.min(more_range.from),
            self.range.till.max(more_range.till),
        );
    }
}

/// Result of querying a `SparseIdsList` around a target message ID
/// (`Storage::SparseIdsListResult`).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SparseIdsSnapshot {
    /// Total server count if known.
    pub count: Option<i32>,
    /// Number of messages known to exist before `message_ids` (if computable).
    pub skipped_before: Option<i32>,
    /// Number of messages known to exist after `message_ids` (if computable).
    pub skipped_after: Option<i32>,
    /// Ordered message IDs matching the window.
    pub message_ids: Vec<MsgId>,
}

/// Sparse set of disjoint loaded message-ID slices (`Storage::SparseIdsList`).
///
/// Tracks which ID ranges have been loaded from the server without gaps, merges
/// overlapping/adjacent slices automatically, and tracks top (`from == 0`) and
/// bottom (`till == SERVER_MAX_MSG_ID`) completeness.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SparseIdsList {
    count: Option<i32>,
    slices: Vec<IdSlice>,
}

impl SparseIdsList {
    /// Creates an empty `SparseIdsList`.
    pub const fn new() -> Self {
        Self {
            count: None,
            slices: Vec::new(),
        }
    }

    /// Returns all disjoint loaded slices ordered by ascending `range.from`.
    pub fn slices(&self) -> &[IdSlice] {
        &self.slices
    }

    /// Returns the total known message count (if known).
    #[inline]
    pub const fn count(&self) -> Option<i32> {
        self.count
    }

    /// Returns `true` if no slice contains any message ID.
    pub fn is_empty(&self) -> bool {
        self.slices.iter().all(|s| s.messages.is_empty())
    }

    /// Returns `true` if the newest end of history is loaded (`till == SERVER_MAX_MSG_ID`).
    pub fn loaded_at_bottom(&self) -> bool {
        self.slices
            .last()
            .is_some_and(|s| s.range.till == SERVER_MAX_MSG_ID)
    }

    /// Returns `true` if the oldest end of history is loaded (`from == 0`).
    pub fn loaded_at_top(&self) -> bool {
        self.slices
            .first()
            .is_some_and(|s| s.range.from == MsgId::ZERO)
    }

    /// Returns `true` if `msg_id` is covered by any loaded slice.
    pub fn contains_range_for(&self, msg_id: MsgId) -> bool {
        self.slices
            .iter()
            .any(|s| s.range.from <= msg_id && msg_id <= s.range.till)
    }

    /// Returns the smallest loaded message ID, if any.
    pub fn min_id(&self) -> Option<MsgId> {
        self.slices.iter().find_map(|s| s.messages.first().copied())
    }

    /// Returns the largest loaded message ID, if any.
    pub fn max_id(&self) -> Option<MsgId> {
        self.slices
            .iter()
            .rev()
            .find_map(|s| s.messages.last().copied())
    }

    /// Adds a newly arrived message at the bottom of history (`SparseIdsList::addNew`).
    pub fn add_new(&mut self, message_id: MsgId) {
        self.add_range(
            [message_id],
            MsgRange::new(message_id, SERVER_MAX_MSG_ID),
            None,
            true,
        );
    }

    /// Adds an existing message within a known gap-free range (`SparseIdsList::addExisting`).
    pub fn add_existing(
        &mut self,
        message_id: MsgId,
        no_skip_range: MsgRange,
        increment_count: bool,
    ) {
        self.add_range([message_id], no_skip_range, None, increment_count);
    }

    /// Adds a loaded slice of messages with its gap-free `no_skip_range` and optional
    /// server total `count` (`SparseIdsList::addSlice`).
    pub fn add_slice(
        &mut self,
        message_ids: impl IntoIterator<Item = MsgId>,
        no_skip_range: MsgRange,
        count: Option<i32>,
    ) {
        self.add_range(message_ids, no_skip_range, count, false);
    }

    fn add_range<I>(
        &mut self,
        messages: I,
        no_skip_range: MsgRange,
        count: Option<i32>,
        increment_count: bool,
    ) where
        I: IntoIterator<Item = MsgId>,
    {
        if no_skip_range.from > no_skip_range.till {
            return;
        }
        let collected: Vec<MsgId> = messages.into_iter().collect();
        if no_skip_range.from == no_skip_range.till && collected.is_empty() {
            return;
        }

        // Find the first slice whose till >= no_skip_range.from.
        let unite_from = self
            .slices
            .partition_point(|s| s.range.till < no_skip_range.from);
        // Find the first slice whose from > no_skip_range.till.
        let unite_till = self
            .slices
            .partition_point(|s| s.range.from <= no_skip_range.till);

        let (added, updated_len) = if unite_from < unite_till {
            let was = self.slices[unite_from].messages.len();
            self.slices[unite_from].merge_iter(collected, no_skip_range);
            if unite_from + 1 < unite_till {
                let tail: Vec<IdSlice> = self.slices.drain((unite_from + 1)..unite_till).collect();
                for slice in tail {
                    self.slices[unite_from].merge_iter(slice.messages, slice.range);
                }
            }
            let now = self.slices[unite_from].messages.len();
            (now.saturating_sub(was) as i32, now as i32)
        } else {
            let msg_set: BTreeSet<MsgId> = collected.into_iter().collect();
            let len = msg_set.len() as i32;
            self.slices
                .insert(unite_from, IdSlice::new(msg_set, no_skip_range));
            (len, len)
        };

        if let Some(c) = count {
            self.count = Some(c);
        } else if increment_count
            && added > 0
            && let Some(c) = self.count.as_mut()
        {
            *c = (*c as i64 + added as i64).min(i32::MAX as i64) as i32;
        }

        if self.slices.len() == 1 {
            if let Some(c) = self.count
                && self.slices[0].messages.len() as i32 >= c
            {
                self.slices[0].range = MsgRange::new(MsgId::ZERO, SERVER_MAX_MSG_ID);
            }
            if self.slices[0].range == MsgRange::new(MsgId::ZERO, SERVER_MAX_MSG_ID) {
                self.count = Some(self.slices[0].messages.len() as i32);
            }
        }
        if let Some(c) = self.count.as_mut()
            && *c < updated_len
        {
            *c = updated_len;
        }
    }

    /// Removes one message ID from any covering slice (`SparseIdsList::removeOne`).
    pub fn remove_one(&mut self, message_id: MsgId, only_matched: bool) {
        let mut removed = false;
        let idx = self.slices.partition_point(|s| s.range.till < message_id);
        if let Some(slice) = self.slices.get_mut(idx)
            && slice.range.from <= message_id
        {
            removed = slice.messages.remove(&message_id);
        }
        if only_matched && !removed {
            return;
        }
        if let Some(c) = self.count.as_mut()
            && *c > 0
        {
            *c -= 1;
        }
    }

    /// Clears all messages and marks the entire `[0, SERVER_MAX_MSG_ID]` range as empty
    /// (`SparseIdsList::removeAll`).
    pub fn remove_all(&mut self) {
        self.slices.clear();
        self.slices.push(IdSlice::new(
            BTreeSet::new(),
            MsgRange::new(MsgId::ZERO, SERVER_MAX_MSG_ID),
        ));
        self.count = Some(0);
    }

    /// Keeps only the newest slice and, in it, the ids `>= keep_from` (whose range then starts
    /// at `keep_from`); the known count stays. The dropped part reads as not loaded, so it is
    /// requested again on scroll (`History::unloadBlocks`).
    pub fn keep_newest_from(&mut self, keep_from: MsgId) {
        let Some(mut last) = self.slices.pop() else {
            return;
        };
        last.messages = last.messages.split_off(&keep_from);
        if keep_from > last.range.from {
            last.range.from = keep_from;
        }
        self.slices.clear();
        self.slices.push(last);
    }

    /// Marks the bottom of history as no longer loaded (`SparseIdsList::invalidateBottom`).
    pub fn invalidate_bottom(&mut self) {
        if let Some(last) = self.slices.last_mut()
            && last.range.till == SERVER_MAX_MSG_ID
        {
            last.range.till = last.messages.last().copied().unwrap_or(last.range.from);
        }
        self.count = None;
    }

    /// Queries loaded IDs around `around_id` (`SparseIdsList::snapshot`).
    pub fn snapshot(
        &self,
        around_id: MsgId,
        limit_before: usize,
        limit_after: usize,
    ) -> SparseIdsSnapshot {
        if around_id.is_zero() {
            return SparseIdsSnapshot {
                count: self.count,
                ..Default::default()
            };
        }
        let idx = self.slices.partition_point(|s| s.range.till < around_id);
        let Some(slice) = self.slices.get(idx).filter(|s| s.range.from <= around_id) else {
            return SparseIdsSnapshot {
                count: self.count,
                ..Default::default()
            };
        };

        let all: Vec<MsgId> = slice.messages.iter().copied().collect();
        let pos = all.partition_point(|&id| id < around_id);
        let have_before = pos;
        let have_equal_or_after = all.len() - pos;
        let before = have_before.min(limit_before);
        let equal_or_after = have_equal_or_after.min(limit_after.saturating_add(1));

        let mut result = SparseIdsSnapshot {
            count: self.count,
            skipped_before: None,
            skipped_after: None,
            message_ids: all[(pos - before)..(pos + equal_or_after)].to_vec(),
        };
        if slice.range.from == MsgId::ZERO {
            result.skipped_before = Some((have_before - before) as i32);
        }
        if slice.range.till == SERVER_MAX_MSG_ID {
            result.skipped_after = Some((have_equal_or_after - equal_or_after) as i32);
        }
        if let Some(total) = self.count {
            let returned = result.message_ids.len() as i32;
            if result.skipped_before.is_none() {
                if let Some(after) = result.skipped_after {
                    result.skipped_before = Some((total - after - returned).max(0));
                }
            } else if result.skipped_after.is_none()
                && let Some(before_skip) = result.skipped_before
            {
                result.skipped_after = Some((total - before_skip - returned).max(0));
            }
        }
        result
    }

    /// Counts messages strictly after `till_id` satisfying `predicate` if the
    /// bottom slice covers `till_id..SERVER_MAX_MSG_ID` (`SparseIdsList::countAfter`).
    pub fn count_after<F>(&self, till_id: MsgId, limit: usize, mut predicate: F) -> Option<i32>
    where
        F: FnMut(MsgId) -> bool,
    {
        let bottom = self.slices.last()?;
        if bottom.range.till != SERVER_MAX_MSG_ID {
            return None;
        }
        if bottom.range.from > till_id && bottom.range.from != MsgId::ZERO {
            return None;
        }
        let after_iter = bottom.messages.range((
            std::ops::Bound::Excluded(till_id),
            std::ops::Bound::Unbounded,
        ));
        let mut total_after = 0usize;
        let mut matched = 0i32;
        for &id in after_iter {
            total_after += 1;
            if total_after > limit {
                return None;
            }
            if predicate(id) {
                matched += 1;
            }
        }
        Some(matched)
    }
}

/// Webpage preview draft options (`Data::WebPageDraft`).
#[derive(Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct WebPageDraft {
    /// Webpage ID (`WebPageId::ZERO` if none).
    pub id: WebPageId,
    /// Preview URL.
    pub url: String,
    /// Force large media preview.
    pub force_large_media: bool,
    /// Force small media preview.
    pub force_small_media: bool,
    /// Invert media above text.
    pub invert: bool,
    /// Manually selected preview.
    pub manual: bool,
    /// Preview explicitly dismissed by user.
    pub removed: bool,
}

/// Local or cloud message draft (`Data::Draft`).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Draft {
    /// Last modification timestamp.
    pub date: TimeId,
    /// Draft text with formatting entities.
    pub text: TextWithEntities,
    /// Optional reply / topic / quote context (or target message ID for edit drafts).
    pub reply: Option<ReplyInfo>,
    /// Webpage preview settings.
    pub webpage: WebPageDraft,
    /// Cursor position (UTF-16 offset).
    pub cursor_position: i32,
    /// Cursor selection anchor (UTF-16 offset).
    pub cursor_anchor: i32,
    /// Effect ID attached to draft (if any).
    pub effect_id: Option<crate::id::EffectId>,
}

impl Draft {
    /// Returns `true` if this draft has no visible text and no reply target.
    pub fn is_empty(&self) -> bool {
        self.text.text.trim().is_empty() && self.reply.is_none()
    }
}

/// Exact 64-bit draft slot key matching `Data::DraftKey` (`data/data_drafts.h`).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct DraftKey(i64);

impl DraftKey {
    const LOCAL_DRAFT_INDEX: i64 = -1;
    const CLOUD_DRAFT_INDEX: i64 = -2;
    const SCHEDULED_DRAFT_INDEX: i64 = -3;
    const WELCOME_MESSAGES_DRAFT_INDEX: i64 = -4;
    const MONOFORUM_DRAFT_BIT: i64 = 1_i64 << 60;
    const MONOFORUM_DRAFT_MASK: i64 = Self::MONOFORUM_DRAFT_BIT - 1;
    const EDIT_DRAFT_SHIFT: i64 = SERVER_MAX_MSG_ID.0;
    const CLOUD_DRAFT_SHIFT: i64 = 2 * SERVER_MAX_MSG_ID.0;
    const SHORTCUT_DRAFT_SHIFT: i64 = 3 * SERVER_MAX_MSG_ID.0;

    /// Empty / invalid draft key.
    pub const NONE: Self = Self(0);

    #[inline]
    const fn invalid(topic_root_id: MsgId, monoforum_peer_id: PeerId) -> bool {
        topic_root_id.0 < 0
            || topic_root_id.0 >= SERVER_MAX_MSG_ID.0
            || !monoforum_peer_id.is_user()
            || monoforum_peer_id.0 >= SERVER_MAX_MSG_ID.0
    }

    /// Local compose draft key (`DraftKey::Local`).
    pub const fn local(topic_root_id: MsgId, monoforum_peer_id: PeerId) -> Self {
        if Self::invalid(topic_root_id, monoforum_peer_id) {
            Self::NONE
        } else if !topic_root_id.is_zero() {
            Self(topic_root_id.0)
        } else if !monoforum_peer_id.is_zero() {
            Self(monoforum_peer_id.0 + Self::MONOFORUM_DRAFT_BIT)
        } else {
            Self(Self::LOCAL_DRAFT_INDEX)
        }
    }

    /// Local edit-message draft key (`DraftKey::LocalEdit`).
    pub const fn local_edit(topic_root_id: MsgId, monoforum_peer_id: PeerId) -> Self {
        if Self::invalid(topic_root_id, monoforum_peer_id) {
            Self::NONE
        } else {
            let base = if !topic_root_id.is_zero() {
                topic_root_id.0
            } else if !monoforum_peer_id.is_zero() {
                monoforum_peer_id.0 + Self::MONOFORUM_DRAFT_BIT
            } else {
                Self::LOCAL_DRAFT_INDEX
            };
            Self(Self::EDIT_DRAFT_SHIFT + base)
        }
    }

    /// Cloud-synced draft key (`DraftKey::Cloud`).
    pub const fn cloud(topic_root_id: MsgId, monoforum_peer_id: PeerId) -> Self {
        if Self::invalid(topic_root_id, monoforum_peer_id) {
            Self::NONE
        } else if !topic_root_id.is_zero() {
            Self(Self::CLOUD_DRAFT_SHIFT + topic_root_id.0)
        } else if !monoforum_peer_id.is_zero() {
            Self(Self::CLOUD_DRAFT_SHIFT + monoforum_peer_id.0 + Self::MONOFORUM_DRAFT_BIT)
        } else {
            Self(Self::CLOUD_DRAFT_INDEX)
        }
    }

    /// Scheduled messages compose draft key (`DraftKey::Scheduled`).
    pub const fn scheduled() -> Self {
        Self(Self::SCHEDULED_DRAFT_INDEX)
    }

    /// Scheduled messages edit draft key (`DraftKey::ScheduledEdit`).
    pub const fn scheduled_edit() -> Self {
        Self(Self::SCHEDULED_DRAFT_INDEX + Self::EDIT_DRAFT_SHIFT)
    }

    /// Welcome messages compose draft key (`DraftKey::WelcomeMessages`).
    pub const fn welcome_messages() -> Self {
        Self(Self::WELCOME_MESSAGES_DRAFT_INDEX)
    }

    /// Quick-reply shortcut compose draft key (`DraftKey::Shortcut`).
    pub const fn shortcut(shortcut_id: BusinessShortcutId) -> Self {
        if shortcut_id < 0 || (shortcut_id as i64) >= SERVER_MAX_MSG_ID.0 {
            Self::NONE
        } else {
            Self(Self::SHORTCUT_DRAFT_SHIFT + shortcut_id as i64)
        }
    }

    /// Deserializes from tdesktop's `qint64` storage key.
    pub const fn from_serialized(value: i64) -> Self {
        Self(value)
    }

    /// Serializes to tdesktop's `qint64` storage key.
    pub const fn serialize(self) -> i64 {
        self.0
    }

    /// Returns `true` if this is a local compose draft key.
    pub const fn is_local(self) -> bool {
        self.0 == Self::LOCAL_DRAFT_INDEX
            || (self.0 > 0 && (self.0 & Self::MONOFORUM_DRAFT_MASK) < SERVER_MAX_MSG_ID.0)
    }

    /// Returns `true` if this is a cloud draft key.
    pub const fn is_cloud(self) -> bool {
        let masked = self.0 & Self::MONOFORUM_DRAFT_MASK;
        self.0 == Self::CLOUD_DRAFT_INDEX
            || (masked > Self::CLOUD_DRAFT_SHIFT
                && masked < Self::CLOUD_DRAFT_SHIFT + SERVER_MAX_MSG_ID.0)
    }

    /// Extracts the topic root message ID if this key targets a forum topic.
    pub const fn topic_root_id(self) -> MsgId {
        let max = SERVER_MAX_MSG_ID.0;
        if (self.0 & Self::MONOFORUM_DRAFT_BIT) != 0 {
            MsgId::ZERO
        } else if self.0 > Self::CLOUD_DRAFT_SHIFT && self.0 < Self::CLOUD_DRAFT_SHIFT + max {
            MsgId(self.0 - Self::CLOUD_DRAFT_SHIFT)
        } else if self.0 > Self::EDIT_DRAFT_SHIFT && self.0 < Self::EDIT_DRAFT_SHIFT + max {
            MsgId(self.0 - Self::EDIT_DRAFT_SHIFT)
        } else if self.0 > 0 && self.0 < max {
            MsgId(self.0)
        } else {
            MsgId::ZERO
        }
    }

    /// Extracts the monoforum sublist peer ID if this key targets a monoforum sublist.
    pub const fn monoforum_peer_id(self) -> PeerId {
        let max = SERVER_MAX_MSG_ID.0;
        let value = self.0 & Self::MONOFORUM_DRAFT_MASK;
        if (self.0 & Self::MONOFORUM_DRAFT_BIT) == 0 {
            PeerId::ZERO
        } else if value > Self::CLOUD_DRAFT_SHIFT && value < Self::CLOUD_DRAFT_SHIFT + max {
            PeerId::from_user(UserId((value - Self::CLOUD_DRAFT_SHIFT) as u64))
        } else if value > Self::EDIT_DRAFT_SHIFT && value < Self::EDIT_DRAFT_SHIFT + max {
            PeerId::from_user(UserId((value - Self::EDIT_DRAFT_SHIFT) as u64))
        } else if value > 0 && value < max {
            PeerId::from_user(UserId(value as u64))
        } else {
            PeerId::ZERO
        }
    }
}

/// Per-peer message history and dialog state (`History` model layer).
#[derive(Clone, Debug, PartialEq)]
pub struct History {
    /// Owning peer ID.
    pub peer_id: PeerId,
    /// Folder assignment:
    /// - `None`: folder not yet known (`!folderKnown()`)
    /// - `Some(None)`: main chats list (`folderKnown() && folder() == nullptr`)
    /// - `Some(Some(id))`: inside folder `id` (e.g. `ARCHIVE_FOLDER_ID = 1`)
    pub folder: Option<Option<FolderId>>,
    /// Disjoint loaded server message ID ranges.
    pub loaded_ranges: SparseIdsList,
    /// First unread incoming message ID (`_inboxReadBefore = read_inbox_max_id + 1`).
    pub inbox_read_before: Option<MsgId>,
    /// First unread outgoing message ID (`_outboxReadBefore = read_outbox_max_id + 1`).
    pub outbox_read_before: Option<MsgId>,
    /// Server-reported or locally tracked unread message count (`None` = unknown).
    pub unread_count: Option<i32>,
    /// Explicit "Mark as Unread" flag (`dialog.unread_mark`).
    pub unread_mark: bool,
    /// Keeps chat visible in `NoRead` filters while currently open.
    pub fake_unread_while_opened: bool,
    /// Unread mentions count.
    pub unread_mentions_count: i32,
    /// Known unread mention message IDs.
    pub unread_mentions: BTreeSet<MsgId>,
    /// Unread reactions count.
    pub unread_reactions_count: i32,
    /// Known unread reaction message IDs.
    pub unread_reactions: BTreeSet<MsgId>,
    /// Unread poll votes count.
    pub unread_poll_votes_count: i32,
    /// Whether notifications for this dialog are muted.
    pub muted: bool,
    /// Latest known message ID shown in the dialog list.
    pub top_message_id: Option<MsgId>,
    /// Timestamp of the dialog in the chats list (`_timeId`).
    pub chat_list_time_id: TimeId,
    /// Cached 1-based pinned index per `FilterId` (`0` = main/archive list).
    pub pinned_indices: BTreeMap<FilterId, i32>,
    /// Local, edit, cloud, and scheduled drafts keyed by `DraftKey`.
    pub drafts: BTreeMap<DraftKey, Draft>,
    /// Whether this dialog is currently top-promoted (sponsored proxy channel).
    pub top_promoted: bool,
    /// Minimum available message ID (`available_min_id`).
    pub available_min_id: MsgId,
    /// Whether this history should appear in a chats list.
    pub in_chat_list: bool,
}

impl History {
    /// Creates a new empty `History` for `peer_id`.
    pub fn new(peer_id: PeerId) -> Self {
        Self {
            peer_id,
            folder: None,
            loaded_ranges: SparseIdsList::new(),
            inbox_read_before: None,
            outbox_read_before: None,
            unread_count: None,
            unread_mark: false,
            fake_unread_while_opened: false,
            unread_mentions_count: 0,
            unread_mentions: BTreeSet::new(),
            unread_reactions_count: 0,
            unread_reactions: BTreeSet::new(),
            unread_poll_votes_count: 0,
            muted: false,
            top_message_id: None,
            chat_list_time_id: 0,
            pinned_indices: BTreeMap::new(),
            drafts: BTreeMap::new(),
            top_promoted: false,
            available_min_id: MsgId::ZERO,
            in_chat_list: false,
        }
    }

    /// Returns `true` if folder membership has been established (`History::folderKnown`).
    #[inline]
    pub const fn folder_known(&self) -> bool {
        self.folder.is_some()
    }

    /// Returns the assigned folder ID, or `None` if unknown or in the main list.
    #[inline]
    pub const fn folder_id(&self) -> Option<FolderId> {
        match self.folder {
            Some(Some(id)) => Some(id),
            _ => None,
        }
    }

    /// Returns `true` if this history is in the archive folder (`ARCHIVE_FOLDER_ID`).
    #[inline]
    pub const fn is_archived(&self) -> bool {
        matches!(self.folder, Some(Some(ARCHIVE_FOLDER_ID)))
    }

    /// Assigns this history to `folder_id` (`None` = main list, `Some(1)` = archive).
    #[inline]
    pub fn set_folder(&mut self, folder_id: Option<FolderId>) {
        self.folder = Some(folder_id.filter(|&id| id != 0));
    }

    /// Returns `true` if `inbox_read_before` is known (`History::inboxReadTillKnown`).
    #[inline]
    pub const fn inbox_read_till_known(&self) -> bool {
        self.inbox_read_before.is_some()
    }

    /// Returns the highest read incoming message ID (`History::inboxReadTillId`).
    #[inline]
    pub const fn inbox_read_till_id(&self) -> MsgId {
        match self.inbox_read_before {
            Some(before) => MsgId(before.0 - 1),
            None => MsgId::ZERO,
        }
    }

    /// Returns the highest read outgoing message ID (`History::outboxReadTillId`).
    #[inline]
    pub const fn outbox_read_till_id(&self) -> MsgId {
        match self.outbox_read_before {
            Some(before) => MsgId(before.0 - 1),
            None => MsgId::ZERO,
        }
    }

    /// Advances the inbox read watermark to `up_to` and updates `unread_count`
    /// (`History::inboxRead` / `History::applyInboxReadUpdate`, `history/history.cpp:2266-2304`).
    pub fn inbox_read(&mut self, up_to: MsgId, still_unread: Option<i32>) {
        self.inbox_read_with_filter(up_to, still_unread, |_| true);
    }

    /// Advances the inbox read watermark to `up_to`, ignoring stale updates where
    /// `up_to <= inbox_read_till_id()` (`history/history.cpp:2277`, `History::applyInboxReadUpdate`),
    /// and filtering local unread message counts via `is_unread_candidate`
    /// (`history/history.cpp:2251-2264`, `History::countStillUnreadLocalFromMessages`).
    pub fn inbox_read_with_filter<F>(
        &mut self,
        up_to: MsgId,
        still_unread: Option<i32>,
        is_unread_candidate: F,
    ) where
        F: FnMut(MsgId) -> bool,
    {
        let next_before = MsgId(up_to.0 + 1);
        if let Some(current) = self.inbox_read_before
            && current >= next_before
        {
            // Stale or already-applied read watermark (`_inboxReadBefore > upTo`):
            // do not overwrite `unread_count` with an outdated `still_unread_count`.
            return;
        }
        self.inbox_read_before = Some(next_before);
        if let Some(remaining) = still_unread {
            let clamped = remaining.max(0);
            self.unread_count = Some(clamped);
            if clamped == 0 {
                self.unread_mark = false;
            }
        } else if let Some(counted) =
            self.loaded_ranges
                .count_after(up_to, 1000, is_unread_candidate)
        {
            self.unread_count = Some(counted);
            if counted == 0 {
                self.unread_mark = false;
            }
        }
    }

    /// Advances the outbox read watermark to `up_to` (`History::outboxRead`).
    pub fn outbox_read(&mut self, up_to: MsgId) {
        let next_before = MsgId(up_to.0 + 1);
        match self.outbox_read_before {
            Some(current) if current >= next_before => {}
            _ => self.outbox_read_before = Some(next_before),
        }
    }

    /// Returns `true` if `message` is server-side unread (`History::isServerSideUnread`).
    pub fn is_server_side_unread(&self, message: &Message) -> bool {
        if !message.is_regular() {
            return false;
        }
        if message.is_outgoing() {
            message.id > self.outbox_read_till_id()
        } else {
            message.id > self.inbox_read_till_id()
        }
    }

    /// Returns the 1-based pinned index for `filter_id`, or `0` if unpinned.
    #[inline]
    pub fn pinned_index(&self, filter_id: FilterId) -> i32 {
        self.pinned_indices.get(&filter_id).copied().unwrap_or(0)
    }

    /// Returns `true` if pinned in `filter_id`.
    #[inline]
    pub fn is_pinned(&self, filter_id: FilterId) -> bool {
        self.pinned_index(filter_id) != 0
    }

    /// Caches a 1-based pinned index (`0` removes the pin) (`Entry::cachePinnedIndex`).
    pub fn set_pinned_index(&mut self, filter_id: FilterId, index: i32) -> bool {
        let was = self.pinned_index(filter_id);
        if was == index {
            return false;
        }
        if index <= 0 {
            self.pinned_indices.remove(&filter_id);
        } else {
            self.pinned_indices.insert(filter_id, index);
        }
        true
    }

    /// Returns the fixed-on-top priority index (`0` unless top-promoted).
    #[inline]
    pub const fn fixed_on_top_index(&self) -> i32 {
        if self.top_promoted {
            crate::dialogs::TOP_PROMOTION_FIX_ON_TOP_INDEX
        } else {
            0
        }
    }

    /// Returns the effective dialog timestamp, taking into account the main local/cloud
    /// draft timestamp if newer (`History::adjustedChatListTimeId`).
    pub fn adjusted_chat_list_time_id(&self) -> TimeId {
        let mut date = self.chat_list_time_id;
        let local_key = DraftKey::local(MsgId::ZERO, PeerId::ZERO);
        if let Some(draft) = self.drafts.get(&local_key)
            && !draft.is_empty()
            && draft.date > date
        {
            date = draft.date;
        }
        let cloud_key = DraftKey::cloud(MsgId::ZERO, PeerId::ZERO);
        if let Some(draft) = self.drafts.get(&cloud_key)
            && !draft.is_empty()
            && draft.date > date
        {
            date = draft.date;
        }
        date
    }

    /// Computes this history's `UnreadState` (`History::computeUnreadState` in `history.cpp`).
    pub fn unread_state(&self) -> UnreadState {
        let count = self.unread_count.unwrap_or(0).max(0);
        let mark = count == 0 && self.unread_mark;
        let has_mentions = self.unread_mentions_count > 0 || !self.unread_mentions.is_empty();
        let has_reactions = self.unread_reactions_count > 0 || !self.unread_reactions.is_empty();
        let has_polls = self.unread_poll_votes_count > 0;
        let muted = self.muted;

        let messages = count;
        let chats = i32::from(count > 0);
        let marks = i32::from(mark);
        let mentions = i32::from(has_mentions);
        let reactions = i32::from(has_reactions);
        let polls = i32::from(has_polls);

        UnreadState {
            messages,
            messages_muted: if muted { messages } else { 0 },
            chats,
            chats_muted: if muted { chats } else { 0 },
            marks,
            marks_muted: if muted { marks } else { 0 },
            reactions,
            reactions_muted: if muted { reactions } else { 0 },
            mentions,
            polls,
            polls_muted: if muted { polls } else { 0 },
            known: self.unread_count.is_some(),
        }
    }

    /// Computes this history's row `BadgesState` (`History::computeBadgesState`
    /// + `History::adjustBadgesStateByFolder`).
    pub fn badges_state(&self) -> BadgesState {
        let mut state = badges_for_unread(
            &self.unread_state(),
            CountInBadge::Messages,
            IncludeInBadge::All,
            BadgeSettings::default(),
        );
        if self.folder_id().is_some() {
            state.mention_muted = true;
            state.reaction_muted = true;
            state.poll_muted = true;
            state.unread_muted = true;
        }
        state
    }

    /// Computes the `[min_server_id, max_server_id + 1]` range covering loaded messages
    /// for `getChannelDifference` range validation (`History::rangeForDifferenceRequest`).
    pub fn range_for_difference_request(&self) -> MsgRange {
        let Some(min_id) = self.loaded_ranges.min_id().filter(|id| id.0 > 0) else {
            return MsgRange::default();
        };
        let Some(max_id) = self.loaded_ranges.max_id().filter(|id| id.0 > 0) else {
            return MsgRange::default();
        };
        MsgRange::new(min_id, MsgId(max_id.0 + 1))
    }

    /// Sets a draft under `key` (or removes it if `draft` is empty).
    pub fn set_draft(&mut self, key: DraftKey, draft: Draft) {
        if draft.is_empty() {
            self.drafts.remove(&key);
        } else {
            self.drafts.insert(key, draft);
        }
    }

    /// Removes the draft under `key`.
    pub fn clear_draft(&mut self, key: DraftKey) -> Option<Draft> {
        self.drafts.remove(&key)
    }

    /// Returns the draft under `key`, if any.
    pub fn draft(&self, key: DraftKey) -> Option<&Draft> {
        self.drafts.get(&key)
    }

    /// Drops any loaded message IDs `<= available_min_id` (`History::clearUpTill`).
    pub fn clear_up_till(&mut self, available_min_id: MsgId) {
        if available_min_id > self.available_min_id {
            self.available_min_id = available_min_id;
        }
        let to_remove: Vec<MsgId> = self
            .loaded_ranges
            .slices()
            .iter()
            .flat_map(|s| s.messages.iter().copied())
            .filter(|&id| id <= available_min_id)
            .collect();
        for id in to_remove {
            self.loaded_ranges.remove_one(id, true);
        }
        self.unread_mentions.retain(|&id| id > available_min_id);
        self.unread_reactions.retain(|&id| id > available_min_id);
    }
}
