//! Port of `data/data_saved_messages.h`, `data/data_saved_messages.cpp`,
//! `data/data_saved_sublist.h`, `data/data_saved_sublist.cpp`, and saved reaction tags
//! state from `data/data_message_reactions.cpp`.
//!
//! Tracks Saved Messages sublists (and monoforum channel sublists), pagination offsets,
//! pinned sublist ordering, monoforum per-sublist inbox/outbox read state, and
//! hash-cached Saved Messages reaction tags.

use std::collections::{BTreeSet, HashMap, HashSet};

use crate::history::{Draft, SparseIdsList};
use crate::id::{ChannelId, MsgId, PeerId, TimeId};
use crate::peer::ReactionId;

/// First page limit for `messages.getSavedDialogs` (`kListFirstPerPage` in `data_saved_messages.cpp:33`).
pub const SAVED_LIST_FIRST_PER_PAGE: i32 = 20;

/// Subsequent page limit for `messages.getSavedDialogs` (`kListPerPage` in `data_saved_messages.cpp:32`).
pub const SAVED_LIST_PER_PAGE: i32 = 100;

/// Page size for `messages.getSavedHistory` (`kMessagesPerPage` in `data_saved_sublist.cpp`).
pub const SAVED_MESSAGES_PER_PAGE: i32 = 50;

/// Maximum stale sublist peer IDs per `messages.getSavedDialogsByID`
/// (`kStalePerRequest` in `data_saved_messages.cpp:36`).
pub const SAVED_STALE_PER_REQUEST: usize = 100;

/// Delay before refreshing Saved Messages reaction tags after `updateSavedReactionTags`
/// (`kMyTagsRequestTimeout` in `data_message_reactions.cpp:53`).
pub const MY_TAGS_REQUEST_TIMEOUT_MS: u64 = 1_000;

/// Pagination cursor for `messages.getSavedDialogs` (`SavedMessages::Offset`).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct SavedDialogsOffset {
    /// Top message date of the last sublist in the previous slice (`0` on first load).
    pub date: TimeId,
    /// Top message ID of the last sublist in the previous slice (`0` on first load).
    pub id: MsgId,
    /// Peer ID of the last sublist (`PeerId::ZERO` on first load -> `InputPeer::Empty`).
    pub peer_id: PeerId,
}

/// Domain model for one Saved Messages or monoforum sublist (`Data::SavedSublist`).
#[derive(Clone, Debug, PartialEq)]
pub struct SavedSublistData {
    /// Owning monoforum channel peer ID, or `None` for the user's own Saved Messages.
    pub parent_peer_id: Option<PeerId>,
    /// Sublist peer ID (origin peer in Saved Messages, or sender peer in monoforum).
    pub peer_id: PeerId,
    /// Top server message ID in this sublist.
    pub top_message_id: Option<MsgId>,
    /// Timestamp of the top message in this sublist (`0` if unknown).
    pub last_message_date: TimeId,
    /// Whether this sublist is pinned (Saved Messages only).
    pub pinned: bool,
    /// Whether to restore pinned flag once the sublist becomes non-empty
    /// (`_restorePinnedWhenNonEmpty` in `data_saved_messages.cpp:450`).
    pub restore_pinned_when_non_empty: bool,
    /// Explicit unread mark (monoforum only).
    pub unread_mark: bool,
    /// Paid-messages exemption flag (monoforum only).
    pub nopaid_messages_exception: bool,
    /// Highest incoming message ID read (monoforum only).
    pub read_inbox_max_id: MsgId,
    /// Highest outgoing message ID read (monoforum only).
    pub read_outbox_max_id: MsgId,
    /// Unread message count (monoforum only).
    pub unread_count: Option<i32>,
    /// Unread reactions count (monoforum only).
    pub unread_reactions_count: i32,
    /// Cloud draft for this sublist (monoforum only).
    pub draft: Option<Draft>,
    /// Loaded server message IDs in this sublist.
    pub messages: SparseIdsList,
}

impl SavedSublistData {
    /// Creates a new `SavedSublistData` for `peer_id` under `parent_peer_id`.
    #[must_use]
    pub fn new(parent_peer_id: Option<PeerId>, peer_id: PeerId) -> Self {
        Self {
            parent_peer_id,
            peer_id,
            top_message_id: None,
            last_message_date: 0,
            pinned: false,
            restore_pinned_when_non_empty: false,
            unread_mark: false,
            nopaid_messages_exception: false,
            read_inbox_max_id: MsgId::ZERO,
            read_outbox_max_id: MsgId::ZERO,
            unread_count: None,
            unread_reactions_count: 0,
            draft: None,
            messages: SparseIdsList::new(),
        }
    }

    /// Updates monoforum `read_inbox_max_id` monotonically (`SavedSublist::setInboxReadTill`).
    pub fn apply_inbox_read_till(&mut self, max_id: MsgId, unread_count: Option<i32>) {
        if max_id > self.read_inbox_max_id {
            self.read_inbox_max_id = max_id;
        }
        if let Some(count) = unread_count {
            self.unread_count = Some(count.max(0));
        } else if self
            .top_message_id
            .is_some_and(|top| self.read_inbox_max_id >= top)
        {
            self.unread_count = Some(0);
        }
    }

    /// Updates monoforum `read_outbox_max_id` monotonically (`SavedSublist::setOutboxReadTill`).
    pub fn apply_outbox_read_till(&mut self, max_id: MsgId) {
        if max_id > self.read_outbox_max_id {
            self.read_outbox_max_id = max_id;
        }
    }
}

/// Registry of Saved Messages sublists or monoforum sublists (`Data::SavedMessages`).
#[derive(Clone, Debug, Default)]
pub struct SavedMessagesState {
    /// Owning monoforum channel ID, or `None` for the user's Saved Messages.
    pub parent_chat: Option<ChannelId>,
    /// Set `true` when the server returns `SAVED_DIALOGS_UNSUPPORTED`
    /// (`SavedMessages::markUnsupported` in `data_saved_messages.cpp:127`).
    pub unsupported: bool,
    /// Loaded sublists keyed by sublist `PeerId`.
    pub sublists: HashMap<PeerId, SavedSublistData>,
    /// Ordered pinned sublist peer IDs (Saved Messages only; unused for monoforum).
    pub pinned_order: Vec<PeerId>,
    /// Whether `messages.getPinnedSavedDialogs` has completed at least once.
    pub pinned_loaded: bool,
    /// Whether all unpinned sublist pages have been loaded.
    pub all_loaded: bool,
    /// Current pagination offset (`_offset`).
    pub offset: SavedDialogsOffset,
    /// Stale sublist peer IDs queued for `messages.getSavedDialogsByID` (monoforum only).
    pub stale_peers: BTreeSet<PeerId>,
}

impl SavedMessagesState {
    /// Creates a new `SavedMessagesState` (`parent_chat = None` for own Saved Messages).
    #[must_use]
    pub fn new(parent_chat: Option<ChannelId>) -> Self {
        Self {
            parent_chat,
            ..Self::default()
        }
    }

    /// Returns `true` if Saved Messages dialogs are supported by the server.
    #[inline]
    #[must_use]
    pub const fn supported(&self) -> bool {
        !self.unsupported
    }

    /// Marks Saved Messages dialogs unsupported (`SAVED_DIALOGS_UNSUPPORTED`).
    pub fn mark_unsupported(&mut self) {
        self.unsupported = true;
        self.all_loaded = true;
    }

    /// Returns a loaded sublist for `peer_id`.
    #[must_use]
    pub fn sublist(&self, peer_id: PeerId) -> Option<&SavedSublistData> {
        self.sublists.get(&peer_id)
    }

    /// Returns a mutable reference to a loaded sublist for `peer_id`.
    pub fn sublist_mut(&mut self, peer_id: PeerId) -> Option<&mut SavedSublistData> {
        self.sublists.get_mut(&peer_id)
    }

    /// Ensures a sublist exists for `peer_id`.
    pub fn ensure_sublist(&mut self, peer_id: PeerId) -> &mut SavedSublistData {
        self.stale_peers.remove(&peer_id);
        let parent_peer = self.parent_chat.map(PeerId::from_channel);
        self.sublists
            .entry(peer_id)
            .or_insert_with(|| SavedSublistData::new(parent_peer, peer_id))
    }

    /// Removes a deleted sublist (`SavedMessages::applySublistDeleted`).
    pub fn apply_sublist_deleted(&mut self, peer_id: PeerId) -> bool {
        self.stale_peers.remove(&peer_id);
        self.pinned_order.retain(|&p| p != peer_id);
        self.sublists.remove(&peer_id).is_some()
    }

    /// Applies `updateSavedDialogPinned` (`data_saved_messages.cpp:505-522`).
    ///
    /// Returns `true` if `peer_id` was loaded and updated, or `false` if unloaded (caller
    /// must invoke `messages.getPinnedSavedDialogs` via `loadPinned()`).
    pub fn set_sublist_pinned(&mut self, peer_id: PeerId, pinned: bool) -> bool {
        let Some(sublist) = self.sublists.get_mut(&peer_id) else {
            return false;
        };
        sublist.pinned = pinned;
        if pinned {
            if !self.pinned_order.contains(&peer_id) {
                self.pinned_order.insert(0, peer_id);
            }
        } else {
            self.pinned_order.retain(|&p| p != peer_id);
        }
        true
    }

    /// Applies `updatePinnedSavedDialogs` (`data_saved_messages.cpp:474-503`).
    ///
    /// Returns `false` if `order` is `None` or references any peer not currently loaded in
    /// `self.sublists`, signalling the caller to run `messages.getPinnedSavedDialogs`.
    pub fn apply_pinned_order(&mut self, order: Option<&[PeerId]>) -> bool {
        let Some(peers) = order else {
            return false;
        };
        if peers.iter().any(|p| !self.sublists.contains_key(p)) {
            return false;
        }
        let pinned_set: HashSet<PeerId> = peers.iter().copied().collect();
        for (&peer_id, sublist) in &mut self.sublists {
            sublist.pinned = pinned_set.contains(&peer_id);
        }
        self.pinned_order = peers.to_vec();
        true
    }

    /// Reconciles local pinned sublists against the authoritative list returned by
    /// `messages.getPinnedSavedDialogs` (`data_saved_messages.cpp:444-455`).
    pub fn reconcile_pinned_list(&mut self, server_pinned: &[PeerId]) {
        let server_set: HashSet<PeerId> = server_pinned.iter().copied().collect();
        for (&peer_id, sublist) in &mut self.sublists {
            if sublist.pinned && !server_set.contains(&peer_id) {
                if self.parent_chat.is_none() && sublist.top_message_id.is_none() {
                    sublist.restore_pinned_when_non_empty = true;
                }
                sublist.pinned = false;
            } else if server_set.contains(&peer_id) {
                sublist.pinned = true;
            }
        }
        self.pinned_order = server_pinned.to_vec();
        self.pinned_loaded = true;
    }

    /// Pops up to [`SAVED_STALE_PER_REQUEST`] (100) stale sublist peers for
    /// `messages.getSavedDialogsByID`.
    pub fn take_stale_batch(&mut self) -> Vec<PeerId> {
        let mut out = Vec::with_capacity(self.stale_peers.len().min(SAVED_STALE_PER_REQUEST));
        while out.len() < SAVED_STALE_PER_REQUEST {
            let Some(peer) = self.stale_peers.pop_first() else {
                break;
            };
            out.push(peer);
        }
        out
    }

    /// Returns sublist peer IDs sorted in display order: pinned sublists first in
    /// `pinned_order`, then unpinned sublists descending by `last_message_date`.
    #[must_use]
    pub fn sorted_sublist_peers(&self) -> Vec<PeerId> {
        let mut peers: Vec<PeerId> = self.sublists.keys().copied().collect();
        let pin_pos = |pid: PeerId| self.pinned_order.iter().position(|&p| p == pid);
        peers.sort_by(|&a, &b| {
            let sa = &self.sublists[&a];
            let sb = &self.sublists[&b];
            match (pin_pos(a), pin_pos(b)) {
                (Some(pa), Some(pb)) => pa.cmp(&pb),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => sb
                    .last_message_date
                    .cmp(&sa.last_message_date)
                    .then_with(|| sb.top_message_id.cmp(&sa.top_message_id)),
            }
        });
        peers
    }
}

/// One Saved Messages reaction tag (`SavedReactionTag` in `data_message_reactions.cpp`).
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct SavedReactionTagData {
    /// Reaction emoji or custom emoji ID.
    pub reaction: ReactionId,
    /// Optional user-defined label (up to 12 chars).
    pub title: Option<String>,
    /// Number of saved messages carrying this tag.
    pub count: i32,
}

/// Per-sublist cache entry for `messages.getSavedReactionTags`.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SavedReactionTagsBucket {
    /// Server-returned 64-bit hash passed back on subsequent `messages.getSavedReactionTags`.
    pub hash: i64,
    /// Current list of reaction tags.
    pub tags: Vec<SavedReactionTagData>,
    /// Set `true` when `updateSavedReactionTags` signals a refresh is needed.
    pub dirty: bool,
}

/// State store for Saved Messages reaction tags (`Reactions::_myTags` in `data_message_reactions.cpp`).
#[derive(Clone, Debug, Default)]
pub struct SavedReactionTagsState {
    /// Keyed by `None` (all Saved Messages) or `Some(sublist_peer_id)`.
    buckets: HashMap<Option<PeerId>, SavedReactionTagsBucket>,
}

impl SavedReactionTagsState {
    /// Returns the stored hash for `sublist_peer` (`0` if never fetched or reset on error).
    #[must_use]
    pub fn hash_for(&self, sublist_peer: Option<PeerId>) -> i64 {
        self.buckets.get(&sublist_peer).map_or(0, |b| b.hash)
    }

    /// Returns the cached tags slice for `sublist_peer`.
    #[must_use]
    pub fn tags_for(&self, sublist_peer: Option<PeerId>) -> &[SavedReactionTagData] {
        self.buckets
            .get(&sublist_peer)
            .map_or(&[], |b| b.tags.as_slice())
    }

    /// Returns `true` if `sublist_peer` was invalidated by `updateSavedReactionTags`.
    #[must_use]
    pub fn is_dirty(&self, sublist_peer: Option<PeerId>) -> bool {
        self.buckets.get(&sublist_peer).is_some_and(|b| b.dirty)
    }

    /// Applies a non-modified or full `messages.savedReactionTags` response.
    pub fn apply_tags(
        &mut self,
        sublist_peer: Option<PeerId>,
        hash: i64,
        tags: Vec<SavedReactionTagData>,
    ) {
        let bucket = self.buckets.entry(sublist_peer).or_default();
        bucket.hash = hash;
        bucket.tags = tags;
        bucket.dirty = false;
    }

    /// Clears the `dirty` flag after receiving `messages.savedReactionTagsNotModified`.
    pub fn mark_not_modified(&mut self, sublist_peer: Option<PeerId>) {
        if let Some(bucket) = self.buckets.get_mut(&sublist_peer) {
            bucket.dirty = false;
        }
    }

    /// Resets the stored hash to `0` after an RPC failure
    /// (`Reactions::requestMyTags` fail handler in `data_message_reactions.cpp:1075`).
    pub fn reset_hash_on_error(&mut self, sublist_peer: Option<PeerId>) {
        self.buckets.entry(sublist_peer).or_default().hash = 0;
    }

    /// Marks the global tags bucket (and any loaded sublist buckets) dirty upon
    /// `updateSavedReactionTags` (`api_updates.cpp:2879-2881`).
    pub fn mark_invalidated(&mut self) {
        self.buckets.entry(None).or_default().dirty = true;
        for bucket in self.buckets.values_mut() {
            bucket.dirty = true;
        }
    }

    /// Applies a local tag title rename across all loaded buckets
    /// (`Reactions::renameTag` in `data_message_reactions.cpp:562-575`).
    ///
    /// Returns `true` if any loaded tag title changed.
    pub fn rename_tag(&mut self, reaction: &ReactionId, title: Option<String>) -> bool {
        let normalized = title.filter(|t| !t.is_empty());
        let mut changed = false;
        for bucket in self.buckets.values_mut() {
            for tag in &mut bucket.tags {
                if &tag.reaction == reaction && tag.title != normalized {
                    tag.title = normalized.clone();
                    changed = true;
                }
            }
        }
        changed
    }
}
