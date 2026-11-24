//! Domain types, constants, validation rules, and pure logic for extended history sections,
//! compose extras, and message actions.
//!
//! Port origin:
//! - `Telegram/SourceFiles/data/data_forum_topic.cpp` (`ForumTopicIcons`, `ExtractNonEmojiLetter`)
//! - `Telegram/SourceFiles/data/data_saved_sublist.cpp`
//! - `Telegram/SourceFiles/history/admin_log/history_admin_log_filter_value.h`
//! - `Telegram/SourceFiles/boxes/create_poll_box.cpp` & `data/data_poll.h`
//! - `Telegram/SourceFiles/history/view/history_view_schedule_box.cpp`
//! - `Telegram/SourceFiles/history/view/history_view_scheduled_section.cpp`
//! - `Telegram/SourceFiles/history/view/history_view_pinned_section.cpp`
//! - `Telegram/SourceFiles/history/view/history_view_chat_section.cpp`

use tg_chat::send_options::SCHEDULED_UNTIL_ONLINE_TIMESTAMP;
use tg_text::{EntityInText, EntityType, TextWithEntities, emoji, utf16_to_utf8_offset};

use crate::stand_in::types::{MsgId, PeerId};

// ---------------------------------------------------------------------------
// Thread keys & per-thread drafts
// ---------------------------------------------------------------------------

/// Identifies a distinct message thread within a peer for per-thread drafts and navigation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ThreadKey {
    /// Primary chat history for `peer_id`.
    Main(PeerId),
    /// Forum topic thread inside a forum supergroup.
    Topic {
        /// Owning forum peer id.
        peer_id: PeerId,
        /// Topic root message id (`1` for the General topic).
        topic_root_id: MsgId,
    },
    /// Channel post discussion / comments thread.
    Replies {
        /// Channel or discussion group peer id.
        peer_id: PeerId,
        /// Root post message id.
        root_msg_id: MsgId,
    },
    /// Saved Messages per-chat sublist.
    SavedSublist {
        /// Saved Messages self peer id.
        peer_id: PeerId,
        /// Original chat/author peer id of the saved sublist.
        sublist_peer_id: PeerId,
    },
}

impl ThreadKey {
    /// Returns the owning peer id of this thread.
    #[must_use]
    pub const fn peer_id(self) -> PeerId {
        match self {
            Self::Main(p)
            | Self::Topic { peer_id: p, .. }
            | Self::Replies { peer_id: p, .. }
            | Self::SavedSublist { peer_id: p, .. } => p,
        }
    }
}

/// Optional quoted excerpt when replying to a specific passage of a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplyQuote {
    /// Quoted substring text.
    pub text: String,
    /// Preserved UTF-16 entities relative to `text`.
    pub entities: Vec<EntityInText>,
    /// UTF-16 offset of the quote within the original message.
    pub offset_utf16: i32,
}

// ---------------------------------------------------------------------------
// Forum topics (`data/data_forum_topic.cpp`)
// ---------------------------------------------------------------------------

/// Exact 6 Telegram Desktop forum topic icon colors (`Data::ForumTopicIcons` in
/// `Telegram/SourceFiles/data/data_forum_topic.cpp:47-57`).
pub const FORUM_TOPIC_ICON_COLORS: [(u32, &str); 6] = [
    (0x6FB9F0, "blue"),
    (0xFFD67E, "yellow"),
    (0xCB86DB, "violet"),
    (0x8EEE98, "green"),
    (0xFF93B2, "rose"),
    (0xFB6F5F, "red"),
];

/// Fallback gray color for unknown forum topic color ids (`ForumTopicDefaultIcon`).
pub const FORUM_TOPIC_DEFAULT_COLOR: u32 = 0x8E9BA8;

/// General topic root message id (`kGeneralId = 1` in `data_forum_topic.h`).
pub const FORUM_GENERAL_TOPIC_ID: MsgId = MsgId(1);

/// A selectable custom emoji icon for forum topics (`Data::ForumIcons` in `data/data_forum_icons.cpp`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForumTopicIconEntry {
    /// Telegram custom emoji document ID sent as `icon_emoji_id`.
    pub document_id: i64,
    /// Fallback Unicode emoji character (from `DocumentAttribute::CustomEmoji` / `Sticker`).
    pub alt_emoji: String,
    /// True if the icon document is animated (`.tgs` or `.webm`).
    pub is_animated: bool,
    /// Descriptive label or tooltip.
    pub title: String,
}

/// Built-in default forum topic icon choices used for offline/mock sessions before live
/// `messages.getStickerSet(inputStickerSetEmojiDefaultTopicIcons)` loads.
#[must_use]
pub fn default_forum_topic_icons() -> Vec<ForumTopicIconEntry> {
    [
        (5309984423003823246_i64, "💬", "General"),
        (5310023225525163645_i64, "📢", "Announcements"),
        (5310042299441387822_i64, "💡", "Ideas"),
        (5310095610857165401_i64, "🔥", "Hot"),
        (5310114887026890011_i64, "❓", "Questions"),
        (5310158101863759020_i64, "🛠", "Support"),
        (5310182972844973015_i64, "🎨", "Design"),
        (5310207719272112144_i64, "🎮", "Games"),
        (5310246040530607250_i64, "🎵", "Music"),
        (5310275917099047933_i64, "📚", "Resources"),
        (5310309562010574911_i64, "⭐", "Featured"),
        (5310339947654291502_i64, "📌", "Pinned"),
    ]
    .into_iter()
    .map(|(document_id, alt_emoji, title)| ForumTopicIconEntry {
        document_id,
        alt_emoji: alt_emoji.to_owned(),
        is_animated: false,
        title: title.to_owned(),
    })
    .collect()
}

/// Position of the subsection tabs strip (`HistoryView::SubsectionTabsMode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(i32)]
pub enum SubsectionTabsMode {
    /// Horizontal strip above the message history (`Top = 0`).
    #[default]
    Top = 0,
    /// Vertical sidebar on the left edge of the history pane (`Left = 1`).
    Left = 1,
    /// Horizontal strip below the compose bar (`Bottom = 2`).
    Bottom = 2,
}

impl SubsectionTabsMode {
    /// Cycles `Top -> Left -> Bottom -> Top`.
    #[must_use]
    pub const fn next(self) -> Self {
        match self {
            Self::Top => Self::Left,
            Self::Left => Self::Bottom,
            Self::Bottom => Self::Top,
        }
    }

    /// Parses a stored mode integer (`0 = Top`, `1 = Left`, `2 = Bottom`).
    #[must_use]
    pub const fn from_i32(raw: i32) -> Self {
        match raw {
            1 => Self::Left,
            2 => Self::Bottom,
            _ => Self::Top,
        }
    }

    /// Human-readable label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Top => "Tabs on Top",
            Self::Left => "Tabs on Left",
            Self::Bottom => "Tabs on Bottom",
        }
    }
}

/// Visual appearance of subsection tabs (`HistoryView::SubsectionTabsAppearance`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(i32)]
pub enum SubsectionTabsAppearance {
    /// Show both icon/avatar and text title (`TextAndIcons = 0`).
    #[default]
    TextAndIcons = 0,
    /// Show text title only (`TextOnly = 1`).
    TextOnly = 1,
    /// Show icon/avatar only (`IconsOnly = 2`).
    IconsOnly = 2,
}

impl SubsectionTabsAppearance {
    /// Parses a stored appearance integer (`0 = TextAndIcons`, `1 = TextOnly`, `2 = IconsOnly`).
    #[must_use]
    pub const fn from_i32(raw: i32) -> Self {
        match raw {
            1 => Self::TextOnly,
            2 => Self::IconsOnly,
            _ => Self::TextAndIcons,
        }
    }

    /// Human-readable label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::TextAndIcons => "Icons and Text",
            Self::TextOnly => "Text Only",
            Self::IconsOnly => "Icons Only",
        }
    }
}

/// Packs `(mode, appearance)` into tdesktop's `session_settings.subsection_tabs_modes` bit format
/// (`kModeShift = 0`, `kAppearanceShift = 8`, `kFieldMask = 0xFF`).
#[must_use]
pub const fn pack_subsection_tabs_state(
    mode: SubsectionTabsMode,
    appearance: SubsectionTabsAppearance,
) -> i32 {
    ((mode as u32 & 0xFF) | (((appearance as u32) & 0xFF) << 8)) as i32
}

/// Unpacks a stored `subsection_tabs_modes` integer back into `(mode, appearance)`.
#[must_use]
pub const fn unpack_subsection_tabs_state(
    stored: i32,
) -> (SubsectionTabsMode, SubsectionTabsAppearance) {
    let bits = stored as u32;
    let mode = SubsectionTabsMode::from_i32((bits & 0xFF) as i32);
    let appearance = SubsectionTabsAppearance::from_i32(((bits >> 8) & 0xFF) as i32);
    (mode, appearance)
}

/// Unpacks a `0xRRGGBB` color integer into `[r, g, b]`.
#[inline]
#[must_use]
pub const fn rgb_from_hex(hex: u32) -> [u8; 3] {
    [
        ((hex >> 16) & 0xFF) as u8,
        ((hex >> 8) & 0xFF) as u8,
        (hex & 0xFF) as u8,
    ]
}

/// Returns the canonical tdesktop icon color name (`"blue"`, `"yellow"`, ..., or `"gray"`).
#[must_use]
pub fn forum_topic_color_name(color_id: u32) -> &'static str {
    FORUM_TOPIC_ICON_COLORS
        .iter()
        .find(|&&(hex, _)| hex == color_id)
        .map_or("gray", |&(_, name)| name)
}

/// Extracts the first non-emoji alphanumeric character from a topic title for rendering
/// inside the default colored topic bubble icon.
///
/// Direct port of `Data::ExtractNonEmojiLetter` in `data/data_forum_topic.cpp:98-123`.
#[must_use]
pub fn extract_non_emoji_letter(title: &str) -> Option<String> {
    let mut pos = 0usize;
    while pos < title.len() {
        if let Some((_, matched_bytes)) = emoji::find_emoji(title, pos) {
            pos += matched_bytes;
            continue;
        }
        let Some(ch) = title[pos..].chars().next() else {
            break;
        };
        if ch.is_alphanumeric() {
            return Some(ch.to_uppercase().collect());
        }
        pos += ch.len_utf8();
    }
    None
}

/// Forum topic metadata (`Data::ForumTopic`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForumTopic {
    /// Root message id identifying this topic (`MsgId(1)` for General).
    pub root_id: MsgId,
    /// Owning forum peer id.
    pub peer_id: PeerId,
    /// Topic title.
    pub title: String,
    /// `0xRRGGBB` icon color id from [`FORUM_TOPIC_ICON_COLORS`].
    pub icon_color: u32,
    /// Optional custom emoji glyph shown instead of the colored bubble icon.
    pub custom_emoji: Option<String>,
    /// True if this is the General topic (`#`).
    pub is_general: bool,
    /// True if pinned at the top of the topics list.
    pub is_pinned: bool,
    /// True if closed (non-admins cannot send messages).
    pub is_closed: bool,
    /// True if muted.
    pub muted: bool,
    /// Unread message count in this topic.
    pub unread_count: u32,
    /// Unread `@` mentions count in this topic.
    pub unread_mentions: u32,
    /// Sender name of the most recent message.
    pub last_sender: String,
    /// Single-line preview of the most recent message.
    pub last_preview: String,
    /// Unix timestamp of the most recent message.
    pub last_date_unix: i32,
    /// Optional unsent draft preview text for this topic.
    pub draft_preview: Option<String>,
}

impl ForumTopic {
    /// Returns the `[r, g, b]` color for this topic's bubble icon.
    #[must_use]
    pub const fn icon_rgb(&self) -> [u8; 3] {
        if self.icon_color == 0 {
            rgb_from_hex(FORUM_TOPIC_DEFAULT_COLOR)
        } else {
            rgb_from_hex(self.icon_color)
        }
    }
}

// ---------------------------------------------------------------------------
// Saved Messages per-chat sublists (`data/data_saved_sublist.cpp`)
// ---------------------------------------------------------------------------

/// Saved Messages sublist grouped by original peer (`Data::SavedSublist`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedSublist {
    /// Original chat/author peer id (or self peer id for "My Notes").
    pub sublist_peer_id: PeerId,
    /// Display title (e.g. `"My Notes"`, `"Rustgram Core Port"`).
    pub title: String,
    /// Avatar initials.
    pub initials: String,
    /// Avatar fallback RGB color.
    pub avatar_rgb: [u8; 3],
    /// True if this sublist is "My Notes" (messages authored directly in Saved Messages).
    pub is_my_notes: bool,
    /// True if pinned to the top of the sublists list.
    pub is_pinned: bool,
    /// Number of saved messages in this sublist.
    pub message_count: u32,
    /// Preview of the newest saved message in this sublist.
    pub last_preview: String,
    /// Timestamp of the newest saved message.
    pub last_date_unix: i32,
}

// ---------------------------------------------------------------------------
// Channel Admin Log / Recent Actions (`history/admin_log/history_admin_log_filter_value.h`)
// ---------------------------------------------------------------------------

/// Bitmask of channel admin log filter flags matching `AdminLog::FilterValue::Flag`
/// in `Telegram/SourceFiles/history/admin_log/history_admin_log_filter_value.h:13-35`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct AdminLogFilterFlags(pub u32);

impl AdminLogFilterFlags {
    /// New members joined (`1 << 0`).
    pub const JOIN: Self = Self(1 << 0);
    /// Members left (`1 << 1`).
    pub const LEAVE: Self = Self(1 << 1);
    /// Members invited (`1 << 2`).
    pub const INVITE: Self = Self(1 << 2);
    /// Members banned / restricted (`1 << 3`).
    pub const BAN: Self = Self(1 << 3);
    /// Members unbanned (`1 << 4`).
    pub const UNBAN: Self = Self(1 << 4);
    /// Members kicked (`1 << 5`).
    pub const KICK: Self = Self(1 << 5);
    /// Members unkicked (`1 << 6`).
    pub const UNKICK: Self = Self(1 << 6);
    /// Admin promoted (`1 << 7`).
    pub const PROMOTE: Self = Self(1 << 7);
    /// Admin demoted (`1 << 8`).
    pub const DEMOTE: Self = Self(1 << 8);
    /// Channel/group info changed (`1 << 9`).
    pub const INFO: Self = Self(1 << 9);
    /// Channel/group settings changed (`1 << 10`).
    pub const SETTINGS: Self = Self(1 << 10);
    /// Messages pinned/unpinned (`1 << 11`).
    pub const PINNED: Self = Self(1 << 11);
    /// Messages edited (`1 << 12`).
    pub const EDIT: Self = Self(1 << 12);
    /// Messages deleted (`1 << 13`).
    pub const DELETE: Self = Self(1 << 13);
    /// Group call / live stream events (`1 << 14`).
    pub const GROUP_CALL: Self = Self(1 << 14);
    /// Invite links created/edited/revoked (`1 << 15`).
    pub const INVITES: Self = Self(1 << 15);
    /// Forum topics created/edited/deleted (`1 << 16`).
    pub const TOPICS: Self = Self(1 << 16);
    /// Subscription extended (`1 << 17`).
    pub const SUB_EXTEND: Self = Self(1 << 17);
    /// Member rank edited (`1 << 18`).
    pub const EDIT_RANK: Self = Self(1 << 18);

    /// All 19 event flags enabled.
    pub const ALL: Self = Self((1 << 19) - 1);

    /// Returns `true` if `flag` bits are all set in `self`.
    #[inline]
    #[must_use]
    pub const fn contains(self, flag: Self) -> bool {
        (self.0 & flag.0) == flag.0
    }

    /// Returns `self | other`.
    #[inline]
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Toggles all bits in `flag`.
    #[inline]
    pub fn toggle(&mut self, flag: Self) {
        if self.contains(flag) {
            self.0 &= !flag.0;
        } else {
            self.0 |= flag.0;
        }
    }

    /// Sets or clears `flag`.
    #[inline]
    pub fn set(&mut self, flag: Self, enabled: bool) {
        if enabled {
            self.0 |= flag.0;
        } else {
            self.0 &= !flag.0;
        }
    }

    /// Returns `true` if all 19 flags are set.
    #[inline]
    #[must_use]
    pub const fn is_all(self) -> bool {
        (self.0 & Self::ALL.0) == Self::ALL.0
    }

    /// Human-readable label for a single flag.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self.0 {
            x if x == Self::JOIN.0 => "New members",
            x if x == Self::LEAVE.0 => "Members left",
            x if x == Self::INVITE.0 => "Members invited",
            x if x == Self::BAN.0 => "Banned & restricted",
            x if x == Self::UNBAN.0 => "Unbanned",
            x if x == Self::KICK.0 => "Kicked",
            x if x == Self::UNKICK.0 => "Unkicked",
            x if x == Self::PROMOTE.0 => "New admins",
            x if x == Self::DEMOTE.0 => "Admin rights changed",
            x if x == Self::INFO.0 => "Group/channel info",
            x if x == Self::SETTINGS.0 => "Group/channel settings",
            x if x == Self::PINNED.0 => "Pinned messages",
            x if x == Self::EDIT.0 => "Edited messages",
            x if x == Self::DELETE.0 => "Deleted messages",
            x if x == Self::GROUP_CALL.0 => "Video chats",
            x if x == Self::INVITES.0 => "Invite links",
            x if x == Self::TOPICS.0 => "Forum topics",
            x if x == Self::SUB_EXTEND.0 => "Subscriptions",
            x if x == Self::EDIT_RANK.0 => "Member ranks",
            _ => "Events",
        }
    }
}

/// Logical category rows displayed in the Admin Log Filter box (`history_admin_log_filter.cpp`).
pub const ADMIN_LOG_FILTER_CATEGORIES: &[(&str, AdminLogFilterFlags)] = &[
    (
        "Member restrictions",
        AdminLogFilterFlags(
            AdminLogFilterFlags::BAN.0
                | AdminLogFilterFlags::UNBAN.0
                | AdminLogFilterFlags::KICK.0
                | AdminLogFilterFlags::UNKICK.0,
        ),
    ),
    (
        "New members & invites",
        AdminLogFilterFlags(
            AdminLogFilterFlags::JOIN.0
                | AdminLogFilterFlags::INVITE.0
                | AdminLogFilterFlags::INVITES.0,
        ),
    ),
    ("Members leaving", AdminLogFilterFlags::LEAVE),
    (
        "Admin changes",
        AdminLogFilterFlags(
            AdminLogFilterFlags::PROMOTE.0
                | AdminLogFilterFlags::DEMOTE.0
                | AdminLogFilterFlags::EDIT_RANK.0,
        ),
    ),
    (
        "Group & channel info",
        AdminLogFilterFlags(AdminLogFilterFlags::INFO.0 | AdminLogFilterFlags::SETTINGS.0),
    ),
    ("Deleted messages", AdminLogFilterFlags::DELETE),
    ("Edited messages", AdminLogFilterFlags::EDIT),
    ("Pinned messages", AdminLogFilterFlags::PINNED),
    ("Forum topics", AdminLogFilterFlags::TOPICS),
    ("Video chats & calls", AdminLogFilterFlags::GROUP_CALL),
];

/// Active filter for Channel Admin Log (`AdminLog::FilterValue`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AdminLogFilter {
    /// `None` means all events; `Some(flags)` restricts to matching event flags.
    pub flags: Option<AdminLogFilterFlags>,
    /// `None` means all admins; `Some(ids)` restricts to specific admin actor peer ids.
    pub admin_ids: Option<Vec<PeerId>>,
    /// Optional free-text search query.
    pub search_query: String,
}

impl AdminLogFilter {
    /// Returns `true` when any event flag, admin subset, or search filter is active.
    #[must_use]
    pub fn is_filtered(&self) -> bool {
        self.flags.is_some_and(|f| !f.is_all())
            || self.admin_ids.is_some()
            || !self.search_query.trim().is_empty()
    }

    /// Returns `true` if `entry` passes all active filter criteria.
    #[must_use]
    pub fn matches(&self, entry: &AdminLogEntry) -> bool {
        if let Some(flags) = self.flags
            && !flags.contains(entry.flag)
        {
            return false;
        }
        if let Some(ref admins) = self.admin_ids
            && !admins.contains(&entry.actor_id)
        {
            return false;
        }
        let q = self.search_query.trim().to_lowercase();
        if !q.is_empty() {
            let haystack_hit = entry.summary.to_lowercase().contains(&q)
                || entry.actor_name.to_lowercase().contains(&q)
                || entry
                    .before_text
                    .as_deref()
                    .is_some_and(|s| s.to_lowercase().contains(&q))
                || entry
                    .after_text
                    .as_deref()
                    .is_some_and(|s| s.to_lowercase().contains(&q));
            if !haystack_hit {
                return false;
            }
        }
        true
    }
}

/// Administrator descriptor shown in the Admin Log Filter box.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminLogAdmin {
    /// Admin peer id.
    pub peer_id: PeerId,
    /// Display name.
    pub name: String,
    /// Role label (e.g. `"Owner"`, `"Admin"`).
    pub role: String,
}

/// Single Channel Admin Log / Recent Actions record (`AdminLog::HistoryItem`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminLogEntry {
    /// Monotonic event id.
    pub id: u64,
    /// Unix timestamp in seconds.
    pub date_unix: i32,
    /// Actor who performed the action.
    pub actor_id: PeerId,
    /// Actor display name.
    pub actor_name: String,
    /// Event category flag (one of [`AdminLogFilterFlags`]).
    pub flag: AdminLogFilterFlags,
    /// Human-readable description of the event.
    pub summary: String,
    /// Optional "before" text snapshot (for edited/deleted messages or changed settings).
    pub before_text: Option<String>,
    /// Optional "after" text snapshot (for edited messages or new settings).
    pub after_text: Option<String>,
}

// ---------------------------------------------------------------------------
// Scheduled messages & civil calendar math (`history_view_schedule_box.cpp`)
// ---------------------------------------------------------------------------

/// Maximum future scheduling window in Telegram (365 days in seconds).
pub const MAX_SCHEDULE_HORIZON_SECS: i32 = 365 * 86_400;

/// Target schedule mode emitted by the Schedule Box.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduleTarget {
    /// Explicit Unix UTC timestamp in seconds.
    Timestamp(i32),
    /// Send when the recipient comes online (`0x7FFFFFFE`).
    WhenOnline,
}

impl ScheduleTarget {
    /// Converts to the MTProto/Telegram `schedule_date` integer (`TimeId`).
    #[inline]
    #[must_use]
    pub const fn to_time_id(self) -> i32 {
        match self {
            Self::Timestamp(ts) => ts,
            Self::WhenOnline => SCHEDULED_UNTIL_ONLINE_TIMESTAMP,
        }
    }

    /// Converts from a `schedule_date` integer.
    #[inline]
    #[must_use]
    pub const fn from_time_id(ts: i32) -> Self {
        if ts == SCHEDULED_UNTIL_ONLINE_TIMESTAMP {
            Self::WhenOnline
        } else {
            Self::Timestamp(ts)
        }
    }
}

/// Pure civil date + time (UTC) for the Schedule Box calendar picker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScheduleDateTime {
    /// Full Gregorian year (e.g. `2026`).
    pub year: i32,
    /// Month `1..=12`.
    pub month: u32,
    /// Day of month `1..=31`.
    pub day: u32,
    /// Hour `0..=23`.
    pub hour: u32,
    /// Minute `0..=59`.
    pub minute: u32,
}

impl ScheduleDateTime {
    /// Converts Unix UTC seconds to a [`ScheduleDateTime`].
    #[must_use]
    pub fn from_unix_utc(unix_secs: i64) -> Self {
        let days = unix_secs.div_euclid(86_400);
        let rem = unix_secs.rem_euclid(86_400) as u32;
        let hour = rem / 3600;
        let minute = (rem % 3600) / 60;
        let (year, month, day) = civil_from_days(days);
        Self {
            year,
            month,
            day,
            hour,
            minute,
        }
    }

    /// Converts this [`ScheduleDateTime`] back to Unix UTC seconds.
    #[must_use]
    pub fn to_unix_utc(self) -> i64 {
        let days = days_from_civil(self.year, self.month, self.day);
        days * 86_400 + i64::from(self.hour) * 3600 + i64::from(self.minute) * 60
    }

    /// Formats a friendly date/time label (e.g. `"Nov 22, 2026 at 14:30"`).
    #[must_use]
    pub fn format_display(self) -> String {
        format!(
            "{} {}, {} at {:02}:{:02}",
            short_month_name(self.month),
            self.day,
            self.year,
            self.hour,
            self.minute
        )
    }
}

/// Returns `true` if `year` is a Gregorian leap year.
#[inline]
#[must_use]
pub const fn is_leap_year(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}

/// Returns the number of days in `(year, month)` (`month` in `1..=12`).
#[must_use]
pub const fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap_year(year) {
                29
            } else {
                28
            }
        }
        _ => 30,
    }
}

/// Returns weekday index (`0 = Monday .. 6 = Sunday`) for `(year, month, day)`.
#[must_use]
pub const fn weekday_monday_zero(year: i32, month: u32, day: u32) -> u32 {
    // 1970-01-01 was a Thursday (index 3 in Monday=0..Sunday=6).
    let days = days_from_civil(year, month, day);
    (days + 3).rem_euclid(7) as u32
}

/// Howard Hinnant's exact civil-date-to-days-since-1970-01-01 conversion.
#[must_use]
pub const fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    let y = (year as i64) - if month <= 2 { 1 } else { 0 };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let m = month as u64;
    let d = day as u64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + (doe as i64) - 719_468
}

/// Howard Hinnant's exact days-since-1970-01-01-to-civil-date conversion.
#[must_use]
pub const fn civil_from_days(days: i64) -> (i32, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = (y + if m <= 2 { 1 } else { 0 }) as i32;
    (year, m as u32, d as u32)
}

/// Full English month name for `1..=12`.
#[must_use]
pub const fn month_name(month: u32) -> &'static str {
    match month {
        1 => "January",
        2 => "February",
        3 => "March",
        4 => "April",
        5 => "May",
        6 => "June",
        7 => "July",
        8 => "August",
        9 => "September",
        10 => "October",
        11 => "November",
        12 => "December",
        _ => "Month",
    }
}

/// Short 3-letter month name for `1..=12`.
#[must_use]
pub const fn short_month_name(month: u32) -> &'static str {
    match month {
        1 => "Jan",
        2 => "Feb",
        3 => "Mar",
        4 => "Apr",
        5 => "May",
        6 => "Jun",
        7 => "Jul",
        8 => "Aug",
        9 => "Sep",
        10 => "Oct",
        11 => "Nov",
        12 => "Dec",
        _ => "Mon",
    }
}

/// Validates that `target` is in the future relative to `now_unix` and within 1 year.
pub fn validate_schedule_target(target: ScheduleTarget, now_unix: i32) -> Result<(), String> {
    match target {
        ScheduleTarget::WhenOnline => Ok(()),
        ScheduleTarget::Timestamp(ts) => {
            if ts <= now_unix {
                Err("Scheduled time must be in the future.".to_owned())
            } else if ts - now_unix > MAX_SCHEDULE_HORIZON_SECS {
                Err("Messages can be scheduled at most 365 days in advance.".to_owned())
            } else {
                Ok(())
            }
        }
    }
}

/// Formats a scheduled message's schedule badge label.
#[must_use]
pub fn format_schedule_badge(schedule_date: i32) -> String {
    if schedule_date == SCHEDULED_UNTIL_ONLINE_TIMESTAMP {
        "Send when online".to_owned()
    } else {
        let dt = ScheduleDateTime::from_unix_utc(i64::from(schedule_date));
        format!("Scheduled for {}", dt.format_display())
    }
}

// ---------------------------------------------------------------------------
// Polls & Quizzes creation (`boxes/create_poll_box.cpp`, `data/data_poll.h`)
// ---------------------------------------------------------------------------

/// Maximum poll question length in characters (`kQuestionLimit = 255` in `create_poll_box.cpp:107`).
pub const POLL_QUESTION_LIMIT: usize = 255;
/// Remaining-char threshold to display warning counter on question (`kWarnQuestionLimit = 80`).
pub const POLL_WARN_QUESTION_LIMIT: usize = 80;
/// Maximum poll option length in characters (`kOptionLimit = 100` in `create_poll_box.cpp:109`).
pub const POLL_OPTION_LIMIT: usize = 100;
/// Remaining-char threshold to display warning counter on option (`kWarnOptionLimit = 30`).
pub const POLL_WARN_OPTION_LIMIT: usize = 30;
/// Maximum quiz explanation length (`kSolutionLimit = 200` in `create_poll_box.cpp:112`).
pub const POLL_SOLUTION_LIMIT: usize = 200;
/// Remaining-char threshold to display warning counter on explanation (`kWarnSolutionLimit = 60`).
pub const POLL_WARN_SOLUTION_LIMIT: usize = 60;
/// Minimum non-empty options required to create a poll.
pub const POLL_MIN_OPTIONS: usize = 2;
/// Standard maximum poll options count in Telegram (`10`, extended `PollData::kMaxOptions = 32`).
pub const POLL_MAX_OPTIONS: usize = 10;
/// Extended maximum poll options (`PollData::kMaxOptions = 32` in `data/data_poll.h:143`).
pub const POLL_MAX_OPTIONS_EXTENDED: usize = 32;

/// Layer-233 media or link attachment attached to a poll option, question, or quiz solution
/// (`poll/poll_link_box.cpp`, `poll/poll_media_upload.cpp`, `InputPollAnswer.media`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PollOptionAttachment {
    /// Web link attached to an option (`InputMediaWebPage`).
    Link {
        /// Target URL (must start with `http://`, `https://`, or `t.me/`).
        url: String,
        /// Optional resolved preview title.
        title: Option<String>,
    },
    /// Photo or document file attached to an option (`InputMediaUploadedPhoto` / `Document`).
    Media {
        /// Display file name (e.g. `"option_photo.jpg"`).
        file_name: String,
        /// MIME type (e.g. `"image/jpeg"`, `"image/png"`, `"application/pdf"`).
        mime_type: String,
        /// Raw bytes to upload (or empty for already-received remote media preview).
        bytes: Vec<u8>,
    },
}

impl PollOptionAttachment {
    /// Short badge label displayed beside a poll option row.
    #[must_use]
    pub fn display_badge(&self) -> String {
        match self {
            Self::Link { url, title } => {
                if let Some(t) = title.as_deref().filter(|s| !s.trim().is_empty()) {
                    format!("🔗 {t}")
                } else {
                    format!("🔗 {url}")
                }
            }
            Self::Media { file_name, .. } => format!("📎 {file_name}"),
        }
    }
}

/// Layer-233 poll extensions stored alongside a poll message (`Poll.open_answers`,
/// `Poll.shuffle_answers`, `Poll.revoting_disabled`, `Poll.hide_results_until_close`,
/// and per-option media/link/author metadata).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PollMessageExtras {
    /// Allow chat members to append new options (`messages.addPollAnswer`).
    pub open_answers: bool,
    /// Shuffle answer display order.
    pub shuffle_answers: bool,
    /// Disallow retracting/changing votes after voting.
    pub revoting_disabled: bool,
    /// Hide intermediate percentages until the poll closes.
    pub hide_results_until_close: bool,
    /// Optional per-option media or link attachments aligned by option index.
    pub option_attachments: Vec<Option<PollOptionAttachment>>,
    /// Raw Telegram option byte tokens aligned by option index (for `messages.deletePollAnswer`).
    pub option_bytes: Vec<Vec<u8>>,
    /// Optional author display names for options appended by members (`PollAnswer.added_by`).
    pub option_added_by: Vec<Option<String>>,
    /// Optional media attached to the poll question (`InputMediaPoll.attached_media`).
    pub attached_media: Option<PollOptionAttachment>,
    /// Optional media attached to the quiz explanation (`InputMediaPoll.solution_media`).
    pub solution_media: Option<PollOptionAttachment>,
}

/// Request payload for `messages.addPollAnswer` (`history_view_add_poll_option.cpp`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddPollOptionRequest {
    /// Option label text (1..=100 chars).
    pub text: String,
    /// Optional Layer-233 link or media attachment on the new option.
    pub attachment: Option<PollOptionAttachment>,
}

/// Validates a URL entered in `PollLinkBox` (`poll/poll_link_box.cpp`).
pub fn validate_poll_link_url(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("Please enter a link URL.".to_owned());
    }
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        Ok(trimmed.to_owned())
    } else if trimmed.contains('.') && !trimmed.contains(char::is_whitespace) {
        Ok(format!("https://{trimmed}"))
    } else {
        Err("Please enter a valid http(s) URL.".to_owned())
    }
}

/// Request payload emitted when creating a new poll or quiz.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatePollRequest {
    /// Poll question text (1..=255 chars).
    pub question: String,
    /// Non-empty answer choices (2..=10, each 1..=100 chars).
    pub options: Vec<String>,
    /// Anonymous voting flag.
    pub is_anonymous: bool,
    /// Multiple answers allowed (must be `false` when `is_quiz` is `true`).
    pub multiple_choice: bool,
    /// Quiz mode flag.
    pub is_quiz: bool,
    /// Zero-based index of the correct option (required when `is_quiz` is `true`).
    pub correct_option: Option<usize>,
    /// Optional quiz explanation shown after voting (max 200 chars).
    pub explanation: Option<String>,
    /// Optional silent send flag.
    pub silent: bool,
    /// Optional schedule timestamp.
    pub schedule_date: Option<i32>,
}

/// Validation failure returned by [`validate_create_poll`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PollValidationError {
    /// Question is blank.
    EmptyQuestion,
    /// Question exceeds 255 characters.
    QuestionTooLong {
        /// Actual length in characters.
        len: usize,
    },
    /// Fewer than 2 non-empty options.
    TooFewOptions {
        /// Actual count of non-empty options.
        count: usize,
    },
    /// More than `max_options` choices.
    TooManyOptions {
        /// Actual count.
        count: usize,
        /// Limit.
        max: usize,
    },
    /// An individual option exceeds 100 characters.
    OptionTooLong {
        /// Option index (0-based).
        index: usize,
        /// Actual length.
        len: usize,
    },
    /// Quiz mode requires selecting a valid correct option index.
    MissingQuizAnswer,
    /// Quiz mode cannot combine with multiple-choice voting.
    QuizCannotBeMultipleChoice,
    /// Quiz explanation exceeds 200 characters.
    ExplanationTooLong {
        /// Actual length.
        len: usize,
    },
}

impl PollValidationError {
    /// Human-readable validation message for the poll creation box.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::EmptyQuestion => "Please enter a poll question.".to_owned(),
            Self::QuestionTooLong { len } => {
                format!("Question is too long ({len}/{POLL_QUESTION_LIMIT} characters).")
            }
            Self::TooFewOptions { .. } => {
                format!("Please add at least {POLL_MIN_OPTIONS} answer options.")
            }
            Self::TooManyOptions { count, max } => {
                format!("Too many options ({count}/{max} maximum).")
            }
            Self::OptionTooLong { index, len } => {
                format!(
                    "Option #{} is too long ({len}/{POLL_OPTION_LIMIT} characters).",
                    index + 1
                )
            }
            Self::MissingQuizAnswer => "Please choose the correct answer for Quiz Mode.".to_owned(),
            Self::QuizCannotBeMultipleChoice => {
                "Quiz Mode does not support multiple answers.".to_owned()
            }
            Self::ExplanationTooLong { len } => {
                format!("Explanation is too long ({len}/{POLL_SOLUTION_LIMIT} characters).")
            }
        }
    }
}

/// Validates a [`CreatePollRequest`] against tdesktop's exact `create_poll_box.cpp` rules.
pub fn validate_create_poll(
    req: &CreatePollRequest,
    max_options: usize,
) -> Result<(), PollValidationError> {
    let q = req.question.trim();
    if q.is_empty() {
        return Err(PollValidationError::EmptyQuestion);
    }
    let q_len = q.chars().count();
    if q_len > POLL_QUESTION_LIMIT {
        return Err(PollValidationError::QuestionTooLong { len: q_len });
    }

    let non_empty: Vec<&str> = req
        .options
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    if non_empty.len() < POLL_MIN_OPTIONS {
        return Err(PollValidationError::TooFewOptions {
            count: non_empty.len(),
        });
    }
    let cap = max_options.clamp(POLL_MIN_OPTIONS, POLL_MAX_OPTIONS_EXTENDED);
    if non_empty.len() > cap {
        return Err(PollValidationError::TooManyOptions {
            count: non_empty.len(),
            max: cap,
        });
    }
    for (idx, opt) in non_empty.iter().enumerate() {
        let len = opt.chars().count();
        if len > POLL_OPTION_LIMIT {
            return Err(PollValidationError::OptionTooLong { index: idx, len });
        }
    }

    if req.is_quiz {
        if req.multiple_choice {
            return Err(PollValidationError::QuizCannotBeMultipleChoice);
        }
        match req.correct_option {
            Some(idx) if idx < non_empty.len() => {}
            _ => return Err(PollValidationError::MissingQuizAnswer),
        }
        if let Some(ref expl) = req.explanation {
            let len = expl.trim().chars().count();
            if len > POLL_SOLUTION_LIMIT {
                return Err(PollValidationError::ExplanationTooLong { len });
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Compose extras domain types: Send-As, Slow Mode, Location, Contact,
// Round Video, Link Preview, Inline Bots, Message Effects, Forward/Report
// ---------------------------------------------------------------------------

/// Peer selectable in the "Send As" chooser (`SendAsButton`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendAsPeer {
    /// Target peer id to send messages as.
    pub peer_id: PeerId,
    /// Display title.
    pub title: String,
    /// Subtitle (e.g. `"Personal account"`, `"Channel · 4.2K subscribers"`).
    pub subtitle: String,
    /// True if this is a broadcast channel identity.
    pub is_channel: bool,
    /// Avatar fallback RGB.
    pub avatar_rgb: [u8; 3],
}

/// Per-peer slow mode configuration and active cooldown timer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SlowModeState {
    /// Configured slow mode delay in seconds (`0` = disabled).
    pub seconds: u32,
    /// Unix timestamp (seconds) when the user is allowed to send next (`0` = ready now).
    pub next_send_unix: i64,
}

impl SlowModeState {
    /// Returns remaining cooldown seconds at `now_unix` (`0` if ready to send).
    #[must_use]
    pub fn remaining_secs(&self, now_unix: i64) -> u32 {
        if self.seconds == 0 || self.next_send_unix <= now_unix {
            0
        } else {
            (self.next_send_unix - now_unix).min(i64::from(u32::MAX)) as u32
        }
    }

    /// Formats remaining cooldown as `M:SS` (or `H:MM:SS` if >= 1 hour).
    #[must_use]
    pub fn format_countdown(secs: u32) -> String {
        if secs >= 3600 {
            format!("{}:{:02}:{:02}", secs / 3600, (secs % 3600) / 60, secs % 60)
        } else {
            format!("{}:{:02}", secs / 60, secs % 60)
        }
    }
}

/// Location / Venue attachment metadata.
#[derive(Debug, Clone, PartialEq)]
pub struct LocationAttachment {
    /// Latitude in degrees (`-90.0..=90.0`).
    pub lat: f64,
    /// Longitude in degrees (`-180.0..=180.0`).
    pub lon: f64,
    /// Optional venue title (e.g. `"Harbor Coffee Roasters"`).
    pub venue_title: Option<String>,
    /// Optional venue street address.
    pub venue_address: Option<String>,
    /// Optional live-location sharing period in seconds (`None` for static pin).
    pub live_period_secs: Option<u32>,
}

impl Eq for LocationAttachment {}

/// Nearby venue entry displayed in the Location Picker placeholder list.
#[derive(Debug, Clone, PartialEq)]
pub struct VenueItem {
    /// Venue identifier.
    pub id: String,
    /// Venue display name.
    pub title: String,
    /// Street address.
    pub address: String,
    /// Short category label (e.g. `"Cafe"`, `"Library"`, `"Park"`).
    pub category: String,
    /// Latitude.
    pub lat: f64,
    /// Longitude.
    pub lon: f64,
}

/// Shared contact card attachment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContactAttachment {
    /// Optional Telegram user peer id.
    pub user_id: Option<PeerId>,
    /// First name.
    pub first_name: String,
    /// Last name (may be empty).
    pub last_name: String,
    /// Formatted phone number (synthetic test number).
    pub phone_number: String,
}

impl ContactAttachment {
    /// Returns full formatted display name.
    #[must_use]
    pub fn full_name(&self) -> String {
        if self.last_name.trim().is_empty() {
            self.first_name.trim().to_owned()
        } else {
            format!("{} {}", self.first_name.trim(), self.last_name.trim())
        }
    }
}

/// Round video message placeholder attachment (`HistoryView::RoundVideo`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoundVideoAttachment {
    /// Recording duration in seconds.
    pub duration_secs: u32,
    /// Rendered circular diameter in logical pixels (tdesktop default `240px`).
    pub diameter_px: i32,
    /// Procedural gradient colors for the circular video preview frame.
    pub accent_rgb: ([u8; 3], [u8; 3]),
}

/// Link preview card displayed above the compose field and inside bubbles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkPreviewData {
    /// Resolved URL.
    pub url: String,
    /// Site name (e.g. `"Rust Lang"`, `"GitHub"`).
    pub site_name: String,
    /// Page title.
    pub title: String,
    /// Short description snippet.
    pub description: String,
    /// Optional thumbnail accent RGB.
    pub thumb_rgb: Option<[u8; 3]>,
    /// True to show large media preview rather than small square thumb.
    pub large_media: bool,
    /// True to display link preview above the message text (`invert_media`).
    pub above_text: bool,
}

/// Display kind for an inline bot query result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InlineBotResultKind {
    /// Text/article row (e.g. `@wiki`).
    Article,
    /// Photo mosaic tile (e.g. `@pic`).
    Photo,
    /// Animated GIF mosaic tile (e.g. `@gif`).
    Gif,
}

/// Single inline bot result returned by `@bot query` hook/backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineBotResult {
    /// Result id.
    pub id: String,
    /// Bot username without `@` (e.g. `"gif"`, `"wiki"`, `"pic"`).
    pub bot_username: String,
    /// Result title.
    pub title: String,
    /// Result subtitle/description.
    pub description: String,
    /// Message text inserted/sent when clicked.
    pub send_text: String,
    /// Preview swatch color.
    pub thumb_rgb: [u8; 3],
    /// Visual layout kind.
    pub kind: InlineBotResultKind,
}

/// Parses an inline bot query from the compose text (e.g. `"@gif rust"` -> `Some(("gif", "rust"))`).
#[must_use]
pub fn parse_inline_bot_query(text: &str) -> Option<(&str, &str)> {
    let trimmed = text.strip_prefix('@')?;
    let space_idx = trimmed.find(' ')?;
    let bot = &trimmed[..space_idx];
    if bot.len() < 3 || !bot.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    let query = &trimmed[space_idx + 1..];
    Some((bot, query))
}

/// Fallback message visual effect descriptor (used when offline or before
/// `messages.getAvailableEffects` hydrates from a live session).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MessageEffect {
    /// Stable Telegram effect id.
    pub id: u64,
    /// Representative emoji.
    pub emoji: &'static str,
    /// Display name.
    pub name: &'static str,
    /// Accent badge RGB.
    pub accent_rgb: [u8; 3],
}

/// Built-in fallback message effects catalog used offline before `messages.getAvailableEffects`
/// hydrates from Telegram.
pub const MESSAGE_EFFECTS: &[MessageEffect] = &[
    MessageEffect {
        id: 5104841245755180586,
        emoji: "🔥",
        name: "Fire",
        accent_rgb: [0xF2, 0x6B, 0x3A],
    },
    MessageEffect {
        id: 5107584321108051014,
        emoji: "👍",
        name: "Thumbs Up",
        accent_rgb: [0x40, 0xA7, 0xE3],
    },
    MessageEffect {
        id: 5159385139981059251,
        emoji: "❤️",
        name: "Hearts",
        accent_rgb: [0xE8, 0x4A, 0x5F],
    },
    MessageEffect {
        id: 5046509860389126442,
        emoji: "🎉",
        name: "Confetti",
        accent_rgb: [0x9B, 0x59, 0xB6],
    },
    MessageEffect {
        id: 5046589136895476101,
        emoji: "💩",
        name: "Poop",
        accent_rgb: [0x8D, 0x6E, 0x63],
    },
    MessageEffect {
        id: 5104858069142078462,
        emoji: "👎",
        name: "Thumbs Down",
        accent_rgb: [0x60, 0x7D, 0x8B],
    },
];

/// Looks up a [`MessageEffect`] by its id in the built-in fallback table.
#[must_use]
pub fn find_message_effect(id: u64) -> Option<&'static MessageEffect> {
    MESSAGE_EFFECTS.iter().find(|e| e.id == id)
}

/// Resolves a human-readable display name and accent RGB color for a live `AvailableEffect`
/// returned by `messages.getAvailableEffects`.
#[must_use]
pub fn effect_name_and_accent(id: u64, emoticon: &str) -> (String, [u8; 3]) {
    if let Some(known) = find_message_effect(id) {
        return (known.name.to_owned(), known.accent_rgb);
    }
    match emoticon {
        "🔥" => ("Fire".to_owned(), [0xF2, 0x6B, 0x3A]),
        "👍" => ("Thumbs Up".to_owned(), [0x40, 0xA7, 0xE3]),
        "❤️" | "❤" => ("Hearts".to_owned(), [0xE8, 0x4A, 0x5F]),
        "🎉" => ("Confetti".to_owned(), [0x9B, 0x59, 0xB6]),
        "💩" => ("Poop".to_owned(), [0x8D, 0x6E, 0x63]),
        "👎" => ("Thumbs Down".to_owned(), [0x60, 0x7D, 0x8B]),
        "🎆" | "🎇" => ("Fireworks".to_owned(), [0xF5, 0xA6, 0x23]),
        "🎈" => ("Balloons".to_owned(), [0xE9, 0x1E, 0x63]),
        "⭐" | "✨" => ("Sparkles".to_owned(), [0xF7, 0xB7, 0x31]),
        "❄️" | "❄" => ("Snow".to_owned(), [0x54, 0xC3, 0xF3]),
        _ => {
            let palette: &[[u8; 3]] = &[
                [0x40, 0xA7, 0xE3],
                [0xF2, 0x6B, 0x3A],
                [0xE8, 0x4A, 0x5F],
                [0x9B, 0x59, 0xB6],
                [0x2E, 0xCC, 0x71],
                [0xF5, 0xA6, 0x23],
            ];
            let rgb = palette[(id as usize) % palette.len()];
            let label = if emoticon.is_empty() {
                "Effect".to_owned()
            } else {
                emoticon.to_owned()
            };
            (label, rgb)
        }
    }
}

/// Builds a valid procedural Lottie JSON burst composition for message-effect playback via
/// `tg-lottie` when no server `.tgs` animation payload is cached yet.
#[must_use]
pub fn animated_effect_lottie_json(emoji: &str, accent_rgb: [u8; 3]) -> String {
    let r = f64::from(accent_rgb[0]) / 255.0;
    let g = f64::from(accent_rgb[1]) / 255.0;
    let b = f64::from(accent_rgb[2]) / 255.0;
    let r2 = (r + 0.25).min(1.0);
    let g2 = (g + 0.25).min(1.0);
    let b2 = (b + 0.25).min(1.0);
    let points = match emoji {
        "🎉" | "🎆" | "✨" => 8,
        "❤️" | "❤" => 6,
        _ => 5,
    };
    format!(
        r#"{{
  "v": "5.5.2",
  "fr": 30,
  "ip": 0,
  "op": 54,
  "w": 160,
  "h": 160,
  "nm": "MessageEffect",
  "layers": [
    {{
      "ty": 4,
      "nm": "EffectBurst",
      "ip": 0,
      "op": 54,
      "st": 0,
      "ks": {{
        "o": {{ "a": 1, "k": [
          {{ "t": 0, "s": [0], "e": [100] }},
          {{ "t": 8, "s": [100], "e": [90] }},
          {{ "t": 54, "s": [0] }}
        ]}},
        "r": {{ "a": 1, "k": [
          {{ "t": 0, "s": [0], "e": [72] }},
          {{ "t": 54, "s": [72] }}
        ]}},
        "p": {{ "a": 0, "k": [80, 80, 0] }},
        "a": {{ "a": 0, "k": [0, 0, 0] }},
        "s": {{ "a": 1, "k": [
          {{ "t": 0, "s": [30, 30, 100], "e": [118, 118, 100] }},
          {{ "t": 22, "s": [118, 118, 100], "e": [132, 132, 100] }},
          {{ "t": 54, "s": [132, 132, 100] }}
        ]}}
      }},
      "shapes": [
        {{
          "ty": "gr",
          "nm": "StarRing",
          "it": [
            {{
              "ty": "sr",
              "sy": 1,
              "pt": {{ "a": 0, "k": {points} }},
              "p": {{ "a": 0, "k": [0, 0] }},
              "r": {{ "a": 0, "k": 0 }},
              "ir": {{ "a": 0, "k": 24 }},
              "or": {{ "a": 0, "k": 56 }},
              "is": {{ "a": 0, "k": 0 }},
              "os": {{ "a": 0, "k": 0 }}
            }},
            {{
              "ty": "fl",
              "c": {{ "a": 0, "k": [{r:.3}, {g:.3}, {b:.3}, 1.0] }},
              "o": {{ "a": 0, "k": 85 }}
            }},
            {{
              "ty": "st",
              "c": {{ "a": 0, "k": [{r2:.3}, {g2:.3}, {b2:.3}, 1.0] }},
              "o": {{ "a": 0, "k": 95 }},
              "w": {{ "a": 0, "k": 4 }}
            }},
            {{
              "ty": "tr",
              "p": {{ "a": 0, "k": [0, 0] }},
              "a": {{ "a": 0, "k": [0, 0] }},
              "s": {{ "a": 0, "k": [100, 100] }},
              "r": {{ "a": 0, "k": 0 }},
              "o": {{ "a": 0, "k": 100 }}
            }}
          ]
        }}
      ]
    }}
  ]
}}"#
    )
}

/// Forwarding options matching `Data::ForwardOptions` (`HideNames`, `NoCaptions`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ForwardOptions {
    /// Hide original sender name (`drop_author`).
    pub drop_author: bool,
    /// Hide media captions on forwarded media messages (`drop_media_captions`).
    pub drop_media_captions: bool,
}

/// Reason category for reporting messages (`boxes/report_messages_box.cpp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageReportReason {
    /// Unsolicited spam.
    Spam,
    /// Violence.
    Violence,
    /// Child abuse.
    ChildAbuse,
    /// Pornography.
    Pornography,
    /// Illegal drugs.
    IllegalDrugs,
    /// Personal details / doxxing.
    PersonalDetails,
    /// Copyright infringement.
    Copyright,
    /// Fake / impersonation.
    Fake,
    /// Other reason (requires comment).
    Other,
}

impl MessageReportReason {
    /// All selectable report categories in tdesktop order.
    pub const ALL: [Self; 9] = [
        Self::Spam,
        Self::Violence,
        Self::Pornography,
        Self::ChildAbuse,
        Self::Copyright,
        Self::IllegalDrugs,
        Self::PersonalDetails,
        Self::Fake,
        Self::Other,
    ];

    /// Display label for the report dialog.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Spam => "Spam",
            Self::Violence => "Violence",
            Self::ChildAbuse => "Child abuse",
            Self::Pornography => "Pornography",
            Self::IllegalDrugs => "Illegal drugs",
            Self::PersonalDetails => "Personal details",
            Self::Copyright => "Copyright",
            Self::Fake => "Fake account or scam",
            Self::Other => "Other",
        }
    }
}

// ---------------------------------------------------------------------------
// Copy selected text with entities (`TextWithEntities` slicing & roundtrip)
// ---------------------------------------------------------------------------

/// Clipboard payload preserving both plain text and `tg-text` entities.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CopiedRichText {
    /// Sliced text with adjusted UTF-16 entities.
    pub content: TextWithEntities,
}

impl CopiedRichText {
    /// Formats the rich text back into compose-ready Telegram Markdown so pasting preserves styling.
    #[must_use]
    pub fn to_markdown(&self) -> String {
        text_with_entities_to_markdown(&self.content)
    }
}

/// Slices a [`TextWithEntities`] by Unicode scalar (char) range `[char_start..char_end)` while
/// clipping and rebasing all overlapping UTF-16 [`EntityInText`] spans!
#[must_use]
pub fn slice_text_with_entities(
    source: &TextWithEntities,
    char_start: usize,
    char_end: usize,
) -> TextWithEntities {
    if char_end <= char_start || source.text.is_empty() {
        return TextWithEntities::default();
    }

    let mut byte_start = None;
    let mut byte_end = source.text.len();
    let mut utf16_start = 0usize;
    let mut utf16_end = 0usize;
    let mut u16_cursor = 0usize;

    for (c_idx, (b_idx, ch)) in source.text.char_indices().enumerate() {
        if c_idx == char_start {
            byte_start = Some(b_idx);
            utf16_start = u16_cursor;
        }
        if c_idx == char_end {
            byte_end = b_idx;
            utf16_end = u16_cursor;
            break;
        }
        u16_cursor += ch.len_utf16();
        utf16_end = u16_cursor;
    }

    let Some(bs) = byte_start else {
        return TextWithEntities::default();
    };
    if byte_end <= bs {
        return TextWithEntities::default();
    }

    let sliced_text = source.text[bs..byte_end].to_owned();
    let mut entities = Vec::new();

    for ent in &source.entities {
        let e_start = ent.offset().max(0) as usize;
        let e_end = e_start.saturating_add(ent.length().max(0) as usize);
        let ov_start = e_start.max(utf16_start);
        let ov_end = e_end.min(utf16_end);
        if ov_end > ov_start {
            let new_offset = (ov_start - utf16_start) as i32;
            let new_len = (ov_end - ov_start) as i32;
            entities.push(EntityInText::with_data(
                ent.entity_type(),
                new_offset,
                new_len,
                ent.data(),
            ));
        }
    }

    let mut out = TextWithEntities {
        text: sliced_text,
        entities,
    };
    out.sort_entities();
    out
}

/// Serializes a [`TextWithEntities`] into Telegram Markdown markers (`**bold**`, `__italic__`,
/// `` `code` ``, `~~strike~~`, `||spoiler||`, `[label](url)`) for entity-preserving copy/paste.
#[must_use]
pub fn text_with_entities_to_markdown(source: &TextWithEntities) -> String {
    if source.entities.is_empty() {
        return source.text.clone();
    }
    let text_u16_len = source.utf16_len();
    let mut opens: Vec<(usize, usize, String)> = Vec::new();
    let mut closes: Vec<(usize, usize, String)> = Vec::new();

    for (idx, ent) in source.entities.iter().enumerate() {
        if !ent.valid_for_text(text_u16_len) {
            continue;
        }
        let s_u16 = ent.offset() as usize;
        let e_u16 = (ent.offset() + ent.length()) as usize;
        let (Some(start), Some(end)) = (
            utf16_to_utf8_offset(&source.text, s_u16),
            utf16_to_utf8_offset(&source.text, e_u16),
        ) else {
            continue;
        };
        if end <= start {
            continue;
        }
        let (open, close) = match ent.entity_type() {
            EntityType::Bold | EntityType::Semibold => ("**".to_owned(), "**".to_owned()),
            EntityType::Italic => ("__".to_owned(), "__".to_owned()),
            EntityType::StrikeOut => ("~~".to_owned(), "~~".to_owned()),
            EntityType::Spoiler => ("||".to_owned(), "||".to_owned()),
            EntityType::Code => ("`".to_owned(), "`".to_owned()),
            EntityType::Pre => {
                let lang = ent.data();
                (format!("```{lang}\n"), "\n```".to_owned())
            }
            EntityType::CustomUrl => ("[".to_owned(), format!("]({})", ent.data())),
            _ => continue,
        };
        opens.push((start, idx, open));
        closes.push((end, idx, close));
    }

    if opens.is_empty() {
        return source.text.clone();
    }

    let mut boundaries: Vec<usize> = vec![0, source.text.len()];
    for (p, _, _) in &opens {
        boundaries.push(*p);
    }
    for (p, _, _) in &closes {
        boundaries.push(*p);
    }
    boundaries.sort_unstable();
    boundaries.dedup();

    let mut out = String::with_capacity(source.text.len() + 32);
    for win in boundaries.windows(2) {
        let pos = win[0];
        let next = win[1];
        // Close entities ending at `pos` in reverse order.
        for (_, _, tag) in closes.iter().rev().filter(|(p, _, _)| *p == pos) {
            out.push_str(tag);
        }
        // Open entities starting at `pos` in forward order.
        for (_, _, tag) in opens.iter().filter(|(p, _, _)| *p == pos) {
            out.push_str(tag);
        }
        out.push_str(&source.text[pos..next]);
    }
    let end_pos = source.text.len();
    for (_, _, tag) in closes.iter().rev().filter(|(p, _, _)| *p == end_pos) {
        out.push_str(tag);
    }
    out
}

// ---------------------------------------------------------------------------
// Procedural Lottie animations for animated reactions (`tg-lottie`)
// ---------------------------------------------------------------------------

/// Generates a procedural GPL-3.0 Lottie JSON burst animation tailored to `emoji`,
/// rasterized via `tg-lottie` for animated reaction hover previews and toggle bursts.
#[must_use]
pub fn animated_reaction_lottie_json(emoji: &str) -> String {
    let (primary, secondary, shape_points) = match emoji {
        "❤️" | "❤" | "😍" | "🥰" => ([0.95, 0.24, 0.36, 1.0], [1.0, 0.56, 0.65, 1.0], 6),
        "🔥" | "⚡" => ([0.98, 0.45, 0.14, 1.0], [1.0, 0.82, 0.24, 1.0], 5),
        "👍" | "👏" | "🤝" => ([0.25, 0.65, 0.92, 1.0], [0.55, 0.84, 1.0, 1.0], 5),
        "🎉" | "🤩" | "✨" | "⭐" => ([0.98, 0.78, 0.18, 1.0], [0.68, 0.42, 0.96, 1.0], 7),
        "😂" | "🤣" | "😊" => ([1.0, 0.80, 0.20, 1.0], [0.32, 0.76, 0.98, 1.0], 5),
        _ => ([0.34, 0.72, 0.94, 1.0], [0.98, 0.76, 0.26, 1.0], 5),
    };
    format!(
        r#"{{
  "v": "5.5.2",
  "fr": 30,
  "ip": 0,
  "op": 45,
  "w": 128,
  "h": 128,
  "nm": "AnimatedReaction",
  "layers": [
    {{
      "ty": 4,
      "nm": "BurstStar",
      "ip": 0,
      "op": 45,
      "st": 0,
      "ks": {{
        "o": {{ "a": 1, "k": [
          {{ "t": 0, "s": [100], "e": [100] }},
          {{ "t": 30, "s": [100], "e": [20] }},
          {{ "t": 45, "s": [20] }}
        ]}},
        "r": {{ "a": 1, "k": [
          {{ "t": 0, "s": [0], "e": [36] }},
          {{ "t": 45, "s": [36] }}
        ]}},
        "p": {{ "a": 0, "k": [64, 64, 0] }},
        "a": {{ "a": 0, "k": [0, 0, 0] }},
        "s": {{ "a": 1, "k": [
          {{ "t": 0, "s": [45, 45, 100], "e": [112, 112, 100] }},
          {{ "t": 22, "s": [112, 112, 100], "e": [92, 92, 100] }},
          {{ "t": 45, "s": [92, 92, 100] }}
        ]}}
      }},
      "shapes": [
        {{
          "ty": "gr",
          "nm": "Star",
          "it": [
            {{
              "ty": "sr",
              "sy": 1,
              "pt": {{ "a": 0, "k": {shape_points} }},
              "p": {{ "a": 0, "k": [0, 0] }},
              "r": {{ "a": 0, "k": 0 }},
              "ir": {{ "a": 0, "k": 22 }},
              "or": {{ "a": 0, "k": 48 }},
              "is": {{ "a": 0, "k": 8 }},
              "os": {{ "a": 0, "k": 8 }}
            }},
            {{ "ty": "fl", "c": {{ "a": 0, "k": [{:.2}, {:.2}, {:.2}, {:.2}] }}, "o": {{ "a": 0, "k": 100 }} }},
            {{ "ty": "st", "c": {{ "a": 0, "k": [{:.2}, {:.2}, {:.2}, {:.2}] }}, "o": {{ "a": 0, "k": 100 }}, "w": {{ "a": 0, "k": 4 }} }}
          ]
        }}
      ]
    }}
  ]
}}"#,
        primary[0],
        primary[1],
        primary[2],
        primary[3],
        secondary[0],
        secondary[1],
        secondary[2],
        secondary[3],
    )
}
