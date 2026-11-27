//! Pages of loaded history that never cross a gap (TGB-3176).
//!
//! A history can hold several loaded ranges ([`crate::SparseIdsList`] slices): the bottom, a
//! page around the first unread message, a jump target. The messages between two ranges are
//! not loaded, so a page shown in the history view comes from one range only, and its
//! `reaches_top` / `reaches_bottom` say whether that range ends there. tdesktop keeps one
//! contiguous block per history and loads older messages from `minMsgId()`
//! (`HistoryWidget::loadMessages`), so it never shows two ranges as one.

use std::ops::Bound;

use super::Session;
use crate::history::{IdSlice, history_page_anchor};
use crate::id::{MsgId, PeerId, SERVER_MAX_MSG_ID};
use crate::message::Message;

/// Messages of one loaded range of a history, oldest first.
#[derive(Debug, Default)]
pub struct HistoryPage<'a> {
    /// The messages, oldest first.
    pub messages: Vec<&'a Message>,
    /// Nothing older than the page is missing: its range starts at the top of history and the
    /// page holds the range's oldest message (a page read after an anchor leaves the older
    /// part to the caller, who holds it, so there only the range counts).
    pub reaches_top: bool,
    /// Nothing newer than the page is missing: the mirror of [`Self::reaches_top`].
    pub reaches_bottom: bool,
}

impl Session {
    fn history_slices(&self, peer_id: PeerId) -> &[IdSlice] {
        self.histories
            .get(&peer_id)
            .map_or(&[], |h| h.loaded_ranges.slices())
    }

    fn covering_slice(&self, peer_id: PeerId, msg_id: MsgId) -> Option<&IdSlice> {
        self.histories
            .get(&peer_id)
            .and_then(|h| h.loaded_ranges.slice_covering(msg_id))
    }

    /// Up to `limit` stored messages of `ids`; `true` when `ids` has nothing left after them.
    fn take_page<'a>(
        &'a self,
        peer_id: PeerId,
        ids: &mut dyn Iterator<Item = &'a MsgId>,
        limit: usize,
    ) -> (Vec<&'a Message>, bool) {
        let mut out = Vec::with_capacity(limit.min(128));
        while out.len() < limit {
            let Some(&id) = ids.next() else {
                return (out, true);
            };
            if let Some(m) = self.messages.get(&crate::id::FullMsgId::new(peer_id, id)) {
                out.push(m);
            }
        }
        (out, ids.next().is_none())
    }

    /// Up to `limit` messages older than `before` in the range that covers `before` (empty
    /// when no range covers it: the messages below `before` are not loaded).
    #[must_use]
    pub fn history_page_before(
        &self,
        peer_id: PeerId,
        before: MsgId,
        limit: usize,
    ) -> HistoryPage<'_> {
        let anchor = history_page_anchor(before);
        let Some(slice) = self.covering_slice(peer_id, anchor) else {
            return HistoryPage::default();
        };
        let mut older = slice.messages.range(..anchor).rev();
        let (mut messages, exhausted) = self.take_page(peer_id, &mut older, limit);
        messages.reverse();
        HistoryPage {
            messages,
            reaches_top: exhausted && slice.range.from == MsgId::ZERO,
            reaches_bottom: slice.range.till == SERVER_MAX_MSG_ID,
        }
    }

    /// Up to `limit` messages newer than `after` in the range that covers `after` (empty when
    /// no range covers it).
    #[must_use]
    pub fn history_page_after(
        &self,
        peer_id: PeerId,
        after: MsgId,
        limit: usize,
    ) -> HistoryPage<'_> {
        let anchor = history_page_anchor(after);
        let Some(slice) = self.covering_slice(peer_id, anchor) else {
            return HistoryPage::default();
        };
        let mut newer = slice
            .messages
            .range((Bound::Excluded(anchor), Bound::Unbounded));
        let (messages, exhausted) = self.take_page(peer_id, &mut newer, limit);
        HistoryPage {
            messages,
            reaches_top: slice.range.from == MsgId::ZERO,
            reaches_bottom: exhausted && slice.range.till == SERVER_MAX_MSG_ID,
        }
    }

    /// The newest `limit` messages of the newest range.
    #[must_use]
    pub fn history_page_newest(&self, peer_id: PeerId, limit: usize) -> HistoryPage<'_> {
        let Some(slice) = self.history_slices(peer_id).last() else {
            return HistoryPage::default();
        };
        let mut older = slice.messages.iter().rev();
        let (mut messages, exhausted) = self.take_page(peer_id, &mut older, limit);
        messages.reverse();
        HistoryPage {
            messages,
            reaches_top: exhausted && slice.range.from == MsgId::ZERO,
            reaches_bottom: slice.range.till == SERVER_MAX_MSG_ID,
        }
    }

    /// The window around `target` in the range that covers it: up to `limit_after + 1`
    /// messages from the first one `>= target` and `limit_before` older ones (one more when
    /// nothing in the range is `>= target`, so the window ends at the range's newest message).
    /// When no range covers `target`, the nearest newer range is used, else the newest one.
    #[must_use]
    pub fn history_page_around(
        &self,
        peer_id: PeerId,
        target: MsgId,
        limit_before: usize,
        limit_after: usize,
    ) -> HistoryPage<'_> {
        let slices = self.history_slices(peer_id);
        let Some(slice) = self
            .covering_slice(peer_id, target)
            .or_else(|| slices.iter().find(|s| s.range.from > target))
            .or_else(|| slices.last())
        else {
            return HistoryPage::default();
        };
        let mut newer = slice.messages.range(target..);
        let (from, newer_exhausted) =
            self.take_page(peer_id, &mut newer, limit_after.saturating_add(1));
        let before_count = if from.is_empty() {
            limit_before.saturating_add(1)
        } else {
            limit_before
        };
        let mut older = slice.messages.range(..target).rev();
        let (mut messages, older_exhausted) = self.take_page(peer_id, &mut older, before_count);
        messages.reverse();
        messages.extend(from);
        HistoryPage {
            messages,
            reaches_top: older_exhausted && slice.range.from == MsgId::ZERO,
            reaches_bottom: newer_exhausted && slice.range.till == SERVER_MAX_MSG_ID,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::id::{ChannelId, MsgId, MsgRange, PeerId, SERVER_MAX_MSG_ID, UserId};
    use crate::message::Message;
    use crate::session::Session;

    fn peer() -> PeerId {
        PeerId::from_channel(ChannelId(5))
    }

    fn msg(id: i64) -> Message {
        Message::new_text(
            peer(),
            MsgId(id),
            None,
            1_700_000_000,
            crate::text::TextWithEntities::plain(format!("m{id}")),
        )
    }

    fn add(s: &mut Session, ids: std::ops::RangeInclusive<i64>, from: i64, till: MsgId) {
        s.add_history_slice(
            peer(),
            ids.map(msg).collect(),
            MsgRange::new(MsgId(from), till),
            None,
        );
    }

    fn ids(page: &super::HistoryPage<'_>) -> Vec<i64> {
        page.messages.iter().map(|m| m.id.0).collect()
    }

    /// A top range `[0, 2]` and a bottom range `[300, MAX]` with 3..=299 not loaded.
    fn two_ranges() -> Session {
        let mut s = Session::new(UserId(1));
        add(&mut s, 1..=2, 0, MsgId(2));
        add(&mut s, 300..=330, 300, SERVER_MAX_MSG_ID);
        s
    }

    #[test]
    fn a_page_before_stops_at_the_gap_and_is_not_the_top() {
        let s = two_ranges();
        let page = s.history_page_before(peer(), MsgId(310), 40);
        assert_eq!(ids(&page), (300..=309).collect::<Vec<_>>());
        assert!(!page.reaches_top, "the top range is behind a gap");
        let below_gap = s.history_page_before(peer(), MsgId(300), 40);
        assert!(below_gap.messages.is_empty());
        assert!(!below_gap.reaches_top);
        assert!(
            !s.history(peer())
                .is_some_and(|h| h.loaded_ranges.loaded_at_top_from(MsgId(300)))
        );
        assert!(
            s.history(peer())
                .is_some_and(|h| h.loaded_ranges.loaded_at_top())
        );
    }

    #[test]
    fn a_page_after_stops_at_the_gap_and_is_not_the_bottom() {
        let s = two_ranges();
        let page = s.history_page_after(peer(), MsgId(1), 40);
        assert_eq!(ids(&page), vec![2]);
        assert!(!page.reaches_bottom, "the bottom range is behind a gap");
        assert!(
            page.reaches_top,
            "the range of the anchor starts at the top"
        );
        assert!(
            !s.history(peer())
                .is_some_and(|h| h.loaded_ranges.loaded_at_bottom_from(MsgId(2)))
        );
    }

    #[test]
    fn the_newest_page_and_a_window_stay_in_their_range() {
        let s = two_ranges();
        let newest = s.history_page_newest(peer(), 100);
        assert_eq!(ids(&newest), (300..=330).collect::<Vec<_>>());
        assert!(newest.reaches_bottom);
        assert!(
            !newest.reaches_top,
            "the newest page does not reach the top range"
        );

        let around = s.history_page_around(peer(), MsgId(2), 40, 40);
        assert_eq!(ids(&around), vec![1, 2]);
        assert!(around.reaches_top);
        assert!(!around.reaches_bottom);

        // Not covered: the nearest newer range.
        let in_gap = s.history_page_around(peer(), MsgId(100), 5, 5);
        assert_eq!(ids(&in_gap), (300..=305).collect::<Vec<_>>());
        assert!(!in_gap.reaches_top);
    }

    #[test]
    fn one_range_reads_like_before() {
        let mut s = Session::new(UserId(1));
        add(&mut s, 1..=50, 0, SERVER_MAX_MSG_ID);
        let before = s.history_page_before(peer(), MsgId(11), 40);
        assert_eq!(ids(&before), (1..=10).collect::<Vec<_>>());
        assert!(before.reaches_top);
        let exact = s.history_page_before(peer(), MsgId(41), 40);
        assert_eq!(ids(&exact), (1..=40).collect::<Vec<_>>());
        assert!(exact.reaches_top, "the last id of the range was taken");
        let after = s.history_page_after(peer(), MsgId(45), 40);
        assert_eq!(ids(&after), (46..=50).collect::<Vec<_>>());
        assert!(after.reaches_bottom);
        // A local message sits below every server message.
        let before_local = s.history_page_before(peer(), MsgId(-5), 3);
        assert_eq!(ids(&before_local), vec![48, 49, 50]);
        assert!(!before_local.reaches_top);
    }
}
