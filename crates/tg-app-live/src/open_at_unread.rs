//! Opening a chat at its first unread message (`ShowAtUnreadMsgId`) and the first full history
//! page of a chat whose only loaded message is the dialog's top message.
//!
//! Port origin: `History::loadAroundId` (`Telegram/SourceFiles/history/history.cpp`) and
//! `HistoryWidget::firstLoadMessages` (`Telegram/SourceFiles/history/history_widget.cpp`):
//! with unread messages the first request loads `kMessagesPerPage` messages around
//! `inboxReadBefore`; without them it loads the newest `kMessagesPerPageFirst` messages.

use tg_data::{History, MsgId as DataMsgId, SparseIdsList};

/// `kMessagesPerPageFirst` (`history_widget.cpp`): a first page at the bottom.
pub(crate) const MESSAGES_PER_PAGE_FIRST: usize = 30;

/// `History::loadAroundId`: `inboxReadBefore` when the chat has unread messages, else `None`
/// (open at the bottom).
pub(crate) fn load_around_id(history: &History) -> Option<DataMsgId> {
    if history.unread_count.unwrap_or(0) > 0 {
        history.inbox_read_before.filter(|id| id.0 > 0)
    } else {
        None
    }
}

/// Whether opening the chat must request a first history page (`History::isReadyFor` is
/// false): nothing is loaded, the first unread message is not loaded, or — on the first open in
/// this session — only the dialog's top message(s) are known and the history is not complete.
pub(crate) fn first_open_needs_load(
    ranges: &SparseIdsList,
    already_opened: bool,
    unread_missing: bool,
) -> bool {
    if unread_missing {
        return true;
    }
    // A complete history needs no page, also when the chat has no messages: tdesktop's
    // `isReadyFor(ShowAtUnreadMsgId)` is `loadedAtBottom()` there (TGB-3079).
    if ranges.loaded_at_top() && ranges.loaded_at_bottom() {
        return false;
    }
    if ranges.is_empty() {
        return true;
    }
    if already_opened {
        return false;
    }
    ranges
        .slices()
        .last()
        .is_none_or(|bottom| bottom.messages.len() < MESSAGES_PER_PAGE_FIRST)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tg_data::{MsgRange, PeerId as DataPeerId, SERVER_MAX_MSG_ID, UserId};

    fn history_with(unread: i32, read_before: Option<i64>) -> History {
        let mut h = History::new(DataPeerId::from_user(UserId::from_tl(7)));
        h.unread_count = Some(unread);
        h.inbox_read_before = read_before.map(DataMsgId);
        h
    }

    #[test]
    fn load_around_id_follows_history_load_around_id() {
        assert_eq!(
            load_around_id(&history_with(3, Some(41))),
            Some(DataMsgId(41))
        );
        assert_eq!(load_around_id(&history_with(0, Some(41))), None);
        assert_eq!(load_around_id(&history_with(3, None)), None);
    }

    #[test]
    fn dialog_top_message_alone_is_not_a_loaded_first_page() {
        let mut ranges = SparseIdsList::new();
        assert!(first_open_needs_load(&ranges, false, false));
        // The dialog list adds only the top message at the bottom.
        ranges.add_new(DataMsgId(100));
        assert!(first_open_needs_load(&ranges, false, false));
        assert!(
            !first_open_needs_load(&ranges, true, false),
            "requested once"
        );
        assert!(
            first_open_needs_load(&ranges, true, true),
            "unread not loaded"
        );

        let mut full = SparseIdsList::new();
        full.add_slice(
            (71..=100).map(DataMsgId),
            MsgRange::new(DataMsgId(71), SERVER_MAX_MSG_ID),
            None,
        );
        assert!(!first_open_needs_load(&full, false, false));

        let mut whole = SparseIdsList::new();
        whole.add_slice(
            (1..=3).map(DataMsgId),
            MsgRange::new(DataMsgId(0), SERVER_MAX_MSG_ID),
            Some(3),
        );
        assert!(!first_open_needs_load(&whole, false, false));
    }

    #[test]
    fn a_complete_empty_history_needs_no_page() {
        let mut empty = SparseIdsList::new();
        empty.add_slice(
            std::iter::empty(),
            MsgRange::new(DataMsgId(0), SERVER_MAX_MSG_ID),
            Some(0),
        );
        assert!(empty.is_empty());
        assert!(!first_open_needs_load(&empty, false, false));
        assert!(
            first_open_needs_load(&empty, false, true),
            "unread not loaded"
        );

        let mut bottom_only = SparseIdsList::new();
        bottom_only.add_slice(
            std::iter::empty(),
            MsgRange::new(DataMsgId(50), SERVER_MAX_MSG_ID),
            None,
        );
        assert!(
            first_open_needs_load(&bottom_only, false, false),
            "older messages may exist"
        );
    }
}
