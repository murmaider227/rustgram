//! Port of `mtproto/details/mtproto_received_ids_manager.cpp`.
//!
//! Tracks recently received server `msg_id`s to reject duplicates and detect stale messages
//! requiring a session reset.

use std::collections::BTreeMap;

/// Maximum number of received / acked `msg_id`s retained (`kIdsBufferSize`).
pub const IDS_BUFFER_SIZE: usize = 400;

/// Lookup result for a previously seen `msg_id`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceivedIdState {
    /// Not in the recent buffer.
    NotFound,
    /// Seen and content-related (requires acknowledgement).
    NeedsAck,
    /// Seen and not content-related.
    NoAckNeeded,
}

/// Registration outcome when inspecting an incoming server `msg_id`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegisterIdResult {
    /// Fresh `msg_id`; should be processed.
    Success,
    /// Already processed; skip duplicate.
    Duplicate,
    /// Older than the minimum tracked ID when buffer is full; session must be reset.
    TooOld,
}

/// Bounded ordered map of received server `msg_id`s (`MTP::details::ReceivedIdsManager`).
#[derive(Debug, Default, Clone)]
pub struct ReceivedIdsManager {
    ids_need_ack: BTreeMap<u64, bool>,
}

impl ReceivedIdsManager {
    /// Creates an empty manager.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers an incoming server `msg_id` (`ReceivedIdsManager::registerMsgId`).
    pub fn register_msg_id(&mut self, msg_id: u64, need_ack: bool) -> RegisterIdResult {
        if self.ids_need_ack.contains_key(&msg_id) {
            return RegisterIdResult::Duplicate;
        }
        if self.ids_need_ack.len() < IDS_BUFFER_SIZE || msg_id > self.min() {
            self.ids_need_ack.insert(msg_id, need_ack);
            return RegisterIdResult::Success;
        }
        RegisterIdResult::TooOld
    }

    /// Smallest tracked `msg_id`, or 0 if empty.
    pub fn min(&self) -> u64 {
        self.ids_need_ack.keys().next().copied().unwrap_or(0)
    }

    /// Largest tracked `msg_id`, or 0 if empty.
    pub fn max(&self) -> u64 {
        self.ids_need_ack.keys().next_back().copied().unwrap_or(0)
    }

    /// Queries the state of `msg_id`.
    pub fn lookup(&self, msg_id: u64) -> ReceivedIdState {
        match self.ids_need_ack.get(&msg_id) {
            None => ReceivedIdState::NotFound,
            Some(true) => ReceivedIdState::NeedsAck,
            Some(false) => ReceivedIdState::NoAckNeeded,
        }
    }

    /// Shrinks the map back down to [`IDS_BUFFER_SIZE`] by evicting oldest IDs.
    pub fn shrink(&mut self) {
        while self.ids_need_ack.len() > IDS_BUFFER_SIZE {
            if let Some(&first) = self.ids_need_ack.keys().next() {
                self.ids_need_ack.remove(&first);
            } else {
                break;
            }
        }
    }

    /// Clears all tracked IDs (called on session reset).
    pub fn clear(&mut self) {
        self.ids_need_ack.clear();
    }

    /// Number of tracked IDs.
    pub fn len(&self) -> usize {
        self.ids_need_ack.len()
    }

    /// Returns `true` if no IDs are tracked.
    pub fn is_empty(&self) -> bool {
        self.ids_need_ack.is_empty()
    }
}
