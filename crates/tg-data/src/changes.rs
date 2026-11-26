//! Port of `data/data_changes.h`.
//!
//! Defines strongly-typed change-notification flags for peers, histories,
//! messages, dialog entries, and forum topics, delivered via
//! `tokio::sync::broadcast`.

use bitflags::bitflags;
use tokio::sync::broadcast;

use crate::dialogs::DialogKey;
use crate::id::{FullMsgId, MsgId, PeerId, UserId};
use crate::rights::ChatAdminRights;

/// Default capacity of the `Changes` broadcast channel.
pub const DEFAULT_CHANGES_CAPACITY: usize = 1024;

bitflags! {
    /// Flags indicating which peer fields changed (`Data::PeerUpdate::Flag`).
    #[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
    pub struct PeerUpdateFlags: u64 {
        /// Display name changed.
        const NAME                 = 1 << 0;
        /// Primary username changed.
        const USERNAME             = 1 << 1;
        /// Profile photo changed.
        const PHOTO                = 1 << 2;
        /// Bio / about text changed.
        const ABOUT                = 1 << 3;
        /// Notification settings changed.
        const NOTIFICATIONS        = 1 << 4;
        /// Basic group migrated to megagroup.
        const MIGRATION            = 1 << 5;
        /// Unavailability / restriction reasons changed.
        const UNAVAILABLE_REASON   = 1 << 6;
        /// Chat theme token changed.
        const CHAT_THEME_TOKEN     = 1 << 7;
        /// Chat wallpaper changed.
        const CHAT_WALLPAPER       = 1 << 8;
        /// Blocked status changed.
        const IS_BLOCKED           = 1 << 9;
        /// Auto-delete TTL changed.
        const MESSAGES_TTL         = 1 << 10;
        /// Full peer info refreshed.
        const FULL_INFO            = 1 << 11;
        /// Active usernames list changed.
        const USERNAMES            = 1 << 12;
        /// Translation-disabled preference changed.
        const TRANSLATION_DISABLED = 1 << 13;
        /// Accent color changed.
        const COLOR                = 1 << 14;
        /// Profile accent color changed.
        const COLOR_PROFILE        = 1 << 15;
        /// Background emoji changed.
        const BACKGROUND_EMOJI     = 1 << 16;
        /// Stories ring state changed.
        const STORIES_STATE        = 1 << 17;
        /// Verification info changed.
        const VERIFY_INFO          = 1 << 18;
        /// Stars-per-message price changed.
        const STARS_PER_MESSAGE    = 1 << 19;

        /// Contact shareability changed.
        const CAN_SHARE_CONTACT    = 1 << 20;
        /// Contact status changed.
        const IS_CONTACT           = 1 << 21;
        /// Phone number changed.
        const PHONE_NUMBER         = 1 << 22;
        /// Online / last-seen status changed.
        const ONLINE_STATUS        = 1 << 23;
        /// Bot commands changed.
        const BOT_COMMANDS         = 1 << 24;
        /// Bot group-invite capability changed.
        const BOT_CAN_BE_INVITED   = 1 << 25;
        /// Bot start token changed.
        const BOT_START_TOKEN      = 1 << 26;
        /// Common chats count changed.
        const COMMON_CHATS         = 1 << 27;
        /// Profile gifts count changed.
        const PEER_GIFTS           = 1 << 28;
        /// Calls capability changed.
        const HAS_CALLS            = 1 << 29;
        /// Support info changed.
        const SUPPORT_INFO         = 1 << 30;
        /// Is-bot flag changed.
        const IS_BOT               = 1 << 31;
        /// Emoji status changed.
        const EMOJI_STATUS         = 1 << 32;
        /// Business details changed.
        const BUSINESS_DETAILS     = 1 << 33;
        /// Birthday changed.
        const BIRTHDAY             = 1 << 34;
        /// Linked personal channel changed.
        const PERSONAL_CHANNEL     = 1 << 35;
        /// Star referral program changed.
        const STAR_REF_PROGRAM     = 1 << 36;
        /// Pays-per-message info changed.
        const PAYS_PER_MESSAGE     = 1 << 37;
        /// Gift settings changed.
        const GIFT_SETTINGS        = 1 << 38;
        /// Stars rating changed.
        const STARS_RATING         = 1 << 39;
        /// Private contact note changed.
        const CONTACT_NOTE         = 1 << 40;

        /// Invite links changed.
        const INVITE_LINKS         = 1 << 41;
        /// Member count / list changed.
        const MEMBERS              = 1 << 42;
        /// Admins list / count changed.
        const ADMINS               = 1 << 43;
        /// Banned / restricted users changed.
        const BANNED_USERS         = 1 << 44;
        /// Admin rights or restrictions changed.
        const RIGHTS               = 1 << 45;
        /// Pending join requests changed.
        const PENDING_REQUESTS     = 1 << 46;
        /// Allowed reactions changed.
        const REACTIONS            = 1 << 47;

        /// Channel membership (`am_in`) changed.
        const CHANNEL_AM_IN        = 1 << 48;
        /// Group sticker set changed.
        const STICKERS_SET         = 1 << 49;
        /// Group emoji set changed.
        const EMOJI_SET            = 1 << 50;
        /// Linked discussion chat changed.
        const DISCUSSION_LINK      = 1 << 51;
        /// Linked monoforum changed.
        const MONOFORUM_LINK       = 1 << 52;
        /// Channel geolocation changed.
        const CHANNEL_LOCATION     = 1 << 53;
        /// Slow-mode interval or cooldown changed.
        const SLOWMODE             = 1 << 54;
        /// Group call state changed.
        const GROUP_CALL           = 1 << 55;
        /// Managed bot relationship changed.
        const MANAGED_BOT          = 1 << 56;
        /// Main profile tab changed.
        const MAIN_PROFILE_TAB     = 1 << 57;
        /// Wallet card changed.
        const WALLET_CARD          = 1 << 58;
        /// Action bar settings changed.
        const BAR_SETTINGS         = 1 << 59;
    }
}

bitflags! {
    /// Flags indicating which history fields changed (`Data::HistoryUpdate::Flag`).
    #[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
    pub struct HistoryUpdateFlags: u32 {
        /// Pinned status changed.
        const IS_PINNED            = 1 << 0;
        /// Unread count / mark / badges changed.
        const UNREAD_VIEW          = 1 << 1;
        /// Top-promoted sponsor status changed.
        const TOP_PROMOTED         = 1 << 2;
        /// Folder (main vs archive) changed.
        const FOLDER               = 1 << 3;
        /// Unread mentions list/count changed.
        const UNREAD_MENTIONS      = 1 << 4;
        /// Unread reactions list/count changed.
        const UNREAD_REACTIONS     = 1 << 5;
        /// Client-side pending messages changed.
        const CLIENT_SIDE_MESSAGES = 1 << 6;
        /// Active chat occupation changed.
        const CHAT_OCCUPIED        = 1 << 7;
        /// Outgoing message sent.
        const MESSAGE_SENT         = 1 << 8;
        /// Scheduled message sent.
        const SCHEDULED_SENT       = 1 << 9;
        /// Outbox read watermark advanced.
        const OUTBOX_READ          = 1 << 10;
        /// Active bot reply keyboard changed.
        const BOT_KEYBOARD         = 1 << 11;
        /// Cloud draft changed.
        const CLOUD_DRAFT          = 1 << 12;
        /// Source translation language changed.
        const TRANSLATE_FROM       = 1 << 13;
        /// Target translation language changed.
        const TRANSLATED_TO        = 1 << 14;
        /// Unread poll votes changed.
        const UNREAD_POLL_VOTES    = 1 << 15;
        /// Streamed drafts changed.
        const STREAMED_DRAFTS      = 1 << 16;
        /// Compose stash changed.
        const COMPOSE_STASH        = 1 << 17;
        /// Old loaded messages were unloaded to bound memory (`History::unloadBlocks`).
        const MESSAGES_UNLOADED    = 1 << 18;
    }
}

bitflags! {
    /// Flags indicating which message fields changed (`Data::MessageUpdate::Flag`).
    #[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
    pub struct MessageUpdateFlags: u32 {
        /// Message content or metadata edited.
        const EDITED              = 1 << 0;
        /// Message deleted / destroyed.
        const DESTROYED           = 1 << 1;
        /// Dialog list row needs repaint.
        const DIALOG_ROW_REPAINT  = 1 << 2;
        /// Dialog list row needs full refresh.
        const DIALOG_ROW_REFRESH  = 1 << 3;
        /// New message added to history.
        const NEW_ADDED           = 1 << 4;
        /// Reply markup updated.
        const REPLY_MARKUP        = 1 << 5;
        /// Inline bot callback sent.
        const BOT_CALLBACK_SENT   = 1 << 6;
        /// Message maybe added (from slice/sync).
        const NEW_MAYBE_ADDED     = 1 << 7;
        /// Reply-to-top root message added.
        const REPLY_TO_TOP_ADDED  = 1 << 8;
        /// New unread reaction arrived on message.
        const NEW_UNREAD_REACTION = 1 << 9;
        /// The new message arrived as a live update (`NewMessageType::Unread`), not with a
        /// history slice or dialogs page, so it may raise a desktop notification
        /// (`History::addNewMessage` -> `History::newItemAdded`, `history/history.cpp:674,1674`).
        const NEW_UNREAD          = 1 << 10;
    }
}

bitflags! {
    /// Flags indicating which dialog entry properties changed (`Data::EntryUpdate::Flag`).
    #[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
    pub struct EntryUpdateFlags: u32 {
        /// Entry row needs repaint.
        const REPAINT             = 1 << 0;
        /// Pinned messages presence changed.
        const HAS_PINNED_MESSAGES = 1 << 1;
        /// Forward draft changed.
        const FORWARD_DRAFT       = 1 << 2;
        /// Local draft set or cleared.
        const LOCAL_DRAFT_SET     = 1 << 3;
        /// Entry row height changed.
        const HEIGHT              = 1 << 4;
        /// Entry removed from dialog list.
        const DESTROYED           = 1 << 5;
    }
}

bitflags! {
    /// Flags indicating which forum topic fields changed (`Data::TopicUpdate::Flag`).
    #[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
    pub struct TopicUpdateFlags: u32 {
        /// Unread count / badges changed.
        const UNREAD_VIEW       = 1 << 1;
        /// Unread mentions changed.
        const UNREAD_MENTIONS   = 1 << 2;
        /// Unread reactions changed.
        const UNREAD_REACTIONS  = 1 << 3;
        /// Topic notification settings changed.
        const NOTIFICATIONS     = 1 << 4;
        /// Topic title changed.
        const TITLE             = 1 << 5;
        /// Topic icon emoji ID changed.
        const ICON_ID           = 1 << 6;
        /// Topic icon color ID changed.
        const COLOR_ID          = 1 << 7;
        /// Topic cloud draft changed.
        const CLOUD_DRAFT       = 1 << 8;
        /// Topic closed state changed.
        const CLOSED            = 1 << 9;
        /// Topic creator changed.
        const CREATOR           = 1 << 10;
        /// Topic destroyed.
        const DESTROYED         = 1 << 11;
        /// Unread poll votes changed.
        const UNREAD_POLL_VOTES = 1 << 12;
    }
}

/// Typing or activity action reported by a peer (`SendMessageAction`).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum TypingAction {
    /// Typing text.
    Typing,
    /// Cancelled typing / activity.
    Cancel,
    /// Recording video.
    RecordVideo,
    /// Uploading video (progress 0..=100).
    UploadVideo(i32),
    /// Recording voice message.
    RecordVoice,
    /// Uploading voice message (progress 0..=100).
    UploadVoice(i32),
    /// Uploading photo (progress 0..=100).
    UploadPhoto(i32),
    /// Uploading file/document (progress 0..=100).
    UploadDocument(i32),
    /// Choosing geo location.
    ChooseLocation,
    /// Choosing contact.
    ChooseContact,
    /// Playing game.
    PlayGame,
    /// Recording round video.
    RecordRound,
    /// Uploading round video (progress 0..=100).
    UploadRound(i32),
    /// Speaking in group call.
    Speaking,
    /// Importing history (progress 0..=100).
    HistoryImport(i32),
    /// Choosing sticker.
    ChooseSticker,
    /// Emoji interaction or other send action.
    Other,
}

/// Event emitted on `Session`'s `tokio::sync::broadcast` channel (`Data::Changes`).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ChangeEvent {
    /// Peer data changed.
    PeerUpdated {
        /// Affected peer ID.
        peer_id: PeerId,
        /// Changed field flags.
        flags: PeerUpdateFlags,
    },
    /// Peer name / first-letters index changed.
    NameUpdated {
        /// Affected peer ID.
        peer_id: PeerId,
        /// Previous first letters before the rename.
        old_first_letters: Vec<char>,
    },
    /// History state changed.
    HistoryUpdated {
        /// Affected peer ID.
        peer_id: PeerId,
        /// Changed field flags.
        flags: HistoryUpdateFlags,
    },
    /// Message added, edited, or destroyed.
    MessageUpdated {
        /// Affected message ID.
        id: FullMsgId,
        /// Changed field flags.
        flags: MessageUpdateFlags,
    },
    /// Dialog list entry changed.
    EntryUpdated {
        /// Affected dialog entry key.
        key: DialogKey,
        /// Changed field flags.
        flags: EntryUpdateFlags,
    },
    /// Forum topic changed.
    TopicUpdated {
        /// Owning peer ID.
        peer_id: PeerId,
        /// Topic root message ID.
        root_id: MsgId,
        /// Changed field flags.
        flags: TopicUpdateFlags,
    },
    /// Chat filters (folders) list or order changed.
    FiltersChanged,
    /// A per-type default notification setting (users, groups, channels) changed
    /// (`Data::NotifySettings::defaultUpdates`).
    NotifyDefaultsChanged,
    /// Admin rights for a member changed in a group or channel.
    ChatAdminChanged {
        /// Group or channel peer ID.
        peer_id: PeerId,
        /// Target user ID.
        user_id: UserId,
        /// New admin rights.
        rights: ChatAdminRights,
        /// Custom admin rank/title.
        rank: String,
    },
    /// User typing or send-progress activity changed in a conversation.
    UserTyping {
        /// Conversation peer ID.
        peer_id: PeerId,
        /// Acting user/peer ID.
        from_id: PeerId,
        /// Optional forum topic root message ID.
        top_msg_id: Option<MsgId>,
        /// Activity action.
        action: TypingAction,
    },
}

/// Broadcast notification hub analog of `Data::Changes`.
#[derive(Debug)]
pub struct Changes {
    tx: broadcast::Sender<ChangeEvent>,
}

impl Default for Changes {
    fn default() -> Self {
        Self::new(DEFAULT_CHANGES_CAPACITY)
    }
}

impl Changes {
    /// Creates a new `Changes` hub with the given broadcast ring buffer capacity.
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity.max(1));
        Self { tx }
    }

    /// Subscribes to all change notifications.
    pub fn subscribe(&self) -> broadcast::Receiver<ChangeEvent> {
        self.tx.subscribe()
    }

    /// Returns the number of active subscribers.
    pub fn receiver_count(&self) -> usize {
        self.tx.receiver_count()
    }

    /// Broadcasts an arbitrary `ChangeEvent`.
    pub fn send(&self, event: ChangeEvent) {
        let _ = self.tx.send(event);
    }

    /// Broadcasts a `PeerUpdated` event if `flags` is non-empty.
    pub fn peer_updated(&self, peer_id: PeerId, flags: PeerUpdateFlags) {
        if !flags.is_empty() {
            self.send(ChangeEvent::PeerUpdated { peer_id, flags });
        }
    }

    /// Broadcasts a `NameUpdated` event.
    pub fn name_updated(&self, peer_id: PeerId, old_first_letters: Vec<char>) {
        self.send(ChangeEvent::NameUpdated {
            peer_id,
            old_first_letters,
        });
    }

    /// Broadcasts a `HistoryUpdated` event if `flags` is non-empty.
    pub fn history_updated(&self, peer_id: PeerId, flags: HistoryUpdateFlags) {
        if !flags.is_empty() {
            self.send(ChangeEvent::HistoryUpdated { peer_id, flags });
        }
    }

    /// Broadcasts a `MessageUpdated` event if `flags` is non-empty.
    pub fn message_updated(&self, id: FullMsgId, flags: MessageUpdateFlags) {
        if !flags.is_empty() {
            self.send(ChangeEvent::MessageUpdated { id, flags });
        }
    }

    /// Broadcasts an `EntryUpdated` event if `flags` is non-empty.
    pub fn entry_updated(&self, key: DialogKey, flags: EntryUpdateFlags) {
        if !flags.is_empty() {
            self.send(ChangeEvent::EntryUpdated { key, flags });
        }
    }

    /// Broadcasts a `TopicUpdated` event if `flags` is non-empty.
    pub fn topic_updated(&self, peer_id: PeerId, root_id: MsgId, flags: TopicUpdateFlags) {
        if !flags.is_empty() {
            self.send(ChangeEvent::TopicUpdated {
                peer_id,
                root_id,
                flags,
            });
        }
    }

    /// Broadcasts `FiltersChanged`.
    pub fn filters_changed(&self) {
        self.send(ChangeEvent::FiltersChanged);
    }

    /// Broadcasts `ChatAdminChanged`.
    pub fn chat_admin_changed(
        &self,
        peer_id: PeerId,
        user_id: UserId,
        rights: ChatAdminRights,
        rank: impl Into<String>,
    ) {
        self.send(ChangeEvent::ChatAdminChanged {
            peer_id,
            user_id,
            rights,
            rank: rank.into(),
        });
    }

    /// Broadcasts `UserTyping`.
    pub fn user_typing(
        &self,
        peer_id: PeerId,
        from_id: PeerId,
        top_msg_id: Option<MsgId>,
        action: TypingAction,
    ) {
        self.send(ChangeEvent::UserTyping {
            peer_id,
            from_id,
            top_msg_id,
            action,
        });
    }
}
