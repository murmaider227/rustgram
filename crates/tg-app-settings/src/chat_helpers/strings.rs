//! Rustgram-specific strings for `chat_helpers` with no exact `resources/lang.strings` key.
//!
//! All standard Telegram Desktop labels live in `tg_lang::tr::*`.

/// Short badge text painted on GIF mosaic tiles when not hovered.
pub const GIF_BADGE: &str = "GIF";

/// Inline-bot prefix badge shown in the GIF search field.
pub const INLINE_GIF_BOT_PREFIX: &str = "@gif";

/// Compact pill text for ephemeral bot commands in the autocomplete popup
/// (tooltip uses `tg_lang::tr::lng_ephemeral_command_tooltip`).
pub const EPHEMERAL_BADGE: &str = "ephemeral";

/// Hover tooltip for removing a recent inline bot row in mention autocomplete.
pub const REMOVE_RECENT_BOT: &str = "Remove recent bot";

/// Hover tooltip for removing a recent hashtag row in hashtag autocomplete.
pub const REMOVE_RECENT_HASHTAG: &str = "Remove from recent hashtags";

/// Showcase header room title.
pub const SHOWCASE_ROOM_TITLE: &str = "Rustgram Design Room";

/// Showcase sender label for outgoing preview messages.
pub const SHOWCASE_SELF_SENDER: &str = "You";

/// Showcase initial status hint.
pub const SHOWCASE_INITIAL_HINT: &str =
    "Click the smiley button or type @ / # : in the compose bar";

/// Showcase autocomplete screen header title.
pub const SHOWCASE_AUTOCOMPLETE_TITLE: &str =
    "Field Autocomplete Popup — Mentions, Hashtags, Bot Commands & Emoji Suggestions";

/// Showcase autocomplete card headings.
pub const SHOWCASE_AUTOCOMPLETE_CASES: [(&str, &str); 4] = [
    ("@mention autocomplete", "Hey @"),
    ("#hashtag autocomplete", "Tagging #"),
    ("/command autocomplete (with ephemeral)", "/"),
    (":emoji suggestion autocomplete", "Feeling :smi"),
];
