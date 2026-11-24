//! Rustgram-specific user-facing strings for `boxes/` that have no equivalent
//! key in `resources/lang.strings`.
//!
//! All standard Telegram Desktop phrases live in `tg_lang::tr::*`.

/// About box Rustgram-specific description and attribution copy.
pub const ABOUT_TITLE: &str = "Rustgram";
pub const ABOUT_VERSION_SUFFIX: &str = "(Rust · egui)";
pub const ABOUT_INTRO_PREFIX: &str = "Rustgram is an open-source Rust port of ";
pub const ABOUT_TDESKTOP_NAME: &str = "Telegram Desktop";
pub const ABOUT_INTRO_MID: &str = ", built on the official ";
pub const ABOUT_INTRO_SUFFIX: &str = " for speed and memory safety.";
pub const ABOUT_LICENSE_PREFIX: &str = "This software is free software licensed under ";
pub const ABOUT_GPL_LABEL: &str = "GNU GPL version 3";
pub const ABOUT_LICENSE_MID: &str =
    ". Source code and upstream attribution follow the original project at ";
pub const ABOUT_GITHUB_LABEL: &str = "github.com/telegramdesktop/tdesktop";
pub const ABOUT_NOTICE: &str = "No proprietary assets or trademarks are redistributed. All protocol and UI components are ported under GPL-3.0.";

/// Add contact validation & placeholders without direct `lang.strings` keys.
pub const ADD_CONTACT_FIRST_REQUIRED: &str = "Please enter a first name.";
pub const ADD_CONTACT_PHONE_INVALID: &str =
    "Please enter a valid phone number (at least 8 digits).";
pub const ADD_CONTACT_LAST_OPTIONAL: &str = "Last name (optional)";
pub const ADD_CONTACT_PHONE_PH: &str = "Phone number (e.g. +999 661 0900)";

/// Create group / channel wizard copy without direct `lang.strings` keys.
pub const GROUP_NAME_REQUIRED: &str = "Please enter a group name.";
pub const GROUP_NAME_PH: &str = "Group name";
pub const CHANNEL_NAME_REQUIRED: &str = "Please enter a channel name.";
pub const CHANNEL_NAME_PH: &str = "Channel name";
pub const CHANNEL_SETTINGS_TITLE: &str = "Channel Settings";
pub const CHANNEL_PUBLIC_LABEL: &str = "Public Channel";
pub const CHANNEL_PUBLIC_HINT: &str =
    "Public channels can be found in search; anyone can join them.";
pub const CHANNEL_PRIVATE_LABEL: &str = "Private Channel";
pub const CHANNEL_PRIVATE_HINT: &str = "Private channels can only be joined via an invite link.";
pub const CHANNEL_PUBLIC_LINK_PH: &str = "Public link (t.me/username)";
pub const CHANNEL_ENTER_USERNAME_OR_PRIVATE: &str =
    "Enter a public username or switch to Private Channel.";
pub const RESTRICT_SAVING_CONTENT: &str = "Restrict saving content";
pub const STEP_1_OF_2: &str = "Step 1 of 2";
pub const STEP_2_OF_2: &str = "Step 2 of 2";
pub const SELECTED_SUFFIX: &str = "selected";
pub const PHOTO_PICKER_TOOLTIP: &str = "Set photo (opens crop editor)";
pub const AUTO_DELETE_PRESET_NOTE_PREFIX: &str = "New messages will auto-delete after";

/// Edit peer / permissions / admin / TTL copy without direct `lang.strings` keys.
pub const EDIT_CHANNEL_TITLE: &str = "Edit Channel";
pub const EDIT_INFO_TITLE: &str = "Edit Info";
pub const DISPLAY_NAME_PH: &str = "Display name";
pub const CHANNEL_TYPE_HEADER: &str = "Channel Type";
pub const GROUP_TYPE_HEADER: &str = "Group Type";
pub const VISIBILITY_PUBLIC: &str = "Public";
pub const VISIBILITY_PRIVATE: &str = "Private";
pub const LINKED_GROUP_NONE: &str = "Add a group";
pub const COLLAPSE_MEDIA_TYPES: &str = "Collapse media types";
pub const EXPAND_MEDIA_TYPES: &str = "Expand media types";
pub const ADD_ADMIN_BTN: &str = "Add Admin";
pub const ADD_ADMIN_TITLE: &str = "Add Administrator";
pub const CONTINUE_BTN: &str = "Continue";
pub const ROLE_OWNER_TITLE: &str = "Owner";
pub const ROLE_MEMBER_TITLE: &str = "Member";
pub const ADMIN_PERMISSIONS_PREFIX: &str = "Admin";
pub const ADMIN_PERMISSIONS_SUFFIX: &str = "permissions";
pub const DISMISS_BTN: &str = "Dismiss";
pub const CUSTOM_TITLE_OPTIONAL_PH: &str = "Custom title (optional)";
pub const TTL_DIALOG_DESC: &str =
    "Automatically delete messages in this chat for all participants after:";
pub const LINK_DISCUSSION_TITLE: &str = "Link Discussion Group";
pub const LINK_CONFIRM_BTN: &str = "Link";

/// Peer list & Share box copy without direct `lang.strings` keys.
pub const NO_MATCHING_PEERS: &str = "No matching peers found";
pub const PEER_FALLBACK_LABEL: &str = "Peer";
pub const SHARE_SELECT_ONE_ERROR: &str = "Select at least one chat.";
pub const SHARE_SEARCH_PH: &str = "Search chats...";
pub const SHARE_COMMENT_PH: &str = "Write a comment (optional)...";

/// Report box copy without direct `lang.strings` keys.
pub const REPORT_DESCRIBE_REQUIRED: &str = "Please describe the reason for your report.";
pub const REPORT_INTRO: &str = "Choose a reason for reporting this chat or message:";
pub const REPORT_MODERATORS_HINT: &str =
    "Moderators will review your report and take appropriate action.";

/// Delete messages box copy without direct `lang.strings` keys.
pub const DELETE_MESSAGE_TITLE: &str = "Delete Message";
pub const DELETE_MESSAGES_TITLE: &str = "Delete Messages";
pub const DELETE_SELECTED_PREFIX: &str = "Are you sure you want to delete";
pub const DELETE_SELECTED_SUFFIX: &str = "selected messages?";
pub const DELETE_GROUP_ALL_MEMBERS: &str = "Delete group for all members";
pub const REMEMBER_MY_CHOICE: &str = "Remember my choice";

/// Username box copy without direct `lang.strings` keys.
pub const USERNAME_INPUT_PH: &str = "@username";
pub const COLLECTIBLE_USERNAMES_HDR: &str = "Collectible Usernames";
pub const COLLECTIBLE_ACTIVE: &str = "active";
pub const COLLECTIBLE_HIDDEN: &str = "hidden";

/// Language box copy without direct `lang.strings` keys.
pub const SEARCH_LANGUAGES_PH: &str = "Search languages...";

/// Photo crop box copy without direct `lang.strings` keys.
pub const CROP_PHOTO_FALLBACK_TITLE: &str = "Crop Photo";
pub const CROP_GROUP_PHOTO_TITLE: &str = "Group Photo";
pub const CROP_CHANNEL_PHOTO_TITLE: &str = "Channel Photo";
pub const CROP_UPDATE_PHOTO_TITLE: &str = "Update Photo";
pub const CROP_ROTATE_90: &str = "Rotate 90°";
pub const CROP_PRESET_1_1: &str = "1:1";
pub const CROP_FLIPPED_TAG: &str = " · Flipped";

/// Terms of Service copy without direct `lang.strings` keys.
pub const TOS_DECLINE_TITLE: &str = "Decline Terms of Service";
pub const TOS_DECLINE_BODY: &str = "If you decline the Terms of Service, you will not be able to continue using this session. Are you sure?";

/// Host toast strings without direct `lang.strings` keys.
pub const TOAST_TOS_ACCEPTED: &str = "Terms of Service accepted";
pub const TOAST_TOS_DECLINED: &str = "Terms of Service declined";
pub const TOAST_CONTACT_ADDED: &str = "Contact added";
pub const TOAST_GROUP_CREATED: &str = "Group created";
pub const TOAST_CHANNEL_CREATED: &str = "Channel created";
pub const TOAST_SAVED_CHANGES: &str = "Saved changes";
pub const TOAST_PERMISSIONS_UPDATED: &str = "Permissions updated";
pub const TOAST_ADMIN_UPDATED: &str = "Admin rights updated";
pub const TOAST_ADMIN_DISMISSED: &str = "Admin dismissed";
pub const TOAST_TTL_UPDATED: &str = "Auto-delete timer updated";
pub const TOAST_DISCUSSION_UPDATED: &str = "Discussion group updated";
pub const TOAST_DELETED: &str = "Deleted";
pub const TOAST_USERNAME_SAVED: &str = "Username saved";
