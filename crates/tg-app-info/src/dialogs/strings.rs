//! Rustgram-specific UI strings for `dialogs/` that have no exact key in `resources/lang.strings`.

/// Toast shown after marking all chats in a folder as read.
pub const TOAST_FOLDER_MARKED_READ: &str = "Folder marked as read";
/// Toast shown after collapsing the Archive row.
pub const TOAST_ARCHIVE_COLLAPSED: &str = "Archive collapsed";
/// Toast shown after expanding the Archive row.
pub const TOAST_ARCHIVE_EXPANDED: &str = "Archive expanded";
/// Toast shown after marking all archived chats as read.
pub const TOAST_ARCHIVE_MARKED_READ: &str = "Archived chats marked as read";
/// Toast shown after adding a peer to a custom folder.
pub const TOAST_ADDED_TO_FOLDER: &str = "Added to folder";
/// Toast shown after removing a peer from a custom folder.
pub const TOAST_REMOVED_FROM_FOLDER: &str = "Removed from folder";
/// Toast shown after pinning a dialog to the top.
pub const TOAST_PINNED_TO_TOP: &str = "Chat pinned to top";
/// Toast shown after unpinning a dialog.
pub const TOAST_UNPINNED: &str = "Chat unpinned";
/// Toast shown after muting a dialog.
pub const TOAST_MUTED: &str = "Notifications muted";
/// Toast shown after unmuting a dialog.
pub const TOAST_UNMUTED: &str = "Notifications unmuted";
/// Toast shown after marking a single dialog as read.
pub const TOAST_MARKED_READ: &str = "Marked as read";
/// Toast shown after marking a single dialog as unread.
pub const TOAST_MARKED_UNREAD: &str = "Marked as unread";
/// Placeholder preview shown on a dialog whose history was cleared.
pub const PREVIEW_HISTORY_CLEARED: &str = "History was cleared";
/// Empty state message inside the Archived Chats view.
pub const EMPTY_ARCHIVED_CHATS: &str = "No archived chats";
/// Pagination button at the bottom of message search results.
pub const LOAD_MORE_MESSAGES: &str = "Load more messages";
/// Tooltip on the search icon inside the Archived Chats header.
pub const TOOLTIP_SEARCH_IN_ARCHIVE: &str = "Search in Archive";
/// Context menu item for adding a chat to a named folder.
#[must_use]
pub fn add_to_named_folder(title: &str) -> String {
    format!("Add to \"{title}\"")
}
/// Context menu item for removing a chat from a named folder.
#[must_use]
pub fn remove_from_named_folder(title: &str) -> String {
    format!("Remove from \"{title}\"")
}
/// Scope chip label when searching within a specific chat or folder.
#[must_use]
pub fn search_in_scope(name: &str) -> String {
    format!("In: {name}")
}
/// Message search results header. Telegram Desktop 7.4.6 shows the cloud English pack's
/// `"{count} messages found"`; the bundled `lng_search_found_results` still reads
/// `"Found {count} messages"`, so only that bundled English text is replaced and other
/// languages keep their translation.
#[must_use]
pub fn search_found_results(count: i64) -> String {
    let translated = tg_lang::tr::lng_search_found_results(count);
    let (bundled, cloud) = if count == 1 {
        (
            format!("Found {count} message"),
            format!("{count} message found"),
        )
    } else {
        (
            format!("Found {count} messages"),
            format!("{count} messages found"),
        )
    };
    if translated == bundled {
        cloud
    } else {
        translated
    }
}
