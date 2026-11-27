//! "Restrict saving content" for copying message text.
//!
//! Port origin: `HistoryInner::hasCopyRestriction`, `hasCopyRestrictionForSelected`,
//! `showCopyRestriction` and the copy actions of `HistoryInner::showContextMenu`
//! (`history/history_inner_widget.cpp`). A chat that does not allow forwarding
//! (`PeerData::allowsForwarding`) or a protected message (`HistoryItem::forbidsForward`) is
//! not copied: `Ctrl+C` shows a toast instead and the menu has no copy actions.

use tg_app_core::stand_in::types::PeerKind;
use tg_ui::widgets::menu::MenuItem;

use crate::history::backend::HistoryMessage;
use crate::history::copy_selection::MENU_ID_COPY_SELECTED;

/// `HistoryInner::hasCopyRestriction(item)`.
#[must_use]
pub fn restricts(chat_allows_forwarding: bool, msg: &HistoryMessage) -> bool {
    !chat_allows_forwarding || msg.extra.no_forwards
}

/// `hasCopyRestrictionForSelected`: the chat, or any message the selection takes text from.
#[must_use]
pub fn restricts_any<'a>(
    chat_allows_forwarding: bool,
    selected: impl IntoIterator<Item = &'a HistoryMessage>,
) -> bool {
    !chat_allows_forwarding || selected.into_iter().any(|m| m.extra.no_forwards)
}

/// The toast of `showCopyRestriction` (`lng_error_nocopy_*`).
#[must_use]
pub fn toast_text(kind: PeerKind) -> String {
    match kind {
        PeerKind::Channel => tg_lang::tr::lng_error_nocopy_channel(),
        PeerKind::User => tg_lang::tr::lng_error_nocopy_user(),
        PeerKind::Group => tg_lang::tr::lng_error_nocopy_group(),
    }
}

/// Drops "Copy Text" when the message is protected and "Copy Selected Text" when the selection
/// is (`if (!hasCopyRestriction(item))`, `if (!hasCopyRestrictionForSelected())`).
pub fn drop_restricted_copy_items(
    items: &mut Vec<MenuItem>,
    message_restricted: bool,
    selection_restricted: bool,
) {
    items.retain(|item| match item {
        MenuItem::Action { id, .. } => {
            !(message_restricted && id == "copy_text"
                || selection_restricted && id == MENU_ID_COPY_SELECTED)
        }
        _ => true,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::tests_w4_text_snap::{group_peer, message};

    #[test]
    fn a_protected_message_or_chat_restricts_copying() {
        let mut msg = message(group_peer(), 1, 1001, "Mira Vance", "hello");
        assert!(!restricts(true, &msg));
        assert!(restricts(false, &msg), "the chat restricts saving content");
        msg.extra.no_forwards = true;
        assert!(restricts(true, &msg), "the message is protected");

        let free = message(group_peer(), 2, 1001, "Mira Vance", "free");
        assert!(!restricts_any(true, [&free]));
        assert!(restricts_any(true, [&free, &msg]));
        assert!(restricts_any(false, std::iter::empty()));
    }

    #[test]
    fn the_toast_names_the_kind_of_chat() {
        assert_eq!(
            toast_text(PeerKind::Channel),
            "Sorry, copying from this channel is disabled by admins."
        );
        assert_eq!(
            toast_text(PeerKind::Group),
            "Sorry, copying from this group is disabled by admins."
        );
        assert_eq!(
            toast_text(PeerKind::User),
            "Sorry, copying from this chat is restricted."
        );
    }
}
