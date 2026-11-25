//! Live session runtime actor owning a dedicated Tokio runtime thread, `tg_data::Session`,
//! `tg_api::UpdatesHandler`, `DialogsLoader`, `HistoryLoader`, `MessageSender`,
//! `ReadHistoryManager`, and `SendProgressManager`.
//!
//! Port origin: `Telegram/SourceFiles/main/main_session.cpp`, `apiwrap.cpp`, and
//! `api/api_updates.cpp`.
//!
//! Designed to back both synchronous GUI threads (such as `egui` in `tg-app`) and async
//! callers (such as CLI tools and tests), exposing a channel/async command API plus a
//! live [`tg_data::ChangeEvent`] broadcast stream.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use tg_api::convert::{apply_tl_users_and_chats, entities_from_tl_with_self, input_peer_for};
use tg_api::{
    DialogsLoader, DialogsPageResult, DraftManager, FlushedRead, HistoryLoader, HistorySlice,
    Invoker, LoadDirection, MessageSender, PresenceUpdateResult, ReadHistoryManager, RpcClient,
    SendMessageOutcome, SendOptions, SendProgressKind, SendProgressManager, SendProgressUpdate,
    TypingPeerInfo, UpdateEvent, UpdatesHandler,
};
use tg_data::{
    ChangeEvent, ChannelId, DialogKey, FolderId, FullMsgId, Message, MessageFlags,
    MessageUpdateFlags, MsgId, Peer, PeerId, PersistedUpdatesState, Session, TimeId, UserId,
};
use tg_tl::{enums as tl_enums, functions as tl_fns, types as tl_types};
use tokio::runtime::Handle;
use tokio::sync::{RwLock, broadcast, mpsc, oneshot};
use tracing::{debug, warn};

use crate::error::{Result, SessionError};
use crate::rpc::MtprotoRpc;
use crate::updates_store::{FileUpdatesStateStore, UpdatesStateStore};

/// Periodic tick interval for gap timers, throttled inbox reads, and debounced cloud drafts.
const TICK_INTERVAL: Duration = Duration::from_millis(500);

/// Wall-clock vs monotonic drift threshold (seconds) used to detect OS sleep/suspend.
const SLEEP_DETECT_JUMP_SECS: i64 = 15;

/// Capacity of the high-level [`SessionEvent`] broadcast channel.
const SESSION_EVENTS_CAPACITY: usize = 64;

/// Configuration for starting a [`SessionRuntime`].
#[derive(Clone)]
pub struct SessionRuntimeConfig {
    /// Whether to invoke `updates.getState` on startup (default: `true`).
    pub init_updates_state: bool,
    /// Whether to invoke `updates.getDifference` right after `updates.getState` on startup
    /// (default: `true`).
    pub run_get_difference_on_start: bool,
    /// Whether to load the first page (`limit = 20`) of main dialogs on startup (default: `true`).
    pub load_initial_dialogs: bool,
    /// Whether to call `users.getUsers([InputUser::Self_])` on startup if self peer is missing
    /// (default: `true`).
    pub fetch_self_user_on_start: bool,
    /// The account's own user as saved in local storage, applied before the startup sync, so
    /// `users.getUsers(Self)` is not needed before the chat list (tdesktop reads it from local
    /// storage as well). It is ignored unless it is the authorized user, and refreshed once in
    /// the background after the startup sync.
    pub initial_self_user: Option<tg_tl::types::User>,
    /// Capacity of `tg_data::Session`'s [`ChangeEvent`] broadcast channel.
    pub changes_capacity: usize,
    /// Optional pre-loaded updates state (`pts`/`qts`/`date`/`seq`/channel `pts`) to resume
    /// from on startup via `updates.getDifference` instead of calling `updates.getState`.
    pub initial_updates_state: Option<PersistedUpdatesState>,
    /// Optional persistence backend used to load updates state on startup and save it as
    /// state advances and on shutdown.
    pub updates_state_store: Option<Arc<dyn UpdatesStateStore>>,
    /// Periodic actor maintenance tick interval (default: 500 ms).
    pub tick_interval: Duration,
    /// Whether to send idle keepalive pings after `NO_UPDATES_TIMEOUT_MS` (60 s) of silence
    /// (default: `true`).
    pub idle_ping_enabled: bool,
}

impl std::fmt::Debug for SessionRuntimeConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionRuntimeConfig")
            .field("init_updates_state", &self.init_updates_state)
            .field(
                "run_get_difference_on_start",
                &self.run_get_difference_on_start,
            )
            .field("load_initial_dialogs", &self.load_initial_dialogs)
            .field("fetch_self_user_on_start", &self.fetch_self_user_on_start)
            .field("has_initial_self_user", &self.initial_self_user.is_some())
            .field("changes_capacity", &self.changes_capacity)
            .field("initial_updates_state", &self.initial_updates_state)
            .field(
                "has_updates_state_store",
                &self.updates_state_store.is_some(),
            )
            .field("tick_interval", &self.tick_interval)
            .field("idle_ping_enabled", &self.idle_ping_enabled)
            .finish()
    }
}

impl Default for SessionRuntimeConfig {
    fn default() -> Self {
        Self {
            init_updates_state: true,
            run_get_difference_on_start: true,
            load_initial_dialogs: true,
            fetch_self_user_on_start: true,
            initial_self_user: None,
            changes_capacity: tg_data::changes::DEFAULT_CHANGES_CAPACITY,
            initial_updates_state: None,
            updates_state_store: None,
            tick_interval: TICK_INTERVAL,
            idle_ping_enabled: true,
        }
    }
}

impl SessionRuntimeConfig {
    /// Seeds an explicit saved [`PersistedUpdatesState`] for startup resume.
    #[must_use]
    pub fn with_initial_updates_state(mut self, state: PersistedUpdatesState) -> Self {
        self.initial_updates_state = Some(state);
        self
    }

    /// Configures a file-backed [`UpdatesStateStore`] at `path`.
    #[must_use]
    pub fn with_updates_state_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.updates_state_store = Some(Arc::new(FileUpdatesStateStore::new(path)));
        self
    }

    /// Configures a custom [`UpdatesStateStore`] backend.
    #[must_use]
    pub fn with_updates_state_store(mut self, store: Arc<dyn UpdatesStateStore>) -> Self {
        self.updates_state_store = Some(store);
        self
    }

    /// Overrides the periodic maintenance tick interval.
    #[must_use]
    pub fn with_tick_interval(mut self, interval: Duration) -> Self {
        self.tick_interval = interval.max(Duration::from_millis(1));
        self
    }

    /// Enables or disables periodic idle keepalive pings (`kNoUpdatesTimeout`).
    #[must_use]
    pub const fn with_idle_ping_enabled(mut self, enabled: bool) -> Self {
        self.idle_ping_enabled = enabled;
        self
    }
}

/// High-level lifecycle/side-effect event emitted by [`SessionRuntime`] alongside
/// [`tg_data::ChangeEvent`].
#[derive(Debug, Clone, PartialEq)]
pub enum SessionEvent {
    /// Initial startup sync (`getState`, `getDifference`, first dialogs page) completed.
    Ready,
    /// Startup or background sync encountered an error.
    SyncError(String),
    /// Server sent `AUTH_KEY_DROP_*`, requesting immediate forced logout.
    ForceLogout {
        /// Raw notification type string from Telegram.
        notification_type: String,
    },
    /// Channel difference returned `channelDifferenceTooLong`; UI should reload bottom slice.
    ChannelTooLong {
        /// Affected channel ID.
        channel_id: ChannelId,
        /// Updated channel `pts` if known.
        pts: Option<i32>,
    },
    /// Common difference returned `updates.differenceTooLong`; local `pts` was reset and
    /// dialogs / active histories should be refreshed.
    DifferenceTooLong {
        /// New server common `pts`.
        pts: i32,
    },
    /// Server pushed a cloud language pack difference.
    LangPackDifference(tg_lang::LangPackDifference),
    /// Server reported `updateLangPackTooLong`.
    LangPackTooLong {
        /// Language code requiring reload.
        lang_code: String,
    },
    /// Server delivered a story update (`updateStory`, `updateReadStories`,
    /// `updateStoriesStealthMode`, `updateStoryID`) live or via `updates.getDifference`
    /// (`api/api_updates.cpp:2947-2958`).
    StoryUpdate(Box<tl_enums::Update>),
    /// Server pushed `updateNewAuthorization` (`api/api_updates.cpp:2366`).
    NewAuthorization(Box<tl_types::UpdateNewAuthorization>),
    /// Server pushed `updateNotifySettings` (`api/api_updates.cpp:2196-2201`).
    NotifySettingsUpdated {
        /// Affected peer, scope, forum topic, or community.
        peer: tl_enums::NotifyPeer,
        /// Updated TL notification settings.
        settings: Box<tl_enums::PeerNotifySettings>,
    },
    /// Server pushed `updateUser` for a full-loaded user (`api/api_updates.cpp:2179-2186`).
    UserFullRefreshNeeded {
        /// User ID needing `users.getFullUser`.
        user_id: UserId,
    },
    /// Server sent `updateStickerSets` (`api/api_updates.cpp:2828-2840`).
    StickerSets {
        /// Masks flag.
        masks: bool,
        /// Custom emoji flag.
        emojis: bool,
    },
    /// Server sent `updateStickerSetsOrder` (`api/api_updates.cpp:2749-2796`).
    StickerSetsOrder {
        /// Masks flag.
        masks: bool,
        /// Custom emoji flag.
        emojis: bool,
        /// Ordered sticker set IDs.
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
        /// Sticker set ID moved to index 0.
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

/// Lightweight summary of a dialog entry read from [`tg_data::Session`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DialogSummary {
    /// Dialog key (history or folder).
    pub key: DialogKey,
    /// Peer ID (for `DialogKey::History`).
    pub peer_id: Option<PeerId>,
    /// Display title of the peer or folder.
    pub title: String,
    /// Unread message count (`0` if read or unknown).
    pub unread_count: i32,
    /// Explicit "mark as unread" flag.
    pub unread_mark: bool,
    /// Whether notifications are muted.
    pub muted: bool,
    /// Top message ID, if known.
    pub top_message_id: Option<MsgId>,
    /// Preview text of the top message, if loaded.
    pub top_message_preview: String,
}

/// Point-in-time read-only snapshot of key [`SessionRuntime`] state.
#[derive(Debug, Clone)]
pub struct SessionSnapshot {
    /// Logged-in self user ID.
    pub self_user_id: UserId,
    /// Ordered main dialog summaries (up to requested limit).
    pub dialogs: Vec<DialogSummary>,
    /// Current global updates `pts`.
    pub pts: i32,
}

struct SendMessageCommand {
    peer_id: PeerId,
    text: String,
    entities: Vec<tl_enums::MessageEntity>,
    reply_to: Option<tl_enums::InputReplyTo>,
    options: SendOptions,
    reply: oneshot::Sender<Result<SendMessageOutcome>>,
}

enum SessionCommand {
    WaitReady {
        reply: oneshot::Sender<Result<()>>,
    },
    LoadDialogsPage {
        folder_id: FolderId,
        reply: oneshot::Sender<Result<Option<DialogsPageResult>>>,
    },
    OpenHistory {
        peer_id: PeerId,
        around_id: MsgId,
        direction: LoadDirection,
        reply: oneshot::Sender<Result<Option<HistorySlice>>>,
    },
    SendMessage(Box<SendMessageCommand>),
    MarkRead {
        peer_id: PeerId,
        max_id: MsgId,
        force: bool,
        reply: oneshot::Sender<Result<Vec<FlushedRead>>>,
    },
    SendTyping {
        peer_id: PeerId,
        top_msg_id: Option<MsgId>,
        kind: SendProgressKind,
        progress: i32,
        reply: oneshot::Sender<Result<bool>>,
    },
    TriggerDifference {
        reply: oneshot::Sender<Result<()>>,
    },
    SetActiveChannel {
        channel_id: ChannelId,
        active: bool,
        reply: oneshot::Sender<Result<()>>,
    },
    ApplyAffectedHistory {
        peer_id: Option<PeerId>,
        pts: i32,
        pts_count: i32,
        reply: oneshot::Sender<Result<()>>,
    },
    ApplyUpdates {
        updates: Box<tl_enums::Updates>,
        sent_random_id: Option<i64>,
        reply: oneshot::Sender<Result<()>>,
    },
    DeleteMessages {
        peer_id: PeerId,
        ids: Vec<MsgId>,
        revoke: bool,
        reply: oneshot::Sender<Result<tl_types::messages::AffectedMessages>>,
    },
    EditMessage {
        req: Box<tl_fns::messages::EditMessage>,
        reply: oneshot::Sender<Result<Option<tl_enums::Updates>>>,
    },
    InitChannelPts {
        channel_id: ChannelId,
        pts: i32,
        reply: oneshot::Sender<Result<()>>,
    },
    ApplyChannelPts {
        channel_id: ChannelId,
        pts: i32,
        pts_count: i32,
        reply: oneshot::Sender<Result<()>>,
    },
    RestoreUpdatesState {
        state: PersistedUpdatesState,
        reply: oneshot::Sender<Result<()>>,
    },
    OnReconnect {
        restart_sessions: bool,
        reply: oneshot::Sender<Result<()>>,
    },
    OnWakeFromSleep {
        force_resync: bool,
        reply: oneshot::Sender<Result<bool>>,
    },
    PersistedUpdatesState {
        reply: oneshot::Sender<PersistedUpdatesState>,
    },
    Snapshot {
        max_dialogs: usize,
        reply: oneshot::Sender<SessionSnapshot>,
    },
    UpdatePresence {
        window_active: bool,
        idle_ms: u64,
        reply: oneshot::Sender<Result<PresenceUpdateResult>>,
    },
    Shutdown {
        reply: oneshot::Sender<()>,
    },
}

enum RuntimeOwner {
    Owned {
        handle: Handle,
        shutdown_tx: Option<oneshot::Sender<()>>,
        thread: Option<std::thread::JoinHandle<()>>,
    },
    External(Handle),
}

impl RuntimeOwner {
    fn handle(&self) -> &Handle {
        match self {
            Self::Owned { handle, .. } | Self::External(handle) => handle,
        }
    }
}

/// Owns a background Tokio runtime thread (or attaches to an existing handle), a shared
/// [`tg_data::Session`], and the async session actor pumping updates and handling UI commands.
pub struct SessionRuntime<C: RpcClient + Clone + 'static = MtprotoRpc> {
    owner: RuntimeOwner,
    rpc: C,
    session: Arc<RwLock<Session>>,
    changes_sub: broadcast::Receiver<ChangeEvent>,
    initial_changes_rx: Mutex<Option<broadcast::Receiver<ChangeEvent>>>,
    cmd_tx: mpsc::UnboundedSender<SessionCommand>,
    session_events_tx: broadcast::Sender<SessionEvent>,
    initial_events_rx: Mutex<Option<broadcast::Receiver<SessionEvent>>>,
    ready: Arc<AtomicBool>,
    _marker: std::marker::PhantomData<C>,
}

impl SessionRuntime<MtprotoRpc> {
    /// Starts a `SessionRuntime` backed by [`MtprotoRpc`] on its own dedicated Tokio runtime
    /// thread, wiring `Updates`, `new_session_created`, and fatal `401` auth-failure streams.
    pub fn start_mtproto(
        self_user_id: UserId,
        rpc: MtprotoRpc,
        config: SessionRuntimeConfig,
    ) -> Result<Self> {
        let new_session_rx = Some(rpc.subscribe_new_session());
        let auth_fail_rx = Some(rpc.subscribe_auth_failures());
        Self::start_internal(
            self_user_id,
            rpc,
            new_session_rx,
            auth_fail_rx,
            config,
            None,
        )
    }

    /// Starts a `SessionRuntime` backed by [`MtprotoRpc`] on an existing Tokio [`Handle`].
    pub fn start_mtproto_on(
        self_user_id: UserId,
        rpc: MtprotoRpc,
        handle: Handle,
        config: SessionRuntimeConfig,
    ) -> Result<Self> {
        rpc.ensure_updates_bridge(&handle);
        let new_session_rx = Some(rpc.subscribe_new_session());
        let auth_fail_rx = Some(rpc.subscribe_auth_failures());
        Self::start_internal(
            self_user_id,
            rpc,
            new_session_rx,
            auth_fail_rx,
            config,
            Some(handle),
        )
    }
}

impl<C: RpcClient + Clone + 'static> SessionRuntime<C> {
    /// Starts a `SessionRuntime` on its own dedicated multi-thread Tokio runtime thread.
    ///
    /// This is the primary entry point for synchronous GUI shells (like `egui` in `tg-app`)
    /// and CLI tools that do not run their own Tokio runtime.
    pub fn start(self_user_id: UserId, rpc: C, config: SessionRuntimeConfig) -> Result<Self> {
        Self::start_internal(self_user_id, rpc, None, None, config, None)
    }

    /// Starts a `SessionRuntime` on a caller-provided Tokio runtime [`Handle`].
    pub fn start_on(
        self_user_id: UserId,
        rpc: C,
        handle: Handle,
        config: SessionRuntimeConfig,
    ) -> Result<Self> {
        Self::start_internal(self_user_id, rpc, None, None, config, Some(handle))
    }

    /// Starts a `SessionRuntime` on a caller-provided Tokio runtime [`Handle`] with
    /// explicit `new_session_created` and fatal-`401` broadcast receivers (used when
    /// wrapping `MtprotoRpc` in a type-erased `RpcClient`).
    pub fn start_with_new_session_on(
        self_user_id: UserId,
        rpc: C,
        new_session_rx: Option<broadcast::Receiver<i32>>,
        auth_fail_rx: Option<broadcast::Receiver<tg_api::RpcError>>,
        handle: Handle,
        config: SessionRuntimeConfig,
    ) -> Result<Self> {
        Self::start_internal(
            self_user_id,
            rpc,
            new_session_rx,
            auth_fail_rx,
            config,
            Some(handle),
        )
    }

    fn start_internal(
        self_user_id: UserId,
        rpc: C,
        new_session_rx: Option<broadcast::Receiver<i32>>,
        auth_fail_rx: Option<broadcast::Receiver<tg_api::RpcError>>,
        config: SessionRuntimeConfig,
        external_handle: Option<Handle>,
    ) -> Result<Self> {
        let owner = match external_handle {
            Some(handle) => RuntimeOwner::External(handle),
            None => spawn_owned_tokio_runtime()?,
        };
        let handle = owner.handle().clone();

        let raw_session = Session::with_broadcast_capacity(self_user_id, config.changes_capacity);
        let changes_sub = raw_session.subscribe();
        let initial_changes_rx = raw_session.subscribe();
        let session = Arc::new(RwLock::new(raw_session));
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (session_events_tx, initial_events_rx) = broadcast::channel(SESSION_EVENTS_CAPACITY);
        let ready = Arc::new(AtomicBool::new(false));

        let updates_rx = {
            let _guard = handle.enter();
            rpc.subscribe_updates()
        };

        let actor = SessionActor {
            rpc: rpc.clone(),
            session: Arc::clone(&session),
            updates: UpdatesHandler::new(),
            dialogs: DialogsLoader::new(),
            history: HistoryLoader::new(),
            sender: MessageSender::default(),
            read_manager: ReadHistoryManager::new(),
            progress_manager: SendProgressManager::new(),
            draft_manager: DraftManager::new(),
            config,
            cmd_rx,
            updates_rx,
            new_session_rx,
            auth_fail_rx,
            session_events_tx: session_events_tx.clone(),
            ready: Arc::clone(&ready),
            started_at: Instant::now(),
            last_tick_wall_secs: now_unix_secs(),
            last_tick_mono_ms: 0,
            last_persisted_state: None,
            ready_result: None,
            ready_waiters: Vec::new(),
            refresh_self_after_startup: false,
            self_refresh: None,
        };

        handle.spawn(actor.run());

        Ok(Self {
            owner,
            rpc,
            session,
            changes_sub,
            initial_changes_rx: Mutex::new(Some(initial_changes_rx)),
            cmd_tx,
            session_events_tx,
            initial_events_rx: Mutex::new(Some(initial_events_rx)),
            ready,
            _marker: std::marker::PhantomData,
        })
    }

    /// Borrows the underlying RPC client.
    #[must_use]
    pub fn rpc(&self) -> &C {
        &self.rpc
    }

    /// Returns the Tokio [`Handle`] driving this session runtime.
    #[must_use]
    pub fn tokio_handle(&self) -> &Handle {
        self.owner.handle()
    }

    /// Returns `true` once the initial startup sync (`getState`, `getDifference`, and
    /// initial dialogs page) has completed successfully.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::Acquire)
    }

    /// Returns a shared async-locked handle to [`tg_data::Session`] for direct UI inspection
    /// (peers, histories, messages, dialog lists, unread badges).
    #[must_use]
    pub fn session(&self) -> &Arc<RwLock<Session>> {
        &self.session
    }

    /// Subscribes to [`tg_data::ChangeEvent`] notifications emitted by [`tg_data::Session`].
    ///
    /// The first subscriber receives a receiver registered before the actor started, ensuring
    /// startup mutations are not lost due to scheduling races.
    #[must_use]
    pub fn subscribe_changes(&self) -> broadcast::Receiver<ChangeEvent> {
        if let Some(rx) = self.initial_changes_rx.lock().take() {
            return rx;
        }
        self.changes_sub.resubscribe()
    }

    /// Subscribes to high-level [`SessionEvent`] notifications (`Ready`, `ForceLogout`, etc.).
    ///
    /// The first subscriber receives a receiver registered before the actor started, ensuring
    /// [`SessionEvent::Ready`] is not lost even when startup completes immediately.
    #[must_use]
    pub fn subscribe_events(&self) -> broadcast::Receiver<SessionEvent> {
        if let Some(rx) = self.initial_events_rx.lock().take() {
            return rx;
        }
        self.session_events_tx.subscribe()
    }

    /// Seeds a known self TL user into [`tg_data::Session`] immediately (e.g. right after login).
    pub fn seed_self_user(&self, user: &tg_tl::types::User) {
        let users = [tl_enums::User::User(user.clone())];
        if let Ok(mut session) = self.session.try_write() {
            apply_tl_users_and_chats(&mut session, &users, &[]);
        } else {
            self.owner.handle().block_on(async {
                let mut session = self.session.write().await;
                apply_tl_users_and_chats(&mut session, &users, &[]);
            });
        }
    }

    /// Async version of [`Self::seed_self_user`] safe to call from inside a Tokio runtime.
    pub async fn seed_self_user_async(&self, user: &tg_tl::types::User) {
        let users = [tl_enums::User::User(user.clone())];
        let mut session = self.session.write().await;
        apply_tl_users_and_chats(&mut session, &users, &[]);
    }

    // -------------------------------------------------------------------------
    // Async command API
    // -------------------------------------------------------------------------

    /// Awaits completion of the initial startup sequence (`getState`, `getDifference`, and
    /// initial dialogs page).
    pub async fn wait_ready(&self) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SessionCommand::WaitReady { reply: tx })
            .map_err(|_| SessionError::RuntimeClosed)?;
        rx.await.map_err(|_| SessionError::RuntimeClosed)?
    }

    /// Loads the next page of dialogs for `folder_id` (`0` = main list, `1` = archive) and
    /// applies the results to [`tg_data::Session`].
    pub async fn load_dialogs_page(
        &self,
        folder_id: FolderId,
    ) -> Result<Option<DialogsPageResult>> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SessionCommand::LoadDialogsPage {
                folder_id,
                reply: tx,
            })
            .map_err(|_| SessionError::RuntimeClosed)?;
        rx.await.map_err(|_| SessionError::RuntimeClosed)?
    }

    /// Loads a history slice for `peer_id` (`Before`, `Around`, or `After` `around_id`) and
    /// applies the messages and `no_skip_range` to [`tg_data::Session`].
    pub async fn open_history(
        &self,
        peer_id: PeerId,
        around_id: MsgId,
        direction: LoadDirection,
    ) -> Result<Option<HistorySlice>> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SessionCommand::OpenHistory {
                peer_id,
                around_id,
                direction,
                reply: tx,
            })
            .map_err(|_| SessionError::RuntimeClosed)?;
        rx.await.map_err(|_| SessionError::RuntimeClosed)?
    }

    /// Sends a plain-text (or entity-formatted) message to `peer_id` and applies the server
    /// updates to [`tg_data::Session`].
    pub async fn send_message(
        &self,
        peer_id: PeerId,
        text: impl Into<String>,
        options: SendOptions,
    ) -> Result<SendMessageOutcome> {
        self.send_message_with_entities(peer_id, text, Vec::new(), None, options)
            .await
    }

    /// Sends a message with explicit TL entities and optional reply header to `peer_id`.
    pub async fn send_message_with_entities(
        &self,
        peer_id: PeerId,
        text: impl Into<String>,
        entities: Vec<tl_enums::MessageEntity>,
        reply_to: Option<tl_enums::InputReplyTo>,
        options: SendOptions,
    ) -> Result<SendMessageOutcome> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SessionCommand::SendMessage(Box::new(SendMessageCommand {
                peer_id,
                text: text.into(),
                entities,
                reply_to,
                options,
                reply: tx,
            })))
            .map_err(|_| SessionError::RuntimeClosed)?;
        rx.await.map_err(|_| SessionError::RuntimeClosed)?
    }

    /// Marks messages in `peer_id` as read up to `max_id` (throttled unless `force == true`).
    pub async fn mark_read(
        &self,
        peer_id: PeerId,
        max_id: MsgId,
        force: bool,
    ) -> Result<Vec<FlushedRead>> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SessionCommand::MarkRead {
                peer_id,
                max_id,
                force,
                reply: tx,
            })
            .map_err(|_| SessionError::RuntimeClosed)?;
        rx.await.map_err(|_| SessionError::RuntimeClosed)?
    }

    /// Reports a local typing or upload progress action in `peer_id` (`messages.setTyping`).
    pub async fn send_typing(
        &self,
        peer_id: PeerId,
        top_msg_id: Option<MsgId>,
        kind: SendProgressKind,
        progress: i32,
    ) -> Result<bool> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SessionCommand::SendTyping {
                peer_id,
                top_msg_id,
                kind,
                progress,
                reply: tx,
            })
            .map_err(|_| SessionError::RuntimeClosed)?;
        rx.await.map_err(|_| SessionError::RuntimeClosed)?
    }

    /// Manually triggers `updates.getDifference` to catch up any missed server updates.
    pub async fn trigger_difference(&self) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SessionCommand::TriggerDifference { reply: tx })
            .map_err(|_| SessionError::RuntimeClosed)?;
        rx.await.map_err(|_| SessionError::RuntimeClosed)?
    }

    /// Marks `channel_id` as active (or inactive) for short-polling
    /// (`Updates::addActiveChat`).
    pub async fn set_active_channel(&self, channel_id: ChannelId, active: bool) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SessionCommand::SetActiveChannel {
                channel_id,
                active,
                reply: tx,
            })
            .map_err(|_| SessionError::RuntimeClosed)?;
        rx.await.map_err(|_| SessionError::RuntimeClosed)?
    }

    /// Applies a `messages.affectedHistory` `(pts, pts_count)` increment on the session actor's
    /// single [`UpdatesHandler`] (`ApiWrap::applyAffectedHistory` in `apiwrap.cpp:2338-2348`):
    /// channel peers advance that channel's `PtsWaiter`, while non-channel / `None` peers
    /// advance the common `PtsWaiter`, using the same gap wait timers and difference recovery.
    pub async fn apply_affected_history(
        &self,
        peer_id: Option<PeerId>,
        pts: i32,
        pts_count: i32,
    ) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SessionCommand::ApplyAffectedHistory {
                peer_id,
                pts,
                pts_count,
                reply: tx,
            })
            .map_err(|_| SessionError::RuntimeClosed)?;
        rx.await.map_err(|_| SessionError::RuntimeClosed)?
    }

    /// Feeds an RPC-returned TL [`tl_enums::Updates`] batch into the session actor's single
    /// [`UpdatesHandler`] (`ApiWrap::applyUpdates` in `apiwrap.cpp:2332-2336` ->
    /// `Api::Updates::applyUpdates` in `api/api_updates.cpp:1518-1560`), advancing `pts`/`seq`
    /// watermarks, mutating [`tg_data::Session`], emitting [`ChangeEvent`]s, and persisting state.
    pub async fn apply_updates(&self, updates: tl_enums::Updates) -> Result<()> {
        self.apply_updates_with_random_id(updates, None).await
    }

    /// Variant of [`Self::apply_updates`] carrying an optional outgoing `sent_random_id`
    /// (`ApiWrap::applyUpdates(updates, sentMessageRandomId)` in `apiwrap.cpp:2332-2336`).
    pub async fn apply_updates_with_random_id(
        &self,
        updates: tl_enums::Updates,
        sent_random_id: Option<i64>,
    ) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SessionCommand::ApplyUpdates {
                updates: Box::new(updates),
                sent_random_id,
                reply: tx,
            })
            .map_err(|_| SessionError::RuntimeClosed)?;
        rx.await.map_err(|_| SessionError::RuntimeClosed)?
    }

    /// Deletes messages in `peer_id` via `messages.deleteMessages` / `channels.deleteMessages`,
    /// removes them locally from [`tg_data::Session`], and advances the actor's [`UpdatesHandler`]
    /// with the returned `affectedMessages` `(pts, pts_count)`
    /// (`Data::Histories::deleteMessages` in `data/data_histories.cpp:975-1065` and
    /// `ApiWrap::applyAffectedMessages` in `apiwrap.cpp:2350-2358`).
    pub async fn delete_messages(
        &self,
        peer_id: PeerId,
        ids: Vec<MsgId>,
        revoke: bool,
    ) -> Result<tl_types::messages::AffectedMessages> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SessionCommand::DeleteMessages {
                peer_id,
                ids,
                revoke,
                reply: tx,
            })
            .map_err(|_| SessionError::RuntimeClosed)?;
        rx.await.map_err(|_| SessionError::RuntimeClosed)?
    }

    /// Edits a message via `messages.editMessage` and feeds the returned `Updates` through the
    /// session actor's single [`UpdatesHandler`] (`Api::EditMessage` in `api/api_editing.cpp:356`).
    ///
    /// Matches `HistoryWidget::saveEdit` (`history/history_widget.cpp:5736`) and
    /// `EditMessageWithUploadedMedia` (`api/api_editing.cpp:426`): `400 MESSAGE_NOT_MODIFIED`
    /// (including after `MTP::Instance::Private::onErrorDefault` in `mtproto/mtp_instance.cpp:1606`
    /// retries a `500 RPC_CALL_FAIL` whose first attempt already committed on the server) is
    /// treated as non-fatal (`Ok(None)`) and triggers a difference sync so any committed edit is
    /// reflected in [`tg_data::Session`].
    pub async fn edit_message(
        &self,
        req: tl_fns::messages::EditMessage,
    ) -> Result<Option<tl_enums::Updates>> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SessionCommand::EditMessage {
                req: Box::new(req),
                reply: tx,
            })
            .map_err(|_| SessionError::RuntimeClosed)?;
        rx.await.map_err(|_| SessionError::RuntimeClosed)?
    }

    /// Initializes the PTS watermark for `channel_id` on the session actor's single
    /// [`UpdatesHandler`] (`ChannelData::ptsInit` in `data/data_channel.h:480-482`).
    pub async fn init_channel_pts(&self, channel_id: ChannelId, pts: i32) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SessionCommand::InitChannelPts {
                channel_id,
                pts,
                reply: tx,
            })
            .map_err(|_| SessionError::RuntimeClosed)?;
        rx.await.map_err(|_| SessionError::RuntimeClosed)?
    }

    /// Feeds a channel `(pts, pts_count)` into the session actor's single [`UpdatesHandler`]
    /// (`ChannelData::ptsReceived(pts)` when `pts_count == 0`, `ChannelData::ptsUpdateAndApply`
    /// when `pts_count > 0`, `data/data_channel.h:483-488`): initializes if uninitialized,
    /// advances/drains buffered updates on match, or arms the gap timer for `getChannelDifference`.
    pub async fn apply_channel_pts(
        &self,
        channel_id: ChannelId,
        pts: i32,
        pts_count: i32,
    ) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SessionCommand::ApplyChannelPts {
                channel_id,
                pts,
                pts_count,
                reply: tx,
            })
            .map_err(|_| SessionError::RuntimeClosed)?;
        rx.await.map_err(|_| SessionError::RuntimeClosed)?
    }

    /// Restores a [`PersistedUpdatesState`] snapshot onto the session actor's single
    /// [`UpdatesHandler`] without issuing network RPCs.
    pub async fn restore_updates_state(&self, state: PersistedUpdatesState) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SessionCommand::RestoreUpdatesState { state, reply: tx })
            .map_err(|_| SessionError::RuntimeClosed)?;
        rx.await.map_err(|_| SessionError::RuntimeClosed)?
    }

    /// Triggers a post-reconnect resync (`seq = 0`, `updates.getDifference`, and active
    /// channel refresh) without forcing an MTProto transport restart.
    pub async fn on_reconnect(&self) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SessionCommand::OnReconnect {
                restart_sessions: false,
                reply: tx,
            })
            .map_err(|_| SessionError::RuntimeClosed)?;
        rx.await.map_err(|_| SessionError::RuntimeClosed)?
    }

    /// Handles a network interface / connectivity change: restarts MTProto sessions and
    /// immediately resyncs missed updates via `updates.getDifference`.
    pub async fn on_network_change(&self) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SessionCommand::OnReconnect {
                restart_sessions: true,
                reply: tx,
            })
            .map_err(|_| SessionError::RuntimeClosed)?;
        rx.await.map_err(|_| SessionError::RuntimeClosed)?
    }

    /// Handles OS wake from sleep (`Updates::checkLastUpdate(true)`): sends a keepalive ping
    /// if idle past `NO_UPDATES_AFTER_SLEEP_TIMEOUT_MS` (or forces a resync when
    /// `force_resync == true` or if the ping fails). Returns `true` if a ping or resync ran.
    pub async fn on_wake_from_sleep(&self, force_resync: bool) -> Result<bool> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SessionCommand::OnWakeFromSleep {
                force_resync,
                reply: tx,
            })
            .map_err(|_| SessionError::RuntimeClosed)?;
        rx.await.map_err(|_| SessionError::RuntimeClosed)?
    }

    /// Exports the current [`PersistedUpdatesState`] (`pts`, `qts`, `date`, `seq`, channel `pts`).
    pub async fn persisted_updates_state(&self) -> Result<PersistedUpdatesState> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SessionCommand::PersistedUpdatesState { reply: tx })
            .map_err(|_| SessionError::RuntimeClosed)?;
        rx.await.map_err(|_| SessionError::RuntimeClosed)
    }

    /// Evaluates window focus & idle duration and sends `account.updateStatus` only when
    /// required by tdesktop's timing rules (`Updates::updateOnline`, `api/api_updates.cpp:995-1064`).
    pub async fn update_presence(
        &self,
        window_active: bool,
        idle_ms: u64,
    ) -> Result<PresenceUpdateResult> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SessionCommand::UpdatePresence {
                window_active,
                idle_ms,
                reply: tx,
            })
            .map_err(|_| SessionError::RuntimeClosed)?;
        rx.await.map_err(|_| SessionError::RuntimeClosed)?
    }

    /// Takes a snapshot of the first `max_dialogs` main dialogs and current updates state.
    pub async fn snapshot(&self, max_dialogs: usize) -> Result<SessionSnapshot> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SessionCommand::Snapshot {
                max_dialogs,
                reply: tx,
            })
            .map_err(|_| SessionError::RuntimeClosed)?;
        rx.await.map_err(|_| SessionError::RuntimeClosed)
    }

    /// Reads up to `limit` main dialog summaries from the shared [`tg_data::Session`].
    #[must_use]
    pub fn dialog_summaries(&self, limit: usize) -> Vec<DialogSummary> {
        if let Ok(session) = self.session.try_read() {
            return collect_dialog_summaries(&session, limit);
        }
        self.owner.handle().block_on(async {
            let session = self.session.read().await;
            collect_dialog_summaries(&session, limit)
        })
    }

    /// Async version of [`Self::dialog_summaries`] safe to call from inside a Tokio runtime.
    pub async fn dialog_summaries_async(&self, limit: usize) -> Vec<DialogSummary> {
        let session = self.session.read().await;
        collect_dialog_summaries(&session, limit)
    }

    /// Reads the newest `limit` loaded messages for `peer_id` in chronological order
    /// (oldest first, newest last) from [`tg_data::Session`].
    #[must_use]
    pub fn last_messages(&self, peer_id: PeerId, limit: usize) -> Vec<Message> {
        if let Ok(session) = self.session.try_read() {
            return collect_last_messages(&session, peer_id, limit);
        }
        self.owner.handle().block_on(async {
            let session = self.session.read().await;
            collect_last_messages(&session, peer_id, limit)
        })
    }

    /// Async version of [`Self::last_messages`] safe to call from inside a Tokio runtime.
    pub async fn last_messages_async(&self, peer_id: PeerId, limit: usize) -> Vec<Message> {
        let session = self.session.read().await;
        collect_last_messages(&session, peer_id, limit)
    }

    // -------------------------------------------------------------------------
    // Blocking convenience helpers for non-async callers (e.g. GUI / CLI)
    // -------------------------------------------------------------------------

    /// Blocking wrapper around [`Self::wait_ready`].
    pub fn wait_ready_blocking(&self) -> Result<()> {
        self.owner.handle().block_on(self.wait_ready())
    }

    /// Blocking wrapper around [`Self::load_dialogs_page`].
    pub fn load_dialogs_page_blocking(
        &self,
        folder_id: FolderId,
    ) -> Result<Option<DialogsPageResult>> {
        self.owner
            .handle()
            .block_on(self.load_dialogs_page(folder_id))
    }

    /// Blocking wrapper around [`Self::open_history`].
    pub fn open_history_blocking(
        &self,
        peer_id: PeerId,
        around_id: MsgId,
        direction: LoadDirection,
    ) -> Result<Option<HistorySlice>> {
        self.owner
            .handle()
            .block_on(self.open_history(peer_id, around_id, direction))
    }

    /// Blocking wrapper around [`Self::send_message`].
    pub fn send_message_blocking(
        &self,
        peer_id: PeerId,
        text: impl Into<String>,
        options: SendOptions,
    ) -> Result<SendMessageOutcome> {
        self.owner
            .handle()
            .block_on(self.send_message(peer_id, text, options))
    }

    /// Blocking wrapper around [`Self::mark_read`].
    pub fn mark_read_blocking(
        &self,
        peer_id: PeerId,
        max_id: MsgId,
        force: bool,
    ) -> Result<Vec<FlushedRead>> {
        self.owner
            .handle()
            .block_on(self.mark_read(peer_id, max_id, force))
    }

    /// Blocking wrapper around [`Self::send_typing`].
    pub fn send_typing_blocking(
        &self,
        peer_id: PeerId,
        top_msg_id: Option<MsgId>,
        kind: SendProgressKind,
        progress: i32,
    ) -> Result<bool> {
        self.owner
            .handle()
            .block_on(self.send_typing(peer_id, top_msg_id, kind, progress))
    }

    /// Blocking wrapper around [`Self::set_active_channel`].
    pub fn set_active_channel_blocking(&self, channel_id: ChannelId, active: bool) -> Result<()> {
        self.owner
            .handle()
            .block_on(self.set_active_channel(channel_id, active))
    }

    /// Blocking wrapper around [`Self::on_reconnect`].
    pub fn on_reconnect_blocking(&self) -> Result<()> {
        self.owner.handle().block_on(self.on_reconnect())
    }

    /// Blocking wrapper around [`Self::on_network_change`].
    pub fn on_network_change_blocking(&self) -> Result<()> {
        self.owner.handle().block_on(self.on_network_change())
    }

    /// Blocking wrapper around [`Self::on_wake_from_sleep`].
    pub fn on_wake_from_sleep_blocking(&self, force_resync: bool) -> Result<bool> {
        self.owner
            .handle()
            .block_on(self.on_wake_from_sleep(force_resync))
    }

    /// Blocking wrapper around [`Self::persisted_updates_state`].
    pub fn persisted_updates_state_blocking(&self) -> Result<PersistedUpdatesState> {
        self.owner.handle().block_on(self.persisted_updates_state())
    }

    /// Blocking wrapper around [`Self::apply_affected_history`].
    pub fn apply_affected_history_blocking(
        &self,
        peer_id: Option<PeerId>,
        pts: i32,
        pts_count: i32,
    ) -> Result<()> {
        self.owner
            .handle()
            .block_on(self.apply_affected_history(peer_id, pts, pts_count))
    }

    /// Blocking wrapper around [`Self::apply_updates`].
    pub fn apply_updates_blocking(&self, updates: tl_enums::Updates) -> Result<()> {
        self.owner.handle().block_on(self.apply_updates(updates))
    }

    /// Blocking wrapper around [`Self::delete_messages`].
    pub fn delete_messages_blocking(
        &self,
        peer_id: PeerId,
        ids: Vec<MsgId>,
        revoke: bool,
    ) -> Result<tl_types::messages::AffectedMessages> {
        self.owner
            .handle()
            .block_on(self.delete_messages(peer_id, ids, revoke))
    }

    /// Blocking wrapper around [`Self::init_channel_pts`].
    pub fn init_channel_pts_blocking(&self, channel_id: ChannelId, pts: i32) -> Result<()> {
        self.owner
            .handle()
            .block_on(self.init_channel_pts(channel_id, pts))
    }

    /// Blocking wrapper around [`Self::apply_channel_pts`].
    pub fn apply_channel_pts_blocking(
        &self,
        channel_id: ChannelId,
        pts: i32,
        pts_count: i32,
    ) -> Result<()> {
        self.owner
            .handle()
            .block_on(self.apply_channel_pts(channel_id, pts, pts_count))
    }

    /// Blocking wrapper around [`Self::restore_updates_state`].
    pub fn restore_updates_state_blocking(&self, state: PersistedUpdatesState) -> Result<()> {
        self.owner
            .handle()
            .block_on(self.restore_updates_state(state))
    }

    /// Gracefully stops the session actor (flushing any pending drafts/reads) and joins the
    /// owned Tokio runtime thread if this `SessionRuntime` created it.
    pub fn shutdown(mut self) {
        self.shutdown_inner();
    }

    /// Async graceful shutdown safe to call from inside a Tokio runtime.
    pub async fn shutdown_async(mut self) {
        let (tx, rx) = oneshot::channel();
        if self
            .cmd_tx
            .send(SessionCommand::Shutdown { reply: tx })
            .is_ok()
        {
            let _ = tokio::time::timeout(Duration::from_secs(3), rx).await;
        }
        if let RuntimeOwner::Owned {
            shutdown_tx,
            thread,
            ..
        } = &mut self.owner
        {
            if let Some(stop) = shutdown_tx.take() {
                let _ = stop.send(());
            }
            if let Some(join) = thread.take() {
                let _ = tokio::task::spawn_blocking(move || {
                    let _ = join.join();
                })
                .await;
            }
        }
    }

    fn shutdown_inner(&mut self) {
        let (tx, rx) = oneshot::channel();
        if self
            .cmd_tx
            .send(SessionCommand::Shutdown { reply: tx })
            .is_ok()
        {
            // Only block_on if we are NOT already inside a Tokio runtime thread.
            if Handle::try_current().is_err() {
                self.owner.handle().block_on(async {
                    let _ = tokio::time::timeout(Duration::from_secs(3), rx).await;
                });
            }
        }
        if let RuntimeOwner::Owned {
            shutdown_tx,
            thread,
            ..
        } = &mut self.owner
        {
            if let Some(stop) = shutdown_tx.take() {
                let _ = stop.send(());
            }
            if let Some(join) = thread.take() {
                let _ = join.join();
            }
        }
    }
}

impl<C: RpcClient + Clone + 'static> Drop for SessionRuntime<C> {
    fn drop(&mut self) {
        self.shutdown_inner();
    }
}

fn spawn_owned_tokio_runtime() -> Result<RuntimeOwner> {
    let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel::<Result<Handle>>(1);
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

    let thread = std::thread::Builder::new()
        .name("tg-session-rt".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(err) => {
                    let _ = ready_tx.send(Err(SessionError::RuntimeThread(err.to_string())));
                    return;
                }
            };
            let handle = rt.handle().clone();
            if ready_tx.send(Ok(handle)).is_err() {
                return;
            }
            rt.block_on(async move {
                let _ = shutdown_rx.await;
            });
            rt.shutdown_timeout(Duration::from_secs(2));
        })
        .map_err(|e| SessionError::RuntimeThread(e.to_string()))?;

    let handle = ready_rx
        .recv()
        .map_err(|_| SessionError::RuntimeThread("runtime thread exited before ready".into()))??;

    Ok(RuntimeOwner::Owned {
        handle,
        shutdown_tx: Some(shutdown_tx),
        thread: Some(thread),
    })
}

struct SessionActor<C: RpcClient> {
    rpc: C,
    session: Arc<RwLock<Session>>,
    updates: UpdatesHandler,
    dialogs: DialogsLoader,
    history: HistoryLoader,
    sender: MessageSender,
    read_manager: ReadHistoryManager,
    progress_manager: SendProgressManager,
    draft_manager: DraftManager,
    config: SessionRuntimeConfig,
    cmd_rx: mpsc::UnboundedReceiver<SessionCommand>,
    updates_rx: Option<broadcast::Receiver<tl_enums::Updates>>,
    new_session_rx: Option<broadcast::Receiver<i32>>,
    auth_fail_rx: Option<broadcast::Receiver<tg_api::RpcError>>,
    session_events_tx: broadcast::Sender<SessionEvent>,
    ready: Arc<AtomicBool>,
    started_at: Instant,
    last_tick_wall_secs: TimeId,
    last_tick_mono_ms: u64,
    last_persisted_state: Option<PersistedUpdatesState>,
    ready_result: Option<std::result::Result<(), String>>,
    ready_waiters: Vec<oneshot::Sender<Result<()>>>,
    /// Set when the self user came from local storage: it is refreshed once from the server
    /// after the startup sync, like `Main::Session` requesting the full self peer
    /// (`main_session.cpp:195`), without delaying the first chat list.
    refresh_self_after_startup: bool,
    self_refresh: Option<SelfRefreshFuture>,
}

type SelfRefreshFuture = Pin<
    Box<
        dyn Future<Output = std::result::Result<Vec<tl_enums::User>, tg_api::InvocationError>>
            + Send,
    >,
>;

impl<C: RpcClient + Clone + 'static> SessionActor<C> {
    async fn run(mut self) {
        let startup_res = self.run_startup().await;
        match &startup_res {
            Ok(()) => {
                self.ready.store(true, Ordering::Release);
                self.ready_result = Some(Ok(()));
                let _ = self.session_events_tx.send(SessionEvent::Ready);
                if self.refresh_self_after_startup {
                    let rpc = self.rpc.clone();
                    self.self_refresh = Some(Box::pin(async move {
                        rpc.invoke(&tl_fns::users::GetUsers {
                            id: vec![tl_enums::InputUser::Self_],
                        })
                        .await
                        .map_err(Into::into)
                    }));
                }
            }
            Err(err) => {
                let msg = err.to_string();
                warn!(error = %msg, "session startup sync failed");
                self.ready_result = Some(Err(msg.clone()));
                let _ = self.session_events_tx.send(SessionEvent::SyncError(msg));
            }
        }
        let cached_err = match &startup_res {
            Ok(()) => None,
            Err(e) => Some(e.to_string()),
        };
        for waiter in self.ready_waiters.drain(..) {
            let res = match &cached_err {
                None => Ok(()),
                Some(msg) => Err(SessionError::Api(tg_api::ApiError::ServerError(
                    msg.clone(),
                ))),
            };
            let _ = waiter.send(res);
        }

        let mut tick = tokio::time::interval(self.config.tick_interval);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            let next_updates_deadline = self.updates.next_deadline().map(|deadline_ms| {
                tokio::time::Instant::from_std(self.started_at + Duration::from_millis(deadline_ms))
            });
            tokio::select! {
                cmd = self.cmd_rx.recv() => {
                    match cmd {
                        None => break,
                        Some(SessionCommand::Shutdown { reply }) => {
                            let _ = self.flush_before_shutdown().await;
                            let _ = reply.send(());
                            break;
                        }
                        Some(other) => {
                            self.handle_command(other).await;
                        }
                    }
                }
                upd = recv_optional_broadcast(&mut self.updates_rx), if self.updates_rx.is_some() => {
                    match upd {
                        Some(Ok(updates)) => {
                            let now_ms = self.now_ms();
                            let mut session = self.session.write().await;
                            let mut change_rx = session.subscribe();
                            if let Err(err) = self
                                .updates
                                .handle_updates(&self.rpc, &mut session, updates, None, now_ms)
                                .await
                            {
                                warn!(error = %err, "failed to process incoming update");
                            }
                            increment_unread_for_new_incoming_messages(&mut session, &mut change_rx);
                            drop(session);
                            self.drain_update_events().await;
                            self.persist_updates_state_if_changed();
                        }
                        Some(Err(broadcast::error::RecvError::Lagged(skipped))) => {
                            warn!(skipped, "updates receiver lagged; running getDifference");
                            let now_ms = self.now_ms();
                            let mut session = self.session.write().await;
                            let mut change_rx = session.subscribe();
                            if let Err(err) = self
                                .updates
                                .handle_updates(
                                    &self.rpc,
                                    &mut session,
                                    tl_enums::Updates::TooLong,
                                    None,
                                    now_ms,
                                )
                                .await
                            {
                                warn!(error = %err, "getDifference after lag failed");
                            }
                            increment_unread_for_new_incoming_messages(&mut session, &mut change_rx);
                            drop(session);
                            self.drain_update_events().await;
                            self.persist_updates_state_if_changed();
                        }
                        Some(Err(broadcast::error::RecvError::Closed)) | None => {
                            self.updates_rx = None;
                        }
                    }
                }
                new_sess = recv_optional_broadcast(&mut self.new_session_rx), if self.new_session_rx.is_some() => {
                    match new_sess {
                        Some(Ok(dc_id)) => {
                            if dc_id == self.rpc.main_dc_id() {
                                debug!(dc_id, "main DC new_session_created; calling getDifference");
                                let now_ms = self.now_ms();
                                let mut session = self.session.write().await;
                                let mut change_rx = session.subscribe();
                                if let Err(err) = self
                                    .updates
                                    .on_new_session_created(&self.rpc, &mut session, now_ms)
                                    .await
                                {
                                    warn!(error = %err, "getDifference on new_session_created failed");
                                }
                                increment_unread_for_new_incoming_messages(&mut session, &mut change_rx);
                                drop(session);
                                self.drain_update_events().await;
                                self.persist_updates_state_if_changed();
                            }
                        }
                        Some(Err(broadcast::error::RecvError::Lagged(_))) => {
                            let now_ms = self.now_ms();
                            let mut session = self.session.write().await;
                            let mut change_rx = session.subscribe();
                            let _ = self
                                .updates
                                .on_new_session_created(&self.rpc, &mut session, now_ms)
                                .await;
                            increment_unread_for_new_incoming_messages(&mut session, &mut change_rx);
                            drop(session);
                            self.drain_update_events().await;
                            self.persist_updates_state_if_changed();
                        }
                        Some(Err(broadcast::error::RecvError::Closed)) | None => {
                            self.new_session_rx = None;
                        }
                    }
                }
                auth_fail = recv_optional_broadcast(&mut self.auth_fail_rx), if self.auth_fail_rx.is_some() => {
                    // Match `Main::Account::startMtp` (`main/main_account.cpp:469-473`):
                    // fatal main-DC 401 (`AUTH_KEY_UNREGISTERED`, `SESSION_REVOKED`, etc.)
                    // forces session logout.
                    match auth_fail {
                        Some(Ok(err)) => {
                            warn!(code = err.code, name = %err.name, "fatal main-DC 401 received; emitting ForceLogout");
                            let _ = self.session_events_tx.send(SessionEvent::ForceLogout {
                                notification_type: err.name,
                            });
                        }
                        Some(Err(broadcast::error::RecvError::Lagged(_))) => {
                            let _ = self.session_events_tx.send(SessionEvent::ForceLogout {
                                notification_type: "AUTH_KEY_UNREGISTERED".to_string(),
                            });
                        }
                        Some(Err(broadcast::error::RecvError::Closed)) | None => {
                            self.auth_fail_rx = None;
                        }
                    }
                }
                _ = sleep_until_optional(next_updates_deadline), if next_updates_deadline.is_some() => {
                    self.handle_updates_deadline().await;
                }
                _ = tick.tick() => {
                    self.handle_tick().await;
                }
                refreshed = poll_optional(&mut self.self_refresh), if self.self_refresh.is_some() => {
                    self.self_refresh = None;
                    self.apply_self_refresh(refreshed).await;
                }
            }
        }
    }

    async fn apply_self_refresh(
        &mut self,
        res: Option<std::result::Result<Vec<tl_enums::User>, tg_api::InvocationError>>,
    ) {
        match res {
            Some(Ok(users)) => {
                let mut session = self.session.write().await;
                apply_tl_users_and_chats(&mut session, &users, &[]);
                drop(session);
                self.dialogs.register_peers_from_tl(&users, &[]);
                self.drain_update_events().await;
            }
            Some(Err(err)) => debug!(error = %err, "background users.getUsers(Self) failed"),
            None => {}
        }
    }

    async fn run_startup(&mut self) -> Result<()> {
        let now_ms = self.now_ms();

        // 1. Ensure self user is loaded (so Saved Messages and self name work after resume).
        //    A copy saved in local storage is applied first; otherwise it is requested together
        //    with the updates state and the first dialogs page below (one round trip for all of
        //    them) and applied before the dialogs page.
        //    A stored copy of another user is ignored (`storage_account.cpp:3363`).
        let self_user_id = self.session.read().await.self_user_id();
        let stored_self = self
            .config
            .initial_self_user
            .clone()
            .filter(|user| UserId::from_tl(user.id) == self_user_id);
        if let Some(user) = stored_self {
            let users = vec![tl_enums::User::User(user)];
            let mut session = self.session.write().await;
            apply_tl_users_and_chats(&mut session, &users, &[]);
            drop(session);
            self.dialogs.register_peers_from_tl(&users, &[]);
            self.refresh_self_after_startup = self.config.fetch_self_user_on_start;
        }
        let need_self = self.config.fetch_self_user_on_start && {
            let session = self.session.read().await;
            session.peer(session.self_peer_id()).is_none()
        };

        // 2. Initialize or resume updates state.
        //    - If valid persisted state exists (`pts > 0`), restore it and immediately call
        //      `updates.getDifference(pts, date, qts)` (skipping `updates.getState`).
        //    - On first start (no persisted state), call `updates.getState`, followed by
        //      `updates.getDifference` when `run_get_difference_on_start` is enabled.
        // 4. Load the first page of main dialogs (`limit = 20`) + pinned dialogs.
        // Like tdesktop (`Api::Updates` and `ApiWrap::requestDialogs` start independently), the
        // dialogs requests do not wait for the updates sync: both run concurrently.
        let persisted = if self.config.init_updates_state {
            self.load_initial_persisted_state()
        } else {
            None
        };
        let init_updates_state = self.config.init_updates_state;
        let run_get_difference_on_start = self.config.run_get_difference_on_start;
        let load_initial_dialogs = self.config.load_initial_dialogs;
        let started_at = self.started_at;
        let rpc = &self.rpc;
        let session_lock = &self.session;
        let updates = &mut self.updates;
        let dialogs = &mut self.dialogs;
        let new_session_rx = &mut self.new_session_rx;

        let updates_fut = async move {
            if !init_updates_state {
                // No startup `updates.getState` / `getDifference` is in flight, so clear the
                // initial `requesting` gate so RPC-returned `apply_updates` calls are not silently
                // dropped.
                updates.restore_state(&PersistedUpdatesState::default(), now_ms);
                return Ok(());
            }
            let ran_difference =
                if let Some(saved) = persisted.filter(PersistedUpdatesState::is_valid) {
                    let mut session = session_lock.write().await;
                    updates
                        .resume_from_state(rpc, &mut session, &saved, now_ms)
                        .await?;
                    true
                } else {
                    {
                        // The session stays unlocked while `updates.getState` (and, on a cold
                        // start, the auth key exchange before it) is in flight, so the UI can
                        // read it meanwhile.
                        let state = rpc.updates_get_state().await?;
                        let mut session = session_lock.write().await;
                        updates
                            .initialize_with_state(rpc, &mut session, &state, now_ms)
                            .await?;
                    }
                    // 3. Call `updates.getDifference` on start to catch up any gap since last session.
                    if run_get_difference_on_start {
                        let mut session = session_lock.write().await;
                        let started_ms = started_at.elapsed().as_millis() as u64;
                        updates
                            .on_new_session_created(rpc, &mut session, started_ms)
                            .await?;
                    }
                    run_get_difference_on_start
                };
            // The startup `getDifference` was answered after any `new_session_created` the server
            // sent for this session, so a queued notification would only repeat it.
            if ran_difference && let Some(rx) = new_session_rx.as_mut() {
                while rx.try_recv().is_ok() {}
            }
            Ok::<(), tg_api::ApiError>(())
        };
        let dialogs_fut = async move {
            if load_initial_dialogs {
                dialogs.load_next_page(rpc, 0).await
            } else {
                Ok(None)
            }
        };
        let self_fut = async move {
            if need_self {
                Some(
                    rpc.invoke(&tl_fns::users::GetUsers {
                        id: vec![tl_enums::InputUser::Self_],
                    })
                    .await,
                )
            } else {
                None
            }
        };
        let (updates_res, dialogs_res, self_res) = tokio::join!(updates_fut, dialogs_fut, self_fut);

        match self_res {
            Some(Ok(users)) => {
                let mut session = self.session.write().await;
                apply_tl_users_and_chats(&mut session, &users, &[]);
                drop(session);
                self.dialogs.register_peers_from_tl(&users, &[]);
            }
            Some(Err(err)) => {
                let mapped: tg_api::InvocationError = err.into();
                debug!(error = %mapped, "users.getUsers(Self) skipped on startup");
            }
            None => {}
        }
        self.drain_update_events().await;
        if let Ok(Some(page)) = &dialogs_res {
            self.apply_dialogs_page(page).await;
        }
        self.persist_updates_state_if_changed();
        updates_res?;
        dialogs_res?;
        Ok(())
    }

    async fn handle_command(&mut self, cmd: SessionCommand) {
        match cmd {
            SessionCommand::WaitReady { reply } => {
                if let Some(res) = &self.ready_result {
                    let out = match res {
                        Ok(()) => Ok(()),
                        Err(msg) => Err(SessionError::Api(tg_api::ApiError::ServerError(
                            msg.clone(),
                        ))),
                    };
                    let _ = reply.send(out);
                } else {
                    self.ready_waiters.push(reply);
                }
            }
            SessionCommand::LoadDialogsPage { folder_id, reply } => {
                let res = match self.dialogs.load_next_page(&self.rpc, folder_id).await {
                    Ok(Some(page)) => {
                        self.apply_dialogs_page(&page).await;
                        Ok(Some(page))
                    }
                    Ok(None) => Ok(None),
                    Err(err) => Err(SessionError::Api(err)),
                };
                let _ = reply.send(res);
            }
            SessionCommand::OpenHistory {
                peer_id,
                around_id,
                direction,
                reply,
            } => {
                let input_peer = {
                    let session = self.session.read().await;
                    input_peer_for(&session, peer_id)
                };
                if matches!(input_peer, tl_enums::InputPeer::Empty) {
                    let _ = reply.send(Err(SessionError::InvalidPeer(peer_id)));
                    return;
                }
                let res = match self
                    .history
                    .request_history(&self.rpc, input_peer, around_id, direction)
                    .await
                {
                    Ok(Some(slice)) => {
                        self.dialogs
                            .register_peers_from_tl(&slice.users, &slice.chats);
                        if let Some(ch_id) = peer_id.to_channel()
                            && let Some(pts) = slice.channel_pts
                        {
                            self.updates.init_channel_pts(ch_id, pts);
                            self.persist_updates_state_if_changed();
                        }
                        let mut session = self.session.write().await;
                        slice.apply_to_session(&mut session, peer_id);
                        Ok(Some(slice))
                    }
                    Ok(None) => Ok(None),
                    Err(err) => Err(SessionError::Api(err)),
                };
                let _ = reply.send(res);
            }
            SessionCommand::SendMessage(cmd) => {
                let SendMessageCommand {
                    peer_id,
                    text,
                    entities,
                    reply_to,
                    options,
                    reply,
                } = *cmd;
                let (input_peer, self_user_id, self_peer_id) = {
                    let session = self.session.read().await;
                    (
                        input_peer_for(&session, peer_id),
                        session.self_user_id(),
                        session.self_peer_id(),
                    )
                };
                if matches!(input_peer, tl_enums::InputPeer::Empty) {
                    let _ = reply.send(Err(SessionError::InvalidPeer(peer_id)));
                    return;
                }

                // Match `ApiWrap::sendMessage` (`apiwrap.cpp:4866-4995`):
                // allocate local pending messages and register `random_id -> local_id` in
                // `Session` BEFORE invoking `messages.sendMessage` so both `UpdateMessageID` +
                // `UpdateNewMessage` and `UpdateShortSentMessage` merge into the local message,
                // and RPC failures mark it `SENDING_FAILED` (or destroy it on `MESSAGE_EMPTY`).
                let prepared = self.sender.prepare_text_chunks(peer_id, &text, &entities);
                let now_secs = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs() as TimeId;
                {
                    let mut session = self.session.write().await;
                    for chunk in &prepared {
                        let mut local_msg = Message::new_text(
                            peer_id,
                            chunk.local_id.msg,
                            Some(self_peer_id),
                            now_secs,
                            tg_data::TextWithEntities::new(
                                chunk.text.clone(),
                                entities_from_tl_with_self(&chunk.entities, self_user_id),
                            ),
                        );
                        local_msg
                            .flags
                            .insert(MessageFlags::OUTGOING | MessageFlags::BEING_SENT);
                        local_msg.flags.set(MessageFlags::SILENT, options.silent);
                        local_msg
                            .flags
                            .set(MessageFlags::NO_FORWARDS, options.noforwards);
                        local_msg
                            .flags
                            .set(MessageFlags::INVERT_MEDIA, options.invert_media);
                        session.register_random_id(chunk.random_id, chunk.local_id);
                        session.insert_message(local_msg);
                    }
                }

                let mut out_messages = Vec::with_capacity(prepared.len());
                let mut out_updates = Vec::with_capacity(prepared.len());
                let mut send_err = None;

                for (idx, chunk) in prepared.iter().enumerate() {
                    match self
                        .sender
                        .send_prepared_chunk(
                            &self.rpc,
                            input_peer.clone(),
                            chunk,
                            reply_to.clone(),
                            &options,
                        )
                        .await
                    {
                        Ok((pending, batch)) => {
                            let now_ms = self.now_ms();
                            let mut session = self.session.write().await;
                            if let Err(err) = self
                                .updates
                                .handle_updates(
                                    &self.rpc,
                                    &mut session,
                                    batch.clone(),
                                    Some(chunk.random_id),
                                    now_ms,
                                )
                                .await
                            {
                                warn!(error = %err, "failed to apply sendMessage updates");
                            }
                            out_messages.push(pending);
                            out_updates.push(batch);
                        }
                        Err(kind) => {
                            // Mark this chunk (and any remaining unsent chunks) failed or
                            // destroy on `MESSAGE_EMPTY` (`apiwrap.cpp:545-580,4988-4991`).
                            let mut session = self.session.write().await;
                            for failed_chunk in &prepared[idx..] {
                                session.unregister_random_id(failed_chunk.random_id);
                                if kind == tg_api::SendMessageErrorKind::MessageEmpty {
                                    session.delete_messages(peer_id, &[failed_chunk.local_id.msg]);
                                } else if let Some(msg) = session.message_mut(failed_chunk.local_id)
                                {
                                    msg.flags.remove(MessageFlags::BEING_SENT);
                                    msg.flags.insert(MessageFlags::SENDING_FAILED);
                                }
                            }
                            send_err = Some(kind);
                            break;
                        }
                    }
                }

                if let Some(kind) = send_err {
                    let _ = reply.send(Err(SessionError::SendMessage(kind)));
                } else {
                    self.drain_update_events().await;
                    self.persist_updates_state_if_changed();
                    let _ = reply.send(Ok(SendMessageOutcome {
                        messages: out_messages,
                        updates: out_updates,
                    }));
                }
            }
            SessionCommand::MarkRead {
                peer_id,
                max_id,
                force,
                reply,
            } => {
                let (input_peer, still_unread) = {
                    let session = self.session.read().await;
                    let peer = input_peer_for(&session, peer_id);
                    let unread = session.history(peer_id).and_then(|h| h.unread_count);
                    (peer, unread)
                };
                if matches!(input_peer, tl_enums::InputPeer::Empty) {
                    let _ = reply.send(Err(SessionError::InvalidPeer(peer_id)));
                    return;
                }
                let now = Instant::now();
                let due_now =
                    self.read_manager
                        .read_inbox_till(input_peer, max_id, still_unread, force, now);
                if due_now {
                    match self.read_manager.flush_due(&self.rpc, now).await {
                        Ok(flushed) => {
                            self.apply_flushed_reads(&flushed).await;
                            let _ = reply.send(Ok(flushed));
                        }
                        Err(err) => {
                            let _ = reply.send(Err(SessionError::Api(err)));
                        }
                    }
                } else {
                    let _ = reply.send(Ok(Vec::new()));
                }
            }
            SessionCommand::SendTyping {
                peer_id,
                top_msg_id,
                kind,
                progress,
                reply,
            } => {
                let (input_peer, info) = {
                    let session = self.session.read().await;
                    let peer = input_peer_for(&session, peer_id);
                    let info = build_typing_peer_info(&session, peer_id);
                    (peer, info)
                };
                if matches!(input_peer, tl_enums::InputPeer::Empty) {
                    let _ = reply.send(Err(SessionError::InvalidPeer(peer_id)));
                    return;
                }
                let res = self
                    .progress_manager
                    .update(
                        &self.rpc,
                        SendProgressUpdate {
                            peer: input_peer,
                            info,
                            top_msg_id,
                            kind,
                            progress,
                        },
                        Instant::now(),
                        now_unix_secs(),
                    )
                    .await
                    .map_err(SessionError::Api);
                let _ = reply.send(res);
            }
            SessionCommand::TriggerDifference { reply } => {
                let now_ms = self.now_ms();
                let mut session = self.session.write().await;
                let res = self
                    .updates
                    .on_new_session_created(&self.rpc, &mut session, now_ms)
                    .await
                    .map_err(SessionError::Api);
                drop(session);
                self.drain_update_events().await;
                self.persist_updates_state_if_changed();
                let _ = reply.send(res);
            }
            SessionCommand::SetActiveChannel {
                channel_id,
                active,
                reply,
            } => {
                let now_ms = self.now_ms();
                self.updates.set_active_channel(channel_id, active, now_ms);
                let _ = reply.send(Ok(()));
            }
            SessionCommand::ApplyAffectedHistory {
                peer_id,
                pts,
                pts_count,
                reply,
            } => {
                let now_ms = self.now_ms();
                let mut session = self.session.write().await;
                let mut change_rx = session.subscribe();
                let res = match peer_id.and_then(PeerId::to_channel) {
                    Some(channel_id) => self
                        .updates
                        .apply_channel_pts(
                            &self.rpc,
                            &mut session,
                            channel_id,
                            pts,
                            pts_count,
                            now_ms,
                        )
                        .await
                        .map_err(SessionError::Api),
                    None => self
                        .updates
                        .apply_pts_only(&self.rpc, &mut session, None, pts, pts_count, now_ms)
                        .await
                        .map_err(SessionError::Api),
                };
                increment_unread_for_new_incoming_messages(&mut session, &mut change_rx);
                drop(session);
                self.drain_update_events().await;
                self.persist_updates_state_if_changed();
                let _ = reply.send(res);
            }
            SessionCommand::ApplyUpdates {
                updates,
                sent_random_id,
                reply,
            } => {
                // Match `ApiWrap::applyUpdates` (`apiwrap.cpp:2332-2336`) ->
                // `Api::Updates::applyUpdates` (`api/api_updates.cpp:1518-1560`).
                let now_ms = self.now_ms();
                let mut session = self.session.write().await;
                let mut change_rx = session.subscribe();
                let res = self
                    .updates
                    .handle_updates(&self.rpc, &mut session, *updates, sent_random_id, now_ms)
                    .await
                    .map_err(SessionError::Api);
                increment_unread_for_new_incoming_messages(&mut session, &mut change_rx);
                drop(session);
                self.drain_update_events().await;
                self.persist_updates_state_if_changed();
                let _ = reply.send(res);
            }
            SessionCommand::DeleteMessages {
                peer_id,
                ids,
                revoke,
                reply,
            } => {
                // Match `Data::Histories::deleteMessages` (`data/data_histories.cpp:975-1065`)
                // and `ApiWrap::applyAffectedMessages` (`apiwrap.cpp:2350-2358`).
                let input_peer = {
                    let session = self.session.read().await;
                    input_peer_for(&session, peer_id)
                };
                if matches!(input_peer, tl_enums::InputPeer::Empty) {
                    let _ = reply.send(Err(SessionError::InvalidPeer(peer_id)));
                    return;
                }
                match self
                    .sender
                    .delete_messages(&self.rpc, input_peer, ids.clone(), revoke)
                    .await
                {
                    Ok(affected) => {
                        let now_ms = self.now_ms();
                        let mut session = self.session.write().await;
                        let mut change_rx = session.subscribe();
                        session.delete_messages(peer_id, &ids);
                        let pts_res = match peer_id.to_channel() {
                            Some(channel_id) => self
                                .updates
                                .apply_channel_pts(
                                    &self.rpc,
                                    &mut session,
                                    channel_id,
                                    affected.pts,
                                    affected.pts_count,
                                    now_ms,
                                )
                                .await
                                .map_err(SessionError::Api),
                            None => self
                                .updates
                                .apply_pts_only(
                                    &self.rpc,
                                    &mut session,
                                    None,
                                    affected.pts,
                                    affected.pts_count,
                                    now_ms,
                                )
                                .await
                                .map_err(SessionError::Api),
                        };
                        increment_unread_for_new_incoming_messages(&mut session, &mut change_rx);
                        drop(session);
                        self.drain_update_events().await;
                        self.persist_updates_state_if_changed();
                        let _ = reply.send(pts_res.map(|()| affected));
                    }
                    Err(err) => {
                        let _ = reply.send(Err(SessionError::Api(err)));
                    }
                }
            }
            SessionCommand::EditMessage { req, reply } => {
                // Match `Api::EditMessage` (`api/api_editing.cpp:356, 426`) and
                // `HistoryWidget::saveEdit` (`history/history_widget.cpp:5736`): apply returned
                // `Updates` via `UpdatesHandler`, and treat `MESSAGE_NOT_MODIFIED` (which also
                // occurs when MTProto retries a `500 RPC_CALL_FAIL` whose first attempt committed
                // on the server, per `mtproto/mtp_instance.cpp:1606`) as non-fatal while syncing
                // difference so `Session` converges to the server state.
                let peer_channel = match &req.peer {
                    tl_enums::InputPeer::Channel(ch) => Some(ChannelId::from_tl(ch.channel_id)),
                    _ => None,
                };
                match self.sender.edit_message(&self.rpc, *req).await {
                    Ok(updates) => {
                        let now_ms = self.now_ms();
                        let mut session = self.session.write().await;
                        let mut change_rx = session.subscribe();
                        let apply_res = self
                            .updates
                            .handle_updates(&self.rpc, &mut session, updates.clone(), None, now_ms)
                            .await
                            .map_err(SessionError::Api);
                        increment_unread_for_new_incoming_messages(&mut session, &mut change_rx);
                        drop(session);
                        self.drain_update_events().await;
                        self.persist_updates_state_if_changed();
                        let _ = reply.send(apply_res.map(|()| Some(updates)));
                    }
                    Err(tg_api::ApiError::Invocation(tg_api::InvocationError::Rpc(ref r)))
                        if r.name == "MESSAGE_NOT_MODIFIED" =>
                    {
                        let now_ms = self.now_ms();
                        let mut session = self.session.write().await;
                        let mut change_rx = session.subscribe();
                        let sync_res = if let Some(ch_id) = peer_channel {
                            if self.updates.channel_pts(ch_id).is_some() {
                                let too_long =
                                    tl_enums::Updates::UpdateShort(tl_types::UpdateShort {
                                        update: tl_enums::Update::ChannelTooLong(
                                            tl_types::UpdateChannelTooLong {
                                                channel_id: ch_id.to_tl(),
                                                pts: None,
                                            },
                                        ),
                                        date: now_unix_secs(),
                                    });
                                self.updates
                                    .handle_updates(&self.rpc, &mut session, too_long, None, now_ms)
                                    .await
                                    .map_err(SessionError::Api)
                            } else {
                                Ok(())
                            }
                        } else {
                            self.updates
                                .on_new_session_created(&self.rpc, &mut session, now_ms)
                                .await
                                .map_err(SessionError::Api)
                        };
                        increment_unread_for_new_incoming_messages(&mut session, &mut change_rx);
                        drop(session);
                        self.drain_update_events().await;
                        self.persist_updates_state_if_changed();
                        if let Err(err) = sync_res {
                            warn!(error = %err, "difference sync after MESSAGE_NOT_MODIFIED failed");
                        }
                        let _ = reply.send(Ok(None));
                    }
                    Err(err) => {
                        let _ = reply.send(Err(SessionError::Api(err)));
                    }
                }
            }
            SessionCommand::InitChannelPts {
                channel_id,
                pts,
                reply,
            } => {
                self.updates.init_channel_pts(channel_id, pts);
                self.persist_updates_state_if_changed();
                let _ = reply.send(Ok(()));
            }
            SessionCommand::ApplyChannelPts {
                channel_id,
                pts,
                pts_count,
                reply,
            } => {
                let now_ms = self.now_ms();
                let mut session = self.session.write().await;
                let mut change_rx = session.subscribe();
                let res = self
                    .updates
                    .apply_channel_pts(&self.rpc, &mut session, channel_id, pts, pts_count, now_ms)
                    .await
                    .map_err(SessionError::Api);
                increment_unread_for_new_incoming_messages(&mut session, &mut change_rx);
                drop(session);
                self.drain_update_events().await;
                self.persist_updates_state_if_changed();
                let _ = reply.send(res);
            }
            SessionCommand::RestoreUpdatesState { state, reply } => {
                let now_ms = self.now_ms();
                self.updates.restore_state(&state, now_ms);
                self.persist_updates_state_if_changed();
                let _ = reply.send(Ok(()));
            }
            SessionCommand::OnReconnect {
                restart_sessions,
                reply,
            } => {
                if restart_sessions {
                    self.rpc.restart_sessions();
                }
                let now_ms = self.now_ms();
                let mut session = self.session.write().await;
                let mut change_rx = session.subscribe();
                let res = self
                    .updates
                    .on_reconnect(&self.rpc, &mut session, now_ms)
                    .await
                    .map_err(SessionError::Api);
                // Match `Api::Updates::applyDifferenceSlice` -> `History::addNewMessage`
                // (`api/api_updates.cpp`, `history/history.cpp`): incoming messages recovered
                // via `getDifference` on reconnect must update unread counters too.
                increment_unread_for_new_incoming_messages(&mut session, &mut change_rx);
                drop(session);
                self.drain_update_events().await;
                self.persist_updates_state_if_changed();
                let _ = reply.send(res);
            }
            SessionCommand::OnWakeFromSleep {
                force_resync,
                reply,
            } => {
                let res = self.handle_wake_from_sleep(force_resync).await;
                let _ = reply.send(res);
            }
            SessionCommand::PersistedUpdatesState { reply } => {
                let _ = reply.send(self.updates.export_state());
            }
            SessionCommand::Snapshot { max_dialogs, reply } => {
                let session = self.session.read().await;
                let snapshot = SessionSnapshot {
                    self_user_id: session.self_user_id(),
                    dialogs: collect_dialog_summaries(&session, max_dialogs),
                    pts: self.updates.pts(),
                };
                let _ = reply.send(snapshot);
            }
            SessionCommand::UpdatePresence {
                window_active,
                idle_ms,
                reply,
            } => {
                let now_ms = self.now_ms();
                let last_non_idle_ms = now_ms.saturating_sub(idle_ms);
                let mut session = self.session.write().await;
                let res = self
                    .updates
                    .update_online_timed(
                        &self.rpc,
                        &mut session,
                        window_active,
                        last_non_idle_ms,
                        now_ms,
                        now_unix_secs(),
                        false,
                    )
                    .await
                    .map_err(SessionError::Api);
                let _ = reply.send(res);
            }
            SessionCommand::Shutdown { reply } => {
                let _ = reply.send(());
            }
        }
    }

    async fn handle_updates_deadline(&mut self) {
        let now_ms = self.now_ms();
        {
            let mut session = self.session.write().await;
            let mut change_rx = session.subscribe();
            if let Err(err) = self.updates.on_tick(&self.rpc, &mut session, now_ms).await {
                warn!(error = %err, "updates deadline tick failed");
            }
            increment_unread_for_new_incoming_messages(&mut session, &mut change_rx);
        }
        self.drain_update_events().await;
        self.persist_updates_state_if_changed();
    }

    async fn handle_wake_from_sleep(&mut self, force_resync: bool) -> Result<bool> {
        let now_ms = self.now_ms();
        let need_ping = self.updates.check_last_update(true, now_ms);
        let mut acted = false;
        let mut ping_failed = false;
        if need_ping || force_resync {
            acted = true;
            if let Err(err) = self.rpc.ping().await {
                let mapped: tg_api::InvocationError = err.into();
                warn!(error = %mapped, "post-sleep keepalive ping failed; resyncing updates");
                ping_failed = true;
            } else {
                self.updates.mark_ping_sent(self.now_ms());
            }
        }
        if force_resync || ping_failed {
            let mut session = self.session.write().await;
            let mut change_rx = session.subscribe();
            let res = self
                .updates
                .on_reconnect(&self.rpc, &mut session, self.now_ms())
                .await
                .map_err(SessionError::Api);
            // Match `Api::Updates::applyDifferenceSlice` -> `History::addNewMessage`
            // (`api/api_updates.cpp`, `history/history.cpp`).
            increment_unread_for_new_incoming_messages(&mut session, &mut change_rx);
            drop(session);
            self.drain_update_events().await;
            self.persist_updates_state_if_changed();
            res?;
        }
        Ok(acted)
    }

    async fn handle_tick(&mut self) {
        let now = Instant::now();
        let now_ms = self.now_ms();
        let now_wall = now_unix_secs();

        // Detect OS suspend/sleep by comparing wall-clock jump vs monotonic elapsed time.
        let wall_delta = i64::from(now_wall) - i64::from(self.last_tick_wall_secs);
        let mono_delta_secs = (now_ms.saturating_sub(self.last_tick_mono_ms) / 1_000) as i64;
        let detected_sleep = (wall_delta - mono_delta_secs).abs() >= SLEEP_DETECT_JUMP_SECS;
        self.last_tick_wall_secs = now_wall;
        self.last_tick_mono_ms = now_ms;

        // 1. Advance updates gap / short-poll / backoff timers.
        {
            let mut session = self.session.write().await;
            let mut change_rx = session.subscribe();
            if let Err(err) = self.updates.on_tick(&self.rpc, &mut session, now_ms).await {
                warn!(error = %err, "updates tick failed");
            }
            increment_unread_for_new_incoming_messages(&mut session, &mut change_rx);
        }
        self.drain_update_events().await;

        // 2. Idle keepalive ping (`kNoUpdatesTimeout` / `kNoUpdatesAfterSleepTimeout`).
        if self.config.idle_ping_enabled && self.updates.check_last_update(detected_sleep, now_ms) {
            match self.rpc.ping().await {
                Ok(()) => {
                    self.updates.mark_ping_sent(self.now_ms());
                }
                Err(err) => {
                    let mapped: tg_api::InvocationError = err.into();
                    warn!(error = %mapped, "idle updates keepalive ping failed; triggering resync");
                    let mut session = self.session.write().await;
                    let mut change_rx = session.subscribe();
                    if let Err(resync_err) = self
                        .updates
                        .on_reconnect(&self.rpc, &mut session, self.now_ms())
                        .await
                    {
                        warn!(error = %resync_err, "updates resync after failed ping failed");
                    }
                    // Match `Api::Updates::applyDifferenceSlice` -> `History::addNewMessage`
                    // (`api/api_updates.cpp`, `history/history.cpp`).
                    increment_unread_for_new_incoming_messages(&mut session, &mut change_rx);
                    drop(session);
                    self.drain_update_events().await;
                }
            }
        }

        self.persist_updates_state_if_changed();

        // 3. Flush due inbox read requests.
        if self
            .read_manager
            .next_deadline()
            .is_some_and(|deadline| deadline <= now)
        {
            match self.read_manager.flush_due(&self.rpc, now).await {
                Ok(flushed) if !flushed.is_empty() => {
                    self.apply_flushed_reads(&flushed).await;
                }
                Ok(_) => {}
                Err(err) => {
                    warn!(error = %err, "flushing inbox read requests failed");
                }
            }
        }

        // 4. Flush due cloud drafts.
        if self
            .draft_manager
            .next_deadline()
            .is_some_and(|deadline| deadline <= now)
            && let Err(err) = self.draft_manager.flush_due(&self.rpc, now).await
        {
            warn!(error = %err, "flushing cloud drafts failed");
        }
    }

    async fn flush_before_shutdown(&mut self) {
        let now = Instant::now();
        // Match `Updates::isQuitPrevent` (`api/api_updates.cpp:1088-1095`):
        // if last sent presence was online, send `account.updateStatus(offline: true)` before quitting.
        if self.updates.is_quit_prevent() {
            let _ = self.updates.update_online(&self.rpc, false).await;
        }
        if self.draft_manager.is_quit_prevent() {
            let _ = self.draft_manager.flush_all(&self.rpc, now).await;
        }
        if let Ok(flushed) = self.read_manager.flush_due(&self.rpc, now).await {
            self.apply_flushed_reads(&flushed).await;
        }
        self.persist_updates_state_if_changed();
    }

    fn load_initial_persisted_state(&self) -> Option<PersistedUpdatesState> {
        if let Some(state) = &self.config.initial_updates_state {
            return Some(state.clone());
        }
        let store = self.config.updates_state_store.as_ref()?;
        match store.load() {
            Ok(state) => state,
            Err(err) => {
                warn!(error = %err, "failed to load persisted updates state; starting fresh");
                None
            }
        }
    }

    fn persist_updates_state_if_changed(&mut self) {
        let Some(store) = self.config.updates_state_store.as_ref() else {
            return;
        };
        let current = self.updates.export_state();
        if !current.is_valid() {
            return;
        }
        if self.last_persisted_state.as_ref() == Some(&current) {
            return;
        }
        match store.save(&current) {
            Ok(()) => {
                self.last_persisted_state = Some(current);
            }
            Err(err) => {
                warn!(error = %err, "failed to persist updates state");
            }
        }
    }

    async fn apply_dialogs_page(&mut self, page: &DialogsPageResult) {
        let now_unix = now_unix_secs();
        let mut session = self.session.write().await;
        page.apply_to_session(&mut session, now_unix);
        drop(session);

        // Initialize per-channel PTS watermarks from every dialog in this page.
        let init_from_dialogs = |updates: &mut UpdatesHandler, dialogs: &[tl_enums::Dialog]| {
            for dialog in dialogs {
                if let tl_enums::Dialog::Dialog(d) = dialog
                    && let tl_enums::Peer::Channel(ch) = &d.peer
                    && let Some(pts) = d.pts
                {
                    updates.init_channel_pts(ChannelId::from_tl(ch.channel_id), pts);
                }
            }
        };
        init_from_dialogs(&mut self.updates, &page.dialogs);
        if let Some(pinned) = &page.pinned {
            init_from_dialogs(&mut self.updates, &pinned.dialogs);
        }
        self.persist_updates_state_if_changed();
    }

    async fn apply_flushed_reads(&mut self, flushed: &[FlushedRead]) {
        let now_ms = self.now_ms();
        let mut session = self.session.write().await;
        for item in flushed {
            // Match `Data::Histories::readInboxTill` (`data/data_histories.cpp:273-284`):
            // only force `still_unread = Some(0)` when reading up to or past the known top
            // message; otherwise let `History::inbox_read_with_filter` count remaining
            // unread incoming messages locally.
            let still_unread = session
                .history(item.peer_id)
                .and_then(|h| h.top_message_id)
                .filter(|&top| item.max_id >= top)
                .map(|_| 0);
            session.mark_inbox_read(item.peer_id, item.max_id, still_unread);
            if let Some(affected) = &item.affected {
                let _ = self
                    .updates
                    .apply_affected_messages(&self.rpc, &mut session, None, affected, now_ms)
                    .await;
            }
        }
        drop(session);
        self.drain_update_events().await;
        self.persist_updates_state_if_changed();
    }

    async fn drain_update_events(&mut self) {
        let mut needs_dialog_refetch = false;
        for event in self.updates.drain_events() {
            match event {
                UpdateEvent::MessageIdMapped {
                    random_id,
                    server_id,
                } => {
                    let _ = self.sender.resolve_update_message_id(random_id, server_id);
                }
                UpdateEvent::ChannelTooLong { channel_id, pts } => {
                    let _ = self
                        .session_events_tx
                        .send(SessionEvent::ChannelTooLong { channel_id, pts });
                }
                UpdateEvent::DifferenceTooLong { pts } => {
                    needs_dialog_refetch = true;
                    let _ = self
                        .session_events_tx
                        .send(SessionEvent::DifferenceTooLong { pts });
                }
                UpdateEvent::ForceLogout { notification_type } => {
                    let _ = self
                        .session_events_tx
                        .send(SessionEvent::ForceLogout { notification_type });
                }
                UpdateEvent::LangPackDifference(diff) => {
                    let _ = self
                        .session_events_tx
                        .send(SessionEvent::LangPackDifference(diff));
                }
                UpdateEvent::LangPackTooLong { lang_code } => {
                    let _ = self
                        .session_events_tx
                        .send(SessionEvent::LangPackTooLong { lang_code });
                }
                UpdateEvent::StoryUpdate(update) => {
                    let _ = self
                        .session_events_tx
                        .send(SessionEvent::StoryUpdate(update));
                }
                UpdateEvent::NewAuthorization(auth) => {
                    let _ = self
                        .session_events_tx
                        .send(SessionEvent::NewAuthorization(auth));
                }
                UpdateEvent::NotifySettingsUpdated { peer, settings } => {
                    let _ = self
                        .session_events_tx
                        .send(SessionEvent::NotifySettingsUpdated { peer, settings });
                }
                UpdateEvent::UserFullRefreshNeeded { user_id } => {
                    let _ = self
                        .session_events_tx
                        .send(SessionEvent::UserFullRefreshNeeded { user_id });
                }
                UpdateEvent::StickerSets { masks, emojis } => {
                    let _ = self
                        .session_events_tx
                        .send(SessionEvent::StickerSets { masks, emojis });
                }
                UpdateEvent::StickerSetsOrder {
                    masks,
                    emojis,
                    order,
                } => {
                    let _ = self.session_events_tx.send(SessionEvent::StickerSetsOrder {
                        masks,
                        emojis,
                        order,
                    });
                }
                UpdateEvent::NewStickerSet(ss) => {
                    let _ = self.session_events_tx.send(SessionEvent::NewStickerSet(ss));
                }
                UpdateEvent::RecentStickers => {
                    let _ = self.session_events_tx.send(SessionEvent::RecentStickers);
                }
                UpdateEvent::FavedStickers => {
                    let _ = self.session_events_tx.send(SessionEvent::FavedStickers);
                }
                UpdateEvent::ReadFeaturedStickers => {
                    let _ = self
                        .session_events_tx
                        .send(SessionEvent::ReadFeaturedStickers);
                }
                UpdateEvent::SavedGifs => {
                    let _ = self.session_events_tx.send(SessionEvent::SavedGifs);
                }
                UpdateEvent::RecentReactions => {
                    let _ = self.session_events_tx.send(SessionEvent::RecentReactions);
                }
                UpdateEvent::SavedReactionTags => {
                    let _ = self.session_events_tx.send(SessionEvent::SavedReactionTags);
                }
                UpdateEvent::RecentEmojiStatuses => {
                    let _ = self
                        .session_events_tx
                        .send(SessionEvent::RecentEmojiStatuses);
                }
                UpdateEvent::MoveStickerSetToTop {
                    masks,
                    emojis,
                    stickerset,
                } => {
                    let _ = self
                        .session_events_tx
                        .send(SessionEvent::MoveStickerSetToTop {
                            masks,
                            emojis,
                            stickerset,
                        });
                }
                UpdateEvent::Config => {
                    let _ = self.session_events_tx.send(SessionEvent::Config);
                }
                UpdateEvent::AttachMenuBots => {
                    let _ = self.session_events_tx.send(SessionEvent::AttachMenuBots);
                }
                UpdateEvent::SavedRingtones => {
                    let _ = self.session_events_tx.send(SessionEvent::SavedRingtones);
                }
                UpdateEvent::Theme(theme) => {
                    let _ = self.session_events_tx.send(SessionEvent::Theme(theme));
                }
                UpdateEvent::QuickReplies(list) => {
                    let _ = self
                        .session_events_tx
                        .send(SessionEvent::QuickReplies(list));
                }
                UpdateEvent::LoginToken
                | UpdateEvent::CallUpdate(_)
                | UpdateEvent::SelfOtherOffline { .. }
                | UpdateEvent::ForumTopicReloadNeeded { .. }
                | UpdateEvent::ForumTopicsReloadNeeded { .. }
                | UpdateEvent::SavedPinnedReloadNeeded
                | UpdateEvent::SavedReactionTagsInvalidated
                | UpdateEvent::ScheduledMessagesSent { .. }
                | UpdateEvent::SentConvertedToScheduled { .. }
                | UpdateEvent::QuickRepliesReloadNeeded
                | UpdateEvent::QuickReplyMessagesReloadNeeded { .. }
                | UpdateEvent::ShortcutIdChanged(_)
                | UpdateEvent::PrivacyUpdated { .. } => {}
            }
        }
        if needs_dialog_refetch {
            // `updates.differenceTooLong`: reset main dialog pagination and refetch dialogs
            // so dialog unread counts, top messages, and channel PTSs resynchronize with the server.
            self.dialogs.reset_folder(0);
            match self.dialogs.load_next_page(&self.rpc, 0).await {
                Ok(Some(page)) => {
                    self.apply_dialogs_page(&page).await;
                }
                Ok(None) => {}
                Err(err) => {
                    warn!(error = %err, "failed to refetch dialogs after updates.differenceTooLong");
                }
            }
        }
    }

    fn now_ms(&self) -> u64 {
        self.started_at.elapsed().as_millis() as u64
    }
}

async fn poll_optional<F: Future + Unpin>(fut: &mut Option<F>) -> Option<F::Output> {
    match fut {
        Some(f) => Some(f.await),
        None => None,
    }
}

async fn recv_optional_broadcast<T: Clone>(
    rx: &mut Option<broadcast::Receiver<T>>,
) -> Option<std::result::Result<T, broadcast::error::RecvError>> {
    match rx {
        Some(r) => Some(r.recv().await),
        None => None,
    }
}

async fn sleep_until_optional(deadline: Option<tokio::time::Instant>) {
    if let Some(when) = deadline {
        tokio::time::sleep_until(when).await;
    }
}

fn now_unix_secs() -> TimeId {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs().min(i32::MAX as u64) as TimeId)
}

fn build_typing_peer_info(session: &Session, peer_id: PeerId) -> TypingPeerInfo {
    let is_self = peer_id == session.self_peer_id();
    match session.peer(peer_id) {
        Some(Peer::User(u)) => TypingPeerInfo {
            peer_id,
            is_self,
            is_broadcast_channel: false,
            is_bot: u.is_bot(),
            is_support: u.is_support(),
            lastseen_recently: u.lastseen.is_recently(),
            online_till: u.lastseen.online_till(),
        },
        Some(Peer::Channel(ch)) => TypingPeerInfo {
            peer_id,
            is_self: false,
            is_broadcast_channel: ch.is_broadcast(),
            is_bot: false,
            is_support: false,
            lastseen_recently: true,
            online_till: 0,
        },
        Some(Peer::Chat(_)) | None => TypingPeerInfo::group(peer_id),
    }
}

fn collect_dialog_summaries(session: &Session, limit: usize) -> Vec<DialogSummary> {
    session
        .main_dialogs()
        .iter()
        .take(limit)
        .map(|&key| match key {
            DialogKey::History(peer_id) => {
                let title = if peer_id == session.self_peer_id() {
                    "Saved Messages".to_owned()
                } else {
                    session
                        .peer(peer_id)
                        .map(|p| {
                            let name = p.name();
                            if name.is_empty() {
                                format!("Peer({})", peer_id.0)
                            } else {
                                name.to_owned()
                            }
                        })
                        .unwrap_or_else(|| format!("Peer({})", peer_id.0))
                };
                let (unread_count, unread_mark, muted, top_message_id) =
                    match session.history(peer_id) {
                        Some(h) => (
                            h.unread_count.unwrap_or(0),
                            h.unread_mark,
                            h.muted,
                            h.top_message_id,
                        ),
                        None => (0, false, false, None),
                    };
                let top_message_preview = top_message_id
                    .and_then(|msg_id| session.message(FullMsgId::new(peer_id, msg_id)))
                    .map(format_message_preview)
                    .unwrap_or_default();
                DialogSummary {
                    key,
                    peer_id: Some(peer_id),
                    title,
                    unread_count,
                    unread_mark,
                    muted,
                    top_message_id,
                    top_message_preview,
                }
            }
            DialogKey::Folder(folder_id) => DialogSummary {
                key,
                peer_id: None,
                title: if folder_id == tg_data::ARCHIVE_FOLDER_ID {
                    "Archived Chats".to_owned()
                } else {
                    format!("Folder {folder_id}")
                },
                unread_count: 0,
                unread_mark: false,
                muted: false,
                top_message_id: None,
                top_message_preview: String::new(),
            },
            DialogKey::ForumTopic { peer, root_id } => DialogSummary {
                key,
                peer_id: Some(peer),
                title: format!("Topic #{}", root_id.0),
                unread_count: 0,
                unread_mark: false,
                muted: false,
                top_message_id: None,
                top_message_preview: String::new(),
            },
            DialogKey::SavedSublist { parent, peer } => DialogSummary {
                key,
                peer_id: Some(peer),
                title: format!("Saved Sublist ({})", parent.0),
                unread_count: 0,
                unread_mark: false,
                muted: false,
                top_message_id: None,
                top_message_preview: String::new(),
            },
        })
        .collect()
}

fn collect_last_messages(session: &Session, peer_id: PeerId, limit: usize) -> Vec<Message> {
    let Some(history) = session.history(peer_id) else {
        return Vec::new();
    };
    let mut ids: Vec<MsgId> = history
        .loaded_ranges
        .slices()
        .iter()
        .flat_map(|slice| slice.messages.iter().copied())
        .collect();
    ids.sort();
    ids.dedup();
    let start = ids.len().saturating_sub(limit);
    ids[start..]
        .iter()
        .filter_map(|&msg_id| session.message(FullMsgId::new(peer_id, msg_id)).cloned())
        .collect()
}

/// Formats a concise single-line preview of a [`tg_data::Message`] for logs, summaries, and CLI output.
#[must_use]
pub fn format_message_preview(msg: &Message) -> String {
    match &msg.content {
        tg_data::MessageContent::Regular { text, media } => {
            let trimmed = text.text.trim();
            if !trimmed.is_empty() {
                trimmed.replace(['\r', '\n'], " ")
            } else {
                match media {
                    tg_data::MessageMedia::Empty => "<empty>".to_owned(),
                    tg_data::MessageMedia::Photo(_) => "[Photo]".to_owned(),
                    tg_data::MessageMedia::Document(doc) => {
                        if doc.file_name.is_empty() {
                            format!("[{:?}]", doc.kind)
                        } else {
                            format!("[{:?}: {}]", doc.kind, doc.file_name)
                        }
                    }
                    tg_data::MessageMedia::WebPage(_) => "[WebPage]".to_owned(),
                    tg_data::MessageMedia::Location(_)
                    | tg_data::MessageMedia::LiveLocation(_)
                    | tg_data::MessageMedia::Venue(_) => "[Location]".to_owned(),
                    tg_data::MessageMedia::Contact(c) => {
                        format!("[Contact: {}]", c.first_name)
                    }
                    tg_data::MessageMedia::Poll(p) => format!("[Poll: {}]", p.question.text),
                    tg_data::MessageMedia::Dice(d) => format!("[Dice: {}]", d.emoticon),
                    other => format!("[{other:?}]").chars().take(48).collect(),
                }
            }
        }
        tg_data::MessageContent::Service(action) => {
            let raw = format!("[Service: {action:?}]");
            raw.chars().take(64).collect()
        }
        tg_data::MessageContent::Empty => "<empty>".to_owned(),
    }
}

/// Initializes per-history `unread_count` for newly added incoming regular messages when the
/// history's `unread_count` was not yet known (`None`) prior to receiving live updates.
///
/// Matches `History::addNewMessage` (`history/history.cpp:1717-1723`): when `unread_count` is
/// already `Some(n)`, [`tg_data::Session::insert_message`] has already incremented it once per
/// new unread incoming message, so incrementing again here would double-count (`0 -> 2` for one
/// message).
fn increment_unread_for_new_incoming_messages(
    session: &mut Session,
    change_rx: &mut broadcast::Receiver<ChangeEvent>,
) {
    let mut new_ids: Vec<FullMsgId> = Vec::new();
    while let Ok(ev) = change_rx.try_recv() {
        if let ChangeEvent::MessageUpdated { id, flags } = ev
            && flags.contains(MessageUpdateFlags::NEW_ADDED)
        {
            new_ids.push(id);
        }
    }
    if new_ids.is_empty() {
        return;
    }
    let mut uninitialized_peers = std::collections::HashSet::new();
    for id in &new_ids {
        if session
            .history(id.peer)
            .is_some_and(|h| h.unread_count.is_none())
        {
            uninitialized_peers.insert(id.peer);
        }
    }
    if uninitialized_peers.is_empty() {
        return;
    }
    for id in new_ids {
        if !uninitialized_peers.contains(&id.peer) {
            continue;
        }
        let should_increment = session
            .message(id)
            .is_some_and(|msg| (!msg.is_outgoing() || msg.is_post()) && msg.is_regular())
            && session
                .history(id.peer)
                .is_some_and(|h| id.msg > h.inbox_read_till_id());
        if should_increment {
            let next = session
                .history(id.peer)
                .and_then(|h| h.unread_count)
                .unwrap_or(0)
                .saturating_add(1);
            session.set_history_unread_count(id.peer, next);
        }
    }
}
