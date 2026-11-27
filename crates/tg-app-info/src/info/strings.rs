//! Rustgram-specific user-facing strings for `info/` that have no equivalent
//! key in `resources/lang.strings`.
//!
//! All standard Telegram Desktop phrases live in `tg_lang::tr::*`.

/// Fallback heading when a peer cannot be found.
pub const PEER_UNAVAILABLE: &str = "Peer unavailable";
/// Fallback top-bar heading when peer metadata is absent.
pub const FALLBACK_INFO_HEADING: &str = "Info";

/// Top-bar tooltips without direct `lang.strings` keys.
pub const TOOLTIP_CANCEL_SELECTION: &str = "Cancel selection";
pub const TOOLTIP_DELETE_SELECTED: &str = "Delete selected";
pub const TOOLTIP_FORWARD_SELECTED: &str = "Forward selected";
pub const TOOLTIP_EXIT_SEARCH: &str = "Exit search";
pub const TOOLTIP_CLEAR_SEARCH: &str = "Clear";
pub const TOOLTIP_MORE_ACTIONS: &str = "More actions";
pub const TOOLTIP_EDIT: &str = "Edit";
pub const TOOLTIP_SEARCH: &str = "Search";
pub const TOP_BAR_SELECTED_ONE: &str = "1 selected";
pub const TOP_BAR_SELECTED_SUFFIX: &str = "selected";

/// Cover badge & hover label.
pub const COVER_EDIT_OVERLAY: &str = "Edit";
pub const COVER_BOT_BADGE: &str = "BOT";

/// Profile details & actions copy without direct `lang.strings` keys.
pub const PHONE_NUMBER_LABEL: &str = "Phone number";
pub const LINKED_DISCUSSION_GROUP: &str = "Linked discussion group";
pub const LINKED_CHANNEL: &str = "Linked channel";
pub const AUTO_DELETE_CAPTION: &str = "Auto-Delete";
pub const AUTO_DELETE_AFTER_PREFIX: &str = "Messages auto-delete after";
pub const OPEN_CHANNEL: &str = "Open Channel";
pub const MINI_APP_BADGE: &str = "Mini App";
pub const SLOW_MODE_PREFIX: &str = "Slow mode";
pub const EDIT_BADGE: &str = "Edit";
pub const EDIT_USERNAME: &str = "Edit Username";
pub const PRIVACY_MODE_ENABLED: &str = "Privacy mode enabled";
pub const BOT_CMDS_SUFFIX: &str = "cmds";
pub const SHARE_BOT: &str = "Share Bot";
pub const SHARE_GROUP: &str = "Share Group";
pub const SHARE_CHANNEL: &str = "Share Channel";

/// Overflow menu items without direct `lang.strings` keys.
pub const MENU_EDIT_INFO: &str = "Edit info";
pub const MENU_EDIT_USERNAME: &str = "Edit username";
pub const MENU_SHARE: &str = "Share";
pub const MENU_MUTE_NOTIFICATIONS: &str = "Mute notifications";
pub const MENU_AUTO_DELETE: &str = "Auto-delete messages";
pub const MENU_ABOUT_TGFORK: &str = "About Rustgram";

pub const TOAST_MEMBER_REMOVED: &str = "Member removed";

/// Members & common groups copy.
pub const SEARCH_MEMBERS_PH: &str = "Search members...";
pub const NO_MATCHING_MEMBERS: &str = "No matching members";
pub const NO_COMMON_GROUPS: &str = "No groups in common";
pub const TOOLTIP_REMOVE_MEMBER: &str = "Remove member";
pub const TOOLTIP_EDIT_ADMIN_RIGHTS: &str = "Edit admin rights";

/// Shared media copy.
pub const MEDIA_GIF_BADGE: &str = "GIF";
pub const MEDIA_EMPTY_PREFIX: &str = "No";
pub const MEDIA_EMPTY_SUFFIX: &str = "yet";

/// Member role fallback badges (preserving `MemberRole::badge_label()` contract).
pub use crate::info::backend::{ROLE_ADMIN, ROLE_OWNER, ROLE_RESTRICTED};
