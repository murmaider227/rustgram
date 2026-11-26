//! Bounded message memory for long sessions (TGB-2893, package F; tdesktop
//! `History::unloadBlocks`).
//!
//! When more than [`Session::message_budget`] messages are stored, the least recently loaded
//! chats that are not open keep only their newest [`UNLOAD_KEEP_TAIL`] loaded messages (and
//! every unread one). Their `loaded_ranges` shrink to match, so the dropped part reads as not
//! loaded and comes back from the server on scroll, the same path a first open takes. The top
//! message, topic and sublist last messages stay, so chat-list rows do not change.

use rustc_hash::FxHashSet;

use super::{Session, UNLOAD_KEEP_TAIL};
use crate::changes::HistoryUpdateFlags;
use crate::id::{FullMsgId, MsgId, PeerId};

impl Session {
    /// Message budget across all chats (`None` = unbounded).
    #[must_use]
    pub const fn message_budget(&self) -> Option<usize> {
        self.message_budget
    }

    /// Sets the message budget ([`super::DEFAULT_MESSAGE_BUDGET`] by default; `None` turns
    /// unloading off) and applies it at once.
    pub fn set_message_budget(&mut self, budget: Option<usize>) {
        self.message_budget = budget;
        self.unload_armed = true;
        self.next_unload_check = budget.unwrap_or(usize::MAX);
        self.maybe_unload_except(None);
    }

    /// Marks `peer_id`'s history as shown (or no longer shown) in a chat view: an open
    /// history is never unloaded, and opening counts as a use for the least-recently-used order.
    ///
    /// The first call arms unloading ([`super::DEFAULT_MESSAGE_BUDGET`]): until the UI reports
    /// its open chats, nothing is unloaded.
    pub fn set_history_open(&mut self, peer_id: PeerId, open: bool) {
        self.unload_armed = true;
        if open {
            self.open_histories.insert(peer_id);
            self.touch_history(peer_id);
        } else {
            self.open_histories.remove(&peer_id);
        }
    }

    /// Number of stored messages across all chats.
    #[must_use]
    pub fn stored_message_count(&self) -> usize {
        self.messages.len()
    }

    pub(super) fn touch_history(&mut self, peer_id: PeerId) {
        self.touch_tick += 1;
        self.history_touch.insert(peer_id, self.touch_tick);
    }

    /// Unloads old messages when over budget, never from `current` (the chat being written to).
    pub(super) fn maybe_unload(&mut self, current: PeerId) {
        self.maybe_unload_except(Some(current));
    }

    fn maybe_unload_except(&mut self, current: Option<PeerId>) {
        let Some(budget) = self.message_budget.filter(|_| self.unload_armed) else {
            return;
        };
        if self.messages.len() <= self.next_unload_check.max(budget) {
            return;
        }
        let low_water = budget - budget / 4;
        // The most recently loaded chat is the one on screen even when the UI has not marked
        // it open.
        let latest = self
            .history_touch
            .iter()
            .max_by_key(|(_, tick)| **tick)
            .map(|(peer, _)| *peer);
        let mut candidates: Vec<(u64, PeerId)> = self
            .histories
            .iter()
            .filter(|(peer, history)| {
                Some(**peer) != current
                    && Some(**peer) != latest
                    && !self.open_histories.contains(*peer)
                    && history
                        .loaded_ranges
                        .slices()
                        .iter()
                        .any(|s| !s.messages.is_empty())
            })
            .map(|(peer, _)| (self.history_touch.get(peer).copied().unwrap_or(0), *peer))
            .collect();
        candidates.sort_unstable();
        // First every old chat keeps its newest screenful; when the tails alone are over the
        // budget (hundreds of chats opened), the oldest drop to the never-opened state.
        for keep_tail in [UNLOAD_KEEP_TAIL, 1] {
            for (_, peer) in &candidates {
                if self.messages.len() <= low_water {
                    break;
                }
                self.unload_history_down_to(*peer, keep_tail);
            }
        }
        // When the rest is open or recent, wait for an eighth of the budget more before the
        // next pass instead of rescanning on every insert.
        self.next_unload_check = budget.max(self.messages.len().saturating_add(budget / 8));
    }

    /// Unloads `peer_id`'s history down to its newest [`UNLOAD_KEEP_TAIL`] loaded messages
    /// (and all unread ones), keeping the messages other views refer to. Returns the number of
    /// messages dropped; `0` for an open chat, one with messages being sent, or one with unread
    /// messages outside its newest loaded slice.
    pub fn unload_history_messages(&mut self, peer_id: PeerId) -> usize {
        self.unload_history_down_to(peer_id, UNLOAD_KEEP_TAIL)
    }

    /// Like [`Self::unload_history_messages`], keeping the newest `keep_tail` (at least one)
    /// loaded messages.
    fn unload_history_down_to(&mut self, peer_id: PeerId, keep_tail: usize) -> usize {
        if self.open_histories.contains(&peer_id) {
            return 0;
        }
        let Some(history) = self.histories.get(&peer_id) else {
            return 0;
        };
        let slices = history.loaded_ranges.slices();
        let Some(last) = slices.last() else {
            return 0;
        };
        // Messages being sent (or failed) have client ids, which sort outside the server range.
        let stored = self.peer_message_ids.get(&peer_id);
        let has_client_ids = stored.is_some_and(|ids| {
            ids.first().is_some_and(|id| !id.is_server())
                || ids.last().is_some_and(|id| !id.is_server())
        });
        if has_client_ids {
            return 0;
        }
        let Some(&last_first) = last.messages.first() else {
            return 0;
        };
        let mut keep_from = last
            .messages
            .iter()
            .rev()
            .nth(keep_tail.max(1) - 1)
            .copied()
            .unwrap_or(last_first);
        if history.unread_count.unwrap_or(0) > 0 {
            let read_till = history.inbox_read_till_id();
            let first_unread = slices
                .iter()
                .find_map(|s| s.messages.range(read_till..).find(|id| **id > read_till))
                .copied();
            if let Some(first_unread) = first_unread {
                if first_unread < last_first {
                    return 0;
                }
                keep_from = keep_from.min(first_unread);
            }
        }
        let mut dropped: Vec<MsgId> = slices[..slices.len() - 1]
            .iter()
            .flat_map(|s| s.messages.iter().copied())
            .collect();
        dropped.extend(last.messages.range(..keep_from).copied());
        if dropped.is_empty() {
            return 0;
        }
        let referenced = self.referenced_message_ids(peer_id);
        if let Some(history) = self.histories.get_mut(&peer_id) {
            history.loaded_ranges.keep_newest_from(keep_from);
        }
        let mut removed = 0;
        for id in dropped {
            if referenced.contains(&id) {
                continue;
            }
            if self.take_message(FullMsgId::new(peer_id, id)).is_some() {
                removed += 1;
                if !peer_id.is_channel() {
                    self.non_channel_messages.remove(&id);
                }
            }
        }
        if removed > 0 {
            for list in self.messages_with_peer.values_mut() {
                list.retain(|fid| fid.peer != peer_id || self.messages.contains_key(fid));
            }
            self.changes
                .history_updated(peer_id, HistoryUpdateFlags::MESSAGES_UNLOADED);
        }
        removed
    }

    /// Messages of `peer_id` that rows outside the chat view show: the chat-list top message,
    /// forum topic roots and last messages, saved / monoforum sublist last messages.
    fn referenced_message_ids(&self, peer_id: PeerId) -> FxHashSet<MsgId> {
        let mut out = FxHashSet::default();
        if let Some(top) = self.histories.get(&peer_id).and_then(|h| h.top_message_id) {
            out.insert(top);
        }
        if let Some(forum) = self.forums.get(&peer_id) {
            for topic in forum.topics.values() {
                out.insert(topic.root_id);
                out.extend(topic.top_message_id);
            }
        }
        let sublists = if peer_id == self.self_peer_id() {
            Some(&self.saved_messages)
        } else {
            peer_id
                .to_channel()
                .and_then(|channel| self.monoforums.get(&channel))
        };
        if let Some(sublists) = sublists {
            out.extend(sublists.sublists.values().filter_map(|s| s.top_message_id));
        }
        out
    }
}
