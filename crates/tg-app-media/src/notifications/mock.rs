//! Mock [`NotifyBackend`] implementation with `"simulate incoming message"` debug actions.
//!
//! All sample chats and senders use synthetic, non-personal placeholder names.

use std::collections::{BTreeMap, VecDeque};

use crate::notifications::backend::{
    IncomingMessageNotification, IncomingNotifyEvent, ItemNotificationType, NotificationMediaKind,
    NotifyBackend, NotifyPeerInfo,
};
use crate::notifications::settings::UnreadBadgeState;
use tg_app_core::stand_in::types::{MsgId, PeerId, PeerKind};

/// Recorded quick-reply action sent through [`MockNotifyBackend`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedQuickReply {
    /// Session ID.
    pub session_id: u64,
    /// Target chat peer ID.
    pub peer_id: PeerId,
    /// Topic root ID (`MsgId(0)` for normal chats).
    pub topic_root_id: MsgId,
    /// Message being replied to.
    pub reply_to_msg_id: MsgId,
    /// Reply body text.
    pub text: String,
}

/// Recorded mark-read action dispatched through [`MockNotifyBackend`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedMarkRead {
    /// Session ID.
    pub session_id: u64,
    /// Target chat peer ID.
    pub peer_id: PeerId,
    /// Topic root ID.
    pub topic_root_id: MsgId,
    /// Maximum message ID marked read.
    pub max_msg_id: MsgId,
}

/// Recorded chat activation requested from a toast or popup click.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedOpenChat {
    /// Session ID.
    pub session_id: u64,
    /// Target chat peer ID.
    pub peer_id: PeerId,
    /// Topic root ID.
    pub topic_root_id: MsgId,
    /// Target message ID.
    pub msg_id: MsgId,
    /// Optional draft text carried from Windows inline reply box.
    pub draft: Option<String>,
}

/// Synthetic chat entry tracked by [`MockNotifyBackend`].
#[derive(Debug, Clone)]
struct MockChatState {
    peer: NotifyPeerInfo,
    muted: bool,
    unread_count: u32,
    unread_mentions: u32,
    marked_unread: bool,
}

/// Local mock implementation of [`NotifyBackend`] for offline UI testing and debug simulation.
#[derive(Debug)]
pub struct MockNotifyBackend {
    session_id: u64,
    chats: BTreeMap<i64, MockChatState>,
    events: VecDeque<IncomingNotifyEvent>,
    next_msg_id: i32,
    simulation_step: usize,
    /// Replies submitted via toast/popup quick-reply.
    pub sent_replies: Vec<RecordedQuickReply>,
    /// Read receipts submitted via toast/popup "Mark as read".
    pub marked_read: Vec<RecordedMarkRead>,
    /// Chat-open requests submitted via toast/popup clicks.
    pub opened_chats: Vec<RecordedOpenChat>,
}

impl Default for MockNotifyBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl MockNotifyBackend {
    /// Synthetic peer ID for `"Support Desk"` (unmuted private chat).
    pub const PEER_SUPPORT_DESK: PeerId = PeerId::from_user(2001);
    /// Synthetic peer ID for `"Build Bot"` (unmuted private chat).
    pub const PEER_BUILD_BOT: PeerId = PeerId::from_user(2002);
    /// Synthetic peer ID for `"Team Alpha"` (muted group chat).
    pub const PEER_TEAM_ALPHA: PeerId = PeerId::from_chat(3001);
    /// Synthetic peer ID for `"News Digest"` (muted broadcast channel).
    pub const PEER_NEWS_DIGEST: PeerId = PeerId::from_channel(4001);
    /// Synthetic peer ID for `"Saved Messages"` (self chat).
    pub const PEER_SAVED_MESSAGES: PeerId = PeerId::from_user(1000);

    /// Creates a new [`MockNotifyBackend`] seeded with synthetic sample chats.
    #[must_use]
    pub fn new() -> Self {
        let mut chats = BTreeMap::new();
        let entries = [
            MockChatState {
                peer: NotifyPeerInfo {
                    id: Self::PEER_SUPPORT_DESK,
                    kind: PeerKind::User,
                    title: "Support Desk".to_owned(),
                    initials: "SD".to_owned(),
                    avatar_rgb: [0x45, 0x98, 0xE5],
                    is_self: false,
                    is_replies_chat: false,
                    is_blocked: false,
                    community_id: None,
                },
                muted: false,
                unread_count: 1,
                unread_mentions: 0,
                marked_unread: false,
            },
            MockChatState {
                peer: NotifyPeerInfo {
                    id: Self::PEER_BUILD_BOT,
                    kind: PeerKind::User,
                    title: "Build Bot".to_owned(),
                    initials: "BB".to_owned(),
                    avatar_rgb: [0x5B, 0xA8, 0x5C],
                    is_self: false,
                    is_replies_chat: false,
                    is_blocked: false,
                    community_id: None,
                },
                muted: false,
                unread_count: 0,
                unread_mentions: 0,
                marked_unread: false,
            },
            MockChatState {
                peer: NotifyPeerInfo {
                    id: Self::PEER_TEAM_ALPHA,
                    kind: PeerKind::Group,
                    title: "Team Alpha".to_owned(),
                    initials: "TA".to_owned(),
                    avatar_rgb: [0xE1, 0x70, 0x55],
                    is_self: false,
                    is_replies_chat: false,
                    is_blocked: false,
                    community_id: None,
                },
                muted: true,
                unread_count: 3,
                unread_mentions: 0,
                marked_unread: false,
            },
            MockChatState {
                peer: NotifyPeerInfo {
                    id: Self::PEER_NEWS_DIGEST,
                    kind: PeerKind::Channel,
                    title: "News Digest".to_owned(),
                    initials: "ND".to_owned(),
                    avatar_rgb: [0x8E, 0x6C, 0xE6],
                    is_self: false,
                    is_replies_chat: false,
                    is_blocked: false,
                    community_id: None,
                },
                muted: true,
                unread_count: 2,
                unread_mentions: 0,
                marked_unread: false,
            },
            MockChatState {
                peer: NotifyPeerInfo {
                    id: Self::PEER_SAVED_MESSAGES,
                    kind: PeerKind::User,
                    title: "Saved Messages".to_owned(),
                    initials: "SM".to_owned(),
                    avatar_rgb: [0x40, 0xA7, 0xE3],
                    is_self: true,
                    is_replies_chat: false,
                    is_blocked: false,
                    community_id: None,
                },
                muted: false,
                unread_count: 0,
                unread_mentions: 0,
                marked_unread: false,
            },
        ];
        for entry in entries {
            chats.insert(entry.peer.id.0, entry);
        }

        Self {
            session_id: 1,
            chats,
            events: VecDeque::new(),
            next_msg_id: 500,
            simulation_step: 0,
            sent_replies: Vec::new(),
            marked_read: Vec::new(),
            opened_chats: Vec::new(),
        }
    }

    /// Updates a chat's muted flag in the mock unread-badge tracker.
    pub fn set_chat_muted(&mut self, peer_id: PeerId, muted: bool) {
        if let Some(chat) = self.chats.get_mut(&peer_id.0) {
            chat.muted = muted;
        }
    }

    /// Pushes an arbitrary custom event onto the backend queue.
    pub fn push_event(&mut self, event: IncomingNotifyEvent) {
        self.events.push_back(event);
    }

    /// Debug action: `"simulate incoming message"`.
    ///
    /// Cycles through realistic scenarios:
    /// 0. Direct message from `"Support Desk"`
    /// 1. Mention in muted group `"Team Alpha"` (from unmuted sender `"Build Bot"`)
    /// 2. Pinned message in `"Team Alpha"`
    /// 3. Reaction on a message in `"Support Desk"`
    /// 4. Burst of 4 messages from `"Build Bot"` (tests collapsing to `"4 new messages"`)
    pub fn simulate_incoming_message(&mut self, now_unix: i64) -> Vec<IncomingMessageNotification> {
        let step = self.simulation_step;
        self.simulation_step = self.simulation_step.wrapping_add(1);
        match step % 5 {
            0 => vec![self.simulate_direct_message(
                Self::PEER_SUPPORT_DESK,
                "Ticket #42 has been updated with build logs.",
                now_unix,
            )],
            1 => vec![self.simulate_group_mention(
                Self::PEER_TEAM_ALPHA,
                Self::PEER_BUILD_BOT,
                "Build Bot",
                "@you Please review the nightly release checklist.",
                now_unix,
            )],
            2 => vec![self.simulate_pinned_message(
                Self::PEER_TEAM_ALPHA,
                Self::PEER_SUPPORT_DESK,
                "Support Desk",
                "Release freeze starts at 18:00 UTC.",
                false,
                now_unix,
            )],
            3 => vec![self.simulate_reaction(
                Self::PEER_SUPPORT_DESK,
                "👍",
                "Ticket #42 has been updated with build logs.",
                now_unix,
            )],
            _ => self.simulate_message_burst(
                Self::PEER_BUILD_BOT,
                &[
                    "Build job #101 started.",
                    "Unit test suite passed (42/42).",
                    "Clippy lint check passed.",
                    "Artifact package ready.",
                ],
                now_unix,
            ),
        }
    }

    /// Enqueues a single direct message notification and increments unread count.
    pub fn simulate_direct_message(
        &mut self,
        peer_id: PeerId,
        text: &str,
        now_unix: i64,
    ) -> IncomingMessageNotification {
        let msg_id = self.alloc_msg_id();
        let (title, kind, is_self) = self
            .chats
            .get_mut(&peer_id.0)
            .map(|c| {
                c.unread_count = c.unread_count.saturating_add(1);
                (c.peer.title.clone(), c.peer.kind, c.peer.is_self)
            })
            .unwrap_or_else(|| ("Support Desk".to_owned(), PeerKind::User, false));

        let notif = IncomingMessageNotification {
            session_id: self.session_id,
            peer_id,
            peer_kind: kind,
            msg_id,
            notification_type: ItemNotificationType::Message,
            date_unix: now_unix,
            sender_id: peer_id,
            sender_name: title.clone(),
            chat_title: title,
            text: text.to_owned(),
            is_self_chat: is_self,
            ..IncomingMessageNotification::default()
        };
        self.events
            .push_back(IncomingNotifyEvent::Notification(Box::new(notif.clone())));
        notif
    }

    /// Enqueues a group mention notification (e.g. in a muted group).
    pub fn simulate_group_mention(
        &mut self,
        group_id: PeerId,
        sender_id: PeerId,
        sender_name: &str,
        text: &str,
        now_unix: i64,
    ) -> IncomingMessageNotification {
        let msg_id = self.alloc_msg_id();
        let title = self
            .chats
            .get_mut(&group_id.0)
            .map(|c| {
                c.unread_count = c.unread_count.saturating_add(1);
                c.unread_mentions = c.unread_mentions.saturating_add(1);
                c.peer.title.clone()
            })
            .unwrap_or_else(|| "Team Alpha".to_owned());

        let notif = IncomingMessageNotification {
            session_id: self.session_id,
            peer_id: group_id,
            peer_kind: PeerKind::Group,
            msg_id,
            notification_type: ItemNotificationType::Message,
            date_unix: now_unix,
            sender_id,
            sender_name: sender_name.to_owned(),
            chat_title: title,
            text: text.to_owned(),
            mentions_me: true,
            ..IncomingMessageNotification::default()
        };
        self.events
            .push_back(IncomingNotifyEvent::Notification(Box::new(notif.clone())));
        notif
    }

    /// Enqueues a pinned-message service notification.
    pub fn simulate_pinned_message(
        &mut self,
        chat_id: PeerId,
        sender_id: PeerId,
        sender_name: &str,
        pinned_text: &str,
        silent: bool,
        now_unix: i64,
    ) -> IncomingMessageNotification {
        let msg_id = self.alloc_msg_id();
        let (title, kind) = self
            .chats
            .get_mut(&chat_id.0)
            .map(|c| {
                c.unread_count = c.unread_count.saturating_add(1);
                (c.peer.title.clone(), c.peer.kind)
            })
            .unwrap_or_else(|| ("Team Alpha".to_owned(), PeerKind::Group));

        let notif = IncomingMessageNotification {
            session_id: self.session_id,
            peer_id: chat_id,
            peer_kind: kind,
            msg_id,
            notification_type: ItemNotificationType::PinnedMessage,
            date_unix: now_unix,
            sender_id,
            sender_name: sender_name.to_owned(),
            chat_title: title,
            text: pinned_text.to_owned(),
            silent,
            is_pinned: true,
            ..IncomingMessageNotification::default()
        };
        self.events
            .push_back(IncomingNotifyEvent::Notification(Box::new(notif.clone())));
        notif
    }

    /// Enqueues a reaction notification on an existing message.
    pub fn simulate_reaction(
        &mut self,
        peer_id: PeerId,
        emoji: &str,
        original_text: &str,
        now_unix: i64,
    ) -> IncomingMessageNotification {
        let msg_id = self.alloc_msg_id();
        let (title, kind) = self
            .chats
            .get(&peer_id.0)
            .map(|c| (c.peer.title.clone(), c.peer.kind))
            .unwrap_or_else(|| ("Support Desk".to_owned(), PeerKind::User));

        let notif = IncomingMessageNotification {
            session_id: self.session_id,
            peer_id,
            peer_kind: kind,
            msg_id,
            notification_type: ItemNotificationType::Reaction,
            date_unix: now_unix,
            sender_id: peer_id,
            sender_name: title.clone(),
            chat_title: title,
            text: original_text.to_owned(),
            media_kind: NotificationMediaKind::Text,
            reaction_emoji: Some(emoji.to_owned()),
            ..IncomingMessageNotification::default()
        };
        self.events
            .push_back(IncomingNotifyEvent::Notification(Box::new(notif.clone())));
        notif
    }

    /// Enqueues a burst of multiple messages in the same chat to exercise collapsing.
    pub fn simulate_message_burst(
        &mut self,
        peer_id: PeerId,
        messages: &[&str],
        now_unix: i64,
    ) -> Vec<IncomingMessageNotification> {
        messages
            .iter()
            .map(|text| self.simulate_direct_message(peer_id, text, now_unix))
            .collect()
    }

    fn alloc_msg_id(&mut self) -> MsgId {
        let id = self.next_msg_id;
        self.next_msg_id = self.next_msg_id.saturating_add(1);
        MsgId(id)
    }
}

impl NotifyBackend for MockNotifyBackend {
    fn poll_events(&mut self) -> Vec<IncomingNotifyEvent> {
        self.events.drain(..).collect()
    }

    fn mark_read(
        &mut self,
        session_id: u64,
        peer_id: PeerId,
        topic_root_id: MsgId,
        max_msg_id: MsgId,
    ) {
        if let Some(chat) = self.chats.get_mut(&peer_id.0) {
            chat.unread_count = 0;
            chat.unread_mentions = 0;
            chat.marked_unread = false;
        }
        self.marked_read.push(RecordedMarkRead {
            session_id,
            peer_id,
            topic_root_id,
            max_msg_id,
        });
        self.events.push_back(if topic_root_id.0 != 0 {
            IncomingNotifyEvent::TopicRead {
                session_id,
                peer_id,
                topic_root_id,
                read_till_id: max_msg_id,
            }
        } else {
            IncomingNotifyEvent::HistoryRead {
                session_id,
                peer_id,
                read_till_id: max_msg_id,
            }
        });
    }

    fn send_quick_reply(
        &mut self,
        session_id: u64,
        peer_id: PeerId,
        topic_root_id: MsgId,
        reply_to_msg_id: MsgId,
        text: String,
    ) {
        let trimmed = text.trim().to_owned();
        if !trimmed.is_empty() {
            self.sent_replies.push(RecordedQuickReply {
                session_id,
                peer_id,
                topic_root_id,
                reply_to_msg_id,
                text: trimmed,
            });
        }
        self.mark_read(session_id, peer_id, topic_root_id, reply_to_msg_id);
    }

    fn open_chat(
        &mut self,
        session_id: u64,
        peer_id: PeerId,
        topic_root_id: MsgId,
        msg_id: MsgId,
        draft: Option<String>,
    ) {
        self.opened_chats.push(RecordedOpenChat {
            session_id,
            peer_id,
            topic_root_id,
            msg_id,
            draft,
        });
        self.mark_read(session_id, peer_id, topic_root_id, MsgId::SHOW_AT_END);
    }

    fn unread_state(&self) -> UnreadBadgeState {
        let mut state = UnreadBadgeState::default();
        for chat in self.chats.values() {
            if chat.unread_count > 0 {
                state.messages = state.messages.saturating_add(chat.unread_count);
                state.chats = state.chats.saturating_add(1);
                if chat.muted {
                    state.messages_muted = state.messages_muted.saturating_add(chat.unread_count);
                    state.chats_muted = state.chats_muted.saturating_add(1);
                }
            }
            if chat.marked_unread {
                state.marks = state.marks.saturating_add(1);
                if chat.muted {
                    state.marks_muted = state.marks_muted.saturating_add(1);
                }
            }
            state.mentions = state.mentions.saturating_add(chat.unread_mentions);
        }
        state
    }

    fn peer_info(&self, peer_id: PeerId) -> Option<NotifyPeerInfo> {
        self.chats.get(&peer_id.0).map(|c| c.peer.clone())
    }
}
