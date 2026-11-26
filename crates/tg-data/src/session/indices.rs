//! Id indices and ordered range queries over loaded history (TGB-2893, package F).
//!
//! Every `messages` insert and removal goes through [`Session::store_message`] /
//! [`Session::take_message`], which keep the per-peer id sets and the media index in step, so
//! lookups by peer, by media and by position never scan every loaded message.

use std::ops::Bound;

use super::{Session, message_media_key};
use crate::id::{FullMsgId, MsgId, PeerId};
use crate::message::Message;

impl Session {
    /// Stores `message` under its `FullMsgId`, keeping the indices in step.
    pub(super) fn store_message(&mut self, message: Message) -> Option<Message> {
        self.flush_media_dirty();
        let full_id = message.full_id();
        let media = message_media_key(&message);
        let previous = self.messages.insert(full_id, message);
        self.peer_message_ids
            .entry(full_id.peer)
            .or_default()
            .insert(full_id.msg);
        self.index_media(full_id, media);
        previous
    }

    /// Removes a stored message, keeping the indices in step.
    pub(super) fn take_message(&mut self, full_id: FullMsgId) -> Option<Message> {
        self.flush_media_dirty();
        let removed = self.messages.remove(&full_id)?;
        if let Some(ids) = self.peer_message_ids.get_mut(&full_id.peer) {
            ids.remove(&full_id.msg);
            if ids.is_empty() {
                self.peer_message_ids.remove(&full_id.peer);
            }
        }
        self.unindex_media(full_id);
        Some(removed)
    }

    fn index_media(&mut self, full_id: FullMsgId, media: Option<u64>) {
        if self.message_media.get(&full_id).copied() == media {
            return;
        }
        self.unindex_media(full_id);
        if let Some(media_id) = media {
            self.media_messages
                .entry(media_id)
                .or_default()
                .push(full_id);
            self.message_media.insert(full_id, media_id);
        }
    }

    fn unindex_media(&mut self, full_id: FullMsgId) {
        let Some(media_id) = self.message_media.remove(&full_id) else {
            return;
        };
        if let Some(list) = self.media_messages.get_mut(&media_id) {
            list.retain(|id| *id != full_id);
            if list.is_empty() {
                self.media_messages.remove(&media_id);
            }
        }
    }

    /// Reindexes the messages handed out by `message_mut` since the last insert or delete.
    fn flush_media_dirty(&mut self) {
        if self.media_dirty.is_empty() {
            return;
        }
        let dirty: Vec<FullMsgId> = self.media_dirty.drain().collect();
        for full_id in dirty {
            match self.messages.get(&full_id).map(message_media_key) {
                Some(media) => self.index_media(full_id, media),
                None => self.unindex_media(full_id),
            }
        }
    }

    fn message_shows_media(&self, full_id: FullMsgId, media_id: u64) -> bool {
        self.messages
            .get(&full_id)
            .and_then(message_media_key)
            .is_some_and(|id| id == media_id)
    }

    /// Loaded messages whose photo or document id is `media_id`, sorted by `FullMsgId`
    /// (photo and document ids are matched alike, as the media viewer does).
    #[must_use]
    pub fn messages_with_media(&self, media_id: u64) -> Vec<FullMsgId> {
        let mut out: Vec<FullMsgId> = self
            .media_messages
            .get(&media_id)
            .into_iter()
            .flatten()
            .copied()
            .filter(|id| self.message_shows_media(*id, media_id))
            .collect();
        for &id in &self.media_dirty {
            if !out.contains(&id) && self.message_shows_media(id, media_id) {
                out.push(id);
            }
        }
        out.sort_unstable();
        out
    }

    /// Ids of every stored message of `peer_id` (loaded history, reply targets, pending sends),
    /// ascending.
    pub fn stored_message_ids(
        &self,
        peer_id: PeerId,
    ) -> impl DoubleEndedIterator<Item = MsgId> + '_ {
        self.peer_message_ids
            .get(&peer_id)
            .into_iter()
            .flatten()
            .copied()
    }

    fn loaded_message(&self, peer_id: PeerId, id: MsgId) -> Option<&Message> {
        self.messages.get(&FullMsgId::new(peer_id, id))
    }

    /// Loaded messages of `peer_id`'s history (its `loaded_ranges`) oldest first, without
    /// collecting them: the same sequence as walking every slice and skipping ids with no
    /// stored message.
    pub fn loaded_messages(
        &self,
        peer_id: PeerId,
    ) -> impl DoubleEndedIterator<Item = &Message> + '_ {
        self.histories
            .get(&peer_id)
            .into_iter()
            .flat_map(|history| history.loaded_ranges.slices())
            .flat_map(|slice| slice.messages.iter())
            .filter_map(move |&id| self.loaded_message(peer_id, id))
    }

    /// [`Self::loaded_messages`] with id `< before`, newest first.
    pub fn loaded_messages_before(
        &self,
        peer_id: PeerId,
        before: MsgId,
    ) -> impl Iterator<Item = &Message> + '_ {
        self.histories
            .get(&peer_id)
            .into_iter()
            .flat_map(|history| history.loaded_ranges.slices().iter().rev())
            .flat_map(move |slice| slice.messages.range(..before).rev())
            .filter_map(move |&id| self.loaded_message(peer_id, id))
    }

    /// [`Self::loaded_messages`] with id `> after`, oldest first.
    pub fn loaded_messages_after(
        &self,
        peer_id: PeerId,
        after: MsgId,
    ) -> impl Iterator<Item = &Message> + '_ {
        self.histories
            .get(&peer_id)
            .into_iter()
            .flat_map(|history| history.loaded_ranges.slices())
            .flat_map(move |slice| {
                slice
                    .messages
                    .range((Bound::Excluded(after), Bound::Unbounded))
            })
            .filter_map(move |&id| self.loaded_message(peer_id, id))
    }

    /// [`Self::loaded_messages`] with id `>= from`, oldest first.
    pub fn loaded_messages_from(
        &self,
        peer_id: PeerId,
        from: MsgId,
    ) -> impl Iterator<Item = &Message> + '_ {
        self.histories
            .get(&peer_id)
            .into_iter()
            .flat_map(|history| history.loaded_ranges.slices())
            .flat_map(move |slice| slice.messages.range(from..))
            .filter_map(move |&id| self.loaded_message(peer_id, id))
    }

    /// Number of [`Self::loaded_messages`] (no allocation).
    #[must_use]
    pub fn loaded_message_count(&self, peer_id: PeerId) -> usize {
        self.loaded_messages(peer_id).count()
    }

    /// The newest `limit` loaded messages, oldest first.
    #[must_use]
    pub fn last_loaded_messages(&self, peer_id: PeerId, limit: usize) -> Vec<&Message> {
        let mut out: Vec<&Message> = self.loaded_messages(peer_id).rev().take(limit).collect();
        out.reverse();
        out
    }

    /// Window of loaded messages around `target`, oldest first, equal to taking
    /// `[center - limit_before, center + limit_after]` of the full ordered list where `center`
    /// is the first message with id `>= target` (the last message when there is none).
    #[must_use]
    pub fn loaded_messages_around(
        &self,
        peer_id: PeerId,
        target: MsgId,
        limit_before: usize,
        limit_after: usize,
    ) -> Vec<&Message> {
        let from: Vec<&Message> = self
            .loaded_messages_from(peer_id, target)
            .take(limit_after.saturating_add(1))
            .collect();
        let before_count = if from.is_empty() {
            limit_before.saturating_add(1)
        } else {
            limit_before
        };
        let mut out: Vec<&Message> = self
            .loaded_messages_before(peer_id, target)
            .take(before_count)
            .collect();
        out.reverse();
        out.extend(from);
        out
    }

    /// First loaded incoming message after the inbox read mark while the chat has unread
    /// messages (the "unread messages" bar position).
    #[must_use]
    pub fn first_unread_incoming(&self, peer_id: PeerId) -> Option<MsgId> {
        let history = self.histories.get(&peer_id)?;
        if history.unread_count.unwrap_or(0) <= 0 {
            return None;
        }
        self.loaded_messages_after(peer_id, history.inbox_read_till_id())
            .find(|m| !m.is_outgoing())
            .map(|m| m.id)
    }
}
