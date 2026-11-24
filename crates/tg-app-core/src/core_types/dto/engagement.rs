//! Pure DTOs, constants, and eligibility helpers for WP-53 Chat Engagement & Read-Receipt Protocol.
//!
//! Port origin:
//! - `Telegram/SourceFiles/api/api_views.{h,cpp}` (`ViewsManager`)
//! - `Telegram/SourceFiles/api/api_unread_things.{h,cpp}` (`UnreadThings`)
//! - `Telegram/SourceFiles/api/api_who_reacted.{h,cpp}` (`WhoReadExists`, `WhoReadIds`)
//! - `Telegram/SourceFiles/data/data_replies_list.cpp` (`messages.readDiscussion`)
//! - `Telegram/SourceFiles/data/data_histories.cpp` (`messages.reportMessagesDelivery`)
//! - `Telegram/SourceFiles/data/components/ephemeral_messages.{h,cpp}` (`ephemeral.deleteMessage`)
//! - `Telegram/SourceFiles/api/api_report.cpp` (`ephemeral.reportMessage`)
//! - `Telegram/SourceFiles/apiwrap.cpp` (`channels.exportMessageLink`, `messages.migrateChat`)
//! - `Telegram/SourceFiles/history/view/history_view_context_menu.cpp` (`channels.getMessageAuthor`)
//! - `Telegram/SourceFiles/window/window_peer_menu.cpp` (`messages.getFutureChatCreatorAfterLeave`)

use crate::stand_in::types::{MsgId, PeerId};

/// Debounce timeout for batching `messages.getMessagesViews(increment=true)`
/// (`kSendViewsTimeout = 1000` ms in `api/api_views.cpp:22`).
pub const VIEWS_SEND_TIMEOUT_MS: u64 = 1_000;

/// Periodic polling interval for `messages.getExtendedMedia`
/// (`kPollExtendedMediaPeriod = 30 * 1000` ms in `api/api_views.cpp:23`).
pub const POLL_EXTENDED_MEDIA_PERIOD_MS: u64 = 30_000;

/// Maximum number of message IDs per `messages.getExtendedMedia` request
/// (`kMaxPollPerRequest = 100` in `api/api_views.cpp:24`).
pub const MAX_EXTENDED_MEDIA_POLL_PER_REQUEST: usize = 100;

/// Preload trigger threshold for unread mentions/reactions/poll votes
/// (`kPreloadIfLess = 5` in `api/api_unread_things.cpp:24`).
pub const UNREAD_THINGS_PRELOAD_IF_LESS: usize = 5;

/// Initial page size for `messages.getUnread{Mentions,Reactions,PollVotes}`
/// (`kFirstRequestLimit = 10` in `api/api_unread_things.cpp:25`).
pub const UNREAD_THINGS_FIRST_LIMIT: i32 = 10;

/// Subsequent page size for `messages.getUnread{Mentions,Reactions,PollVotes}`
/// (`kNextRequestLimit = 100` in `api/api_unread_things.cpp:26`).
pub const UNREAD_THINGS_NEXT_LIMIT: i32 = 100;

/// Maximum message IDs per `messages.reportMessagesDelivery` RPC
/// (`kReportDeliveriesPerRequest = 50` in `data/data_histories.cpp:43`).
pub const REPORT_DELIVERIES_PER_REQUEST: usize = 50;

/// Default expiry window in seconds for 1:1 DM and group read receipts
/// (`pm_read_date_expire_period` / `chat_read_mark_expire_period` = 7 days,
/// `api/api_who_reacted.cpp:752-754, 769-771`).
pub const DEFAULT_READ_DATE_EXPIRE_PERIOD_SECS: i64 = 7 * 86_400;

/// Default maximum group size eligible for `messages.getMessageReadParticipants`
/// (`chat_read_mark_size_threshold = 50` in `api/api_who_reacted.cpp:775-777`).
pub const DEFAULT_CHAT_READ_MARK_SIZE_THRESHOLD: i32 = 50;

/// Per-message counters returned by `messages.getMessagesViews` (`messageViews#455b853d`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageViewsInfo {
    /// Target message ID.
    pub msg_id: MsgId,
    /// Updated view count, if present on the server response.
    pub views: Option<i32>,
    /// Updated forward count, if present on the server response.
    pub forwards: Option<i32>,
    /// Updated replies/comments count from `MessageReplies`, if present.
    pub replies_count: Option<i32>,
    /// True when `MessageReplies.comments` is set (channel comments thread enabled).
    pub comments: bool,
    /// Optional linked discussion group channel ID (`MessageReplies.channel_id`).
    pub discussion_channel_id: Option<i64>,
    /// Newest reply ID in the thread (`MessageReplies.max_id`).
    pub max_id: Option<MsgId>,
    /// Highest read reply ID in the thread (`MessageReplies.read_max_id`).
    pub read_max_id: Option<MsgId>,
}

/// Summary of an applied `updateMessageExtendedMedia` entry returned by `messages.getExtendedMedia`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExtendedMediaRefreshEntry {
    /// Owning peer ID.
    pub peer_id: PeerId,
    /// Message ID whose extended/paid media was refreshed.
    pub msg_id: MsgId,
    /// Total extended media items returned in the update.
    pub total_items: usize,
    /// Number of items that are unlocked (`messageExtendedMedia`) vs locked previews.
    pub unlocked_items: usize,
}

/// Category of unread engagement jump-button items (`HistoryUnreadThings::Type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnreadThingsKind {
    /// Unread `@mentions` and replies (`messages.getUnreadMentions` / `messages.readMentions`).
    Mentions,
    /// Unread message reactions (`messages.getUnreadReactions` / `messages.readReactions`).
    Reactions,
    /// Unread poll votes (`messages.getUnreadPollVotes` / `messages.readPollVotes`).
    PollVotes,
}

/// Target thread/sublist scope for unread mentions, reactions, or poll votes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct UnreadThingsScope {
    /// Owning chat/channel peer ID.
    pub peer_id: PeerId,
    /// Optional forum topic root message ID (`top_msg_id`).
    pub top_msg_id: Option<MsgId>,
    /// Optional Saved Messages / Monoforum sublist peer ID (`saved_peer_id`, reactions only).
    pub saved_peer_id: Option<PeerId>,
}

impl UnreadThingsScope {
    /// Targets the main history of `peer_id`.
    #[must_use]
    pub const fn history(peer_id: PeerId) -> Self {
        Self {
            peer_id,
            top_msg_id: None,
            saved_peer_id: None,
        }
    }

    /// Targets a forum topic thread `(peer_id, top_msg_id)`.
    #[must_use]
    pub const fn topic(peer_id: PeerId, top_msg_id: MsgId) -> Self {
        Self {
            peer_id,
            top_msg_id: Some(top_msg_id),
            saved_peer_id: None,
        }
    }

    /// Targets a Saved Messages or monoforum sublist `(peer_id, saved_peer_id)`.
    #[must_use]
    pub const fn saved_sublist(peer_id: PeerId, saved_peer_id: PeerId) -> Self {
        Self {
            peer_id,
            top_msg_id: None,
            saved_peer_id: Some(saved_peer_id),
        }
    }
}

/// Snapshot of local unread engagement counters for a chat, forum topic, or sublist.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UnreadThingsCounts {
    /// Unread mention count.
    pub mentions: i32,
    /// Unread reaction count.
    pub reactions: i32,
    /// Unread poll-vote count.
    pub poll_votes: i32,
}

/// Outcome of draining `messages.AffectedHistory` for `readMentions`/`readReactions`/`readPollVotes`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UnreadThingsReadResult {
    /// Number of RPC pages executed before `affected.offset <= 0`.
    pub pages: usize,
    /// Final `pts` reported by the server.
    pub last_pts: i32,
    /// Cumulative `pts_count` across all pages.
    pub total_pts_count: i32,
}

/// Exact tdesktop pagination parameters `(offset_id, add_offset, limit)` for
/// `messages.getUnread{Mentions,Reactions,PollVotes}` (`api/api_unread_things.cpp:124-214`).
#[must_use]
pub fn unread_things_pagination_params(
    already_loaded: usize,
    max_loaded_id: Option<MsgId>,
) -> (i32, i32, i32) {
    let loaded = already_loaded > 0;
    let offset_id = if loaded {
        max_loaded_id.map_or(1, |m| m.0.max(1))
    } else {
        1
    };
    let limit = if loaded {
        UNREAD_THINGS_NEXT_LIMIT
    } else {
        UNREAD_THINGS_FIRST_LIMIT
    };
    let add_offset = if loaded { -(limit + 1) } else { -limit };
    (offset_id, add_offset, limit)
}

/// Single reader entry returned by `messages.getMessageReadParticipants`
/// (`readParticipantDate#4a4ff172`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageReadParticipant {
    /// Reader's user peer ID.
    pub user_id: PeerId,
    /// Resolved display name (or fallback `"User <id>"`).
    pub name: String,
    /// Optional `@username` (without `@`).
    pub username: Option<String>,
    /// Unix timestamp (seconds) when the user read the message.
    pub read_date_unix: i32,
    /// Avatar palette color index (`0..7`).
    pub color_index: u8,
}

/// Decoded result of `messages.getOutboxReadDate` for a 1:1 DM outgoing message
/// (`api/api_who_reacted.cpp:288-317`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutboxReadDateStatus {
    /// Recipient read the message at `date_unix` (`outboxReadDate#3bb842ac`).
    Read {
        /// Unix timestamp (seconds) when the recipient read the message.
        date_unix: i32,
    },
    /// Hidden because the current user hides their own read times (`YOUR_PRIVACY_RESTRICTED`).
    MyPrivacyHidden,
    /// Hidden by the recipient's privacy settings (`USER_PRIVACY_RESTRICTED`).
    PeerPrivacyHidden,
    /// Message is older than the server's read-date retention window (`MESSAGE_TOO_OLD`).
    TooOld,
    /// Read date unavailable for another non-fatal reason.
    Unavailable,
}

/// Evaluates tdesktop's `WhoReadExists` eligibility for a 1:1 DM outgoing message
/// (`api/api_who_reacted.cpp:744-759`).
#[must_use]
#[allow(clippy::too_many_arguments)]
pub const fn who_read_exists_for_dm(
    outgoing: bool,
    unseen: bool,
    is_self: bool,
    is_bot: bool,
    is_service_user: bool,
    read_dates_private: bool,
    msg_date_unix: i64,
    now_unix: i64,
    expire_period_secs: i64,
) -> bool {
    if !outgoing || unseen || is_self || is_bot || is_service_user || read_dates_private {
        return false;
    }
    msg_date_unix.saturating_add(expire_period_secs) > now_unix
}

/// Evaluates tdesktop's `WhoReadExists` eligibility for a basic group or megagroup outgoing message
/// (`api/api_who_reacted.cpp:760-783`).
#[must_use]
#[allow(clippy::too_many_arguments)]
pub const fn who_read_exists_for_group(
    outgoing: bool,
    unseen: bool,
    participants_hidden: bool,
    is_monoforum: bool,
    member_count: i32,
    max_member_threshold: i32,
    msg_date_unix: i64,
    now_unix: i64,
    expire_period_secs: i64,
) -> bool {
    if !outgoing || unseen || participants_hidden || is_monoforum {
        return false;
    }
    if member_count <= 0 || member_count > max_member_threshold {
        return false;
    }
    msg_date_unix.saturating_add(expire_period_secs) > now_unix
}

/// Exported message link returned by `channels.exportMessageLink` (`exportedMessageLink#5dab1af4`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportedMessageLinkInfo {
    /// Direct `https://t.me/...` link to the message.
    pub link: String,
    /// Embeddable HTML snippet returned by Telegram.
    pub html: String,
}

/// Author resolved via `channels.getMessageAuthor` (`history_view_context_menu.cpp:1556-1616`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageAuthorInfo {
    /// Author's user peer ID.
    pub user_id: PeerId,
    /// Display name.
    pub name: String,
    /// Optional `@username` (without `@`).
    pub username: Option<String>,
    /// True if the returned user account is deleted.
    pub is_deleted: bool,
}

/// Single report choice returned by `reportResultChooseOption` (`ephemeral.reportMessage`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EphemeralReportOption {
    /// Opaque server option bytes echoed in the follow-up `ephemeral.reportMessage` call.
    pub option: Vec<u8>,
    /// Human-readable option label.
    pub text: String,
}

/// Decoded `ReportResult` from `ephemeral.reportMessage` (`api/api_report.cpp:162-184`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EphemeralReportOutcome {
    /// Server asks the user to pick a sub-option (`reportResultChooseOption#f0e4e0b6`).
    ChooseOption {
        /// Prompt title.
        title: String,
        /// Available sub-options.
        options: Vec<EphemeralReportOption>,
    },
    /// Server asks for an optional or required free-text comment (`reportResultAddComment#6f09ac31`).
    AddComment {
        /// True if the comment may be left empty.
        optional: bool,
        /// Opaque option bytes to send back with the comment.
        option: Vec<u8>,
    },
    /// Report was accepted (`reportResultReported#8db33c4b`).
    Reported,
}

/// Outcome of upgrading a basic group chat to a supergroup via `messages.migrateChat`
/// (`apiwrap.cpp:1400-1476`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChatMigrationResult {
    /// Original basic group chat peer ID (now deactivated).
    pub old_chat_id: PeerId,
    /// Resulting supergroup/megagroup channel peer ID.
    pub new_channel_id: PeerId,
    /// True if the basic group was already migrated locally before invoking an RPC.
    pub already_migrated: bool,
}

/// Candidate owner who would inherit ownership if the current creator leaves a chat/channel
/// (`messages.getFutureChatCreatorAfterLeave`, `window_peer_menu.cpp:4367-4389`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FutureChatCreatorInfo {
    /// Candidate user peer ID.
    pub user_id: PeerId,
    /// Candidate display name.
    pub name: String,
    /// Optional `@username` (without `@`).
    pub username: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unread_things_pagination_matches_tdesktop_constants() {
        assert_eq!(unread_things_pagination_params(0, None), (1, -10, 10));
        assert_eq!(
            unread_things_pagination_params(0, Some(MsgId(42))),
            (1, -10, 10)
        );
        assert_eq!(
            unread_things_pagination_params(10, Some(MsgId(55))),
            (55, -101, 100)
        );
        assert_eq!(unread_things_pagination_params(3, None), (1, -101, 100));
    }

    #[test]
    fn who_read_exists_enforces_expiry_and_size_thresholds() {
        let now = 1_700_700_000_i64;
        assert!(who_read_exists_for_dm(
            true,
            false,
            false,
            false,
            false,
            false,
            now - 100,
            now,
            DEFAULT_READ_DATE_EXPIRE_PERIOD_SECS,
        ));
        assert!(!who_read_exists_for_dm(
            true,
            false,
            false,
            false,
            false,
            false,
            now - DEFAULT_READ_DATE_EXPIRE_PERIOD_SECS,
            now,
            DEFAULT_READ_DATE_EXPIRE_PERIOD_SECS,
        ));
        assert!(who_read_exists_for_group(
            true,
            false,
            false,
            false,
            DEFAULT_CHAT_READ_MARK_SIZE_THRESHOLD,
            DEFAULT_CHAT_READ_MARK_SIZE_THRESHOLD,
            now - 100,
            now,
            DEFAULT_READ_DATE_EXPIRE_PERIOD_SECS,
        ));
        assert!(!who_read_exists_for_group(
            true,
            false,
            false,
            false,
            DEFAULT_CHAT_READ_MARK_SIZE_THRESHOLD + 1,
            DEFAULT_CHAT_READ_MARK_SIZE_THRESHOLD,
            now - 100,
            now,
            DEFAULT_READ_DATE_EXPIRE_PERIOD_SECS,
        ));
    }
}
