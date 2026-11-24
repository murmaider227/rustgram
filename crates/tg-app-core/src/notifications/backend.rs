//! `NotifyBackend` trait and notification event data model.
//!
//! Decouples `tg-app::notifications` and `tg-app::tray` from `tg-api` / `tg-data`
//! while matching tdesktop's `Data::ItemNotification` and activation callbacks.

use crate::notifications::settings::UnreadBadgeState;
use crate::stand_in::types::{MsgId, PeerId, PeerKind};

/// Kind of incoming notification event (`Data::ItemNotificationType` in `data/data_types.h`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ItemNotificationType {
    /// Standard or service incoming message (including scheduled reminders).
    Message,
    /// Pinned message announcement in a chat or channel.
    PinnedMessage,
    /// New reaction placed on one of the user's messages.
    Reaction,
    /// New vote cast on one of the user's polls.
    PollVote,
}

/// Media attachment summary used for reaction and message preview formatting.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum NotificationMediaKind {
    /// Plain text message or webpage preview.
    #[default]
    Text,
    /// Photo attachment.
    Photo,
    /// Voice message.
    VoiceMessage,
    /// Round video message.
    VideoMessage,
    /// Animated GIF.
    Gif,
    /// Video file.
    Video,
    /// Sticker with its associated fallback emoji.
    Sticker(String),
    /// Generic file / document.
    Document,
    /// Shared contact with display name.
    Contact(String),
    /// Shared geographic location.
    Location,
    /// Poll or quiz with question text.
    Poll {
        /// Poll question.
        question: String,
        /// `true` for quiz mode.
        is_quiz: bool,
    },
    /// Inline game.
    Game,
    /// Payment invoice.
    Invoice,
}

/// Byte-index spoiler range within a message body (`EntityType::Spoiler`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextSpoilerRange {
    /// Character offset (UTF-16 / char index).
    pub char_offset: usize,
    /// Character length.
    pub char_len: usize,
}

/// Complete payload for an incoming notification candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncomingMessageNotification {
    /// Owning account / session unique identifier (`session->uniqueId()`).
    pub session_id: u64,
    /// Target chat peer identifier.
    pub peer_id: PeerId,
    /// Kind of chat peer (`User`, `Group`, or `Channel`).
    pub peer_kind: PeerKind,
    /// Forum topic root message ID (`MsgId(0)` if not in a forum topic).
    pub topic_root_id: MsgId,
    /// Monoforum sublist peer ID (`PeerId::ZERO` if unused).
    pub monoforum_peer_id: PeerId,
    /// Message ID within the chat.
    pub msg_id: MsgId,
    /// Notification kind (`Message`, `PinnedMessage`, `Reaction`, `PollVote`).
    pub notification_type: ItemNotificationType,
    /// Server timestamp of the message in Unix seconds.
    pub date_unix: i64,
    /// Sender peer ID (author of the message, or user who reacted/voted).
    pub sender_id: PeerId,
    /// Display name of the sender (e.g. group author or reacting user).
    pub sender_name: String,
    /// Display title of the chat (user name, group title, or channel title).
    pub chat_title: String,
    /// Optional forum topic title when `topic_root_id != 0`.
    pub topic_title: Option<String>,
    /// Message body text (or pinned message snippet).
    pub text: String,
    /// Optional spoiler spans to mask in notifications (`TextWithPermanentSpoiler`).
    pub spoiler_ranges: Vec<TextSpoilerRange>,
    /// Media classification for preview / reaction text composition.
    pub media_kind: NotificationMediaKind,
    /// Whether the message was sent by the current account (`item->out()`).
    pub outgoing: bool,
    /// Whether the message was delivered silently (`item->isSilent()`).
    pub silent: bool,
    /// Whether the message mentions the current user (`item->mentionsMe()`).
    pub mentions_me: bool,
    /// Whether the message is a direct reply to one of the current user's messages.
    pub is_reply_to_me: bool,
    /// Whether the message is a pinned-message service notification.
    pub is_pinned: bool,
    /// Whether the message originated from a scheduled message / reminder (`isFromScheduled()`).
    pub is_from_scheduled: bool,
    /// Whether this is a "contact joined" signup service message (`MessageFlag::IsContactSignUp`).
    pub is_contact_signup: bool,
    /// Whether this is an imported forwarded message (`forwarded->imported`).
    pub is_imported_forward: bool,
    /// Whether this message is forwarded (used for batching / `➡️` prefix).
    pub is_forwarded: bool,
    /// Non-zero media album group identifier (`item->groupId()`).
    pub group_id: u64,
    /// `true` if `peer_id` is Saved Messages (`peer->isSelf()`).
    pub is_self_chat: bool,
    /// `true` if `peer_id` is a broadcast channel (`peer->isBroadcast()`).
    pub is_broadcast: bool,
    /// `true` if `peer_id` is Telegram's service notifications or verify-codes account.
    pub is_notifications_service: bool,
    /// `true` if the current user is allowed to send text replies in this chat/topic.
    pub can_send_texts: bool,
    /// Remaining slowmode cooldown in seconds (`0` if none).
    pub slowmode_seconds_left: u32,
    /// Required stars per message (`0` if free).
    pub stars_per_message: u32,
    /// Reaction emoji / alt text when `notification_type == Reaction`.
    pub reaction_emoji: Option<String>,
    /// Poll option answer text when `notification_type == PollVote`.
    pub poll_vote_option: Option<String>,
    /// Whether the sender is blocked (`notifyBy->isBlocked()`).
    pub sender_blocked: bool,
    /// Whether the current user is a member of the channel (`channel->amIn()`).
    pub am_in_chat: bool,
}

impl Default for IncomingMessageNotification {
    fn default() -> Self {
        Self {
            session_id: 1,
            peer_id: PeerId::from_user(1001),
            peer_kind: PeerKind::User,
            topic_root_id: MsgId(0),
            monoforum_peer_id: PeerId::ZERO,
            msg_id: MsgId(1),
            notification_type: ItemNotificationType::Message,
            date_unix: 0,
            sender_id: PeerId::from_user(1001),
            sender_name: "Support Desk".to_owned(),
            chat_title: "Support Desk".to_owned(),
            topic_title: None,
            text: String::new(),
            spoiler_ranges: Vec::new(),
            media_kind: NotificationMediaKind::Text,
            outgoing: false,
            silent: false,
            mentions_me: false,
            is_reply_to_me: false,
            is_pinned: false,
            is_from_scheduled: false,
            is_contact_signup: false,
            is_imported_forward: false,
            is_forwarded: false,
            group_id: 0,
            is_self_chat: false,
            is_broadcast: false,
            is_notifications_service: false,
            can_send_texts: true,
            slowmode_seconds_left: 0,
            stars_per_message: 0,
            reaction_emoji: None,
            poll_vote_option: None,
            sender_blocked: false,
            am_in_chat: true,
        }
    }
}

/// Peer visual & metadata snapshot needed for avatar rendering and mute lookups.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotifyPeerInfo {
    /// Peer ID.
    pub id: PeerId,
    /// Peer classification.
    pub kind: PeerKind,
    /// Display title.
    pub title: String,
    /// Two-letter initials fallback.
    pub initials: String,
    /// RGB avatar circle tint.
    pub avatar_rgb: [u8; 3],
    /// `true` if Saved Messages.
    pub is_self: bool,
    /// `true` if Replies bot chat.
    pub is_replies_chat: bool,
    /// `true` if this peer is blocked by the user.
    pub is_blocked: bool,
    /// Optional linked community channel ID.
    pub community_id: Option<PeerId>,
}

/// Incoming events emitted by a [`NotifyBackend`] for [`crate::notifications::NotificationSystem`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IncomingNotifyEvent {
    /// A new message, pinned message, reaction, or poll vote candidate arrived.
    Notification(Box<IncomingMessageNotification>),
    /// A chat's incoming messages were read up to `read_till_id`.
    HistoryRead {
        /// Account session ID.
        session_id: u64,
        /// Chat peer ID.
        peer_id: PeerId,
        /// Maximum read message ID (`MsgId::SHOW_AT_END` clears the entire chat).
        read_till_id: MsgId,
    },
    /// A forum topic's incoming messages were read up to `read_till_id`.
    TopicRead {
        /// Account session ID.
        session_id: u64,
        /// Forum peer ID.
        peer_id: PeerId,
        /// Forum topic root message ID.
        topic_root_id: MsgId,
        /// Maximum read message ID.
        read_till_id: MsgId,
    },
    /// A specific message was deleted.
    MessageDeleted {
        /// Account session ID.
        session_id: u64,
        /// Chat peer ID.
        peer_id: PeerId,
        /// Deleted message ID.
        msg_id: MsgId,
    },
}

/// Backend contract powering notification events, unread badges, mark-read, and quick-reply actions.
pub trait NotifyBackend: Send {
    /// Drains all newly arrived incoming notification and read-state events.
    fn poll_events(&mut self) -> Vec<IncomingNotifyEvent>;

    /// Marks messages up to `max_msg_id` in `(peer_id, topic_root_id)` as read.
    fn mark_read(
        &mut self,
        session_id: u64,
        peer_id: PeerId,
        topic_root_id: MsgId,
        max_msg_id: MsgId,
    );

    /// Sends a quick reply from a Windows toast or custom notification popup.
    fn send_quick_reply(
        &mut self,
        session_id: u64,
        peer_id: PeerId,
        topic_root_id: MsgId,
        reply_to_msg_id: MsgId,
        text: String,
    );

    /// Opens the target chat/message in the main window, optionally seeding an unsent draft.
    fn open_chat(
        &mut self,
        session_id: u64,
        peer_id: PeerId,
        topic_root_id: MsgId,
        msg_id: MsgId,
        draft: Option<String>,
    );

    /// Returns current aggregated unread badge counts across all chats.
    fn unread_state(&self) -> UnreadBadgeState;

    /// Looks up peer avatar/title metadata for `peer_id`.
    fn peer_info(&self, peer_id: PeerId) -> Option<NotifyPeerInfo>;

    /// Returns the account label appended to titles when multiple accounts are signed in.
    fn account_name(&self, session_id: u64) -> Option<String> {
        let _ = session_id;
        None
    }

    /// Returns `true` if more than one account is signed in (`addTargetAccountName`).
    fn has_multiple_accounts(&self) -> bool {
        false
    }
}
