//! Shared chat helper domain types and re-exports from `tg-data` and `tg-text`.
//!
//! Port of relevant data definitions from:
//! - `Telegram/SourceFiles/data/data_peer_values.cpp` (`SortByOnlineValue`)
//! - `Telegram/SourceFiles/chat_helpers/compose/compose_features.h` (`ComposeFeatures`)
//! - `Telegram/lib_ui/ui/text/text_entity.h` and `text_entity.cpp`

pub use tg_data::peer::BotStatus;
pub use tg_data::{
    BotCommand, BusinessShortcutId, DocumentId, EffectId, LastseenStatus, PeerId, TimeId, UserId,
};
pub use tg_text::{MENTION_TAG_START, is_diacritic, remove_accents, utf16_len};

/// Seconds in a day, matching `kSecondsInDay` in `data/data_peer_values.cpp`.
pub const SECONDS_IN_DAY: i32 = 86_400;

/// Bare sticker set identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct StickerSetId(pub u64);

/// Integer 2D size matching `QSize` geometry operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Size {
    /// Width in pixels (or logical units).
    pub width: i32,
    /// Height in pixels (or logical units).
    pub height: i32,
}

impl Size {
    /// Constructs a new `Size`.
    pub const fn new(width: i32, height: i32) -> Self {
        Self { width, height }
    }

    /// Returns `true` when either dimension is non-positive, matching `QSize::isEmpty()`.
    pub const fn is_empty(self) -> bool {
        self.width <= 0 || self.height <= 0
    }

    /// Scales this size to fit inside `bounds` while keeping aspect ratio,
    /// matching `QSize::scaled(bounds, Qt::KeepAspectRatio)`.
    pub fn scaled_keep_aspect(self, bounds: Size) -> Size {
        if self.width <= 0 || self.height <= 0 || bounds.width <= 0 || bounds.height <= 0 {
            return Size::new(0, 0);
        }
        let rw = div_round_i64(
            i64::from(bounds.height) * i64::from(self.width),
            i64::from(self.height),
        );
        if rw <= i64::from(bounds.width) {
            Size::new(rw as i32, bounds.height)
        } else {
            let rh = div_round_i64(
                i64::from(bounds.width) * i64::from(self.height),
                i64::from(self.width),
            );
            Size::new(bounds.width, rh as i32)
        }
    }
}

/// Rounds `numer / denom` half-away-from-zero for non-negative values, matching Qt's `qRound`.
pub(crate) const fn div_round_i64(numer: i64, denom: i64) -> i64 {
    (numer * 2 + denom) / (denom * 2)
}

/// Computes the sort key for ranking users by last activity.
///
/// Delegates to [`tg_data::sort_by_online_status`] with `-1` for bots and service users,
/// matching `Data::SortByOnlineValue` in `Telegram/SourceFiles/data/data_peer_values.cpp`.
pub const fn sort_by_online_value(
    is_service_or_bot: bool,
    lastseen: LastseenStatus,
    now: TimeId,
) -> TimeId {
    if is_service_or_bot {
        return -1;
    }
    tg_data::sort_by_online_status(lastseen, now)
}

/// Lightweight user descriptor used by autocomplete and send menu logic.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UserInfo {
    /// Bare user ID.
    pub id: UserId,
    /// Access hash used for `mention://` tags.
    pub access_hash: u64,
    /// First name.
    pub first_name: String,
    /// Full display name (fallback when `first_name` is empty).
    pub display_name: String,
    /// Primary username (without `@`), if any.
    pub username: String,
    /// Ordered list of active usernames (first is primary when non-empty).
    pub usernames: Vec<String>,
    /// Pre-tokenized, accent-stripped lowercase name words for mention filtering.
    pub name_words: Vec<String>,
    /// Whether this user is a bot.
    pub is_bot: bool,
    /// Whether this user is a Telegram service user.
    pub is_service_user: bool,
    /// Whether this user is the Telegram service notifications peer (777000).
    pub is_notifications_user: bool,
    /// Whether this user is the current account ("Saved Messages").
    pub is_self: bool,
    /// Whether this user is deleted / inaccessible.
    pub is_inaccessible: bool,
    /// Last-seen status for activity sorting and "send when online".
    pub lastseen: LastseenStatus,
    /// Non-empty when the bot supports inline queries.
    pub inline_placeholder: String,
    /// Paid messages star cost (0 when free).
    pub stars_per_message: u64,
}

impl UserInfo {
    /// Returns the primary username (`usernames[0]` if non-empty, else `username`),
    /// matching `PrimaryUsername` in `chat_helpers/field_autocomplete.cpp`.
    pub fn primary_username(&self) -> &str {
        self.usernames
            .first()
            .map(String::as_str)
            .unwrap_or(self.username.as_str())
    }

    /// Returns the display text inserted for mentions without a username:
    /// `first_name` if non-empty, otherwise `display_name`.
    pub fn mention_display_name(&self) -> &str {
        if self.first_name.is_empty() {
            &self.display_name
        } else {
            &self.first_name
        }
    }
}

/// Business quick-reply shortcut metadata, matching `Data::Shortcut`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BusinessShortcut {
    /// Shortcut identifier.
    pub id: BusinessShortcutId,
    /// Shortcut name without leading `/`.
    pub name: String,
    /// Number of messages stored in this shortcut.
    pub count: i32,
}

/// Compose bar feature toggles, matching `ChatHelpers::ComposeFeatures`
/// in `Telegram/SourceFiles/chat_helpers/compose/compose_features.h`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ComposeFeatures {
    /// Whether `#hashtag` autocomplete is enabled.
    pub autocomplete_hashtags: bool,
    /// Whether `@mention` autocomplete is enabled.
    pub autocomplete_mentions: bool,
    /// Whether `/command` autocomplete is enabled.
    pub autocomplete_commands: bool,
    /// Whether emoji-to-sticker autocomplete is enabled.
    pub suggest_stickers_by_emoji: bool,
    /// Whether inline bots are enabled.
    pub inline_bots: bool,
    /// Whether megagroup sticker set section is shown.
    pub megagroup_set: bool,
    /// Whether sending bot commands immediately on Enter/Click is allowed.
    pub bot_command_send: bool,
    /// Whether silent broadcast toggle is shown in channels.
    pub silent_broadcast_toggle: bool,
}

impl Default for ComposeFeatures {
    fn default() -> Self {
        Self {
            autocomplete_hashtags: true,
            autocomplete_mentions: true,
            autocomplete_commands: true,
            suggest_stickers_by_emoji: true,
            inline_bots: true,
            megagroup_set: true,
            bot_command_send: true,
            silent_broadcast_toggle: true,
        }
    }
}

/// Entity representing an inline mention without a username (`MessageEntityMentionName`),
/// carrying both UTF-8 byte offsets and UTF-16 code-unit offsets (used by Telegram MTProto).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MentionNameEntity {
    /// Start offset in UTF-8 bytes within the resulting text.
    pub byte_offset: usize,
    /// Length in UTF-8 bytes within the resulting text.
    pub byte_length: usize,
    /// Start offset in UTF-16 code units (MTProto entity offset).
    pub utf16_offset: usize,
    /// Length in UTF-16 code units (MTProto entity length).
    pub utf16_length: usize,
    /// Mentioned user ID.
    pub user_id: UserId,
    /// Mentioned user access hash.
    pub access_hash: u64,
    /// Internal input-field tag (`mention://<user_id>.<access_hash>:<self_id>`).
    pub tag: String,
}

impl MentionNameEntity {
    /// Converts this mention into a `tg-text` [`tg_text::EntityInText`] with UTF-16 offsets
    /// and serialized [`tg_text::entity::MentionNameFields`] payload.
    pub fn to_entity_in_text(&self, self_id: UserId) -> tg_text::EntityInText {
        let data =
            tg_text::entity::mention_name_data_from_fields(tg_text::entity::MentionNameFields {
                self_id: self_id.0,
                user_id: self.user_id.0,
                access_hash: self.access_hash,
            });
        tg_text::EntityInText::with_data(
            tg_text::EntityType::MentionName,
            self.utf16_offset as i32,
            self.utf16_length as i32,
            data,
        )
    }
}
