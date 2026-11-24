//! Port of `api/api_hash.h`, `data/components/scheduled_messages.cpp`, and
//! `data/business/data_shortcut_messages.cpp` (model, ID mapping, hash algorithms,
//! and 60-second request throttling).

use std::collections::{BTreeMap, HashMap};

use crate::id::{
    BusinessShortcutId, MsgId, PeerId, SCHEDULED_MAX_MSG_ID, SERVER_MAX_MSG_ID, TimeId,
};
use crate::message::{Message, MessageFlags};

/// Special `schedule_date` value meaning "send when recipient comes online"
/// (`Api::kScheduledUntilOnlineTimestamp` in `api/api_common.h:21`).
pub const SCHEDULED_UNTIL_ONLINE_TIMESTAMP: TimeId = 0x7FFF_FFFE;

/// Throttle interval between repeated `messages.getScheduledHistory` or
/// `messages.getQuickReplyMessages` requests (`kRequestTimeLimit` = 60 seconds).
pub const SCHEDULED_REQUEST_TIME_LIMIT_MS: u64 = 60_000;

/// Throttle interval for `messages.getQuickReplyMessages` (`kRequestTimeLimit` = 60 seconds).
pub const SHORTCUT_REQUEST_TIME_LIMIT_MS: u64 = 60_000;

// ============================================================================
// Exact Telegram 64-bit list hash (`api/api_hash.h:36-70`)
// ============================================================================

/// Initializes Telegram's 64-bit list hash accumulator (`Api::HashInit`).
#[inline]
#[must_use]
pub const fn hash_init() -> u64 {
    0
}

/// Mixes a `u64` into Telegram's 64-bit list hash accumulator (`Api::HashUpdate(uint64&, uint64)`).
///
/// Exact C++ algorithm (`api/api_hash.h:40-45`):
/// ```cpp
/// already ^= (already >> 21);
/// already ^= (already << 35);
/// already ^= (already >> 4);
/// already += value;
/// ```
#[inline]
pub const fn hash_update_u64(already: &mut u64, value: u64) {
    *already ^= *already >> 21;
    *already ^= *already << 35;
    *already ^= *already >> 4;
    *already = already.wrapping_add(value);
}

/// Mixes an `i64` into Telegram's 64-bit list hash accumulator (`Api::HashUpdate(uint64&, int64)`).
#[inline]
pub const fn hash_update_i64(already: &mut u64, value: i64) {
    hash_update_u64(already, value as u64);
}

/// Mixes a `u32` into Telegram's 64-bit list hash accumulator (`Api::HashUpdate(uint64&, uint32)`).
#[inline]
pub const fn hash_update_u32(already: &mut u64, value: u32) {
    hash_update_u64(already, value as u64);
}

/// Mixes an `i32` into Telegram's 64-bit list hash accumulator (`Api::HashUpdate(uint64&, int32)`).
///
/// Matches C++ `HashUpdate(already, int64(value))` — sign-extends `i32` to `i64` before
/// casting to `u64`.
#[inline]
pub const fn hash_update_i32(already: &mut u64, value: i32) {
    hash_update_i64(already, value as i64);
}

/// Finalizes Telegram's 64-bit list hash (`Api::HashFinalize`).
#[inline]
#[must_use]
pub const fn hash_finalize(already: u64) -> u64 {
    already
}

/// Computes Telegram's 64-bit list hash over a sequence of `u64` values (`Api::CountHash`).
#[must_use]
pub fn count_hash<I: IntoIterator<Item = u64>>(values: I) -> u64 {
    let mut hash = hash_init();
    for value in values {
        hash_update_u64(&mut hash, value);
    }
    hash_finalize(hash)
}

// ============================================================================
// Scheduled & Quick-Reply Local <-> Remote Message ID Mapping
// ============================================================================

/// Maps a server-side scheduled message ID to its local `MsgId`
/// (`RemoteToLocalMsgId` in `data/components/scheduled_messages.cpp:28-32`).
#[inline]
#[must_use]
pub const fn scheduled_remote_to_local_id(remote_id: MsgId) -> Option<MsgId> {
    if remote_id.is_server() {
        Some(MsgId(SERVER_MAX_MSG_ID.0 + remote_id.0 + 1))
    } else {
        None
    }
}

/// Maps a local scheduled `MsgId` back to its server-side remote `MsgId`
/// (`LocalToRemoteMsgId` in `data/components/scheduled_messages.cpp:34-38`).
#[inline]
#[must_use]
pub const fn scheduled_local_to_remote_id(local_id: MsgId) -> Option<MsgId> {
    if local_id.is_scheduled() {
        let remote = local_id.0 - SERVER_MAX_MSG_ID.0 - 1;
        if remote > 0 && remote < SERVER_MAX_MSG_ID.0 {
            Some(MsgId(remote))
        } else {
            None
        }
    } else {
        None
    }
}

/// Maps a server-side quick-reply message ID to its local `MsgId`
/// (`RemoteToLocalMsgId` in `data/business/data_shortcut_messages.cpp:28-32`).
#[inline]
#[must_use]
pub const fn shortcut_remote_to_local_id(remote_id: MsgId) -> Option<MsgId> {
    if remote_id.is_server() {
        Some(MsgId(SCHEDULED_MAX_MSG_ID.0 + remote_id.0 + 1))
    } else {
        None
    }
}

/// Maps a local quick-reply `MsgId` back to its server-side remote `MsgId`
/// (`LocalToRemoteMsgId` in `data/business/data_shortcut_messages.cpp:34-38`).
#[inline]
#[must_use]
pub const fn shortcut_local_to_remote_id(local_id: MsgId) -> Option<MsgId> {
    if local_id.is_shortcut() {
        let remote = local_id.0 - SCHEDULED_MAX_MSG_ID.0 - 1;
        if remote > 0 && remote < SERVER_MAX_MSG_ID.0 {
            Some(MsgId(remote))
        } else {
            None
        }
    } else {
        None
    }
}

/// Returns `true` if `msg` has an active future scheduled date (not "until online" and > `now`)
/// (`HasScheduledDate` in `data/components/scheduled_messages.cpp:44-47`).
#[inline]
#[must_use]
pub const fn has_scheduled_date(date: TimeId, now: TimeId) -> bool {
    date != SCHEDULED_UNTIL_ONLINE_TIMESTAMP && date > now
}

// ============================================================================
// Scheduled Messages Store (`Data::ScheduledMessages`)
// ============================================================================

/// Per-peer scheduled message list.
#[derive(Clone, Debug, Default)]
struct ScheduledPeerList {
    items: Vec<Message>,
    /// Remote server ID (`1..SERVER_MAX_MSG_ID`) -> local scheduled ID.
    remote_to_local: HashMap<MsgId, MsgId>,
}

impl ScheduledPeerList {
    fn sort(&mut self) {
        self.items.sort_by_key(Message::position);
    }
}

/// Pair of `(deleted_remote_scheduled_id, optional_sent_regular_msg_id)` produced by
/// `updateDeleteScheduledMessages`.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct DeletedScheduledInfo {
    /// Original server-side scheduled message ID.
    pub remote_id: MsgId,
    /// Local scheduled message ID (`SERVER_MAX_MSG_ID + remote_id + 1`).
    pub local_id: MsgId,
    /// Corresponding regular history message ID if the message was sent rather than cancelled.
    pub sent_id: Option<MsgId>,
}

/// Account-wide scheduled messages registry (`Data::ScheduledMessages`).
#[derive(Clone, Debug, Default)]
pub struct ScheduledMessagesStore {
    peers: HashMap<PeerId, ScheduledPeerList>,
    last_received_ms: HashMap<PeerId, u64>,
}

impl ScheduledMessagesStore {
    /// Returns `true` if `messages.getScheduledHistory` for `peer_id` is still within the
    /// 60-second `kRequestTimeLimit` cooldown (`TooEarlyForRequest` in
    /// `data/components/scheduled_messages.cpp:40-42`).
    #[must_use]
    pub fn too_early_for_request(&self, peer_id: PeerId, now_ms: u64) -> bool {
        self.last_received_ms
            .get(&peer_id)
            .is_some_and(|&received| {
                received > 0 && received.saturating_add(SCHEDULED_REQUEST_TIME_LIMIT_MS) > now_ms
            })
    }

    /// Marks `peer_id`'s scheduled history as received at `now_ms`.
    pub fn mark_received(&mut self, peer_id: PeerId, now_ms: u64) {
        self.last_received_ms.insert(peer_id, now_ms);
    }

    /// Clears the cooldown timer for `peer_id` (e.g. on RPC failure per
    /// `data/components/scheduled_messages.cpp:506-508`).
    pub fn clear_request_state(&mut self, peer_id: PeerId) {
        self.last_received_ms.remove(&peer_id);
    }

    /// Returns all scheduled messages for `peer_id`, sorted by `(date, id)` ascending.
    #[must_use]
    pub fn list(&self, peer_id: PeerId) -> &[Message] {
        self.peers.get(&peer_id).map_or(&[], |l| l.items.as_slice())
    }

    /// Returns the number of scheduled messages stored for `peer_id`.
    #[must_use]
    pub fn count(&self, peer_id: PeerId) -> usize {
        self.peers.get(&peer_id).map_or(0, |l| l.items.len())
    }

    /// Looks up a scheduled message by `(peer_id, local_id)`.
    #[must_use]
    pub fn lookup_local(&self, peer_id: PeerId, local_id: MsgId) -> Option<&Message> {
        self.peers
            .get(&peer_id)?
            .items
            .iter()
            .find(|m| m.id == local_id)
    }

    /// Looks up a scheduled message by `(peer_id, remote_id)`.
    #[must_use]
    pub fn lookup_remote(&self, peer_id: PeerId, remote_id: MsgId) -> Option<&Message> {
        let list = self.peers.get(&peer_id)?;
        let &local_id = list.remote_to_local.get(&remote_id)?;
        list.items.iter().find(|m| m.id == local_id)
    }

    /// Computes tdesktop's exact `ScheduledMessages::countListHash` for `peer_id`
    /// (`data/components/scheduled_messages.cpp:657-676`).
    ///
    /// Iterates non-sending, non-failed items in **reverse** sorted order and hashes:
    /// 1. `remote_id.bare()` (`i64`)
    /// 2. `edit_date.unwrap_or(0)` (`i32`)
    /// 3. `date` (`i32`)
    #[must_use]
    pub fn count_list_hash(&self, peer_id: PeerId) -> i64 {
        let Some(list) = self.peers.get(&peer_id) else {
            return 0;
        };
        let mut hash = hash_init();
        for item in list.items.iter().rev() {
            if item.is_sending() || item.has_failed() {
                continue;
            }
            let Some(remote_id) = scheduled_local_to_remote_id(item.id) else {
                continue;
            };
            hash_update_i64(&mut hash, remote_id.bare());
            hash_update_i32(&mut hash, item.edit_date.unwrap_or(0));
            hash_update_i32(&mut hash, item.date);
        }
        hash_finalize(hash) as i64
    }

    /// Appends or updates one server-side scheduled message (converting its server `id` to
    /// its local scheduled `MsgId` and setting `MessageFlags::IS_OR_WAS_SCHEDULED`).
    ///
    /// Returns the local scheduled `MsgId` if valid.
    pub fn upsert_server_message(&mut self, peer_id: PeerId, mut msg: Message) -> Option<MsgId> {
        if msg.peer_id != peer_id {
            return None;
        }
        let remote_id = msg.id;
        let local_id = scheduled_remote_to_local_id(remote_id)?;
        msg.id = local_id;
        msg.flags.insert(MessageFlags::IS_OR_WAS_SCHEDULED);

        let list = self.peers.entry(peer_id).or_default();
        if let Some(existing) = list.items.iter_mut().find(|m| m.id == local_id) {
            *existing = msg;
        } else {
            list.items.push(msg);
            list.remote_to_local.insert(remote_id, local_id);
        }
        list.sort();
        Some(local_id)
    }

    /// Reconciles the full response from `messages.getScheduledHistory` for `peer_id`
    /// (`ScheduledMessages::parse` in `data/components/scheduled_messages.cpp:511-547`).
    ///
    /// Preserves locally sending items while removing server items absent from `messages`.
    pub fn apply_history_response(&mut self, peer_id: PeerId, messages: Vec<Message>, now_ms: u64) {
        self.last_received_ms.insert(peer_id, now_ms);
        if messages.is_empty() {
            if let Some(list) = self.peers.get_mut(&peer_id) {
                list.items.retain(|m| m.is_sending() || m.has_failed());
                list.remote_to_local.clear();
                if list.items.is_empty() {
                    self.peers.remove(&peer_id);
                }
            }
            return;
        }

        let mut received_locals = Vec::with_capacity(messages.len());
        for msg in messages {
            if let Some(local_id) = self.upsert_server_message(peer_id, msg) {
                received_locals.push(local_id);
            }
        }
        if let Some(list) = self.peers.get_mut(&peer_id) {
            list.items
                .retain(|m| m.is_sending() || received_locals.contains(&m.id));
            list.remote_to_local
                .retain(|_, local_id| received_locals.contains(local_id));
            list.sort();
            if list.items.is_empty() {
                self.peers.remove(&peer_id);
            }
        }
    }

    /// Applies `updateDeleteScheduledMessages` (`data/components/scheduled_messages.cpp:360-395`).
    ///
    /// Returns metadata for each deleted message that existed locally, including its paired
    /// `sent_messages[k]` regular history ID when present.
    pub fn apply_delete_scheduled(
        &mut self,
        peer_id: PeerId,
        remote_ids: &[MsgId],
        sent_messages: Option<&[MsgId]>,
    ) -> Vec<DeletedScheduledInfo> {
        let Some(list) = self.peers.get_mut(&peer_id) else {
            return Vec::new();
        };
        let mut deleted = Vec::new();
        for (idx, &remote_id) in remote_ids.iter().enumerate() {
            if let Some(local_id) = list.remote_to_local.remove(&remote_id) {
                list.items.retain(|m| m.id != local_id);
                let sent_id = sent_messages.and_then(|s| s.get(idx).copied());
                deleted.push(DeletedScheduledInfo {
                    remote_id,
                    local_id,
                    sent_id,
                });
            }
        }
        if list.items.is_empty() {
            self.peers.remove(&peer_id);
        }
        deleted
    }

    /// Given local or remote scheduled IDs for `peer_id`, returns the remote server IDs
    /// sorted by ascending scheduled `date` (matching `window_peer_menu.cpp:4064-4070` for
    /// `messages.sendScheduledMessages`).
    #[must_use]
    pub fn sorted_remote_ids_for_send(&self, peer_id: PeerId, ids: &[MsgId]) -> Vec<i32> {
        let mut pairs: Vec<(TimeId, i32)> = Vec::with_capacity(ids.len());
        for &id in ids {
            let (remote, date) = if let Some(remote_id) = scheduled_local_to_remote_id(id) {
                let d = self.lookup_local(peer_id, id).map_or(0, |m| m.date);
                (remote_id, d)
            } else {
                let d = self.lookup_remote(peer_id, id).map_or(0, |m| m.date);
                (id, d)
            };
            if let Some(raw) = remote.as_i32()
                && raw > 0
            {
                pairs.push((date, raw));
            }
        }
        pairs.sort_by_key(|&(date, _)| date);
        pairs.into_iter().map(|(_, id)| id).collect()
    }
}

// ============================================================================
// Business Quick-Reply Shortcuts Store (`Data::ShortcutMessages`)
// ============================================================================

/// Domain metadata for one Business quick-reply shortcut (`Data::Shortcut`).
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct QuickReplyShortcutData {
    /// Server-assigned positive shortcut ID (or negative local placeholder ID).
    pub id: BusinessShortcutId,
    /// Shortcut keyword name (without leading `/`).
    pub name: String,
    /// Local shortcut message ID of the top message (`SCHEDULED_MAX_MSG_ID + remote + 1`).
    pub top_message_id: MsgId,
    /// Number of messages in this shortcut.
    pub count: i32,
}

/// Notification that a local placeholder shortcut ID was replaced by a server ID
/// (or deleted, `new_id == 0`) (`Data::ShortcutIdChange`).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct ShortcutIdChange {
    /// Previous shortcut ID.
    pub old_id: BusinessShortcutId,
    /// Replacement shortcut ID (`0` if deleted).
    pub new_id: BusinessShortcutId,
}

#[derive(Clone, Debug, Default)]
struct ShortcutMessageList {
    items: Vec<Message>,
    remote_to_local: HashMap<MsgId, MsgId>,
}

impl ShortcutMessageList {
    fn sort(&mut self) {
        self.items.sort_by_key(Message::position);
    }
}

/// Account-wide Business quick-reply shortcuts & messages registry (`Data::ShortcutMessages`).
#[derive(Clone, Debug, Default)]
pub struct ShortcutMessagesStore {
    shortcuts: BTreeMap<BusinessShortcutId, QuickReplyShortcutData>,
    shortcuts_loaded: bool,
    shortcuts_hash: i64,
    next_local_shortcut_id: BusinessShortcutId,
    lists: HashMap<BusinessShortcutId, ShortcutMessageList>,
    last_received_ms: HashMap<BusinessShortcutId, u64>,
}

/// Result of checking a Business quick-reply shortcut rename for collisions
/// (`data_shortcut_messages.cpp:546-560`).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ShortcutRenameCollision {
    /// No conflicting shortcut name exists.
    None,
    /// An empty duplicate shortcut exists and should be deleted before renaming.
    ReplaceEmpty(BusinessShortcutId),
    /// A non-empty shortcut already occupies the requested name (`SHORTCUT_OCCUPIED`).
    Occupied,
}

impl ShortcutMessagesStore {
    /// Returns whether `messages.getQuickReplies` has completed at least once.
    #[inline]
    #[must_use]
    pub const fn shortcuts_loaded(&self) -> bool {
        self.shortcuts_loaded
    }

    /// Returns the stored hash for `messages.getQuickReplies`.
    #[inline]
    #[must_use]
    pub const fn shortcuts_hash(&self) -> i64 {
        self.shortcuts_hash
    }

    /// Sets the stored hash for `messages.getQuickReplies`.
    pub fn set_shortcuts_hash(&mut self, hash: i64) {
        self.shortcuts_hash = hash;
    }

    /// Marks shortcuts loaded (e.g. on `messages.quickRepliesNotModified`).
    pub fn mark_shortcuts_loaded(&mut self) {
        self.shortcuts_loaded = true;
    }

    /// Marks shortcuts as needing reload (`ShortcutMessages::scheduleShortcutsReload`).
    pub fn mark_shortcuts_unloaded(&mut self) {
        self.shortcuts_loaded = false;
    }

    /// Returns all known shortcuts keyed by `BusinessShortcutId`.
    #[must_use]
    pub fn shortcuts(&self) -> &BTreeMap<BusinessShortcutId, QuickReplyShortcutData> {
        &self.shortcuts
    }

    /// Looks up a shortcut by ID.
    #[must_use]
    pub fn shortcut(&self, id: BusinessShortcutId) -> Option<&QuickReplyShortcutData> {
        self.shortcuts.get(&id)
    }

    /// Case-insensitive shortcut lookup by name (`ShortcutMessages::lookupShortcutId`).
    #[must_use]
    pub fn lookup_shortcut_id(&self, name: &str) -> Option<BusinessShortcutId> {
        self.shortcuts
            .iter()
            .find(|(_, s)| s.name.eq_ignore_ascii_case(name))
            .map(|(&id, _)| id)
    }

    /// Creates or returns a local placeholder shortcut for `name`
    /// (`ShortcutMessages::emplaceShortcut` in `data_shortcut_messages.cpp:498-509`).
    pub fn emplace_shortcut(&mut self, name: String) -> BusinessShortcutId {
        if let Some((&id, _)) = self.shortcuts.iter().find(|(_, s)| s.name == name) {
            return id;
        }
        self.next_local_shortcut_id -= 1;
        let id = self.next_local_shortcut_id;
        self.shortcuts.insert(
            id,
            QuickReplyShortcutData {
                id,
                name,
                top_message_id: MsgId::ZERO,
                count: 0,
            },
        );
        id
    }

    /// Returns `true` if `messages.getQuickReplyMessages(shortcut_id)` is still within the
    /// 60-second cooldown (`TooEarlyForRequest` in `data_shortcut_messages.cpp:40-42`).
    #[must_use]
    pub fn too_early_for_request(&self, shortcut_id: BusinessShortcutId, now_ms: u64) -> bool {
        self.last_received_ms
            .get(&shortcut_id)
            .is_some_and(|&received| {
                received > 0 && received.saturating_add(SHORTCUT_REQUEST_TIME_LIMIT_MS) > now_ms
            })
    }

    /// Clears the cooldown timer for `shortcut_id` (e.g. on RPC failure or delete update).
    pub fn clear_request_state(&mut self, shortcut_id: BusinessShortcutId) {
        self.last_received_ms.remove(&shortcut_id);
    }

    /// Returns stored messages for `shortcut_id`, sorted by `(date, id)` ascending.
    #[must_use]
    pub fn messages(&self, shortcut_id: BusinessShortcutId) -> &[Message] {
        self.lists
            .get(&shortcut_id)
            .map_or(&[], |l| l.items.as_slice())
    }

    /// Returns whether messages for `shortcut_id` have already been loaded.
    #[must_use]
    pub fn has_loaded_messages(&self, shortcut_id: BusinessShortcutId) -> bool {
        self.lists.contains_key(&shortcut_id)
    }

    /// Computes tdesktop's exact `ShortcutMessages::countListHash` for `shortcut_id`
    /// (`data/business/data_shortcut_messages.cpp:763-781`).
    ///
    /// Note the critical difference from `ScheduledMessages::countListHash`:
    /// `ShortcutMessages` hashes ONLY `(remote_id: i64, edit_date_or_0: i32)` in **reverse**
    /// sorted order — it does NOT hash `item->date()`!
    #[must_use]
    pub fn count_list_hash(&self, shortcut_id: BusinessShortcutId) -> i64 {
        let Some(list) = self.lists.get(&shortcut_id) else {
            return 0;
        };
        let mut hash = hash_init();
        for item in list.items.iter().rev() {
            if item.is_sending() || item.has_failed() {
                continue;
            }
            let Some(remote_id) = shortcut_local_to_remote_id(item.id) else {
                continue;
            };
            hash_update_i64(&mut hash, remote_id.bare());
            hash_update_i32(&mut hash, item.edit_date.unwrap_or(0));
        }
        hash_finalize(hash) as i64
    }

    /// Replaces the shortcuts map from `messages.quickReplies` or `updateQuickReplies`,
    /// preserving local placeholder shortcuts and migrating any whose name now matches a
    /// server-assigned ID (`ShortcutMessages::updateShortcuts` in
    /// `data_shortcut_messages.cpp:150-183`).
    pub fn update_shortcuts(
        &mut self,
        incoming: Vec<QuickReplyShortcutData>,
    ) -> Vec<ShortcutIdChange> {
        let mut next: BTreeMap<BusinessShortcutId, QuickReplyShortcutData> =
            incoming.into_iter().map(|s| (s.id, s)).collect();
        let mut changes = Vec::new();
        let old_entries: Vec<(BusinessShortcutId, QuickReplyShortcutData)> = self
            .shortcuts
            .iter()
            .map(|(&id, s)| (id, s.clone()))
            .collect();
        for (id, shortcut) in old_entries {
            if next.contains_key(&id) {
                continue;
            }
            let matched_real_id = next
                .iter()
                .find(|(_, real)| real.name == shortcut.name)
                .map(|(&real_id, _)| real_id);
            if let Some(real_id) = matched_real_id {
                self.merge_messages_from_to(id, real_id);
                changes.push(ShortcutIdChange {
                    old_id: id,
                    new_id: real_id,
                });
            } else {
                next.insert(id, shortcut);
            }
        }
        self.shortcuts = next;
        self.shortcuts_loaded = true;
        changes
    }

    /// Returns `true` if any shortcut's `top_message_id` is not present locally
    /// (`ShortcutMessages::scheduleShortcutsReload` in `data_shortcut_messages.cpp:261-281`).
    #[must_use]
    pub fn has_unknown_top_messages(&self) -> bool {
        for (&id, shortcut) in &self.shortcuts {
            if shortcut.top_message_id.is_zero() {
                continue;
            }
            let found = self
                .lists
                .get(&id)
                .is_some_and(|l| l.items.iter().any(|m| m.id == shortcut.top_message_id));
            if !found {
                return true;
            }
        }
        false
    }

    /// Applies `updateNewQuickReply` (`data_shortcut_messages.cpp:283-310`).
    ///
    /// Updates an existing shortcut by ID or migrates a local placeholder matching by name.
    pub fn apply_new_quick_reply(
        &mut self,
        shortcut: QuickReplyShortcutData,
    ) -> Option<ShortcutIdChange> {
        let found_id = self
            .shortcuts
            .iter()
            .find(|(_, existing)| existing.id == shortcut.id || existing.name == shortcut.name)
            .map(|(&id, _)| id);
        if found_id == Some(shortcut.id) {
            self.shortcuts.insert(shortcut.id, shortcut);
            return None;
        }
        if let Some(old_id) = found_id {
            let new_id = shortcut.id;
            self.shortcuts.insert(new_id, shortcut);
            self.merge_messages_from_to(old_id, new_id);
            self.shortcuts.remove(&old_id);
            return Some(ShortcutIdChange { old_id, new_id });
        }
        // Brand-new server shortcut created on another device.
        self.shortcuts.insert(shortcut.id, shortcut);
        None
    }

    /// Applies `updateDeleteQuickReply` (`data_shortcut_messages.cpp:371-392`).
    pub fn apply_delete_quick_reply(
        &mut self,
        shortcut_id: BusinessShortcutId,
    ) -> Option<ShortcutIdChange> {
        if shortcut_id == 0 {
            return None;
        }
        if let Some(list) = self.lists.get_mut(&shortcut_id) {
            list.items.retain(|m| m.is_sending() || m.has_failed());
            list.remote_to_local.clear();
            if list.items.is_empty() {
                self.lists.remove(&shortcut_id);
            }
        }
        self.last_received_ms.remove(&shortcut_id);
        if self.lists.contains_key(&shortcut_id) {
            self.update_count(shortcut_id);
            None
        } else {
            self.shortcuts.remove(&shortcut_id);
            Some(ShortcutIdChange {
                old_id: shortcut_id,
                new_id: 0,
            })
        }
    }

    fn insert_server_message_raw(
        &mut self,
        shortcut_id: BusinessShortcutId,
        mut msg: Message,
    ) -> Option<(MsgId, bool)> {
        if shortcut_id == 0 {
            return None;
        }
        let remote_id = msg.id;
        let local_id = shortcut_remote_to_local_id(remote_id)?;
        msg.id = local_id;
        msg.shortcut_id = Some(shortcut_id);
        msg.flags.insert(MessageFlags::SHORTCUT_MESSAGE);

        let was_loaded = self.lists.contains_key(&shortcut_id);
        let list = self.lists.entry(shortcut_id).or_default();
        if let Some(existing) = list.items.iter_mut().find(|m| m.id == local_id) {
            *existing = msg;
        } else {
            list.items.push(msg);
            list.remote_to_local.insert(remote_id, local_id);
        }
        list.sort();
        Some((local_id, !was_loaded))
    }

    /// Inserts or updates a top preview message delivered alongside `messages.quickReplies`
    /// without overwriting the server-reported [`QuickReplyShortcutData::count`]
    /// (`ShortcutMessages::preloadShortcuts` in `data/business/data_shortcut_messages.cpp:460-479`).
    pub fn upsert_preview_message(
        &mut self,
        shortcut_id: BusinessShortcutId,
        msg: Message,
    ) -> Option<MsgId> {
        self.insert_server_message_raw(shortcut_id, msg)
            .map(|(local_id, _)| local_id)
    }

    /// Appends or updates one server quick-reply message (`updateQuickReplyMessage` or
    /// `messages.getQuickReplyMessages`).
    ///
    /// Returns `(local_msg_id, needed_initial_load)`.
    pub fn upsert_server_message(
        &mut self,
        shortcut_id: BusinessShortcutId,
        msg: Message,
    ) -> Option<(MsgId, bool)> {
        let res = self.insert_server_message_raw(shortcut_id, msg)?;
        self.update_count(shortcut_id);
        Some(res)
    }

    /// Reconciles the full response from `messages.getQuickReplyMessages(shortcut_id)`
    /// (`ShortcutMessages::parse` in `data_shortcut_messages.cpp:624-660`).
    pub fn apply_messages_response(
        &mut self,
        shortcut_id: BusinessShortcutId,
        messages: Vec<Message>,
        now_ms: u64,
    ) {
        self.last_received_ms.insert(shortcut_id, now_ms);
        if messages.is_empty() {
            if let Some(list) = self.lists.get_mut(&shortcut_id) {
                list.items.retain(|m| m.is_sending() || m.has_failed());
                list.remote_to_local.clear();
                if list.items.is_empty() {
                    self.lists.remove(&shortcut_id);
                }
            }
            self.update_count(shortcut_id);
            return;
        }

        let mut received_locals = Vec::with_capacity(messages.len());
        for msg in messages {
            if let Some((local_id, _)) = self.upsert_server_message(shortcut_id, msg) {
                received_locals.push(local_id);
            }
        }
        if let Some(list) = self.lists.get_mut(&shortcut_id) {
            list.items
                .retain(|m| m.is_sending() || received_locals.contains(&m.id));
            list.remote_to_local
                .retain(|_, local_id| received_locals.contains(local_id));
            list.sort();
            if list.items.is_empty() {
                self.lists.remove(&shortcut_id);
            }
        }
        self.update_count(shortcut_id);
    }

    /// Applies `updateDeleteQuickReplyMessages` (`data_shortcut_messages.cpp:344-369`).
    ///
    /// Removes the deleted remote IDs, updates count, and clears the throttle timer so the
    /// caller can reload `shortcut_id`.
    pub fn apply_delete_quick_reply_messages(
        &mut self,
        shortcut_id: BusinessShortcutId,
        remote_ids: &[MsgId],
    ) -> Vec<MsgId> {
        let Some(list) = self.lists.get_mut(&shortcut_id) else {
            return Vec::new();
        };
        let mut deleted_locals = Vec::new();
        for &remote_id in remote_ids {
            if let Some(local_id) = list.remote_to_local.remove(&remote_id) {
                list.items.retain(|m| m.id != local_id);
                deleted_locals.push(local_id);
            }
        }
        if list.items.is_empty() {
            self.lists.remove(&shortcut_id);
        }
        self.last_received_ms.remove(&shortcut_id);
        self.update_count(shortcut_id);
        deleted_locals
    }

    /// Renames a shortcut locally after `messages.editQuickReplyShortcut` (or `SHORTCUT_ID_INVALID`).
    pub fn apply_shortcut_renamed(&mut self, shortcut_id: BusinessShortcutId, name: String) {
        if let Some(sc) = self.shortcuts.get_mut(&shortcut_id) {
            sc.name = name;
        }
    }

    /// Checks whether renaming `shortcut_id` to `name` collides with an existing non-empty
    /// shortcut (`SHORTCUT_OCCUPIED`, `data_shortcut_messages.cpp:546-560`).
    #[must_use]
    pub fn check_rename_collision(
        &self,
        shortcut_id: BusinessShortcutId,
        name: &str,
    ) -> ShortcutRenameCollision {
        for (&existing_id, sc) in &self.shortcuts {
            if sc.name == name {
                if existing_id == shortcut_id {
                    return ShortcutRenameCollision::None;
                }
                let empty_items = self
                    .lists
                    .get(&existing_id)
                    .is_none_or(|l| l.items.is_empty());
                if empty_items && sc.count == 0 {
                    return ShortcutRenameCollision::ReplaceEmpty(existing_id);
                }
                return ShortcutRenameCollision::Occupied;
            }
        }
        ShortcutRenameCollision::None
    }

    fn merge_messages_from_to(&mut self, from_id: BusinessShortcutId, to_id: BusinessShortcutId) {
        let Some(from_list) = self.lists.remove(&from_id) else {
            return;
        };
        self.last_received_ms.remove(&from_id);
        let to_list = self.lists.entry(to_id).or_default();
        let mut had_non_sending = false;
        for mut item in from_list.items {
            if item.is_sending() || item.has_failed() {
                item.shortcut_id = Some(to_id);
                to_list.items.push(item);
            } else {
                had_non_sending = true;
            }
        }
        to_list.sort();
        if had_non_sending {
            self.last_received_ms.remove(&to_id);
        }
    }

    fn update_count(&mut self, shortcut_id: BusinessShortcutId) {
        let count = self
            .lists
            .get(&shortcut_id)
            .map_or(0, |l| l.remote_to_local.len() as i32);
        if let Some(sc) = self.shortcuts.get_mut(&shortcut_id) {
            sc.count = count;
        }
    }
}
