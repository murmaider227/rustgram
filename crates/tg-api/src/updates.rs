//! Real-time updates handler wired to `tg_data::UpdatesStateMachine`, `tg_data::Session`,
//! and `tg_tl::enums::Updates`.
//!
//! Port origin: `api/api_updates.cpp`, `data/data_pts_waiter.cpp`.
//!
//! [`UpdatesHandler`] owns a `tg_data::UpdatesStateMachine`, ingests generated
//! `tg_tl::enums::Updates` (either pushed directly or pumped from
//! [`crate::RpcClient::subscribe_updates`]), triggers `updates.getDifference` /
//! `updates.getChannelDifference` through [`Invoker`], and applies every update
//! directly onto [`tg_data::Session`].

use tg_data::{
    ARCHIVE_FOLDER_ID, BotStatus, BusinessShortcutId, ChannelFlags, ChannelId,
    ChannelUpdateEnvelope, ChatAdminRights, ChatId, ChatUpdateVersionStatus, CommonUpdateEnvelope,
    DeletedScheduledInfo, DialogKey, DraftKey, FolderId, FullMsgId, LoadedStatus, Message,
    MessageContent, MessageFlags, MessageUpdateFlags, MsgId, PeerId, PeerNotifySettings,
    PeerUpdateFlags, SERVER_MAX_MSG_ID, Session, ShortcutIdChange, TimeId, TopicUpdateFlags,
    UpdateAction, UpdatesStateMachine, UserId, UserStatusInput, Username,
    scheduled_remote_to_local_id,
};
use tg_tl::{enums as tl_enums, functions as tl_fns, types as tl_types};
use tokio::sync::broadcast;
use tracing::warn;

use crate::account::DefaultNotifyScope;
use crate::contacts::{
    PrivacyKey, PrivacyRule, privacy_key_from_tl, privacy_key_to_tl,
    privacy_rules_all_chats_loaded, privacy_rules_from_tl,
};
use crate::convert::{
    apply_tl_chat_full, apply_tl_dialog, apply_tl_message, apply_tl_peer_dialogs_for_folder,
    apply_tl_users_and_chats, dialog_filter_from_tl, dialog_key_from_tl, draft_from_tl,
    emoji_status_from_tl, entities_from_tl_with_self, input_channel_for,
    message_from_short_chat_message, message_from_short_message, message_from_tl,
    message_media_from_tl, message_peer_loaded, peer_id_from_tl, peer_notify_settings_from_tl,
    reactions_from_tl, short_chat_message_data_loaded, short_message_data_loaded,
    typing_action_from_tl, user_status_from_tl,
};
use crate::error::ApiError;
use crate::invoker::Invoker;
use crate::rpc::InvocationError;

/// Re-exported `tg_data` timing, persistence & limit types/constants for callers.
pub use tg_data::{
    CHANNEL_GET_DIFF_LIMIT, FAIL_DIFF_INITIAL_BACKOFF_SECS, FAIL_DIFF_MAX_BACKOFF_SECS,
    FAST_GAP_TIMEOUT_MS, NO_UPDATES_AFTER_SLEEP_TIMEOUT_MS, NO_UPDATES_TIMEOUT_MS,
    PersistedUpdatesState, WAIT_FOR_CHANNEL_GET_DIFF_MS, WAIT_FOR_SKIPPED_TIMEOUT_MS,
};

/// Payload queued inside [`tg_data::UpdatesStateMachine`] while waiting for PTS/seq ordering.
#[derive(Debug, Clone)]
pub enum UpdatePayload {
    /// PTS-only increment from `messages.affectedMessages` / `messages.affectedHistory`.
    PtsOnly,
    /// Full `updates` or `updatesCombined` batch buffered on a `seq` gap.
    SeqBatch {
        /// Updates vector.
        updates: Vec<tl_enums::Update>,
        /// Accompanying users.
        users: Vec<tl_enums::User>,
        /// Accompanying chats.
        chats: Vec<tl_enums::Chat>,
    },
    /// A single TL `Update` item.
    Single(Box<tl_enums::Update>),
    /// A converted short message (`updateShortMessage` or `updateShortChatMessage`).
    ShortMessage(Box<Message>),
    /// A `updateShortSentMessage` acknowledgement.
    ShortSent {
        /// Server message ID.
        id: MsgId,
        /// Optional local `random_id` of the sent request.
        random_id: Option<i64>,
    },
}

/// Default online presence refresh interval (`mtproto/mtproto_config.h:20`, 120 s).
pub const DEFAULT_ONLINE_UPDATE_PERIOD_MS: u64 = 120_000;

/// Default user-idle timeout before going offline (`mtproto/mtproto_config.h:22`, 30 s).
pub const DEFAULT_OFFLINE_IDLE_TIMEOUT_MS: u64 = 30_000;

/// Polling interval while idle to detect user return (`api/api_updates.cpp:1013,1076`, 900 ms).
pub const IDLE_FINISH_CHECK_MS: u64 = 900;

/// Result of evaluating and optionally sending `account.updateStatus` (`Updates::updateOnline`,
/// `api/api_updates.cpp:995-1064`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresenceUpdateResult {
    /// Whether `account.updateStatus` was actually invoked.
    pub sent: bool,
    /// Effective online state after evaluation (`offline = !online`).
    pub online: bool,
    /// Delay in milliseconds until the next scheduled `updateOnline` check.
    pub next_update_in_ms: u64,
    /// If the client transitioned into idle while window is active, polling delay (`900` ms).
    pub idle_finish_check_ms: Option<u64>,
}

/// High-level side-effect events surfaced by [`UpdatesHandler`] alongside mutations to [`Session`].
#[derive(Debug, Clone, PartialEq)]
pub enum UpdateEvent {
    /// Confirmed `updateMessageID` (or `updateShortSentMessage`) mapping `random_id -> server_id`.
    MessageIdMapped {
        /// Client random ID.
        random_id: i64,
        /// Server-assigned message ID.
        server_id: MsgId,
    },
    /// Channel difference returned `updates.channelDifferenceTooLong` (`Session` bottom invalidated).
    ChannelTooLong {
        /// Channel ID.
        channel_id: ChannelId,
        /// Updated channel `pts` (if provided).
        pts: Option<i32>,
    },
    /// Common difference returned `updates.differenceTooLong`; local `pts` was reset to `pts`
    /// and callers should refresh dialogs / active histories.
    DifferenceTooLong {
        /// New server common `pts`.
        pts: i32,
    },
    /// Server sent `updateServiceNotification` with `AUTH_KEY_DROP_*`.
    ForceLogout {
        /// Raw notification type string.
        notification_type: String,
    },
    /// Server pushed a `langPackDifference` (`updateLangPack`).
    LangPackDifference(tg_lang::LangPackDifference),
    /// Server reported `updateLangPackTooLong`.
    LangPackTooLong {
        /// Language code requiring reload.
        lang_code: String,
    },
    /// Server sent `updateLoginToken` (QR login scanned/confirmed).
    LoginToken,
    /// Server delivered a 1:1 or group call update (`api/api_updates.cpp:2258-2268`).
    CallUpdate(Box<tl_enums::Update>),
    /// Server delivered a story update (`updateStory`, `updateReadStories`,
    /// `updateStoriesStealthMode`, `updateStoryID`) (`api/api_updates.cpp:2947-2958`).
    StoryUpdate(Box<tl_enums::Update>),
    /// `updatePinnedForumTopic` referenced an unloaded topic; caller should fetch it via
    /// `messages.getForumTopicsByID` (`api/api_updates.cpp:2671-2673`).
    ForumTopicReloadNeeded {
        /// Forum peer ID.
        peer_id: PeerId,
        /// Unloaded topic root ID.
        root_id: MsgId,
    },
    /// `updatePinnedForumTopics` omitted `order` or referenced an unloaded topic; caller
    /// should call `reloadTopics()` (`api/api_updates.cpp:2697-2699`).
    ForumTopicsReloadNeeded {
        /// Forum peer ID.
        peer_id: PeerId,
    },
    /// `updateSavedDialogPinned` or `updatePinnedSavedDialogs` could not be applied locally;
    /// caller should call `messages.getPinnedSavedDialogs` (`data/data_saved_messages.cpp:479,498,515`).
    SavedPinnedReloadNeeded,
    /// `updateSavedReactionTags` invalidated cached Saved Messages reaction tags; caller
    /// should refresh `messages.getSavedReactionTags` after `MY_TAGS_REQUEST_TIMEOUT_MS`
    /// (`api/api_updates.cpp:2879-2881`).
    SavedReactionTagsInvalidated,
    /// `updateDeleteScheduledMessages` reported one or more scheduled messages were delivered
    /// (`sent_messages` paired IDs, `data/components/scheduled_messages.cpp:383-389`).
    ScheduledMessagesSent {
        /// Peer owning the scheduled messages.
        peer_id: PeerId,
        /// Deleted scheduled message metadata (with paired regular `sent_id`).
        items: Vec<DeletedScheduledInfo>,
    },
    /// A regular outgoing message was converted to a scheduled message on send
    /// (`Updates::applyConvertToScheduledOnSend` in `api/api_updates.cpp:912-955`).
    SentConvertedToScheduled {
        /// Peer owning the message.
        peer_id: PeerId,
        /// Local scheduled message ID (`SERVER_MAX_MSG_ID + remote + 1`).
        scheduled_id: MsgId,
    },
    /// `updateQuickReplies` included shortcuts whose `top_message` is missing locally;
    /// caller should reload `messages.getQuickReplies` (`data_shortcut_messages.cpp:261-281`).
    QuickRepliesReloadNeeded,
    /// `updateQuickReplyMessage` (on an unloaded shortcut) or `updateDeleteQuickReplyMessages`
    /// requires fetching `messages.getQuickReplyMessages(shortcut_id)`
    /// (`data_shortcut_messages.cpp:324-326,367-368`).
    QuickReplyMessagesReloadNeeded {
        /// Shortcut ID to reload.
        shortcut_id: BusinessShortcutId,
    },
    /// A local quick-reply placeholder shortcut ID was replaced by a server ID (or deleted
    /// when `new_id == 0`) (`Data::ShortcutIdChange`).
    ShortcutIdChanged(ShortcutIdChange),
    /// Server delivered `updateNewAuthorization` (`api/api_updates.cpp:2366`).
    NewAuthorization(Box<tl_types::UpdateNewAuthorization>),
    /// Server delivered `updateNotifySettings` (`api/api_updates.cpp:2196-2201`).
    NotifySettingsUpdated {
        /// Affected peer, scope, forum topic, or community.
        peer: tl_enums::NotifyPeer,
        /// Updated TL notification settings.
        settings: Box<tl_enums::PeerNotifySettings>,
    },
    /// Server delivered `updateUser` for a full-loaded user (`api/api_updates.cpp:2179-2186`),
    /// signaling that `users.getFullUser` should be re-fetched.
    UserFullRefreshNeeded {
        /// User ID whose full profile changed.
        user_id: UserId,
    },
    /// Server delivered `updateUserStatus` with `Offline` or `Empty` for our own user ID
    /// (another device went offline; `api/api_updates.cpp:2143-2150`). Active desktop clients
    /// should immediately re-assert online status if non-idle.
    SelfOtherOffline {
        /// `was_online` timestamp from `userStatusOffline`, if present.
        was_online: Option<TimeId>,
    },
    /// Server pushed (or reloaded via `account.getPrivacy`) an updated privacy rule
    /// (`api/api_updates.cpp:2400-2429`, `api/api_user_privacy.cpp:203-217`).
    PrivacyUpdated {
        /// Privacy setting key.
        key: PrivacyKey,
        /// Parsed privacy rule.
        rule: PrivacyRule,
    },
    /// Server sent `updateStickerSets` (`api/api_updates.cpp:2828-2840`).
    StickerSets {
        /// Whether mask sets changed.
        masks: bool,
        /// Whether custom emoji sets changed.
        emojis: bool,
    },
    /// Server sent `updateStickerSetsOrder` (`api/api_updates.cpp:2749-2796`).
    StickerSetsOrder {
        /// Whether mask sets order changed.
        masks: bool,
        /// Whether custom emoji sets order changed.
        emojis: bool,
        /// New ordered sticker set IDs.
        order: Vec<u64>,
    },
    /// Server sent `updateNewStickerSet` (`api/api_updates.cpp:2740-2747`).
    NewStickerSet(Box<tl_enums::messages::StickerSet>),
    /// Server sent `updateRecentStickers` (`api/api_updates.cpp:2842-2845`).
    RecentStickers,
    /// Server sent `updateFavedStickers` (`api/api_updates.cpp:2847-2850`).
    FavedStickers,
    /// Server sent `updateReadFeaturedStickers` (`api/api_updates.cpp:2852-2858`).
    ReadFeaturedStickers,
    /// Server sent `updateSavedGifs` (`api/api_updates.cpp:2884-2887`).
    SavedGifs,
    /// Server sent `updateRecentReactions` (`api/api_updates.cpp:2875-2877`).
    RecentReactions,
    /// Server sent `updateSavedReactionTags` (`api/api_updates.cpp:2879-2881`).
    SavedReactionTags,
    /// Server sent `updateRecentEmojiStatuses` (`api/api_updates.cpp:2871-2873`).
    RecentEmojiStatuses,
    /// Server sent `updateMoveStickerSetToTop` (`api/api_updates.cpp:2798-2826`).
    MoveStickerSetToTop {
        /// Masks flag.
        masks: bool,
        /// Custom emoji flag.
        emojis: bool,
        /// Set ID to move to index 0.
        stickerset: u64,
    },
    /// Server sent `updateConfig` (`api/api_updates.cpp:2208-2212`).
    Config,
    /// Server sent `updateAttachMenuBots` (`api/api_updates.cpp:2323-2325`).
    AttachMenuBots,
    /// Server sent `updateSavedRingtones` (`api/api_updates.cpp:2934-2936`).
    SavedRingtones,
    /// Server sent `updateTheme` (`api/api_updates.cpp:2929-2932`).
    Theme(Box<tl_enums::Theme>),
    /// Server sent `updateQuickReplies` (`api/api_updates.cpp:1975-1978`).
    QuickReplies(Vec<tl_enums::QuickReply>),
}

/// Follow-up RPC scheduled by applying an update (executed via [`Invoker`] immediately after
/// applying the update).
#[derive(Debug, Clone, PartialEq, Eq)]
enum PostUpdateRpc {
    ReloadPinnedDialogs { folder_id: FolderId },
    ReloadDialogFilters,
    RefetchChannel { channel_id: ChannelId },
    ReloadFullChat { chat_id: ChatId },
    ReloadPrivacy { key: PrivacyKey },
}

/// Coordinates Telegram updates ordering, gap recovery, and application to [`Session`].
#[derive(Debug)]
pub struct UpdatesHandler {
    machine: UpdatesStateMachine<UpdatePayload>,
    events: Vec<UpdateEvent>,
    last_was_online: bool,
    online_update_period_ms: u64,
    offline_idle_timeout_ms: u64,
    last_set_online_ms: Option<u64>,
    is_idle: bool,
    other_online: TimeId,
    default_notify_users: Option<PeerNotifySettings>,
    default_notify_chats: Option<PeerNotifySettings>,
    default_notify_broadcasts: Option<PeerNotifySettings>,
}

impl Default for UpdatesHandler {
    fn default() -> Self {
        Self::new()
    }
}

impl UpdatesHandler {
    /// Creates a new `UpdatesHandler` backed by a fresh `tg_data::UpdatesStateMachine`.
    ///
    /// Matches `Api::Updates`: `requesting_difference` starts `true` until [`Self::initialize`]
    /// completes `updates.getState`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            machine: UpdatesStateMachine::new(),
            events: Vec::new(),
            last_was_online: false,
            online_update_period_ms: DEFAULT_ONLINE_UPDATE_PERIOD_MS,
            offline_idle_timeout_ms: DEFAULT_OFFLINE_IDLE_TIMEOUT_MS,
            last_set_online_ms: None,
            is_idle: false,
            other_online: 0,
            default_notify_users: None,
            default_notify_chats: None,
            default_notify_broadcasts: None,
        }
    }

    /// Returns a reference to the underlying `tg_data::UpdatesStateMachine`.
    #[must_use]
    pub fn state_machine(&self) -> &UpdatesStateMachine<UpdatePayload> {
        &self.machine
    }

    /// Returns mutable access to the underlying `tg_data::UpdatesStateMachine`.
    pub fn state_machine_mut(&mut self) -> &mut UpdatesStateMachine<UpdatePayload> {
        &mut self.machine
    }

    /// Returns the current confirmed global `pts`.
    #[must_use]
    pub const fn pts(&self) -> i32 {
        self.machine.pts()
    }

    /// Returns the current `(date, qts, seq)` snapshot.
    #[must_use]
    pub const fn state_snapshot(&self) -> (TimeId, i32, i32) {
        (self.machine.date(), self.machine.qts(), self.machine.seq())
    }

    /// Returns the confirmed `pts` for `channel_id`, if initialized.
    #[must_use]
    pub fn channel_pts(&self, channel_id: ChannelId) -> Option<i32> {
        self.machine.channel_pts(channel_id)
    }

    /// Initializes the PTS watermark for a channel (e.g. from `Dialog::pts` or `ChannelMessages::pts`).
    pub fn init_channel_pts(&mut self, channel_id: ChannelId, pts: i32) {
        self.machine.init_channel_pts(channel_id, pts);
    }

    /// Marks whether a channel is currently active/open for short-polling.
    pub fn set_active_channel(&mut self, channel_id: ChannelId, active: bool, now_ms: u64) {
        self.machine.set_active_channel(channel_id, active, now_ms);
    }

    /// Returns the earliest pending timer deadline in milliseconds, if any.
    #[must_use]
    pub fn next_deadline(&self) -> Option<u64> {
        self.machine.next_deadline()
    }

    /// Returns `true` if `NO_UPDATES_TIMEOUT_MS` (60s) has elapsed since the last update.
    #[must_use]
    pub fn needs_ping(&self, now_ms: u64) -> bool {
        self.machine.needs_ping(now_ms)
    }

    /// Checks whether an idle keepalive ping should be sent (`Updates::checkLastUpdate`),
    /// advancing the internal last-update timestamp when returning `true`.
    pub fn check_last_update(&mut self, after_sleep: bool, now_ms: u64) -> bool {
        self.machine.check_last_update(after_sleep, now_ms)
    }

    /// Records that a keepalive ping was sent at `now_ms`.
    pub fn mark_ping_sent(&mut self, now_ms: u64) {
        self.machine.mark_ping_sent(now_ms);
    }

    /// Exports a persistent snapshot of `(pts, qts, date, seq)` and per-channel `pts`.
    #[must_use]
    pub fn export_state(&self) -> PersistedUpdatesState {
        self.machine.export_persisted_state()
    }

    /// Restores a previously persisted state snapshot without issuing RPCs.
    pub fn restore_state(&mut self, state: &PersistedUpdatesState, now_ms: u64) {
        self.machine.restore_persisted_state(state, now_ms);
    }

    /// Drains accumulated high-level [`UpdateEvent`] notifications.
    pub fn drain_events(&mut self) -> Vec<UpdateEvent> {
        std::mem::take(&mut self.events)
    }

    /// Initializes the updates state by calling `updates.getState` (`Updates::stateDone`).
    pub async fn initialize<I: Invoker>(
        &mut self,
        invoker: &I,
        session: &mut Session,
        now_ms: u64,
    ) -> Result<tl_types::updates::State, ApiError> {
        let state = invoker.updates_get_state().await?;
        let actions = self
            .machine
            .init_state(state.pts, state.date, state.qts, state.seq, now_ms);
        self.process_actions(invoker, session, actions, now_ms)
            .await?;
        Ok(state)
    }

    /// Resumes from a previously persisted [`PersistedUpdatesState`]:
    /// restores `(pts, qts, date, seq)` and per-channel `pts`, resets `seq = 0`, and
    /// immediately calls `updates.getDifference` to fetch all missed updates since shutdown.
    ///
    /// If `state` is invalid (`pts <= 0`) or the server rejects the saved state with a
    /// client error (e.g. expired PTS/date), falls back cleanly to `updates.getState`.
    /// Returns `Ok(true)` if resumed via `getDifference`, or `Ok(false)` if it fell back
    /// to a fresh `updates.getState`.
    pub async fn resume_from_state<I: Invoker>(
        &mut self,
        invoker: &I,
        session: &mut Session,
        state: &PersistedUpdatesState,
        now_ms: u64,
    ) -> Result<bool, ApiError> {
        if !state.is_valid() {
            self.initialize(invoker, session, now_ms).await?;
            return Ok(false);
        }
        self.machine.restore_persisted_state(state, now_ms);
        match self.on_new_session_created(invoker, session, now_ms).await {
            Ok(()) => Ok(true),
            Err(ApiError::Invocation(InvocationError::Rpc(ref rpc)))
                if rpc.code == 400
                    && (rpc.name.contains("PTS")
                        || rpc.name.contains("DATE")
                        || rpc.name.contains("PERSISTENT_TIMESTAMP")) =>
            {
                warn!(
                    code = rpc.code,
                    error = %rpc.name,
                    "saved updates state rejected by server; falling back to updates.getState"
                );
                self.initialize(invoker, session, now_ms).await?;
                Ok(false)
            }
            Err(err) => Err(err),
        }
    }

    /// Handles a transport reconnect, sleep wake-up, or network change:
    /// resets `seq = 0`, fetches common `updates.getDifference`, and refreshes active channels.
    pub async fn on_reconnect<I: Invoker>(
        &mut self,
        invoker: &I,
        session: &mut Session,
        now_ms: u64,
    ) -> Result<(), ApiError> {
        let actions = self.machine.on_reconnect(now_ms);
        self.process_actions(invoker, session, actions, now_ms)
            .await
    }

    /// Feeds a pts-only response from `messages.readHistory` / `messages.deleteMessages` /
    /// `channels.deleteMessages` (`ApiWrap::applyAffectedMessages`).
    pub async fn apply_affected_messages<I: Invoker>(
        &mut self,
        invoker: &I,
        session: &mut Session,
        channel_id: Option<ChannelId>,
        affected: &tl_types::messages::AffectedMessages,
        now_ms: u64,
    ) -> Result<(), ApiError> {
        self.apply_pts_only(
            invoker,
            session,
            channel_id,
            affected.pts,
            affected.pts_count,
            now_ms,
        )
        .await
    }

    /// Feeds a pts-only response from `messages.deleteHistory` (`ApiWrap::applyAffectedHistory`).
    pub async fn apply_affected_history<I: Invoker>(
        &mut self,
        invoker: &I,
        session: &mut Session,
        affected: &tl_types::messages::AffectedHistory,
        now_ms: u64,
    ) -> Result<(), ApiError> {
        self.apply_pts_only(
            invoker,
            session,
            None,
            affected.pts,
            affected.pts_count,
            now_ms,
        )
        .await
    }

    /// Feeds a raw `(pts, pts_count)` increment into the common or channel PTS waiter.
    pub async fn apply_pts_only<I: Invoker>(
        &mut self,
        invoker: &I,
        session: &mut Session,
        channel_id: Option<ChannelId>,
        pts: i32,
        pts_count: i32,
        now_ms: u64,
    ) -> Result<(), ApiError> {
        let actions = match channel_id {
            None => self.machine.on_common_update(
                CommonUpdateEnvelope {
                    pts,
                    pts_count,
                    date: None,
                    qts: None,
                    payload: UpdatePayload::PtsOnly,
                },
                now_ms,
            ),
            Some(ch) => self.machine.on_channel_update(
                ChannelUpdateEnvelope {
                    channel_id: ch,
                    pts,
                    pts_count,
                    payload: UpdatePayload::PtsOnly,
                },
                now_ms,
            ),
        };
        self.process_actions(invoker, session, actions, now_ms)
            .await
    }

    /// Feeds a channel PTS watermark (`pts_count == 0`, matching `ChannelData::ptsReceived` in
    /// `data/data_channel.h:483-485`) or channel RPC `(pts, pts_count)` (`pts_count > 0`, matching
    /// `ChannelData::ptsUpdateAndApply` in `data/data_channel.h:486-488` and
    /// `ApiWrap::applyAffectedHistory` in `apiwrap.cpp:2342-2343`) directly into that channel's
    /// `PtsWaiter`, gating only on the channel's own `requesting` flag rather than common
    /// `getDifference` state.
    pub async fn apply_channel_pts<I: Invoker>(
        &mut self,
        invoker: &I,
        session: &mut Session,
        channel_id: ChannelId,
        pts: i32,
        pts_count: i32,
        now_ms: u64,
    ) -> Result<(), ApiError> {
        if pts <= 0 {
            return Ok(());
        }
        let actions = self.machine.on_channel_update_from_difference(
            ChannelUpdateEnvelope {
                channel_id,
                pts,
                pts_count,
                payload: UpdatePayload::PtsOnly,
            },
            now_ms,
        );
        self.process_actions(invoker, session, actions, now_ms)
            .await
    }

    /// Handles a `new_session_created` signal from MTProto (`Updates::mtpNewSessionCreated`):
    /// resets `seq = 0` and calls `updates.getDifference`.
    pub async fn on_new_session_created<I: Invoker>(
        &mut self,
        invoker: &I,
        session: &mut Session,
        now_ms: u64,
    ) -> Result<(), ApiError> {
        let actions = self.machine.on_new_session_created();
        self.process_actions(invoker, session, actions, now_ms)
            .await
    }

    /// Advances time to `now_ms`, triggering any due `getDifference` / `getChannelDifference`
    /// gap, short-poll, or backoff-retry timers.
    pub async fn on_tick<I: Invoker>(
        &mut self,
        invoker: &I,
        session: &mut Session,
        now_ms: u64,
    ) -> Result<bool, ApiError> {
        let actions = self.machine.on_tick(now_ms);
        self.process_actions(invoker, session, actions, now_ms)
            .await?;
        Ok(self.machine.needs_ping(now_ms))
    }

    /// Processes a single incoming `tg_tl::enums::Updates` container (`Updates::applyUpdates`).
    pub async fn handle_updates<I: Invoker>(
        &mut self,
        invoker: &I,
        session: &mut Session,
        updates: tl_enums::Updates,
        sent_random_id: Option<i64>,
        now_ms: u64,
    ) -> Result<(), ApiError> {
        // While requesting getDifference, force-logout notifications and group call participant /
        // E2E chain block updates must still be applied immediately (`api/api_updates.cpp:904-909,
        // 957-977`, `Updates::mtpUpdateReceived` / `Updates::applyGroupCallParticipantUpdates`).
        if self.machine.requesting_difference() {
            self.extract_force_logouts_and_group_call_updates(session, &updates);
            return Ok(());
        }

        match updates {
            tl_enums::Updates::TooLong => {
                let actions = self.machine.on_updates_too_long(now_ms);
                self.process_actions(invoker, session, actions, now_ms)
                    .await?;
            }
            tl_enums::Updates::Updates(u) => {
                let actions = self.machine.on_seq_updates(
                    u.seq,
                    u.seq,
                    u.date,
                    UpdatePayload::SeqBatch {
                        updates: u.updates,
                        users: u.users,
                        chats: u.chats,
                    },
                    now_ms,
                );
                self.process_actions(invoker, session, actions, now_ms)
                    .await?;
            }
            tl_enums::Updates::Combined(c) => {
                let actions = self.machine.on_seq_updates(
                    c.seq_start,
                    c.seq,
                    c.date,
                    UpdatePayload::SeqBatch {
                        updates: c.updates,
                        users: c.users,
                        chats: c.chats,
                    },
                    now_ms,
                );
                self.process_actions(invoker, session, actions, now_ms)
                    .await?;
            }
            tl_enums::Updates::UpdateShort(s) => {
                self.route_single_update(invoker, session, s.update, Some(s.date), now_ms)
                    .await?;
                self.machine.advance_date(s.date);
            }
            tl_enums::Updates::UpdateShortMessage(s) => {
                if !short_message_data_loaded(session, &s) {
                    let actions = self.machine.on_updates_too_long(now_ms);
                    self.process_actions(invoker, session, actions, now_ms)
                        .await?;
                    return Ok(());
                }
                let msg = message_from_short_message(&s, session.self_user_id());
                let actions = self.machine.on_common_update(
                    CommonUpdateEnvelope {
                        pts: s.pts,
                        pts_count: s.pts_count,
                        date: Some(s.date),
                        qts: None,
                        payload: UpdatePayload::ShortMessage(Box::new(msg)),
                    },
                    now_ms,
                );
                self.process_actions(invoker, session, actions, now_ms)
                    .await?;
            }
            tl_enums::Updates::UpdateShortChatMessage(s) => {
                if !short_chat_message_data_loaded(session, &s) {
                    let actions = self.machine.on_updates_too_long(now_ms);
                    self.process_actions(invoker, session, actions, now_ms)
                        .await?;
                    return Ok(());
                }
                let msg = message_from_short_chat_message(&s, session.self_user_id());
                let actions = self.machine.on_common_update(
                    CommonUpdateEnvelope {
                        pts: s.pts,
                        pts_count: s.pts_count,
                        date: Some(s.date),
                        qts: None,
                        payload: UpdatePayload::ShortMessage(Box::new(msg)),
                    },
                    now_ms,
                );
                self.process_actions(invoker, session, actions, now_ms)
                    .await?;
            }
            tl_enums::Updates::UpdateShortSentMessage(s) => {
                let is_server_msg_id = s.id > 0 && i64::from(s.id) < SERVER_MAX_MSG_ID.0;
                let msg_id = MsgId(i64::from(s.id));
                if !is_server_msg_id {
                    warn!(msg_id = s.id, "bad msg_id in updateShortSentMessage");
                } else if let Some(random_id) = sent_random_id {
                    let self_id = session.self_user_id();
                    if let Some(local_full_id) = session.resolve_random_id(random_id, msg_id) {
                        let promoted_id = FullMsgId::new(local_full_id.peer, msg_id);
                        if let Some(msg) = session.message_mut(promoted_id) {
                            // Match `HistoryItem::applySentMessage(MTPDupdateShortSentMessage)`
                            // (`history/history_item.cpp:2958-2976`): always replace entities
                            // with server-returned entities (empty when `None`) and clear
                            // pending/failed send flags.
                            msg.date = s.date;
                            msg.flags
                                .remove(MessageFlags::BEING_SENT | MessageFlags::SENDING_FAILED);
                            msg.flags.set(MessageFlags::OUTGOING, s.out);
                            if let Some(ttl) = s.ttl_period {
                                msg.ttl_period = Some(ttl);
                            }
                            if let MessageContent::Regular { text, media } = &mut msg.content {
                                text.entities = s
                                    .entities
                                    .as_deref()
                                    .map(|e| entities_from_tl_with_self(e, self_id))
                                    .unwrap_or_default();
                                if let Some(tl_media) = &s.media {
                                    *media = message_media_from_tl(tl_media);
                                }
                            }
                        }
                        session
                            .changes()
                            .message_updated(promoted_id, MessageUpdateFlags::EDITED);
                    }
                    self.events.push(UpdateEvent::MessageIdMapped {
                        random_id,
                        server_id: msg_id,
                    });
                }
                let actions = self.machine.on_common_update(
                    CommonUpdateEnvelope {
                        pts: s.pts,
                        pts_count: s.pts_count,
                        date: Some(s.date),
                        qts: None,
                        payload: UpdatePayload::ShortSent {
                            id: msg_id,
                            random_id: sent_random_id,
                        },
                    },
                    now_ms,
                );
                self.process_actions(invoker, session, actions, now_ms)
                    .await?;
            }
        }
        Ok(())
    }

    /// Drains all currently queued `Updates` from a `tokio::sync::broadcast::Receiver`
    /// (e.g. returned by [`crate::RpcClient::subscribe_updates`]).
    ///
    /// If the receiver reports `Lagged`, triggers `updates.getDifference` automatically
    /// to recover any dropped updates.
    pub async fn pump_updates_stream<I: Invoker>(
        &mut self,
        invoker: &I,
        session: &mut Session,
        rx: &mut broadcast::Receiver<tl_enums::Updates>,
        now_ms: u64,
    ) -> Result<usize, ApiError> {
        let mut count = 0;
        loop {
            match rx.try_recv() {
                Ok(updates) => {
                    self.handle_updates(invoker, session, updates, None, now_ms)
                        .await?;
                    count += 1;
                }
                Err(broadcast::error::TryRecvError::Lagged(skipped)) => {
                    warn!(
                        skipped,
                        "Updates stream lagged; triggering updates.getDifference"
                    );
                    let actions = self.machine.on_updates_too_long(now_ms);
                    self.process_actions(invoker, session, actions, now_ms)
                        .await?;
                }
                Err(
                    broadcast::error::TryRecvError::Empty | broadcast::error::TryRecvError::Closed,
                ) => break,
            }
        }
        Ok(count)
    }

    /// Updates online/offline presence via `account.updateStatus` (`Updates::updateOnline`).
    pub async fn update_online<I: Invoker>(
        &mut self,
        invoker: &I,
        online: bool,
    ) -> Result<bool, ApiError> {
        self.last_was_online = online;
        let ok = invoker
            .account_update_status(tl_fns::account::UpdateStatus { offline: !online })
            .await?;
        Ok(ok)
    }

    /// Configures `onlineUpdatePeriod` and `offlineIdleTimeout` from server `help.getConfig`
    /// (`mtproto/mtproto_config.cpp:244-246`).
    pub fn set_online_config(
        &mut self,
        online_update_period_ms: u64,
        offline_idle_timeout_ms: u64,
    ) {
        self.online_update_period_ms = online_update_period_ms.max(1_000);
        self.offline_idle_timeout_ms = offline_idle_timeout_ms.max(1_000);
    }

    /// Evaluates window focus, idle timeout, periodic keepalive, and other-device offline
    /// events, sending `account.updateStatus` only when required by tdesktop's exact rules
    /// (`Updates::updateOnline`, `api/api_updates.cpp:995-1064`).
    #[allow(clippy::too_many_arguments)]
    pub async fn update_online_timed<I: Invoker>(
        &mut self,
        invoker: &I,
        session: &mut Session,
        window_active: bool,
        last_non_idle_ms: u64,
        now_ms: u64,
        now_secs: TimeId,
        got_other_offline: bool,
    ) -> Result<PresenceUpdateResult, ApiError> {
        let mut is_online = window_active;
        let mut update_in = self.online_update_period_ms;
        let mut idle_finish_check_ms = None;

        if is_online {
            let idle = now_ms.saturating_sub(last_non_idle_ms);
            if idle >= self.offline_idle_timeout_ms {
                is_online = false;
                if !self.is_idle {
                    self.is_idle = true;
                    idle_finish_check_ms = Some(IDLE_FINISH_CHECK_MS);
                }
            } else {
                update_in = update_in.min(self.offline_idle_timeout_ms - idle);
            }
        }

        let period_elapsed = self
            .last_set_online_ms
            .is_none_or(|last| last.saturating_add(self.online_update_period_ms) <= now_ms);
        let should_send = is_online != self.last_was_online
            || (is_online && period_elapsed)
            || (is_online && got_other_offline);

        if should_send {
            self.last_was_online = is_online;
            self.last_set_online_ms = Some(now_ms);
            let _ = invoker
                .account_update_status(tl_fns::account::UpdateStatus {
                    offline: !is_online,
                })
                .await?;
            // Match `api/api_updates.cpp:1044-1050`: update local self user's lastseen immediately.
            let online_for_secs = (self.online_update_period_ms / 1_000) as TimeId;
            let expires = if is_online {
                now_secs.saturating_add(online_for_secs)
            } else {
                now_secs.saturating_sub(1)
            };
            session.update_user_status(session.self_user_id(), UserStatusInput::Online { expires });
        } else if is_online && let Some(last) = self.last_set_online_ms {
            let remaining = last
                .saturating_add(self.online_update_period_ms)
                .saturating_sub(now_ms);
            update_in = update_in.min(remaining);
        }

        Ok(PresenceUpdateResult {
            sent: should_send,
            online: is_online,
            next_update_in_ms: update_in,
            idle_finish_check_ms,
        })
    }

    /// Checks whether user activity resumed while idle (`Updates::checkIdleFinish`,
    /// `api/api_updates.cpp:1066-1078`).
    pub async fn check_idle_finish<I: Invoker>(
        &mut self,
        invoker: &I,
        session: &mut Session,
        window_active: bool,
        last_non_idle_ms: u64,
        now_ms: u64,
        now_secs: TimeId,
    ) -> Result<Option<PresenceUpdateResult>, ApiError> {
        if now_ms.saturating_sub(last_non_idle_ms) < self.offline_idle_timeout_ms {
            self.is_idle = false;
            let res = self
                .update_online_timed(
                    invoker,
                    session,
                    window_active,
                    last_non_idle_ms,
                    now_ms,
                    now_secs,
                    false,
                )
                .await?;
            Ok(Some(res))
        } else {
            Ok(None)
        }
    }

    /// Returns whether the last sent presence state was online.
    #[must_use]
    pub const fn last_was_online(&self) -> bool {
        self.last_was_online
    }

    /// Returns whether the client is currently considered idle (`Updates::isIdle`).
    #[must_use]
    pub const fn is_idle(&self) -> bool {
        self.is_idle
    }

    /// Returns the last `was_online`/`expires` reported by another device for our own account
    /// (`cOtherOnline`, `api/api_updates.cpp:2148-2154`).
    #[must_use]
    pub const fn other_online(&self) -> TimeId {
        self.other_online
    }

    /// Returns `true` if shutdown should send `account.updateStatus(offline: true)` first
    /// (`Updates::isQuitPrevent`, `api/api_updates.cpp:1088-1095`).
    #[must_use]
    pub const fn is_quit_prevent(&self) -> bool {
        self.last_was_online
    }

    /// Seeds cached scope default notification settings used to resolve inherited mute state
    /// when `updateNotifySettings` arrives with `mute_until == None`.
    pub fn set_default_notify_settings(
        &mut self,
        scope: DefaultNotifyScope,
        settings: PeerNotifySettings,
    ) {
        match scope {
            DefaultNotifyScope::Users => self.default_notify_users = Some(settings),
            DefaultNotifyScope::Chats => self.default_notify_chats = Some(settings),
            DefaultNotifyScope::Broadcasts => self.default_notify_broadcasts = Some(settings),
        }
    }

    fn effective_peer_muted(&self, session: &Session, peer_id: PeerId, now_secs: TimeId) -> bool {
        if let Some(peer) = session.peer(peer_id) {
            if let Some(until) = peer.common().notify.mute_until {
                return until > now_secs;
            }
            let scope = DefaultNotifyScope::for_peer(peer);
            let default = match scope {
                DefaultNotifyScope::Users => self.default_notify_users.as_ref(),
                DefaultNotifyScope::Chats => self.default_notify_chats.as_ref(),
                DefaultNotifyScope::Broadcasts => self.default_notify_broadcasts.as_ref(),
            };
            if let Some(until) = default.and_then(|d| d.mute_until) {
                return until > now_secs;
            }
        }
        false
    }

    // -------------------------------------------------------------------------
    // Internal action & update dispatch
    // -------------------------------------------------------------------------

    async fn process_actions<I: Invoker>(
        &mut self,
        invoker: &I,
        session: &mut Session,
        actions: Vec<UpdateAction<UpdatePayload>>,
        now_ms: u64,
    ) -> Result<(), ApiError> {
        for action in actions {
            match action {
                UpdateAction::Skip | UpdateAction::Buffer => {}
                UpdateAction::Apply(payload) => {
                    let prev_diff_seq = self.machine.seq();
                    self.apply_payload(invoker, session, payload, now_ms)
                        .await?;
                    // If applying this payload triggered `getDifference` (e.g. missing peer in a
                    // cascaded `SeqBatch`), do not apply remaining pre-difference actions
                    // (`api/api_updates.cpp:378-385`, `Updates::setState`).
                    if self.machine.requesting_difference() || self.machine.seq() != prev_diff_seq {
                        break;
                    }
                }
                UpdateAction::ApplyChannel { payload, .. } => {
                    self.apply_payload(invoker, session, payload, now_ms)
                        .await?;
                }
                UpdateAction::RequestDifference { pts, date, qts } => {
                    self.execute_get_difference(invoker, session, pts, date, qts, now_ms)
                        .await?;
                }
                UpdateAction::RequestChannelDifference {
                    channel_id,
                    pts,
                    force,
                    limit,
                } => {
                    self.execute_get_channel_difference(
                        invoker, session, channel_id, pts, force, limit, now_ms,
                    )
                    .await?;
                }
                UpdateAction::ChannelTooLongReset { channel_id } => {
                    // Keep history bottom marked not-loaded after inserting the TooLong
                    // snapshot messages so the UI reloads recent history across the skipped gap
                    // (`api/api_updates.cpp:414`, `history->setNotLoadedAtBottom()`).
                    let peer_id = PeerId::from_channel(channel_id);
                    session.invalidate_history_bottom(peer_id);
                    self.events.push(UpdateEvent::ChannelTooLong {
                        channel_id,
                        pts: self.machine.channel_pts(channel_id),
                    });
                }
                UpdateAction::DifferenceTooLongReset { pts } => {
                    self.events.push(UpdateEvent::DifferenceTooLong { pts });
                }
            }
        }
        Ok(())
    }

    async fn apply_payload<I: Invoker>(
        &mut self,
        invoker: &I,
        session: &mut Session,
        payload: UpdatePayload,
        now_ms: u64,
    ) -> Result<(), ApiError> {
        match payload {
            UpdatePayload::PtsOnly | UpdatePayload::ShortSent { .. } => {}
            UpdatePayload::ShortMessage(msg) => {
                session.insert_message(*msg);
            }
            UpdatePayload::SeqBatch {
                mut updates,
                users,
                chats,
            } => {
                apply_tl_users_and_chats(session, &users, &chats);
                // Match `Updates::feedUpdateVector` (`api/api_updates.cpp:305-328`):
                // stable-sort `updateGroupCallChainBlocks` (0) before
                // `updateGroupCallParticipants` (1) before all other updates (2).
                if updates.iter().any(|u| {
                    matches!(
                        u,
                        tl_enums::Update::GroupCallParticipants(_)
                            | tl_enums::Update::GroupCallChainBlocks(_)
                    )
                }) {
                    updates.sort_by_key(group_call_update_priority);
                }
                // Match `Updates::applyConvertToScheduledOnSend` (`api/api_updates.cpp:912-955`):
                // if `updateNewScheduledMessage` and `updateMessageID` share a server ID and the
                // local pending message was not scheduled, destroy the regular pending message
                // and emit `SentConvertedToScheduled` BEFORE `UpdateMessageID` promotes it.
                self.apply_convert_to_scheduled_on_send(session, &updates);
                // Process UpdateMessageID first so local messages resolve before NewMessage arrives.
                for u in &updates {
                    if let tl_enums::Update::MessageID(m) = u {
                        self.apply_message_id(session, m.random_id, MsgId(i64::from(m.id)));
                    }
                }
                for u in updates {
                    if self.machine.requesting_difference() {
                        break;
                    }
                    if !matches!(u, tl_enums::Update::MessageID(_)) {
                        Box::pin(self.route_single_update(invoker, session, u, None, now_ms))
                            .await?;
                    }
                }
            }
            UpdatePayload::Single(update) => {
                // Re-check unloaded/min peer after a PTS gap fills (`api/api_updates.cpp:1651-1660`):
                // if a forward-gapped `updateNewMessage` had an unloaded peer and the gap filled,
                // trigger `getDifference` instead of inserting a message for an unknown peer.
                if let tl_enums::Update::NewMessage(ref u) = *update
                    && !self.machine.requesting_difference()
                    && !message_peer_loaded(session, &u.message)
                {
                    let actions = self.machine.on_updates_too_long(now_ms);
                    return Box::pin(self.process_actions(invoker, session, actions, now_ms)).await;
                }
                if let Some(rpc) = self.apply_confirmed_update(session, *update, now_ms) {
                    self.execute_post_update_rpcs(invoker, session, vec![rpc], now_ms)
                        .await;
                }
            }
        }
        Ok(())
    }

    async fn route_single_update<I: Invoker>(
        &mut self,
        invoker: &I,
        session: &mut Session,
        update: tl_enums::Update,
        date: Option<TimeId>,
        now_ms: u64,
    ) -> Result<(), ApiError> {
        // Missing/min peer check on contiguous `updateNewMessage` (`api_updates.cpp:1651-1660`).
        // Forward-gapped messages still enter `PtsWaiter` so the gap can fill (or time out
        // into `getDifference`), whereas contiguous messages with missing/min peers trigger
        // `getDifference` immediately without advancing `pts`.
        if let tl_enums::Update::NewMessage(ref u) = update
            && !self.machine.requesting_difference()
            && u.pts <= self.machine.pts().saturating_add(u.pts_count)
            && !message_peer_loaded(session, &u.message)
        {
            let actions = self.machine.on_updates_too_long(now_ms);
            return Box::pin(self.process_actions(invoker, session, actions, now_ms)).await;
        }

        // Common PTS-bearing updates -> UpdatesStateMachine::on_common_update.
        if let Some((pts, pts_count)) = common_pts_of(&update) {
            let actions = self.machine.on_common_update(
                CommonUpdateEnvelope {
                    pts,
                    pts_count,
                    date,
                    qts: None,
                    payload: UpdatePayload::Single(Box::new(update)),
                },
                now_ms,
            );
            return Box::pin(self.process_actions(invoker, session, actions, now_ms)).await;
        }

        // Unloaded / `min`-only channel checks (`_byMinChannelTimer` in `api_updates.cpp:1666,1766`).
        if let tl_enums::Update::NewChannelMessage(ref u) = update
            && let Some(cid) = message_channel_id(&u.message)
            && !self.machine.requesting_difference()
            && !session.channel_loaded(cid)
        {
            self.machine.on_missing_channel(now_ms);
            return Ok(());
        }
        if let tl_enums::Update::ChannelReadMessagesContents(ref u) = update
            && !session.channel_loaded(ChannelId::from_tl(u.channel_id))
        {
            self.machine.on_missing_channel(now_ms);
            return Ok(());
        }

        // Channel PTS-bearing updates -> UpdatesStateMachine::on_channel_update.
        if let Some((channel_id, pts, pts_count)) = channel_pts_of(&update) {
            let actions = self.machine.on_channel_update(
                ChannelUpdateEnvelope {
                    channel_id,
                    pts,
                    pts_count,
                    payload: UpdatePayload::Single(Box::new(update)),
                },
                now_ms,
            );
            return Box::pin(self.process_actions(invoker, session, actions, now_ms)).await;
        }

        // `updateChannelTooLong` -> UpdatesStateMachine::on_channel_too_long.
        if let tl_enums::Update::ChannelTooLong(ctl) = update {
            let actions = self.machine.on_channel_too_long(
                ChannelId::from_tl(ctl.channel_id),
                ctl.pts,
                now_ms,
            );
            return Box::pin(self.process_actions(invoker, session, actions, now_ms)).await;
        }

        // QTS-bearing updates -> UpdatesStateMachine::on_qts_update.
        if let Some(qts) = qts_of(&update) {
            let actions =
                self.machine
                    .on_qts_update(qts, UpdatePayload::Single(Box::new(update)), now_ms);
            return Box::pin(self.process_actions(invoker, session, actions, now_ms)).await;
        }

        // Non-PTS/QTS updates apply immediately.
        if let Some(rpc) = self.apply_confirmed_update(session, update, now_ms) {
            self.execute_post_update_rpcs(invoker, session, vec![rpc], now_ms)
                .await;
        }
        Ok(())
    }

    fn apply_confirmed_update(
        &mut self,
        session: &mut Session,
        update: tl_enums::Update,
        now_ms: u64,
    ) -> Option<PostUpdateRpc> {
        let now_secs = (now_ms / 1_000) as TimeId;
        let mut post_rpc = None;
        match update {
            tl_enums::Update::MessageID(m) => {
                self.apply_message_id(session, m.random_id, MsgId(i64::from(m.id)));
            }
            tl_enums::Update::NewMessage(u) => {
                if let Some(msg) = message_from_tl(&u.message, session.self_user_id()) {
                    session.apply_sent_or_new_message(msg);
                }
            }
            tl_enums::Update::NewChannelMessage(u) => {
                if let Some(msg) = message_from_tl(&u.message, session.self_user_id()) {
                    let peer_id = msg.peer_id;
                    let msg_id = msg.id;
                    let date = msg.date;
                    let outgoing = msg.is_outgoing();
                    let topic_root = msg.topic_root_id();
                    session.apply_sent_or_new_message(msg);
                    if let Some(root_id) = topic_root
                        && let Some(topic) = session
                            .forum_mut(peer_id)
                            .and_then(|f| f.topic_mut(root_id))
                    {
                        topic.apply_new_message(msg_id, date, outgoing);
                        session.changes().topic_updated(
                            peer_id,
                            root_id,
                            TopicUpdateFlags::UNREAD_VIEW,
                        );
                    }
                }
            }
            tl_enums::Update::EditMessage(u) => {
                // Match `Session::updateEditedMessage` (`data/data_session.cpp:3089-3099`):
                // only update an already-loaded message; never insert an unknown historical
                // message (which would corrupt `SparseIdsList` loaded ranges).
                if let Some(msg) = message_from_tl(&u.message, session.self_user_id()) {
                    let _ = session.edit_message(msg);
                }
            }
            tl_enums::Update::EditChannelMessage(u) => {
                // Match `Session::updateEditedMessage` (`data/data_session.cpp:3089-3099`).
                if let Some(msg) = message_from_tl(&u.message, session.self_user_id()) {
                    let _ = session.edit_message(msg);
                }
            }
            tl_enums::Update::DeleteMessages(u) => {
                let ids: Vec<MsgId> = u
                    .messages
                    .into_iter()
                    .map(|id| MsgId(i64::from(id)))
                    .collect();
                session.delete_non_channel_messages(&ids);
            }
            tl_enums::Update::DeleteChannelMessages(u) => {
                let peer_id = PeerId::from_channel(ChannelId::from_tl(u.channel_id));
                let ids: Vec<MsgId> = u
                    .messages
                    .into_iter()
                    .map(|id| MsgId(i64::from(id)))
                    .collect();
                session.delete_messages(peer_id, &ids);
            }
            tl_enums::Update::ReadMessagesContents(u) => {
                let ids: Vec<MsgId> = u
                    .messages
                    .into_iter()
                    .map(|id| MsgId(i64::from(id)))
                    .collect();
                let _ = session.mark_messages_contents_read(None, &ids);
            }
            tl_enums::Update::ChannelReadMessagesContents(u) => {
                let peer_id = PeerId::from_channel(ChannelId::from_tl(u.channel_id));
                let ids: Vec<MsgId> = u
                    .messages
                    .into_iter()
                    .map(|id| MsgId(i64::from(id)))
                    .collect();
                let _ = session.mark_messages_contents_read(Some(peer_id), &ids);
            }
            tl_enums::Update::ReadHistoryInbox(u) => {
                let peer_id = peer_id_from_tl(&u.peer);
                session.mark_inbox_read(
                    peer_id,
                    MsgId(i64::from(u.max_id)),
                    Some(u.still_unread_count),
                );
            }
            tl_enums::Update::ReadHistoryOutbox(u) => {
                let peer_id = peer_id_from_tl(&u.peer);
                session.mark_outbox_read(peer_id, MsgId(i64::from(u.max_id)));
            }
            tl_enums::Update::ReadChannelInbox(u) => {
                let ch_id = ChannelId::from_tl(u.channel_id);
                let peer_id = PeerId::from_channel(ch_id);
                // Match `History::applyInboxReadUpdate` (`history.cpp:2278`):
                // trust `still_unread_count` only when our channel PTS matches `u.pts`
                // (or when channel PTS has not been initialized yet).
                let still_unread = match self.machine.channel_pts(ch_id) {
                    Some(cur) if cur != u.pts => None,
                    _ => Some(u.still_unread_count),
                };
                session.mark_inbox_read(peer_id, MsgId(i64::from(u.max_id)), still_unread);
            }
            tl_enums::Update::ReadChannelOutbox(u) => {
                let peer_id = PeerId::from_channel(ChannelId::from_tl(u.channel_id));
                session.mark_outbox_read(peer_id, MsgId(i64::from(u.max_id)));
            }
            tl_enums::Update::ChannelAvailableMessages(u) => {
                let peer_id = PeerId::from_channel(ChannelId::from_tl(u.channel_id));
                session.clear_history_up_till(peer_id, MsgId(i64::from(u.available_min_id)));
            }
            tl_enums::Update::UserStatus(u) => {
                let uid = UserId::from_tl(u.user_id);
                session.update_user_status(uid, user_status_from_tl(&u.status));
                // Match `Updates::applyUpdate(mtpc_updateUserStatus)` (`api/api_updates.cpp:2143-2155`):
                // track other-device online/offline for self and signal active clients to re-assert online.
                if uid == session.self_user_id() {
                    match &u.status {
                        tl_enums::UserStatus::Offline(s) => {
                            self.other_online = s.was_online;
                            self.events.push(UpdateEvent::SelfOtherOffline {
                                was_online: Some(s.was_online),
                            });
                        }
                        tl_enums::UserStatus::Empty => {
                            self.events
                                .push(UpdateEvent::SelfOtherOffline { was_online: None });
                        }
                        tl_enums::UserStatus::Online(s) => {
                            self.other_online = s.expires;
                        }
                        _ => {}
                    }
                }
            }
            tl_enums::Update::UserName(u) => {
                // Match `Updates::applyUpdateNoPtsCheck(updateUserName)`
                // (`api/api_updates.cpp:2158-2177`): only update loaded users, preserve
                // contact's local first/last name, normalize with single_line, pick active
                // editable username, and refresh dialog search index.
                let uid = UserId::from_tl(u.user_id);
                if session.user_loaded(uid) {
                    let peer_id = PeerId::from_user(uid);
                    let usernames: Vec<Username> = u
                        .usernames
                        .iter()
                        .map(|un| {
                            let tl_enums::Username::Username(item) = un;
                            Username {
                                username: item.username.clone(),
                                active: item.active,
                                editable: item.editable,
                            }
                        })
                        .collect();
                    // Pick active editable username, falling back to first active (`api_updates.cpp:2166-2175`).
                    let editable = usernames
                        .iter()
                        .find(|x| x.active && x.editable)
                        .or_else(|| usernames.iter().find(|x| x.active))
                        .or_else(|| usernames.iter().find(|x| x.editable))
                        .or_else(|| usernames.first())
                        .map(|x| single_line(&x.username))
                        .unwrap_or_default();
                    let mut old_letters = None;
                    if let Some(user) = session.user_mut(uid) {
                        let prev_name = user.common.name.clone();
                        let prev_letters: Vec<char> =
                            user.common.name_first_letters.iter().copied().collect();
                        let is_contact = user.is_contact();
                        let first = if is_contact {
                            user.first_name.clone()
                        } else {
                            single_line(&u.first_name)
                        };
                        let last = if is_contact {
                            user.last_name.clone()
                        } else {
                            single_line(&u.last_name)
                        };
                        let name_or_phone = if is_contact {
                            user.name_or_phone.clone()
                        } else if !first.is_empty() && !last.is_empty() {
                            format!("{first} {last}")
                        } else if !first.is_empty() {
                            first.clone()
                        } else if !last.is_empty() {
                            last.clone()
                        } else {
                            user.phone.clone()
                        };
                        user.set_name(&first, &last, name_or_phone, &editable);
                        user.usernames.set_usernames(&usernames);
                        user.rebuild_name_index();
                        if user.common.name != prev_name {
                            old_letters = Some(prev_letters);
                        }
                    }
                    if let Some(letters) = old_letters {
                        session.changes().name_updated(peer_id, letters);
                    }
                    session.changes().peer_updated(
                        peer_id,
                        PeerUpdateFlags::NAME
                            | PeerUpdateFlags::USERNAME
                            | PeerUpdateFlags::USERNAMES,
                    );
                    session.sync_dialog_lists(peer_id);
                }
            }
            tl_enums::Update::UserPhone(u) => {
                // Match `Updates::applyUpdateNoPtsCheck(updateUserPhone)`
                // (`api/api_updates.cpp:2214-2236`): update phone and `name_or_phone` on
                // loaded users and emit `PeerUpdateFlags::PHONE_NUMBER`.
                let uid = UserId::from_tl(u.user_id);
                if session.user_loaded(uid) {
                    let peer_id = PeerId::from_user(uid);
                    let changed = if let Some(user) = session.user_mut(uid) {
                        if user.phone != u.phone {
                            user.phone = u.phone;
                            let name_or_phone = if user.is_contact()
                                || user.is_service_user()
                                || user.is_support()
                                || user.is_self()
                                || user.phone.is_empty()
                            {
                                String::new()
                            } else {
                                user.phone.clone()
                            };
                            let first = user.first_name.clone();
                            let last = user.last_name.clone();
                            let editable = user.usernames.editable_username().to_owned();
                            user.set_name(&first, &last, name_or_phone, &editable);
                            true
                        } else {
                            false
                        }
                    } else {
                        false
                    };
                    if changed {
                        session
                            .changes()
                            .peer_updated(peer_id, PeerUpdateFlags::PHONE_NUMBER);
                        session.sync_dialog_lists(peer_id);
                    }
                }
            }
            tl_enums::Update::UserEmojiStatus(u) => {
                // Match `Updates::applyUpdateNoPtsCheck(updateUserEmojiStatus)`
                // (`api/api_updates.cpp:2864-2869`).
                let uid = UserId::from_tl(u.user_id);
                if session.user_loaded(uid) {
                    let peer_id = PeerId::from_user(uid);
                    let next = emoji_status_from_tl(Some(&u.emoji_status));
                    let changed = if let Some(user) = session.user_mut(uid) {
                        if user.common.emoji_status != next {
                            user.common.emoji_status = next;
                            true
                        } else {
                            false
                        }
                    } else {
                        false
                    };
                    if changed {
                        session
                            .changes()
                            .peer_updated(peer_id, PeerUpdateFlags::EMOJI_STATUS);
                    }
                }
            }
            tl_enums::Update::Channel(u) => {
                // Refetch updated channel info via `channels.getChannels` only when the channel
                // is not already known to be forbidden/deleted (`Updates::applyUpdateNoPtsCheck`
                // for `mtpc_updateChannel` in `api/api_updates.cpp:2539-2562` guards refreshes
                // behind `channel->amIn()`, avoiding `CHANNEL_PRIVATE` loops after delete/kick).
                let channel_id = ChannelId::from_tl(u.channel_id);
                let already_forbidden = session
                    .channel(channel_id)
                    .is_some_and(|ch| ch.is_forbidden());
                if already_forbidden {
                    self.machine.remove_channel(channel_id);
                } else if !channel_id.is_zero() {
                    post_rpc = Some(PostUpdateRpc::RefetchChannel { channel_id });
                }
            }
            tl_enums::Update::ChatParticipants(u) => {
                // Match `Data::ApplyChatUpdate(MTPChatParticipants)` (`data/data_chat.cpp:545-636`).
                post_rpc = apply_chat_participants_update(session, &u.participants);
            }
            tl_enums::Update::ChatParticipantAdd(u) => {
                // Match `Data::ApplyChatUpdate(MTPDupdateChatParticipantAdd)`
                // (`data/data_chat.cpp:332-371`).
                let cid = ChatId::from_tl(u.chat_id);
                let uid = UserId::from_tl(u.user_id);
                if session.chat_loaded(cid) {
                    let user_loaded = session.user_loaded(uid);
                    let user_is_bot = session.user(uid).is_some_and(|usr| usr.is_bot());
                    let peer_id = PeerId::from_chat(cid);
                    let mut notify = false;
                    if let Some(chat) = session.chat_mut(cid) {
                        match chat.apply_update_version(u.version) {
                            ChatUpdateVersionStatus::TooOld => {}
                            ChatUpdateVersionStatus::Skipped => {
                                post_rpc = Some(PostUpdateRpc::ReloadFullChat { chat_id: cid });
                            }
                            ChatUpdateVersionStatus::Good if chat.count >= 0 => {
                                if !user_loaded
                                    || (!chat.participants.is_empty()
                                        && chat.participants.contains(&uid))
                                {
                                    chat.invalidate_participants();
                                    chat.count += 1;
                                } else if chat.participants.is_empty() {
                                    if chat.count > 0 {
                                        chat.count += 1;
                                    }
                                    chat.bot_status = BotStatus::Unknown;
                                    notify = true;
                                } else {
                                    chat.participants.insert(uid);
                                    chat.count += 1;
                                    if user_is_bot {
                                        chat.bot_status = BotStatus::HasBots;
                                    }
                                    notify = true;
                                }
                            }
                            ChatUpdateVersionStatus::Good => {}
                        }
                    }
                    if notify {
                        session
                            .changes()
                            .peer_updated(peer_id, PeerUpdateFlags::MEMBERS);
                    }
                }
            }
            tl_enums::Update::ChatParticipantDelete(u) => {
                // Match `Data::ApplyChatUpdate(MTPDupdateChatParticipantDelete)`
                // (`data/data_chat.cpp:373-413`).
                let cid = ChatId::from_tl(u.chat_id);
                let uid = UserId::from_tl(u.user_id);
                if session.chat_loaded(cid) {
                    let user_loaded = session.user_loaded(uid);
                    let is_self = uid == session.self_user_id();
                    let peer_id = PeerId::from_chat(cid);
                    let mut notify = false;
                    if let Some(chat) = session.chat_mut(cid) {
                        match chat.apply_update_version(u.version) {
                            ChatUpdateVersionStatus::TooOld => {}
                            ChatUpdateVersionStatus::Skipped => {
                                post_rpc = Some(PostUpdateRpc::ReloadFullChat { chat_id: cid });
                            }
                            ChatUpdateVersionStatus::Good if chat.count > 0 => {
                                if !user_loaded
                                    || (!chat.participants.is_empty()
                                        && !chat.participants.contains(&uid))
                                {
                                    chat.invalidate_participants();
                                    chat.count -= 1;
                                } else if chat.participants.is_empty() {
                                    chat.count -= 1;
                                    chat.bot_status = BotStatus::Unknown;
                                    notify = true;
                                } else {
                                    chat.participants.remove(&uid);
                                    chat.count -= 1;
                                    chat.admins.remove(&uid);
                                    chat.member_ranks.remove(&uid);
                                    if is_self {
                                        chat.admin_rights = ChatAdminRights::empty();
                                    }
                                    notify = true;
                                }
                            }
                            ChatUpdateVersionStatus::Good => {}
                        }
                    }
                    if notify {
                        session
                            .changes()
                            .peer_updated(peer_id, PeerUpdateFlags::MEMBERS);
                    }
                }
            }
            tl_enums::Update::PeerBlocked(u) => {
                // Match `Updates::applyUpdateNoPtsCheck(updatePeerBlocked)`
                // (`api/api_updates.cpp:2270-2275`).
                let peer_id = peer_id_from_tl(&u.peer_id);
                session.set_peer_blocked(peer_id, u.blocked);
            }
            tl_enums::Update::Privacy(u) => {
                // Match `Updates::applyUpdateNoPtsCheck(updatePrivacy)`
                // (`api/api_updates.cpp:2400-2429`) and `UserPrivacy::apply`
                // (`api/api_user_privacy.cpp:203-217`): parse directly when all chat/channel
                // exceptions are loaded, else reload via `account.getPrivacy`.
                if let Some(key) = privacy_key_from_tl(&u.key) {
                    if privacy_rules_all_chats_loaded(&u.rules, session) {
                        let rule = privacy_rules_from_tl(&u.rules, session);
                        self.events.push(UpdateEvent::PrivacyUpdated { key, rule });
                    } else {
                        post_rpc = Some(PostUpdateRpc::ReloadPrivacy { key });
                    }
                }
            }
            tl_enums::Update::User(u) => {
                // Match `Updates::applyUpdate(mtpc_updateUser)` (`api/api_updates.cpp:2179-2186`):
                // request full user refresh if already full-loaded.
                let uid = UserId::from_tl(u.user_id);
                if session
                    .user(uid)
                    .is_some_and(|user| user.common.loaded_status == LoadedStatus::Full)
                {
                    self.events
                        .push(UpdateEvent::UserFullRefreshNeeded { user_id: uid });
                }
            }
            tl_enums::Update::NewAuthorization(u) => {
                // Match `Updates::applyUpdate(mtpc_updateNewAuthorization)` (`api/api_updates.cpp:2366`).
                self.events.push(UpdateEvent::NewAuthorization(Box::new(u)));
            }
            tl_enums::Update::UserTyping(u) => {
                let peer_id = PeerId::from_user(UserId::from_tl(u.user_id));
                let top_msg_id = u.top_msg_id.map(|id| MsgId(i64::from(id)));
                session.set_typing_action(
                    peer_id,
                    peer_id,
                    top_msg_id,
                    typing_action_from_tl(&u.action),
                );
            }
            tl_enums::Update::ChatUserTyping(u) => {
                let peer_id = PeerId::from_chat(ChatId::from_tl(u.chat_id));
                let from_id = peer_id_from_tl(&u.from_id);
                session.set_typing_action(peer_id, from_id, None, typing_action_from_tl(&u.action));
            }
            tl_enums::Update::ChannelUserTyping(u) => {
                let peer_id = PeerId::from_channel(ChannelId::from_tl(u.channel_id));
                let from_id = peer_id_from_tl(&u.from_id);
                let top_msg_id = u.top_msg_id.map(|id| MsgId(i64::from(id)));
                session.set_typing_action(
                    peer_id,
                    from_id,
                    top_msg_id,
                    typing_action_from_tl(&u.action),
                );
            }
            tl_enums::Update::DialogPinned(u) => {
                // Match `Updates::applyUpdateNoPtsCheck(updateDialogPinned)`
                // (`api/api_updates.cpp:2478-2527`) and `History::applyPinnedUpdate`
                // (`history/history.cpp:2654-2664`).
                let folder_id = u.folder_id.unwrap_or(0);
                let archived = folder_id == ARCHIVE_FOLDER_ID;
                let done = match &u.peer {
                    tl_enums::DialogPeer::DialogPeer(d) => {
                        let peer_id = peer_id_from_tl(&d.peer);
                        if session.history(peer_id).is_some() {
                            session.set_pinned_in_folder(
                                DialogKey::History(peer_id),
                                archived,
                                u.pinned,
                            );
                            true
                        } else {
                            false
                        }
                    }
                    tl_enums::DialogPeer::Folder(f) => {
                        if folder_id != 0 {
                            false
                        } else if f.folder_id == ARCHIVE_FOLDER_ID {
                            session.set_pinned_in_folder(
                                DialogKey::Folder(f.folder_id),
                                false,
                                u.pinned,
                            );
                            true
                        } else {
                            false
                        }
                    }
                    tl_enums::DialogPeer::Community(c) => {
                        let ch_id = ChannelId::from_tl(c.community_id);
                        let peer_id = PeerId::from_channel(ch_id);
                        if session.channel_loaded(ch_id) && session.history(peer_id).is_some() {
                            session.set_pinned_in_folder(
                                DialogKey::History(peer_id),
                                archived,
                                u.pinned,
                            );
                            true
                        } else {
                            false
                        }
                    }
                };
                if !done {
                    post_rpc = Some(PostUpdateRpc::ReloadPinnedDialogs { folder_id });
                }
            }
            tl_enums::Update::PinnedDialogs(u) => {
                // Match `Updates::applyUpdateNoPtsCheck(updatePinnedDialogs)`
                // (`api/api_updates.cpp:2431-2476`) and `Session::applyPinnedChats`
                // (`data/data_session.cpp:2812-2836`):
                // when `order` is absent OR any peer in `order` is unloaded (or nested folder),
                // reload pinned dialogs via `messages.getPinnedDialogs(folder_id)`.
                let folder_id = u.folder_id.unwrap_or(0);
                let archived = folder_id == ARCHIVE_FOLDER_ID;
                let done = match u.order {
                    None => false,
                    Some(order_tl) => {
                        let all_valid = order_tl.iter().all(|peer| match peer {
                            tl_enums::DialogPeer::DialogPeer(d) => {
                                session.history(peer_id_from_tl(&d.peer)).is_some()
                            }
                            tl_enums::DialogPeer::Folder(f) => {
                                folder_id == 0 && f.folder_id == ARCHIVE_FOLDER_ID
                            }
                            tl_enums::DialogPeer::Community(c) => {
                                let ch_id = ChannelId::from_tl(c.community_id);
                                session.channel_loaded(ch_id)
                                    && session.history(PeerId::from_channel(ch_id)).is_some()
                            }
                        });
                        if all_valid {
                            let target_folder = (folder_id != 0).then_some(folder_id);
                            for peer in &order_tl {
                                match peer {
                                    tl_enums::DialogPeer::DialogPeer(d) => {
                                        let pid = peer_id_from_tl(&d.peer);
                                        if let Some(h) = session.history_mut(pid)
                                            && h.folder != Some(target_folder)
                                        {
                                            session.set_history_folder(pid, target_folder);
                                        }
                                    }
                                    tl_enums::DialogPeer::Community(c) => {
                                        let pid = PeerId::from_channel(ChannelId::from_tl(
                                            c.community_id,
                                        ));
                                        if let Some(h) = session.history_mut(pid)
                                            && h.folder != Some(None)
                                        {
                                            session.set_history_folder(pid, None);
                                        }
                                    }
                                    tl_enums::DialogPeer::Folder(_) => {}
                                }
                            }
                            let order: Vec<DialogKey> =
                                order_tl.iter().filter_map(dialog_key_from_tl).collect();
                            session.apply_pinned_list(0, archived, order);
                            true
                        } else {
                            false
                        }
                    }
                };
                if !done {
                    post_rpc = Some(PostUpdateRpc::ReloadPinnedDialogs { folder_id });
                }
            }
            tl_enums::Update::FolderPeers(u) => {
                // Match `Updates::applyUpdateNoPtsCheck(updateFolderPeers)`
                // (`api/api_updates.cpp:1407-1422`): only update loaded histories.
                for fp in u.folder_peers {
                    let tl_enums::FolderPeer::FolderPeer(f) = fp;
                    let peer_id = peer_id_from_tl(&f.peer);
                    if session.history(peer_id).is_some() {
                        session.set_history_folder(peer_id, Some(f.folder_id));
                    }
                }
            }
            tl_enums::Update::DialogUnreadMark(u) => {
                // Match `Updates::applyUpdateNoPtsCheck(updateDialogUnreadMark)`
                // (`api/api_updates.cpp:1903-1914`): handle only `DialogPeer::DialogPeer`
                // for already-loaded histories.
                if let tl_enums::DialogPeer::DialogPeer(d) = &u.peer {
                    let peer_id = peer_id_from_tl(&d.peer);
                    if session.history(peer_id).is_some() {
                        session.set_history_unread_mark(peer_id, u.unread);
                    }
                }
            }
            tl_enums::Update::DraftMessage(u) => {
                let peer_id = peer_id_from_tl(&u.peer);
                let top = MsgId(i64::from(u.top_msg_id.unwrap_or(0)));
                let saved = u
                    .saved_peer_id
                    .as_ref()
                    .map(peer_id_from_tl)
                    .unwrap_or(PeerId::ZERO);
                let key = DraftKey::cloud(top, saved);
                match draft_from_tl(&u.draft) {
                    Some(draft) => session.set_draft(peer_id, key, draft),
                    None => {
                        session.clear_draft(peer_id, key);
                    }
                }
            }
            tl_enums::Update::NotifySettings(u) => {
                // Match `NotifySettings::apply(MTPNotifyPeer, MTPPeerNotifySettings)`
                // (`data/notify/data_notify_settings.cpp:122-227, 369-382, 431-457`).
                let settings = peer_notify_settings_from_tl(&u.notify_settings);
                match &u.peer {
                    tl_enums::NotifyPeer::NotifyPeer(np) => {
                        let peer_id = peer_id_from_tl(&np.peer);
                        if let Some(peer) = session.peer_mut(peer_id) {
                            peer.common_mut().notify = settings;
                        }
                        let muted = self.effective_peer_muted(session, peer_id, now_secs);
                        let prev_muted = session.history(peer_id).map(|h| h.muted);
                        session.set_history_muted(peer_id, muted);
                        if prev_muted == Some(muted) {
                            session
                                .changes()
                                .peer_updated(peer_id, PeerUpdateFlags::NOTIFICATIONS);
                        }
                    }
                    tl_enums::NotifyPeer::NotifyCommunity(c) => {
                        let peer_id = PeerId::from_channel(ChannelId::from_tl(c.community_id));
                        if let Some(peer) = session.peer_mut(peer_id) {
                            peer.common_mut().notify = settings;
                        }
                        let muted = self.effective_peer_muted(session, peer_id, now_secs);
                        let prev_muted = session.history(peer_id).map(|h| h.muted);
                        session.set_history_muted(peer_id, muted);
                        if prev_muted == Some(muted) {
                            session
                                .changes()
                                .peer_updated(peer_id, PeerUpdateFlags::NOTIFICATIONS);
                        }
                    }
                    tl_enums::NotifyPeer::NotifyUsers
                    | tl_enums::NotifyPeer::NotifyChats
                    | tl_enums::NotifyPeer::NotifyBroadcasts => {
                        let scope = match &u.peer {
                            tl_enums::NotifyPeer::NotifyUsers => DefaultNotifyScope::Users,
                            tl_enums::NotifyPeer::NotifyChats => DefaultNotifyScope::Chats,
                            _ => DefaultNotifyScope::Broadcasts,
                        };
                        self.set_default_notify_settings(scope, settings);
                        let peer_ids: Vec<PeerId> = session
                            .main_dialogs()
                            .iter()
                            .chain(session.archive_dialogs().iter())
                            .filter_map(|k| match k {
                                DialogKey::History(pid) => Some(*pid),
                                _ => None,
                            })
                            .collect();
                        for pid in peer_ids {
                            let inherits = session.peer(pid).is_some_and(|p| {
                                DefaultNotifyScope::for_peer(p) == scope
                                    && p.common().notify.mute_until.is_none()
                            });
                            if inherits {
                                let muted = self.effective_peer_muted(session, pid, now_secs);
                                session.set_history_muted(pid, muted);
                            }
                        }
                    }
                    tl_enums::NotifyPeer::NotifyForumTopic(nft) => {
                        // Match `api/api_updates.cpp:2482` (`notifyForumTopic`).
                        let peer_id = peer_id_from_tl(&nft.peer);
                        let root_id = MsgId(i64::from(nft.top_msg_id));
                        let muted = settings.is_muted(now_secs);
                        if let Some(topic) = session
                            .forum_mut(peer_id)
                            .and_then(|f| f.topic_mut(root_id))
                        {
                            topic.notify = settings;
                            topic.muted = muted;
                            session.changes().topic_updated(
                                peer_id,
                                root_id,
                                TopicUpdateFlags::NOTIFICATIONS,
                            );
                        }
                    }
                }
                self.events.push(UpdateEvent::NotifySettingsUpdated {
                    peer: u.peer,
                    settings: Box::new(u.notify_settings),
                });
            }
            tl_enums::Update::PinnedMessages(u) => {
                let peer_id = peer_id_from_tl(&u.peer);
                for id in u.messages {
                    session.set_message_pinned(
                        FullMsgId::new(peer_id, MsgId(i64::from(id))),
                        u.pinned,
                    );
                }
            }
            tl_enums::Update::PinnedChannelMessages(u) => {
                let peer_id = PeerId::from_channel(ChannelId::from_tl(u.channel_id));
                for id in u.messages {
                    session.set_message_pinned(
                        FullMsgId::new(peer_id, MsgId(i64::from(id))),
                        u.pinned,
                    );
                }
            }
            tl_enums::Update::ChannelMessageViews(u) => {
                let peer_id = PeerId::from_channel(ChannelId::from_tl(u.channel_id));
                session
                    .update_message_views(FullMsgId::new(peer_id, MsgId(i64::from(u.id))), u.views);
            }
            tl_enums::Update::ChannelMessageForwards(u) => {
                let peer_id = PeerId::from_channel(ChannelId::from_tl(u.channel_id));
                session.update_message_forwards(
                    FullMsgId::new(peer_id, MsgId(i64::from(u.id))),
                    u.forwards,
                );
            }
            tl_enums::Update::MessageReactions(u) => {
                let peer_id = peer_id_from_tl(&u.peer);
                session.update_message_reactions(
                    FullMsgId::new(peer_id, MsgId(i64::from(u.msg_id))),
                    reactions_from_tl(&u.reactions),
                );
            }
            tl_enums::Update::DialogFilter(u) => {
                let self_id = Some(session.self_user_id());
                match u
                    .filter
                    .as_ref()
                    .and_then(|f| dialog_filter_from_tl(f, self_id))
                {
                    Some(filter) => session.set_filter(filter),
                    None => {
                        session.remove_filter(u.id);
                    }
                }
            }
            tl_enums::Update::DialogFilters => {
                // Match `ChatFilters::apply(MTPDupdateDialogFilters)`
                // (`data/data_chat_filters.cpp:534-535`): reload all dialog filters.
                post_rpc = Some(PostUpdateRpc::ReloadDialogFilters);
            }
            tl_enums::Update::DialogFilterOrder(u) => {
                // Match `ChatFilters::apply(MTPDupdateDialogFilterOrder)`
                // (`data/data_chat_filters.cpp:536-541`): reload all filters if the order
                // vector does not match the loaded filter IDs.
                if !session.apply_filter_order_checked(&u.order) {
                    post_rpc = Some(PostUpdateRpc::ReloadDialogFilters);
                }
            }
            tl_enums::Update::ServiceNotification(u) => {
                if u.r#type.starts_with("AUTH_KEY_DROP_") {
                    self.events.push(UpdateEvent::ForceLogout {
                        notification_type: u.r#type,
                    });
                }
            }
            tl_enums::Update::LangPack(u) => {
                self.events.push(UpdateEvent::LangPackDifference(
                    crate::lang::lang_pack_difference_from_tl(&u.difference),
                ));
            }
            tl_enums::Update::LangPackTooLong(u) => {
                self.events.push(UpdateEvent::LangPackTooLong {
                    lang_code: u.lang_code,
                });
            }
            tl_enums::Update::LoginToken => {
                self.events.push(UpdateEvent::LoginToken);
            }
            // Forum topic updates (`api/api_updates.cpp:2592-2723`).
            tl_enums::Update::PinnedForumTopic(u) => {
                let peer_id = peer_id_from_tl(&u.peer);
                let root_id = MsgId(i64::from(u.topic_id));
                let updated = session
                    .forum_mut(peer_id)
                    .is_some_and(|f| f.set_topic_pinned(root_id, u.pinned));
                if !updated && session.peer(peer_id).is_some() {
                    session.ensure_forum(peer_id).stale_root_ids.insert(root_id);
                    self.events
                        .push(UpdateEvent::ForumTopicReloadNeeded { peer_id, root_id });
                }
            }
            tl_enums::Update::PinnedForumTopics(u) => {
                let peer_id = peer_id_from_tl(&u.peer);
                let order: Option<Vec<MsgId>> = u
                    .order
                    .map(|v| v.into_iter().map(|id| MsgId(i64::from(id))).collect());
                let applied = session
                    .forum_mut(peer_id)
                    .is_some_and(|f| f.apply_pinned_order(order.as_deref()));
                if !applied && session.peer(peer_id).is_some() {
                    if let Some(forum) = session.forum_mut(peer_id) {
                        forum.begin_reload();
                    }
                    self.events
                        .push(UpdateEvent::ForumTopicsReloadNeeded { peer_id });
                }
            }
            tl_enums::Update::EditForumTopic(u) => {
                let peer_id = peer_id_from_tl(&u.peer);
                let root_id = MsgId(i64::from(u.topic_id));
                let _ = session.edit_forum_topic(
                    peer_id,
                    root_id,
                    u.title,
                    u.icon_emoji_id,
                    u.closed,
                    u.hidden,
                );
            }
            tl_enums::Update::ReadChannelDiscussionInbox(u) => {
                let peer_id = PeerId::from_channel(ChannelId::from_tl(u.channel_id));
                let root_id = MsgId(i64::from(u.top_msg_id));
                let read_max_id = MsgId(i64::from(u.read_max_id));
                let _ = session.mark_topic_inbox_read(peer_id, root_id, read_max_id, None);
            }
            tl_enums::Update::ReadChannelDiscussionOutbox(u) => {
                let peer_id = PeerId::from_channel(ChannelId::from_tl(u.channel_id));
                let root_id = MsgId(i64::from(u.top_msg_id));
                let read_max_id = MsgId(i64::from(u.read_max_id));
                let _ = session.mark_topic_outbox_read(peer_id, root_id, read_max_id);
            }
            // Monoforum & Saved Messages updates (`api/api_updates.cpp:2529-2537, 2624-2648, 2879-2881`).
            tl_enums::Update::ReadMonoForumInbox(u) => {
                let ch_id = ChannelId::from_tl(u.channel_id);
                let saved_peer = peer_id_from_tl(&u.saved_peer_id);
                let read_max_id = MsgId(i64::from(u.read_max_id));
                session
                    .ensure_monoforum(ch_id)
                    .ensure_sublist(saved_peer)
                    .apply_inbox_read_till(read_max_id, None);
            }
            tl_enums::Update::ReadMonoForumOutbox(u) => {
                let ch_id = ChannelId::from_tl(u.channel_id);
                let saved_peer = peer_id_from_tl(&u.saved_peer_id);
                let read_max_id = MsgId(i64::from(u.read_max_id));
                session
                    .ensure_monoforum(ch_id)
                    .ensure_sublist(saved_peer)
                    .apply_outbox_read_till(read_max_id);
            }
            tl_enums::Update::SavedDialogPinned(u) => {
                if let tl_enums::DialogPeer::DialogPeer(dp) = u.peer {
                    let peer_id = peer_id_from_tl(&dp.peer);
                    if !session
                        .saved_messages_mut()
                        .set_sublist_pinned(peer_id, u.pinned)
                    {
                        self.events.push(UpdateEvent::SavedPinnedReloadNeeded);
                    }
                }
            }
            tl_enums::Update::PinnedSavedDialogs(u) => {
                let order: Option<Vec<PeerId>> = u.order.map(|list| {
                    list.into_iter()
                        .filter_map(|dp| match dp {
                            tl_enums::DialogPeer::DialogPeer(p) => Some(peer_id_from_tl(&p.peer)),
                            _ => None,
                        })
                        .collect()
                });
                if !session
                    .saved_messages_mut()
                    .apply_pinned_order(order.as_deref())
                {
                    self.events.push(UpdateEvent::SavedPinnedReloadNeeded);
                }
            }
            tl_enums::Update::SavedReactionTags => {
                session.saved_reaction_tags_mut().mark_invalidated();
                self.events.push(UpdateEvent::SavedReactionTagsInvalidated);
                self.events.push(UpdateEvent::SavedReactionTags);
            }
            // Scheduled messages updates (`api/api_updates.cpp:1950-1958`).
            tl_enums::Update::NewScheduledMessage(u) => {
                if let Some(msg) = message_from_tl(&u.message, session.self_user_id()) {
                    let peer_id = msg.peer_id;
                    let _ = session
                        .scheduled_messages_mut()
                        .upsert_server_message(peer_id, msg);
                }
            }
            tl_enums::Update::DeleteScheduledMessages(u) => {
                let peer_id = peer_id_from_tl(&u.peer);
                let remote_ids: Vec<MsgId> = u
                    .messages
                    .into_iter()
                    .map(|id| MsgId(i64::from(id)))
                    .collect();
                let sent_ids: Option<Vec<MsgId>> = u
                    .sent_messages
                    .map(|v| v.into_iter().map(|id| MsgId(i64::from(id))).collect());
                let deleted = session.scheduled_messages_mut().apply_delete_scheduled(
                    peer_id,
                    &remote_ids,
                    sent_ids.as_deref(),
                );
                let sent_items: Vec<DeletedScheduledInfo> = deleted
                    .into_iter()
                    .filter(|d| d.sent_id.is_some())
                    .collect();
                if !sent_items.is_empty() {
                    self.events.push(UpdateEvent::ScheduledMessagesSent {
                        peer_id,
                        items: sent_items,
                    });
                }
            }
            // Business quick-reply updates (`api/api_updates.cpp:1975-1998`).
            tl_enums::Update::QuickReplies(u) => {
                let shortcuts = u
                    .quick_replies
                    .iter()
                    .map(crate::scheduled::quick_reply_from_tl)
                    .collect();
                let changes = session.shortcut_messages_mut().update_shortcuts(shortcuts);
                for change in changes {
                    self.events.push(UpdateEvent::ShortcutIdChanged(change));
                }
                if session.shortcut_messages().has_unknown_top_messages() {
                    session.shortcut_messages_mut().mark_shortcuts_unloaded();
                    self.events.push(UpdateEvent::QuickRepliesReloadNeeded);
                }
                self.events.push(UpdateEvent::QuickReplies(u.quick_replies));
            }
            tl_enums::Update::NewQuickReply(u) => {
                let shortcut = crate::scheduled::quick_reply_from_tl(&u.quick_reply);
                if let Some(change) = session
                    .shortcut_messages_mut()
                    .apply_new_quick_reply(shortcut)
                {
                    self.events.push(UpdateEvent::ShortcutIdChanged(change));
                }
            }
            tl_enums::Update::DeleteQuickReply(u) => {
                if let Some(change) = session
                    .shortcut_messages_mut()
                    .apply_delete_quick_reply(u.shortcut_id)
                {
                    self.events.push(UpdateEvent::ShortcutIdChanged(change));
                }
            }
            tl_enums::Update::QuickReplyMessage(u) => {
                if let Some(msg) = message_from_tl(&u.message, session.self_user_id())
                    && let Some(shortcut_id) = msg.shortcut_id
                    && let Some((_, needed_initial_load)) = session
                        .shortcut_messages_mut()
                        .upsert_server_message(shortcut_id, msg)
                    && needed_initial_load
                {
                    self.events
                        .push(UpdateEvent::QuickReplyMessagesReloadNeeded { shortcut_id });
                }
            }
            tl_enums::Update::DeleteQuickReplyMessages(u) => {
                let remote_ids: Vec<MsgId> = u
                    .messages
                    .into_iter()
                    .map(|id| MsgId(i64::from(id)))
                    .collect();
                let _ = session
                    .shortcut_messages_mut()
                    .apply_delete_quick_reply_messages(u.shortcut_id, &remote_ids);
                if u.shortcut_id != 0 {
                    self.events
                        .push(UpdateEvent::QuickReplyMessagesReloadNeeded {
                            shortcut_id: u.shortcut_id,
                        });
                }
            }
            // Match `Updates::applyUpdate` (`api/api_updates.cpp:2258-2268`):
            // forward all 1:1 and group call updates instead of dropping them.
            update @ (tl_enums::Update::PhoneCall(_)
            | tl_enums::Update::PhoneCallSignalingData(_)
            | tl_enums::Update::GroupCallParticipants(_)
            | tl_enums::Update::GroupCallChainBlocks(_)
            | tl_enums::Update::GroupCallConnection(_)
            | tl_enums::Update::GroupCall(_)
            | tl_enums::Update::GroupCallMessage(_)
            | tl_enums::Update::GroupCallEncryptedMessage(_)
            | tl_enums::Update::DeleteGroupCallMessages(_)) => {
                self.events.push(UpdateEvent::CallUpdate(Box::new(update)));
            }
            // Match `Updates::applyUpdate` (`api/api_updates.cpp:2947-2958`):
            // forward story updates (both live and from `updates.getDifference` /
            // `updates.getChannelDifference`) so story stores never miss them.
            update @ (tl_enums::Update::Story(_)
            | tl_enums::Update::ReadStories(_)
            | tl_enums::Update::StoriesStealthMode(_)
            | tl_enums::Update::StoryID(_)) => {
                self.events.push(UpdateEvent::StoryUpdate(Box::new(update)));
            }
            // Match `Updates::applyUpdate` (`api/api_updates.cpp:2352-2363`):
            // apply `updatePendingJoinRequests` onto Chat or Channel pending request state.
            tl_enums::Update::PendingJoinRequests(u) => {
                let peer_id = peer_id_from_tl(&u.peer);
                let requesters: Vec<UserId> = u
                    .recent_requesters
                    .iter()
                    .copied()
                    .map(UserId::from_tl)
                    .collect();
                if let Some(chat_id) = peer_id.to_chat() {
                    if let Some(mut chat) = session.chat(chat_id).cloned() {
                        chat.pending_requests_count = u.requests_pending;
                        chat.recent_requesters = requesters;
                        session.upsert_chat(chat, PeerUpdateFlags::FULL_INFO);
                    }
                } else if let Some(channel_id) = peer_id.to_channel()
                    && let Some(mut ch) = session.channel(channel_id).cloned()
                {
                    ch.pending_requests_count = u.requests_pending;
                    ch.recent_requesters = requesters;
                    session.upsert_channel(ch, PeerUpdateFlags::FULL_INFO);
                }
            }
            // Match `Api::Updates::applyUpdateNoPtsCheck` (`api/api_updates.cpp:2740-2887`):
            // propagate sticker, emoji, reaction, GIF, config, attach-menu, ringtone,
            // and theme cache invalidation/mutation updates instead of dropping them.
            tl_enums::Update::StickerSets(u) => {
                self.events.push(UpdateEvent::StickerSets {
                    masks: u.masks,
                    emojis: u.emojis,
                });
            }
            tl_enums::Update::StickerSetsOrder(u) => {
                self.events.push(UpdateEvent::StickerSetsOrder {
                    masks: u.masks,
                    emojis: u.emojis,
                    order: u.order.into_iter().map(|id| id as u64).collect(),
                });
            }
            tl_enums::Update::NewStickerSet(u) => {
                self.events
                    .push(UpdateEvent::NewStickerSet(Box::new(u.stickerset)));
            }
            tl_enums::Update::RecentStickers => {
                self.events.push(UpdateEvent::RecentStickers);
            }
            tl_enums::Update::FavedStickers => {
                self.events.push(UpdateEvent::FavedStickers);
            }
            tl_enums::Update::ReadFeaturedStickers => {
                self.events.push(UpdateEvent::ReadFeaturedStickers);
            }
            tl_enums::Update::SavedGifs => {
                self.events.push(UpdateEvent::SavedGifs);
            }
            tl_enums::Update::RecentReactions => {
                self.events.push(UpdateEvent::RecentReactions);
            }
            tl_enums::Update::RecentEmojiStatuses => {
                self.events.push(UpdateEvent::RecentEmojiStatuses);
            }
            tl_enums::Update::MoveStickerSetToTop(u) => {
                self.events.push(UpdateEvent::MoveStickerSetToTop {
                    masks: u.masks,
                    emojis: u.emojis,
                    stickerset: u.stickerset as u64,
                });
            }
            tl_enums::Update::Config => {
                // Match `api/api_updates.cpp:2208-2212` (`mtpc_updateConfig`).
                self.events.push(UpdateEvent::Config);
            }
            tl_enums::Update::AttachMenuBots => {
                // Match `api/api_updates.cpp:2323-2325` (`mtpc_updateAttachMenuBots`).
                self.events.push(UpdateEvent::AttachMenuBots);
            }
            tl_enums::Update::SavedRingtones => {
                // Match `api/api_updates.cpp:2934-2936` (`mtpc_updateSavedRingtones`).
                self.events.push(UpdateEvent::SavedRingtones);
            }
            tl_enums::Update::Theme(u) => {
                // Match `api/api_updates.cpp:2929-2932` (`mtpc_updateTheme`).
                self.events.push(UpdateEvent::Theme(Box::new(u.theme)));
            }
            _ => {}
        }
        post_rpc
    }

    async fn execute_post_update_rpcs<I: Invoker>(
        &mut self,
        invoker: &I,
        session: &mut Session,
        rpcs: Vec<PostUpdateRpc>,
        now_ms: u64,
    ) {
        let now_secs = (now_ms / 1_000) as TimeId;
        for rpc in rpcs {
            match rpc {
                PostUpdateRpc::ReloadPinnedDialogs { folder_id } => {
                    match invoker
                        .messages_get_pinned_dialogs(tl_fns::messages::GetPinnedDialogs {
                            folder_id,
                        })
                        .await
                    {
                        Ok(pd) => {
                            let req_folder = (folder_id != 0).then_some(folder_id);
                            apply_tl_peer_dialogs_for_folder(
                                session, &pd, req_folder, true, now_secs,
                            );
                        }
                        Err(err) => {
                            warn!(folder_id, error = %err, "messages.getPinnedDialogs after update failed");
                        }
                    }
                }
                PostUpdateRpc::ReloadDialogFilters => {
                    match invoker.messages_get_dialog_filters().await {
                        Ok(resp) => {
                            let self_id = Some(session.self_user_id());
                            let parsed: Vec<_> = resp
                                .filters
                                .iter()
                                .filter_map(|f| dialog_filter_from_tl(f, self_id))
                                .collect();
                            session.sync_filters_from_list(parsed, resp.tags_enabled);
                        }
                        Err(err) => {
                            warn!(error = %err, "messages.getDialogFilters after update failed");
                        }
                    }
                }
                PostUpdateRpc::RefetchChannel { channel_id } => {
                    if session
                        .channel(channel_id)
                        .is_some_and(|ch| ch.is_forbidden())
                    {
                        continue;
                    }
                    let input = input_channel_for(session, channel_id);
                    match invoker
                        .channels_get_channels(tl_fns::channels::GetChannels { id: vec![input] })
                        .await
                    {
                        Ok(chats_resp) => {
                            let chats = match chats_resp {
                                tl_enums::messages::Chats::Chats(c) => c.chats,
                                tl_enums::messages::Chats::Slice(c) => c.chats,
                            };
                            apply_tl_users_and_chats(session, &[], &chats);
                        }
                        Err(err) => {
                            // Match `Updates::applyUpdateNoPtsCheck(mtpc_updateChannel)`
                            // (`api/api_updates.cpp:2539-2562`) and `ChatParticipants::requestSelf`
                            // (`api/api_chat_participants.cpp:748`): a `CHANNEL_PRIVATE` /
                            // `CHANNEL_INVALID` response means the channel is no longer accessible
                            // (deleted or kicked), so mark it forbidden and stop short-polling it.
                            let is_inaccessible = matches!(
                                &err,
                                InvocationError::Rpc(r)
                                    if r.name == "CHANNEL_PRIVATE" || r.name == "CHANNEL_INVALID"
                            );
                            if is_inaccessible {
                                if let Some(ch) = session.channel_mut(channel_id) {
                                    ch.flags.insert(ChannelFlags::FORBIDDEN);
                                }
                                self.machine.remove_channel(channel_id);
                            } else {
                                warn!(channel_id = channel_id.0, error = %err, "channels.getChannels after updateChannel failed");
                            }
                        }
                    }
                }
                PostUpdateRpc::ReloadFullChat { chat_id } => {
                    match invoker
                        .messages_get_full_chat(tl_fns::messages::GetFullChat {
                            chat_id: chat_id.to_tl(),
                        })
                        .await
                    {
                        Ok(full) => {
                            apply_tl_users_and_chats(session, &full.users, &full.chats);
                            apply_tl_chat_full(session, &full.full_chat);
                        }
                        Err(err) => {
                            warn!(chat_id = chat_id.0, error = %err, "messages.getFullChat after version skip failed");
                        }
                    }
                }
                PostUpdateRpc::ReloadPrivacy { key } => {
                    match invoker
                        .account_get_privacy(tl_fns::account::GetPrivacy {
                            key: privacy_key_to_tl(key),
                        })
                        .await
                    {
                        Ok(resp) => {
                            apply_tl_users_and_chats(session, &resp.users, &resp.chats);
                            let rule = privacy_rules_from_tl(&resp.rules, session);
                            self.events.push(UpdateEvent::PrivacyUpdated { key, rule });
                        }
                        Err(err) => {
                            warn!(?key, error = %err, "account.getPrivacy after updatePrivacy failed");
                        }
                    }
                }
            }
        }
    }

    async fn execute_get_difference<I: Invoker>(
        &mut self,
        invoker: &I,
        session: &mut Session,
        mut pts: i32,
        mut date: TimeId,
        mut qts: i32,
        now_ms: u64,
    ) -> Result<(), ApiError> {
        loop {
            let req = tl_fns::updates::GetDifference {
                pts,
                pts_limit: None,
                pts_total_limit: None,
                date,
                qts,
                qts_limit: None,
            };
            let diff = match invoker.updates_get_difference(req).await {
                Ok(d) => d,
                Err(err) => {
                    self.machine.on_difference_fail(now_ms);
                    return Err(ApiError::Invocation(err));
                }
            };

            let mut follow_up_actions = Vec::new();
            let mut post_rpcs = Vec::new();
            let actions = match diff {
                tl_enums::updates::Difference::Empty(e) => {
                    self.machine.on_difference_empty(e.date, e.seq, now_ms)
                }
                tl_enums::updates::Difference::Difference(d) => {
                    let tl_enums::updates::State::State(st) = d.state;
                    apply_tl_users_and_chats(session, &d.users, &d.chats);
                    let (fu, rpcs) = self.feed_difference_payload(
                        session,
                        &d.new_messages,
                        d.other_updates,
                        false,
                        now_ms,
                    );
                    follow_up_actions = fu;
                    post_rpcs = rpcs;
                    self.machine.on_difference_final(
                        st.pts,
                        st.date,
                        st.qts,
                        st.seq,
                        Vec::new(),
                        now_ms,
                    )
                }
                tl_enums::updates::Difference::Slice(s) => {
                    let tl_enums::updates::State::State(st) = s.intermediate_state;
                    apply_tl_users_and_chats(session, &s.users, &s.chats);
                    let (fu, rpcs) = self.feed_difference_payload(
                        session,
                        &s.new_messages,
                        s.other_updates,
                        false,
                        now_ms,
                    );
                    follow_up_actions = fu;
                    post_rpcs = rpcs;
                    self.machine.on_difference_slice(
                        st.pts,
                        st.date,
                        st.qts,
                        st.seq,
                        Vec::new(),
                        now_ms,
                    )
                }
                tl_enums::updates::Difference::TooLong(t) => {
                    warn!(
                        pts = t.pts,
                        "updates.differenceTooLong received; resetting local pts"
                    );
                    self.machine.on_difference_too_long(t.pts, now_ms)
                }
            };

            let mut next_diff = None;
            let mut deferred = follow_up_actions;
            for action in actions {
                if let UpdateAction::RequestDifference {
                    pts: np,
                    date: nd,
                    qts: nq,
                } = action
                {
                    next_diff = Some((np, nd, nq));
                } else {
                    deferred.push(action);
                }
            }
            if !post_rpcs.is_empty() {
                self.execute_post_update_rpcs(invoker, session, post_rpcs, now_ms)
                    .await;
            }
            Box::pin(self.process_actions(invoker, session, deferred, now_ms)).await?;

            if let Some((np, nd, nq)) = next_diff {
                pts = np;
                date = nd;
                qts = nq;
            } else {
                return Ok(());
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn execute_get_channel_difference<I: Invoker>(
        &mut self,
        invoker: &I,
        session: &mut Session,
        channel_id: ChannelId,
        mut pts: i32,
        mut force: bool,
        mut limit: i32,
        now_ms: u64,
    ) -> Result<(), ApiError> {
        let now_secs = (now_ms / 1_000) as TimeId;
        loop {
            if session
                .channel(channel_id)
                .is_some_and(|ch| ch.is_forbidden())
            {
                self.machine.remove_channel(channel_id);
                return Ok(());
            }
            let channel = input_channel_for(session, channel_id);
            let req = tl_fns::updates::GetChannelDifference {
                force,
                channel,
                filter: tl_enums::ChannelMessagesFilter::Empty,
                pts,
                limit,
            };
            let diff = match invoker.updates_get_channel_difference(req).await {
                Ok(d) => d,
                Err(err) => {
                    // Match `Updates::channelDifferenceFail` (`api/api_updates.cpp:470-478`) and
                    // `ChatParticipants::requestSelf` (`api/api_chat_participants.cpp:748`):
                    // permanent `CHANNEL_PRIVATE` / `CHANNEL_INVALID` errors indicate a deleted
                    // or forbidden channel; mark it forbidden and stop polling instead of retrying
                    // in an infinite backoff loop.
                    let is_inaccessible = matches!(
                        &err,
                        InvocationError::Rpc(r)
                            if r.name == "CHANNEL_PRIVATE" || r.name == "CHANNEL_INVALID"
                    );
                    if is_inaccessible {
                        if let Some(ch) = session.channel_mut(channel_id) {
                            ch.flags.insert(ChannelFlags::FORBIDDEN);
                        }
                        self.machine.remove_channel(channel_id);
                        return Ok(());
                    }
                    self.machine.on_channel_difference_fail(channel_id, now_ms);
                    return Err(ApiError::Invocation(err));
                }
            };

            let mut post_rpcs = Vec::new();
            let actions = match diff {
                tl_enums::updates::ChannelDifference::Empty(e) => self
                    .machine
                    .on_channel_difference_empty(channel_id, e.pts, e.r#final, e.timeout, now_ms),
                tl_enums::updates::ChannelDifference::ChannelDifference(d) => {
                    apply_tl_users_and_chats(session, &d.users, &d.chats);
                    let (_, rpcs) = self.feed_difference_payload(
                        session,
                        &d.new_messages,
                        d.other_updates,
                        true,
                        now_ms,
                    );
                    post_rpcs = rpcs;
                    self.machine.on_channel_difference(
                        channel_id,
                        d.pts,
                        d.r#final,
                        d.timeout,
                        Vec::new(),
                        now_ms,
                    )
                }
                tl_enums::updates::ChannelDifference::TooLong(t) => {
                    apply_tl_users_and_chats(session, &t.users, &t.chats);
                    let peer_id = PeerId::from_channel(channel_id);
                    // Capture loaded history range and prior `pts` BEFORE invalidating history
                    // bottom and applying snapshot messages so we can run range validation
                    // (`Updates::channelDifferenceDone` + `requestChannelRangeDifference` in
                    // `api/api_updates.cpp:409-431, 797-890`).
                    let old_pts = pts;
                    let range = session
                        .history(peer_id)
                        .map(|h| h.range_for_difference_request())
                        .unwrap_or_default();
                    // Invalidate history bottom BEFORE applying the latest snapshot messages
                    // so prior loaded slices do not falsely merge across the skipped gap
                    // (`api_updates.cpp:414`).
                    session.invalidate_history_bottom(peer_id);
                    let dialog_pts = match &t.dialog {
                        tl_enums::Dialog::Dialog(d) => d.pts,
                        _ => None,
                    };
                    for msg in &t.messages {
                        apply_tl_message(session, msg);
                    }
                    apply_tl_dialog(session, &t.dialog, now_secs);
                    let actions = self.machine.on_channel_difference_too_long(
                        channel_id, dialog_pts, t.r#final, t.timeout, now_ms,
                    );
                    if old_pts > 0 && range.from < range.till {
                        // Match `Updates::channelRangeDifferenceSend` (`api/api_updates.cpp:817-890`):
                        // request edits/deletions for already-loaded messages in `[from, till - 1]`.
                        self.execute_channel_range_difference(
                            invoker, session, channel_id, old_pts, range.from, range.till, now_ms,
                        )
                        .await;
                    }
                    actions
                }
            };

            if !post_rpcs.is_empty() {
                self.execute_post_update_rpcs(invoker, session, post_rpcs, now_ms)
                    .await;
            }

            let mut next_channel_diff = None;
            let mut deferred = Vec::new();
            for action in actions {
                if let UpdateAction::RequestChannelDifference {
                    channel_id: cid,
                    pts: np,
                    force: nf,
                    limit: nl,
                } = action
                    && cid == channel_id
                {
                    next_channel_diff = Some((np, nf, nl));
                } else {
                    deferred.push(action);
                }
            }
            Box::pin(self.process_actions(invoker, session, deferred, now_ms)).await?;

            if let Some((np, nf, nl)) = next_channel_diff {
                pts = np;
                force = nf;
                limit = nl;
            } else {
                return Ok(());
            }
        }
    }

    /// Executes tdesktop's `requestChannelRangeDifference` / `channelRangeDifferenceSend`
    /// (`api/api_updates.cpp:797-890`) after `updates.channelDifferenceTooLong` so edits and
    /// deletions to already-loaded messages in `[from, till)` are reconciled without touching
    /// the channel's main `pts` watermark.
    #[allow(clippy::too_many_arguments)]
    async fn execute_channel_range_difference<I: Invoker>(
        &mut self,
        invoker: &I,
        session: &mut Session,
        channel_id: ChannelId,
        mut range_pts: i32,
        from: MsgId,
        till: MsgId,
        now_ms: u64,
    ) {
        let max_id = (till.0 - 1).clamp(1, i64::from(i32::MAX)) as i32;
        let min_id = from.0.clamp(1, i64::from(max_id)) as i32;
        let limit = (till.0 - from.0).clamp(1, i64::from(i32::MAX)) as i32;
        // Safety cap on non-final slices to prevent infinite loops on misbehaving servers.
        const MAX_RANGE_SLICES: usize = 32;
        for _ in 0..MAX_RANGE_SLICES {
            let channel = input_channel_for(session, channel_id);
            let req = tl_fns::updates::GetChannelDifference {
                force: true,
                channel,
                filter: tl_enums::ChannelMessagesFilter::ChannelMessagesFilter(
                    tl_types::ChannelMessagesFilter {
                        exclude_new_messages: false,
                        ranges: vec![tl_enums::MessageRange::MessageRange(
                            tl_types::MessageRange { min_id, max_id },
                        )],
                    },
                ),
                pts: range_pts,
                limit,
            };
            let diff = match invoker.updates_get_channel_difference(req).await {
                Ok(d) => d,
                Err(err) => {
                    warn!(
                        channel_id = channel_id.0,
                        error = %err,
                        "channel range difference request failed"
                    );
                    return;
                }
            };
            let (is_final, next_pts) = match diff {
                tl_enums::updates::ChannelDifference::Empty(e) => (e.r#final, e.pts),
                tl_enums::updates::ChannelDifference::ChannelDifference(d) => {
                    apply_tl_users_and_chats(session, &d.users, &d.chats);
                    let _ = self.feed_difference_payload(
                        session,
                        &d.new_messages,
                        d.other_updates,
                        true,
                        now_ms,
                    );
                    (d.r#final, d.pts)
                }
                tl_enums::updates::ChannelDifference::TooLong(_) => {
                    // Range difference too long: drop loaded messages in this range
                    // (matching `api/api_updates.cpp:873-880`).
                    return;
                }
            };
            if is_final || next_pts <= 0 || next_pts == range_pts {
                return;
            }
            range_pts = next_pts;
        }
    }

    fn feed_difference_payload(
        &mut self,
        session: &mut Session,
        new_messages: &[tl_enums::Message],
        other_updates: Vec<tl_enums::Update>,
        handling_channel_difference: bool,
        now_ms: u64,
    ) -> (Vec<UpdateAction<UpdatePayload>>, Vec<PostUpdateRpc>) {
        // Matches `Updates::feedDifference` / `feedChannelDifference` exact ordering:
        // 1. UpdateMessageID first
        for u in &other_updates {
            if let tl_enums::Update::MessageID(m) = u {
                self.apply_message_id(session, m.random_id, MsgId(i64::from(m.id)));
            }
        }
        // 2. new_messages
        for msg in new_messages {
            apply_tl_message(session, msg);
        }
        // 3. other_updates (skipping MessageID; common PTS updates bypass common PtsWaiter,
        //    while channel updates & ChannelTooLong inside common getDifference still update
        //    per-channel PTS state per `api_updates.cpp:1676-1684,2564-2572`).
        let mut follow_ups = Vec::new();
        let mut post_rpcs = Vec::new();
        for u in other_updates {
            if matches!(u, tl_enums::Update::MessageID(_)) {
                continue;
            }
            if !handling_channel_difference {
                if let tl_enums::Update::ChannelTooLong(ctl) = u {
                    follow_ups.extend(self.machine.on_channel_too_long(
                        ChannelId::from_tl(ctl.channel_id),
                        ctl.pts,
                        now_ms,
                    ));
                    continue;
                }
                if let Some((channel_id, ch_pts, ch_pts_count)) = channel_pts_of(&u)
                    && (self.machine.channel_pts(channel_id).is_some()
                        || session.channel_loaded(channel_id))
                {
                    // Match `api/api_updates.cpp:1676-1684`: route channel updates arriving
                    // inside common `getDifference` through per-channel `PtsWaiter` whenever
                    // the channel is loaded (initializing its PTS if needed).
                    let ch_actions = self.machine.on_channel_update_from_difference(
                        ChannelUpdateEnvelope {
                            channel_id,
                            pts: ch_pts,
                            pts_count: ch_pts_count,
                            payload: UpdatePayload::Single(Box::new(u)),
                        },
                        now_ms,
                    );
                    for act in ch_actions {
                        match act {
                            UpdateAction::ApplyChannel {
                                payload: UpdatePayload::Single(inner),
                                ..
                            } => {
                                if let Some(rpc) =
                                    self.apply_confirmed_update(session, *inner, now_ms)
                                {
                                    post_rpcs.push(rpc);
                                }
                            }
                            other => follow_ups.push(other),
                        }
                    }
                    continue;
                }
            }
            if let Some(rpc) = self.apply_confirmed_update(session, u, now_ms) {
                post_rpcs.push(rpc);
            }
        }
        (follow_ups, post_rpcs)
    }

    /// Match `Updates::applyConvertToScheduledOnSend` (`api/api_updates.cpp:912-955`):
    /// when a regular outgoing message is converted into a scheduled message by the server,
    /// `updateNewScheduledMessage` and `updateMessageID` share the server ID while the local
    /// message mapped from `random_id` is NOT scheduled. Destroy the regular local message and
    /// surface [`UpdateEvent::SentConvertedToScheduled`].
    fn apply_convert_to_scheduled_on_send(
        &mut self,
        session: &mut Session,
        updates: &[tl_enums::Update],
    ) {
        for update in updates {
            let tl_enums::Update::NewScheduledMessage(nsm) = update else {
                continue;
            };
            let Some((peer_id, remote_id, _)) = crate::convert::tl_message_meta(&nsm.message)
            else {
                continue;
            };
            let Some(scheduled_id) = scheduled_remote_to_local_id(remote_id) else {
                continue;
            };
            for id_upd in updates {
                let tl_enums::Update::MessageID(mid) = id_upd else {
                    continue;
                };
                if MsgId(i64::from(mid.id)) != remote_id {
                    continue;
                }
                if let Some(local_full_id) = session.message_id_by_random_id(mid.random_id)
                    && let Some(local_msg) = session.message(local_full_id)
                    && !local_msg.is_scheduled()
                {
                    session.delete_messages(local_full_id.peer, &[local_full_id.msg]);
                    self.events.push(UpdateEvent::SentConvertedToScheduled {
                        peer_id,
                        scheduled_id,
                    });
                }
            }
        }
    }

    fn apply_message_id(&mut self, session: &mut Session, random_id: i64, server_id: MsgId) {
        session.resolve_random_id(random_id, server_id);
        self.events.push(UpdateEvent::MessageIdMapped {
            random_id,
            server_id,
        });
    }

    /// Extract force-logout notifications and group call participant/chain-block updates while
    /// `getDifference` is in flight (`api/api_updates.cpp:904-909, 957-977`,
    /// `Updates::mtpUpdateReceived` / `Updates::applyGroupCallParticipantUpdates`).
    fn extract_force_logouts_and_group_call_updates(
        &mut self,
        session: &mut Session,
        updates: &tl_enums::Updates,
    ) {
        match updates {
            tl_enums::Updates::UpdateShort(s) => match &s.update {
                tl_enums::Update::ServiceNotification(n)
                    if n.r#type.starts_with("AUTH_KEY_DROP_") =>
                {
                    self.events.push(UpdateEvent::ForceLogout {
                        notification_type: n.r#type.clone(),
                    });
                }
                tl_enums::Update::GroupCallParticipants(_)
                | tl_enums::Update::GroupCallChainBlocks(_) => {
                    self.events
                        .push(UpdateEvent::CallUpdate(Box::new(s.update.clone())));
                }
                _ => {}
            },
            tl_enums::Updates::Updates(u) => {
                self.extract_from_batch_while_requesting_difference(
                    session, &u.updates, &u.users, &u.chats,
                );
            }
            tl_enums::Updates::Combined(c) => {
                self.extract_from_batch_while_requesting_difference(
                    session, &c.updates, &c.users, &c.chats,
                );
            }
            _ => {}
        }
    }

    fn extract_from_batch_while_requesting_difference(
        &mut self,
        session: &mut Session,
        updates: &[tl_enums::Update],
        users: &[tl_enums::User],
        chats: &[tl_enums::Chat],
    ) {
        let mut group_call_updates: Vec<tl_enums::Update> = Vec::new();
        for item in updates {
            match item {
                tl_enums::Update::ServiceNotification(n)
                    if n.r#type.starts_with("AUTH_KEY_DROP_") =>
                {
                    self.events.push(UpdateEvent::ForceLogout {
                        notification_type: n.r#type.clone(),
                    });
                }
                tl_enums::Update::GroupCallParticipants(_)
                | tl_enums::Update::GroupCallChainBlocks(_) => {
                    group_call_updates.push(item.clone());
                }
                _ => {}
            }
        }
        if !group_call_updates.is_empty() {
            apply_tl_users_and_chats(session, users, chats);
            group_call_updates.sort_by_key(group_call_update_priority);
            for item in group_call_updates {
                self.events.push(UpdateEvent::CallUpdate(Box::new(item)));
            }
        }
    }
}

fn single_line(s: &str) -> String {
    s.replace(['\r', '\n'], " ").trim().to_owned()
}

/// Priority ordering for group call updates in `Updates::feedUpdateVector`
/// (`api/api_updates.cpp:317-325`): `GroupCallChainBlocks` (0) before
/// `GroupCallParticipants` (1) before all other updates (2).
const fn group_call_update_priority(update: &tl_enums::Update) -> u8 {
    match update {
        tl_enums::Update::GroupCallChainBlocks(_) => 0,
        tl_enums::Update::GroupCallParticipants(_) => 1,
        _ => 2,
    }
}

const fn common_pts_of(update: &tl_enums::Update) -> Option<(i32, i32)> {
    match update {
        tl_enums::Update::NewMessage(u) => Some((u.pts, u.pts_count)),
        tl_enums::Update::EditMessage(u) => Some((u.pts, u.pts_count)),
        tl_enums::Update::DeleteMessages(u) => Some((u.pts, u.pts_count)),
        tl_enums::Update::ReadHistoryInbox(u) => Some((u.pts, u.pts_count)),
        tl_enums::Update::ReadHistoryOutbox(u) => Some((u.pts, u.pts_count)),
        tl_enums::Update::ReadMessagesContents(u) => Some((u.pts, u.pts_count)),
        tl_enums::Update::WebPage(u) => Some((u.pts, u.pts_count)),
        tl_enums::Update::FolderPeers(u) => Some((u.pts, u.pts_count)),
        tl_enums::Update::PinnedMessages(u) => Some((u.pts, u.pts_count)),
        tl_enums::Update::MessageWidgetData(u) => Some((u.pts, u.pts_count)),
        _ => None,
    }
}

const fn channel_pts_of(update: &tl_enums::Update) -> Option<(ChannelId, i32, i32)> {
    match update {
        tl_enums::Update::NewChannelMessage(u) => {
            let Some(cid) = message_channel_id(&u.message) else {
                return None;
            };
            Some((cid, u.pts, u.pts_count))
        }
        tl_enums::Update::EditChannelMessage(u) => {
            let Some(cid) = message_channel_id(&u.message) else {
                return None;
            };
            Some((cid, u.pts, u.pts_count))
        }
        tl_enums::Update::DeleteChannelMessages(u) => {
            Some((ChannelId::from_tl(u.channel_id), u.pts, u.pts_count))
        }
        tl_enums::Update::PinnedChannelMessages(u) => {
            Some((ChannelId::from_tl(u.channel_id), u.pts, u.pts_count))
        }
        tl_enums::Update::ChannelWebPage(u) => {
            Some((ChannelId::from_tl(u.channel_id), u.pts, u.pts_count))
        }
        _ => None,
    }
}

const fn qts_of(update: &tl_enums::Update) -> Option<i32> {
    match update {
        tl_enums::Update::NewEncryptedMessage(u) => Some(u.qts),
        tl_enums::Update::MessagePollVote(u) => Some(u.qts),
        tl_enums::Update::ChatParticipant(u) => Some(u.qts),
        tl_enums::Update::ChannelParticipant(u) => Some(u.qts),
        tl_enums::Update::BotStopped(u) => Some(u.qts),
        tl_enums::Update::BotChatInviteRequester(u) => Some(u.qts),
        tl_enums::Update::BotChatBoost(u) => Some(u.qts),
        tl_enums::Update::BotMessageReaction(u) => Some(u.qts),
        tl_enums::Update::BotMessageReactions(u) => Some(u.qts),
        tl_enums::Update::BotBusinessConnect(u) => Some(u.qts),
        tl_enums::Update::BotNewBusinessMessage(u) => Some(u.qts),
        tl_enums::Update::BotEditBusinessMessage(u) => Some(u.qts),
        tl_enums::Update::BotDeleteBusinessMessage(u) => Some(u.qts),
        tl_enums::Update::BotPurchasedPaidMedia(u) => Some(u.qts),
        tl_enums::Update::ManagedBot(u) => Some(u.qts),
        tl_enums::Update::BotGuestChatQuery(u) => Some(u.qts),
        tl_enums::Update::BotStarsSubscription(u) => Some(u.qts),
        _ => None,
    }
}

const fn message_channel_id(msg: &tl_enums::Message) -> Option<ChannelId> {
    match msg {
        tl_enums::Message::Message(m) => peer_id_from_tl(&m.peer_id).to_channel(),
        tl_enums::Message::Service(s) => peer_id_from_tl(&s.peer_id).to_channel(),
        tl_enums::Message::Empty(e) => match &e.peer_id {
            Some(p) => peer_id_from_tl(p).to_channel(),
            None => None,
        },
    }
}

/// Applies `MTPChatParticipants` into a loaded basic `Chat`
/// (`Data::ApplyChatUpdate(ChatData*, MTPChatParticipants)` in `data/data_chat.cpp:545-636`).
fn apply_chat_participants_update(
    session: &mut Session,
    participants: &tl_enums::ChatParticipants,
) -> Option<PostUpdateRpc> {
    match participants {
        tl_enums::ChatParticipants::Forbidden(f) => {
            let cid = ChatId::from_tl(f.chat_id);
            if let Some(chat) = session.chat_mut(cid) {
                chat.count = -1;
                chat.invalidate_participants();
            }
            None
        }
        tl_enums::ChatParticipants::ChatParticipants(ps) => {
            let cid = ChatId::from_tl(ps.chat_id);
            if !session.chat_loaded(cid) {
                return None;
            }
            let peer_id = PeerId::from_chat(cid);
            let mut post_rpc = None;
            let mut parsed_creator = None;
            let mut parsed_participants = Vec::with_capacity(ps.participants.len());
            let mut parsed_admins = Vec::new();
            let mut parsed_ranks = Vec::new();
            let mut any_bot = false;
            let mut all_loaded = true;
            for p in &ps.participants {
                let (uid, is_creator, is_admin, rank) = match p {
                    tl_enums::ChatParticipant::ChatParticipant(m) => {
                        (UserId::from_tl(m.user_id), false, false, m.rank.clone())
                    }
                    tl_enums::ChatParticipant::Creator(m) => {
                        (UserId::from_tl(m.user_id), true, false, m.rank.clone())
                    }
                    tl_enums::ChatParticipant::Admin(m) => {
                        (UserId::from_tl(m.user_id), false, true, m.rank.clone())
                    }
                };
                if !session.user_loaded(uid) {
                    all_loaded = false;
                    break;
                }
                if session.user(uid).is_some_and(|u| u.is_bot()) {
                    any_bot = true;
                }
                parsed_participants.push(uid);
                if is_creator {
                    parsed_creator = Some(uid);
                }
                if is_admin {
                    parsed_admins.push(uid);
                }
                if let Some(r) = rank
                    && !r.is_empty()
                {
                    parsed_ranks.push((uid, r));
                }
            }

            let mut notify = false;
            if let Some(chat) = session.chat_mut(cid) {
                let status = chat.apply_update_version(ps.version);
                if status == ChatUpdateVersionStatus::TooOld {
                    return None;
                }
                if status == ChatUpdateVersionStatus::Skipped {
                    post_rpc = Some(PostUpdateRpc::ReloadFullChat { chat_id: cid });
                }
                chat.version = ps.version;
                chat.count = ps.participants.len() as i32;
                chat.participants.clear();
                chat.admins.clear();
                chat.member_ranks.clear();
                chat.admin_rights = ChatAdminRights::empty();
                if all_loaded {
                    if let Some(creator) = parsed_creator {
                        chat.creator = creator;
                    }
                    for uid in parsed_participants {
                        chat.participants.insert(uid);
                    }
                    for uid in parsed_admins {
                        chat.admins.insert(uid);
                    }
                    for (uid, rank) in parsed_ranks {
                        chat.member_ranks.insert(uid, rank);
                    }
                    chat.bot_status = if any_bot {
                        BotStatus::HasBots
                    } else {
                        BotStatus::NoBots
                    };
                    notify = !chat.participants.is_empty();
                } else {
                    chat.invalidate_participants();
                }
            }
            if notify {
                session
                    .changes()
                    .peer_updated(peer_id, PeerUpdateFlags::MEMBERS | PeerUpdateFlags::ADMINS);
            }
            post_rpc
        }
    }
}
