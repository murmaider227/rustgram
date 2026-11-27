//! `LiveSession`: implements all `tg-app` screen backend traits over `tg-session`'s
//! [`SessionRuntime`], `tg-data`'s [`Session`], `tg-api`'s [`RpcClient`], and `tg-media`/`tg-video`.
//!
//! Port origin: `Telegram/SourceFiles/main/main_session.cpp`, `apiwrap.cpp`,
//! `api/api_user_privacy.cpp`, `api/api_authorizations.cpp`, `api/api_editing.cpp`,
//! `api/api_chat_filters.cpp`, and `data/data_session.cpp`.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::anyhow;
use tg_api::convert::{apply_tl_users_and_chats, input_peer_for, input_user_for};
use tg_api::{Invoker, LoadDirection, RpcClient, SendOptions};
use tg_audio::{AudioDataSource, AudioMsgId, PlaylistProvider};
use tg_chat::autocomplete::RecentHashtags;
use tg_chat::emoji::{EmojiKeywords, EmojiVariants, LegacyEmojiCompleter, RecentEmojiStore};
use tg_chat::{BotCommand, UserId as ChatUserId, UserInfo};
use tg_data::rights::{ChatAdminRights, ChatBannedRights};
use tg_data::{
    ARCHIVE_FOLDER_ID, BlockStatus, ChatFilter, ChatFilterFlags, DialogKey, Draft, DraftKey,
    FolderId, FullMsgId as DataFullMsgId, MessageContent, MessageMedia, MsgId as DataMsgId, Peer,
    PeerId as DataPeerId, ReplyInfo, Session, TextWithEntities as DataText, WebPageDraft,
};
use tg_history::LocalDateTime;
use tg_lang::LangPackLanguageInfo;
use tg_session::{MtprotoRpc, SessionRuntime};
use tg_text::TextWithEntities;
use tg_tl::{enums as tl_enums, functions as tl_fns, types as tl_types};
use tg_video::RangeLoader;
use tracing::{debug, warn};

use crate::bridge::{LiveBridgeState, LiveRuntimeBridge};
use crate::convert::{
    chat_filter_to_dialog_folder, chat_filter_to_settings_folder, dialog_entry_from_session,
    dialog_item_from_session, draft_from_history, from_data_msg, from_data_peer,
    history_messages_ordered, history_typing_to_send_progress, known_peer_ids,
    message_to_audio_track_metadata, message_to_history_message,
    message_to_history_message_with_stickers, message_to_stand_in_item,
    message_to_viewer_media_item, now_unix_i32, peer_dialog_kind, peer_display_title,
    peer_status_text, peer_to_auth_user, peer_to_history_peer_info, peer_to_info_profile,
    peer_to_notify_info, peer_to_peer_summary, peer_to_settings_ref, privacy_key_to_tl,
    privacy_rule_to_tl, privacy_rules_from_tl, self_profile_from_session, to_data_msg,
    to_data_peer, ui_text_to_data_text, ui_text_to_tl_entities, unix_to_local_datetime,
};
use crate::media::{
    LiveAudioDataSource, LivePlaylistProvider, ResolvedMediaLocation, TelegramMediaInvoker,
    TelegramRangeLoader, block_on_runtime, download_file_bytes, message_media_to_location,
    with_rpc_timeout,
};
use crate::media_cache::{
    LiveMediaCache, document_cache_key, document_thumb_cache_key, photo_cache_key,
};
use crate::parallel_rpc::{joined, spawn_rpc};
use tg_app_core::core_types::dto::inline_media::InlineMediaDescriptor;
use tg_app_core::core_types::dto::rich_compose::RichThreadDraft;
use tg_app_core::core_types::erased_rpc::ErasedRpcClient;
use tg_app_core::dialogs::backend::{
    ArchiveSummary, ChatFolder as DialogChatFolder, DialogItem, DialogListQuery, DialogSlice,
    DialogsBackend, RecentPeerItem, SearchRequest, SearchResults, SearchTagChip, TopPeerItem,
};
use tg_app_core::history::backend::{
    AvailableMessageEffect, AvailableReactionItem, DraftMessage, HistoryBackend, HistoryLiveUpdate,
    HistoryMessage, HistoryPeerInfo, HistorySlice, Loadable, MediaUploadStatus,
    MessageReactionsPage, PinnedMessageSummary, PollVotesPage, ReactionCounter,
    SavedReactionTagItem, SendMessageExtended, SendMessageRequest, ThreadDraft, TypingAction,
};
use tg_app_core::history::sections_model::{
    AdminLogAdmin, AdminLogEntry, AdminLogFilter, CreatePollRequest, ForumTopic, ForwardOptions,
    SavedSublist, ScheduleTarget, ThreadKey,
};
use tg_app_core::history::sections_model::{LinkPreviewData, SendAsPeer, SlowModeState};
use tg_app_core::info::backend::{
    CommonGroupEntry, CreateChannelRequest, CreateGroupRequest, InfoBackend, LanguageEntry,
    MediaItem as InfoMediaItem, MemberEntry, PeerProfile as InfoPeerProfile, ReportReason,
    SharedMediaTab, TermsOfServiceData, UpdatePeerInfoRequest,
};
use tg_app_core::info::values::UsernameAvailability;
use tg_app_core::media::backend::{
    AudioTrackMetadata, MediaBackend, MediaItem as ViewerMediaItem, MediaItemId,
};
use tg_app_core::notifications::backend::{IncomingNotifyEvent, NotifyBackend, NotifyPeerInfo};
use tg_app_core::notifications::settings::{NotifySettingsStore, UnreadBadgeState};
use tg_app_core::settings::backend::{
    ActiveSession, AutoDownloadSettings, Birthday as SettingsBirthday,
    ChatFolder as SettingsChatFolder, CloudPasswordError, CloudPasswordResetOutcome,
    CloudPasswordState, ExportRequest, GlobalPrivacySettings, LocalStorageStats,
    NotificationEventsSettings, NotifyScope, PeerRef, PrivacyKey, PrivacyRule, ProfileInfo,
    ProfilePhotoUpload, ProxyCheckStatus, SavedRingtone, ScopeNotifySettings, SettingsBackend,
    SettingsRead, StickerSetInfo, SuggestedFolder, UsernameStatus, WebAuthorization,
    validate_username_syntax,
};
use tg_app_core::stand_in::backend::Backend;
use tg_app_core::stand_in::types::{
    AuthUser, CodeSentInfo, CodeVerifyOutcome, ConnectionStatus, DialogEntry, FilterId,
    FolderFilter, MessageItem, MsgId, PeerId, PeerSummary,
};

/// Default timeout for synchronous RPC invocations triggered from UI trait methods.
pub(crate) const LIVE_RPC_TIMEOUT: Duration = Duration::from_secs(10);

/// Main dialog folder constant (`0`).
const MAIN_FOLDER_ID: FolderId = 0;

pub use tg_app_core::core_types::session_ids::PendingChatOpen;

/// Shared mutable cache synchronized between all clones of [`LiveSession`].
pub(super) struct LiveSessionCache {
    pub(super) proxy_enabled: bool,
    pub(super) archive_collapsed: bool,
    pub(super) phone_hidden: bool,
    pub(super) photo_color_index: Option<u8>,
    pub(super) photo_label: Option<String>,
    active_language: String,
    // Lazy RPC fetch guards so egui's 60fps rendering doesn't spam identical read RPCs.
    dialogs_loaded_folders: HashSet<FolderId>,
    /// `true` while [`LiveSession::start_chats_list_request`]'s first main-list page is in flight.
    main_dialogs_pending: bool,
    /// Background loading of the remaining chat-list pages ([`crate::dialogs_loading`]).
    pub(crate) dialogs_preload: crate::dialogs_loading::DialogsPreload,
    /// Requests started together for opened chats ([`crate::open_chat_prefetch`]).
    pub(super) open_chat: crate::open_chat_prefetch::OpenChatRequests,
    history_loaded_peers: HashSet<DataPeerId>,
    /// Background history requests started by `try_load_slice_*`, by peer, direction and anchor.
    history_requests: HashMap<HistoryRequestKey, HistoryRequestState>,
    /// Chats with a background `messages.getMessagesReactions` poll running.
    reaction_polls: HashSet<DataPeerId>,
    filters_loaded: bool,
    /// Set while the background `messages.getDialogFilters` runs; settings screens wait on it.
    filters_loading: Option<Arc<FiltersLoad>>,
    privacy_loaded: HashSet<PrivacyKey>,
    privacy_rules: BTreeMap<PrivacyKey, PrivacyRule>,
    pub(super) global_privacy_loaded: bool,
    pub(super) global_privacy: GlobalPrivacySettings,
    pub(super) global_privacy_extra: Option<crate::settings_sync::GlobalPrivacyExtra>,
    pub(super) account_ttl_loaded: bool,
    pub(super) account_ttl_days: i32,
    pub(super) sessions_loaded: bool,
    pub(super) active_sessions: Vec<ActiveSession>,
    pub(super) session_ttl_days: i32,
    pub(super) blocked_loaded: bool,
    pub(super) blocked_peers: Vec<PeerRef>,
    pub(super) cloud_password_loaded: bool,
    pub(super) cloud_password: CloudPasswordState,
    /// Settings reads answered in the background ([`crate::settings_reads`], TGB-3070).
    pub(crate) settings_reads: crate::settings_reads::SettingsReads,
    pub(super) scope_notify_loaded: HashSet<NotifyScope>,
    pub(super) notify_exceptions_loaded: HashSet<NotifyScope>,
    pub(super) notify_exception_peers: HashSet<DataPeerId>,
    pub(super) scope_notify: BTreeMap<NotifyScope, ScopeNotifySettings>,
    pub(super) notify_settings_version: u64,
    /// Per-type notification defaults load ([`crate::notify_sync`]).
    pub(crate) notify_defaults_load: crate::notify_sync::NotifyDefaultsLoad,
    pub(super) notify_events_loaded: bool,
    pub(super) notify_events: NotificationEventsSettings,
    pub(super) auto_download_loaded: bool,
    /// `true` while an `account.getAutoDownloadSettings` request is in flight.
    pub(super) auto_download_fetching: bool,
    pub(super) auto_download_from_tdata: bool,
    pub(super) auto_download_presets: Option<crate::settings_sync::ServerAutoDownloadPresets>,
    pub(super) active_auto_download_preset: crate::settings_sync::AutoDownloadPreset,
    pub(super) auto_download: AutoDownloadSettings,
    pub(super) trusted_mini_app_bots: BTreeSet<i64>,
    pub(super) session_settings_dirty: bool,
    recent_search_peers: Vec<PeerId>,
    removed_top_peers: HashSet<PeerId>,
    recent_hashtags_list: Vec<String>,
    pub(super) dismissed_contact_banners: HashSet<PeerId>,
    pending_chat_opens: VecDeque<PendingChatOpen>,
    next_random_id: u64,
}

impl LiveSessionCache {
    /// The blocked list changed locally: reload it, and drop an answer already on its way.
    pub(crate) fn invalidate_blocked_users(&mut self) {
        self.blocked_loaded = false;
        self.settings_reads.invalidate(SettingsRead::BlockedUsers);
    }

    fn new() -> Self {
        Self {
            proxy_enabled: false,
            archive_collapsed: false,
            phone_hidden: false,
            photo_color_index: None,
            photo_label: None,
            active_language: tg_lang::default_language_id().to_owned(),
            dialogs_loaded_folders: HashSet::new(),
            main_dialogs_pending: false,
            dialogs_preload: crate::dialogs_loading::DialogsPreload::default(),
            open_chat: crate::open_chat_prefetch::OpenChatRequests::default(),
            history_loaded_peers: HashSet::new(),
            history_requests: HashMap::new(),
            reaction_polls: HashSet::new(),
            filters_loaded: false,
            filters_loading: None,
            privacy_loaded: HashSet::new(),
            privacy_rules: BTreeMap::new(),
            global_privacy_loaded: false,
            global_privacy: GlobalPrivacySettings::default(),
            global_privacy_extra: None,
            account_ttl_loaded: false,
            account_ttl_days: 180,
            sessions_loaded: false,
            active_sessions: Vec::new(),
            session_ttl_days: 180,
            blocked_loaded: false,
            blocked_peers: Vec::new(),
            cloud_password_loaded: false,
            cloud_password: CloudPasswordState::default(),
            settings_reads: crate::settings_reads::SettingsReads::default(),
            scope_notify_loaded: HashSet::new(),
            notify_exceptions_loaded: HashSet::new(),
            notify_exception_peers: HashSet::new(),
            scope_notify: BTreeMap::new(),
            notify_settings_version: 0,
            notify_defaults_load: crate::notify_sync::NotifyDefaultsLoad::default(),
            notify_events_loaded: false,
            notify_events: NotificationEventsSettings::default(),
            auto_download_loaded: false,
            auto_download_fetching: false,
            auto_download_from_tdata: false,
            auto_download_presets: None,
            active_auto_download_preset: crate::settings_sync::AutoDownloadPreset::Medium,
            auto_download: AutoDownloadSettings::default(),
            trusted_mini_app_bots: BTreeSet::new(),
            session_settings_dirty: false,
            recent_search_peers: Vec::new(),
            removed_top_peers: HashSet::new(),
            recent_hashtags_list: Vec::new(),
            dismissed_contact_banners: HashSet::new(),
            pending_chat_opens: VecDeque::new(),
            next_random_id: 1,
        }
    }

    fn next_random_id(&mut self) -> i64 {
        let now = now_unix_i32() as u64;
        self.next_random_id = self.next_random_id.wrapping_add(1);
        ((now << 24) ^ self.next_random_id ^ 0x5A5A_1234_ABCD) as i64
    }
}

/// Completion signal of the background folder load ([`LiveSession::ensure_filters_loaded`]).
#[derive(Default)]
struct FiltersLoad {
    done: Mutex<bool>,
    signal: std::sync::Condvar,
}

impl FiltersLoad {
    fn finish(&self) {
        if let Ok(mut done) = self.done.lock() {
            *done = true;
        }
        self.signal.notify_all();
    }

    fn wait(&self) {
        if let Ok(guard) = self.done.lock() {
            let _ = self
                .signal
                .wait_timeout_while(guard, LIVE_RPC_TIMEOUT, |done| !*done);
        }
    }
}

/// A history page request: peer, direction (0 around, 1 before, 2 after) and the anchor the UI
/// asked with.
type HistoryRequestKey = (DataPeerId, u8, MsgId);

/// State of a background history page request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HistoryRequestState {
    /// `open_history` is running.
    InFlight,
    /// It finished (or failed); the next read builds the slice from what is loaded.
    Done,
}

const fn direction_code(direction: LoadDirection) -> u8 {
    match direction {
        LoadDirection::Around => 0,
        LoadDirection::Before => 1,
        LoadDirection::After => 2,
    }
}

/// Local view cache supporting `&T` / `&[T]` return types in `ChatHelpersBackend`.
#[derive(Clone)]
pub(super) struct LiveChatHelpersView {
    pub(super) recent_emoji: Arc<Mutex<RecentEmojiStore>>,
    pub(super) emoji_keywords: Arc<Mutex<EmojiKeywords>>,
    pub(super) emoji_variants: EmojiVariants,
    pub(super) recent_hashtags: RecentHashtags,
    pub(super) autocomplete_users: Vec<UserInfo>,
    pub(super) autocomplete_last_authors: Vec<ChatUserId>,
    pub(super) autocomplete_bot_commands: HashMap<ChatUserId, Vec<BotCommand>>,
    pub(super) recent_inline_bots: Vec<ChatUserId>,
    pub(super) is_premium: bool,
    pub(super) state: Arc<crate::chat_helpers::LiveChatHelpersState>,
}

impl LiveChatHelpersView {
    fn new() -> Self {
        let keywords = EmojiKeywords::with_legacy(LegacyEmojiCompleter::with_builtin_all());
        Self {
            recent_emoji: Arc::new(Mutex::new(RecentEmojiStore::new())),
            emoji_keywords: Arc::new(Mutex::new(keywords)),
            emoji_variants: EmojiVariants::default(),
            recent_hashtags: RecentHashtags::new(),
            autocomplete_users: Vec::new(),
            autocomplete_last_authors: Vec::new(),
            autocomplete_bot_commands: HashMap::new(),
            recent_inline_bots: Vec::new(),
            is_premium: false,
            state: Arc::new(crate::chat_helpers::LiveChatHelpersState::new()),
        }
    }
}

/// Starts a [`SessionRuntime<ErasedRpcClient>`] backed by [`MtprotoRpc`] on `handle`,
/// preserving the `Updates` bridge, `new_session_created`, and fatal `401` auth-failure streams.
pub fn start_mtproto_erased_on(
    self_user_id: tg_data::UserId,
    rpc: MtprotoRpc,
    handle: tokio::runtime::Handle,
    config: tg_session::SessionRuntimeConfig,
) -> tg_session::Result<SessionRuntime<ErasedRpcClient>> {
    rpc.ensure_updates_bridge(&handle);
    let new_session_rx = Some(rpc.subscribe_new_session());
    let auth_fail_rx = Some(rpc.subscribe_auth_failures());
    SessionRuntime::start_with_new_session_on(
        self_user_id,
        ErasedRpcClient::new(rpc),
        new_session_rx,
        auth_fail_rx,
        handle,
        config,
    )
}

/// Live Telegram account session implementing all `tg-app` screen backend traits over
/// [`tg_session::SessionRuntime`], [`tg_data::Session`], and [`tg_api::RpcClient`].
///
/// Cloning a `LiveSession` creates a cheap shared handle pointing to the same underlying
/// runtime, bridge, and cache so every screen widget observes one unified live state.
pub struct LiveSession {
    pub(super) runtime: Arc<SessionRuntime<ErasedRpcClient>>,
    pub(super) bridge: Arc<LiveRuntimeBridge>,
    pub(super) cache: Arc<Mutex<LiveSessionCache>>,
    media_cache: LiveMediaCache,
    account_temp_dir: Arc<Mutex<Option<PathBuf>>>,
    cached_auth_user: Option<AuthUser>,
    pub(super) chat_helpers: LiveChatHelpersView,
    pub(super) calls: crate::calls::LiveCallsHandle,
    pub(super) langpack: crate::langpack::LiveLangPackHandle,
    pub(super) passkeys: crate::passkeys::LivePasskeysHandle,
    proxy: crate::proxy::LiveProxyController,
    pub(super) transcribes: Arc<Mutex<crate::transcribe::TranscribeState>>,
    pub(super) promo_state: Arc<Mutex<crate::suggestions::LivePromoState>>,
    /// Requests the UI starts without waiting for them (typing, read receipts, drafts).
    pub(crate) ui_rpc: crate::ui_rpc_queue::UiRpcQueue,
    /// Text sends shown before the server answers (TGB-2931).
    pub(crate) sends: crate::optimistic_send::OptimisticSends,
    /// Last answers of per-frame UI reads, served while the session is being written (TGB-2932).
    pub(crate) ui_reads: crate::ui_reads::UiReadCache,
    /// Account id stamped on this session's notifications (`session->uniqueId()`).
    session_id: u64,
}

/// Cheaply cloneable shared handle to a [`LiveSession`].
pub type LiveSessionHandle = LiveSession;

impl Clone for LiveSession {
    fn clone(&self) -> Self {
        Self {
            runtime: Arc::clone(&self.runtime),
            bridge: Arc::clone(&self.bridge),
            cache: Arc::clone(&self.cache),
            media_cache: self.media_cache.clone(),
            account_temp_dir: Arc::clone(&self.account_temp_dir),
            cached_auth_user: self.cached_auth_user.clone(),
            chat_helpers: self.chat_helpers.clone(),
            calls: self.calls.clone(),
            langpack: self.langpack.clone(),
            passkeys: self.passkeys.clone(),
            proxy: self.proxy.clone(),
            transcribes: Arc::clone(&self.transcribes),
            promo_state: Arc::clone(&self.promo_state),
            ui_rpc: self.ui_rpc.clone(),
            sends: self.sends.clone(),
            ui_reads: self.ui_reads.clone(),
            session_id: self.session_id,
        }
    }
}

impl LiveSession {
    /// Creates a new [`LiveSession`] wrapping a running [`SessionRuntime`].
    #[must_use]
    pub fn new(runtime: Arc<SessionRuntime<ErasedRpcClient>>, session_id: u64) -> Self {
        let bridge = Arc::new(LiveRuntimeBridge::spawn(&runtime, session_id));
        let cache = Arc::new(Mutex::new(LiveSessionCache::new()));
        let media_cache = LiveMediaCache::open_default_for_session(session_id);
        let account_temp_dir = Arc::new(Mutex::new(None));
        let calls = crate::calls::LiveCallsHandle::new(Arc::clone(&runtime), &bridge);
        let langpack = crate::langpack::LiveLangPackHandle::new(Arc::clone(&runtime), &bridge);
        let passkeys = crate::passkeys::LivePasskeysHandle::new(Arc::clone(&runtime), &bridge);
        let proxy = crate::proxy::LiveProxyController::new(&runtime, bridge.state());
        let transcribes = Arc::new(Mutex::new(crate::transcribe::TranscribeState::new()));
        let promo_state = Arc::new(Mutex::new(crate::suggestions::LivePromoState::new()));
        let _ =
            crate::transcribe::spawn_transcribe_update_listener(&runtime, &bridge, &transcribes);
        let ui_rpc = crate::ui_rpc_queue::UiRpcQueue::start(runtime.tokio_handle());
        let mut live = Self {
            runtime,
            bridge,
            cache,
            media_cache,
            account_temp_dir,
            cached_auth_user: None,
            chat_helpers: LiveChatHelpersView::new(),
            calls,
            langpack,
            passkeys,
            proxy,
            transcribes,
            promo_state,
            ui_rpc,
            sends: crate::optimistic_send::OptimisticSends::default(),
            ui_reads: crate::ui_reads::UiReadCache::default(),
            session_id,
        };
        live.refresh_cached_views();
        crate::history_sections::attach_history_sections_listener(&live);
        crate::chat_l233::attach_chat_l233_listener(&live);
        crate::dice_pack::attach_dice_pack_updates_listener(&live);
        crate::engagement::attach_engagement_listener(&live);
        crate::contacts_extras::spawn_periodic_contact_statuses_task(&live);
        live
    }

    /// Returns the live cloud language pack controller for this session.
    #[must_use]
    pub fn langpack(&self) -> &crate::langpack::LiveLangPackHandle {
        &self.langpack
    }

    /// Resolves a TL `InputPeer` for `peer_id` from the loaded session (used by `live/suggestions.rs`).
    pub(crate) fn resolve_input_peer(&self, peer_id: PeerId) -> Option<tl_enums::InputPeer> {
        self.input_peer(peer_id)
    }

    /// Refreshes contact online/last-seen statuses via `contacts.getStatuses` if at least
    /// [`crate::contacts_extras::CONTACT_STATUSES_REFRESH_INTERVAL_MS`] has elapsed since the
    /// last refresh.
    pub fn poll_contact_statuses_if_due(
        &self,
        now_ms: u64,
    ) -> Result<Vec<tg_app_core::core_types::dto::contacts_extras::ContactStatusInfo>, String> {
        crate::contacts_extras::maybe_refresh_contact_statuses(self, now_ms, false)
    }

    /// Invalidates cached `help.getPromoData` so the next dialogs query fetches fresh suggestions.
    pub fn invalidate_promo_suggestions_cache(&self) {
        if let Ok(mut guard) = self.promo_state.lock() {
            guard.invalidate();
        }
    }

    /// Returns the live passkeys controller (`account.*Passkey*` RPCs + WebAuthn provider).
    #[must_use]
    pub fn passkeys_handle(&self) -> &crate::passkeys::LivePasskeysHandle {
        &self.passkeys
    }

    /// Returns the live proxy controller managing transport proxy, health checks, rotation,
    /// and `tdata` persistence.
    #[must_use]
    pub fn proxy_controller(&self) -> &crate::proxy::LiveProxyController {
        &self.proxy
    }

    /// Attaches a `tdata` directory to the live proxy controller, loading persisted proxy settings.
    pub fn attach_proxy_tdata_path(
        &self,
        tdata_path: PathBuf,
    ) -> Result<tg_core::SettingsProxy, String> {
        self.proxy.attach_tdata_path(tdata_path)
    }

    /// Returns the shared [`LiveMediaCache`] backing inline downloads and local storage stats.
    #[must_use]
    pub fn media_cache(&self) -> &LiveMediaCache {
        &self.media_cache
    }

    /// Reopens the shared [`LiveMediaCache`] at `dir` (affecting all clones of this session).
    pub fn set_media_cache_dir(&self, dir: impl Into<PathBuf>) -> anyhow::Result<()> {
        self.media_cache.reopen_at(dir)
    }

    /// Reopens the shared [`LiveMediaCache`] at `dir` encrypted with the account's derived
    /// [`tg_storage::StorageEncryptionKey`] (affecting all clones of this session).
    pub fn set_media_cache_dir_with_key(
        &self,
        dir: impl Into<PathBuf>,
        key: tg_storage::StorageEncryptionKey,
    ) -> anyhow::Result<()> {
        self.media_cache.reopen_at_with_key(dir, key)
    }

    /// Configures this account's temporary files directory under `tdata` (`tdata/temp_<dataName>`)
    /// so external-app opens and other transient copies live inside the account folder rather than
    /// `%TEMP%`.
    pub fn set_account_temp_dir(&self, dir: impl Into<PathBuf>) {
        if let Ok(mut guard) = self.account_temp_dir.lock() {
            *guard = Some(dir.into());
        }
    }

    /// Returns the configured per-account temporary directory, if any.
    #[must_use]
    pub fn account_temp_dir(&self) -> Option<PathBuf> {
        self.account_temp_dir.lock().ok().and_then(|g| g.clone())
    }

    /// Resolves the directory where temporary copies for `open_in_external_app*` are written.
    /// `None` until an account folder is attached: plaintext copies never go to `%TEMP%`.
    #[must_use]
    pub fn external_open_temp_dir(&self) -> Option<PathBuf> {
        if let Some(dir) = self.account_temp_dir() {
            return Some(dir.join("external"));
        }
        if self.media_cache.is_persistent() {
            return Some(self.media_cache.root_dir().join("external_tmp"));
        }
        None
    }

    fn require_external_open_temp_dir(&self) -> anyhow::Result<PathBuf> {
        self.external_open_temp_dir().ok_or_else(|| {
            anyhow!("no account folder is attached yet, so the file cannot be opened externally")
        })
    }

    /// Removes unlocked temporary files inside this account's external-open temp directory
    /// without failing when an external viewer still holds a file handle open.
    pub fn cleanup_account_temp_files(&self) {
        let Some(dir) = self.external_open_temp_dir() else {
            return;
        };
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                let _ = std::fs::remove_file(&path);
            } else if path.is_dir() {
                let _ = std::fs::remove_dir_all(&path);
            }
        }
        let _ = std::fs::remove_dir(&dir);
    }

    /// Creates a cheaply cloned shared handle to this live session.
    #[must_use]
    pub fn handle(&self) -> LiveSessionHandle {
        self.clone()
    }

    /// Returns the underlying [`SessionRuntime`].
    #[must_use]
    pub fn runtime(&self) -> &Arc<SessionRuntime<ErasedRpcClient>> {
        &self.runtime
    }

    /// Returns the async-to-egui [`LiveBridgeState`].
    #[must_use]
    pub fn bridge_state(&self) -> &Arc<LiveBridgeState> {
        self.bridge.state()
    }

    /// Attaches an `egui::Context` so background updates wake the UI immediately.
    pub fn set_egui_ctx(&self, ctx: egui::Context) {
        self.bridge.state().set_egui_ctx(ctx);
    }

    /// Sends `req` from the ordered background queue ([`crate::ui_rpc_queue`]) instead of waiting
    /// for it; `on_error` runs on the runtime if it fails.
    pub(crate) fn queue_background_rpc<R>(
        &self,
        req: R,
        on_error: impl FnOnce(String) + Send + 'static,
    ) where
        R: tg_tl::RemoteCall + Send + Sync + 'static,
        R::Return: Send + 'static,
    {
        let rpc = self.runtime.rpc().clone();
        self.ui_rpc.push(async move {
            if let Err(err) =
                with_rpc_timeout(LIVE_RPC_TIMEOUT, async move { rpc.invoke(&req).await }).await
            {
                on_error(err);
            }
        });
    }

    /// Waits until the requests the UI queued without waiting (typing, read receipts, drafts)
    /// have run. Returns `false` after `limit`. For tests and shutdown only.
    #[doc(hidden)]
    pub fn flush_background_rpcs(&self, limit: Duration) -> bool {
        self.ui_rpc.wait_idle(limit)
    }

    /// Returns a [`TelegramMediaInvoker`] backed by this session's RPC client and `Session`.
    #[must_use]
    pub fn media_invoker(&self) -> TelegramMediaInvoker<ErasedRpcClient> {
        TelegramMediaInvoker::new(
            self.runtime.rpc().clone(),
            Arc::clone(self.runtime.session()),
        )
    }

    /// Refreshes local borrowed view caches (`AuthUser`, `@mention` autocomplete users,
    /// bot commands, and premium flag) from the shared `tg_data::Session`.
    pub fn refresh_cached_views(&mut self) {
        let (auth_user, users, bot_commands, is_premium) = self.read_session(|session| {
            let self_id = session.self_peer_id();
            let auth = session.peer(self_id).map(peer_to_auth_user);
            let premium = session
                .peer(self_id)
                .and_then(|p| p.as_user())
                .is_some_and(|u| u.is_premium());
            let mut ac_users = Vec::new();
            let mut ac_bot_commands: HashMap<ChatUserId, Vec<BotCommand>> = HashMap::new();
            for pid in known_peer_ids(session) {
                if let Some(peer) = session.peer(pid)
                    && let Some(user) = peer.as_user()
                {
                    if let Some(bot) = &user.bot_info
                        && !bot.commands.is_empty()
                    {
                        ac_bot_commands.insert(
                            user.user_id(),
                            bot.commands
                                .iter()
                                .map(|c| BotCommand {
                                    command: c.command.clone(),
                                    description: c.description.clone(),
                                })
                                .collect(),
                        );
                    }
                    ac_users.push(crate::chat_helpers::user_to_chat_user_info(
                        user,
                        pid == self_id,
                    ));
                }
            }
            (auth, ac_users, ac_bot_commands, premium)
        });
        if auth_user.is_some() {
            self.cached_auth_user = auth_user;
        }
        self.chat_helpers.autocomplete_users = users;
        self.chat_helpers.autocomplete_bot_commands = bot_commands;
        self.chat_helpers.is_premium = is_premium;
        self.chat_helpers.state.set_is_premium(is_premium);
    }

    pub(super) fn record_recent_hashtag_cache(&self, tag: &str) {
        let clean = tag.trim_start_matches('#').to_owned();
        if !clean.is_empty()
            && let Ok(mut c) = self.cache.lock()
        {
            c.recent_hashtags_list.retain(|t| t != &clean);
            c.recent_hashtags_list.insert(0, clean);
            c.recent_hashtags_list.truncate(20);
        }
    }

    /// Drains any queued chat-open requests triggered by notifications or media viewer "Show in Chat".
    pub fn drain_pending_chat_opens(&self) -> Vec<PendingChatOpen> {
        self.cache
            .lock()
            .map(|mut c| c.pending_chat_opens.drain(..).collect())
            .unwrap_or_default()
    }

    /// Invalidates cached server settings so the next read re-fetches from Telegram.
    pub fn invalidate_settings_caches(&self) {
        if let Ok(mut c) = self.cache.lock() {
            c.privacy_loaded.clear();
            c.global_privacy_loaded = false;
            c.global_privacy_extra = None;
            c.account_ttl_loaded = false;
            c.sessions_loaded = false;
            c.blocked_loaded = false;
            c.cloud_password_loaded = false;
            c.scope_notify_loaded.clear();
            c.notify_exceptions_loaded.clear();
            c.notify_events_loaded = false;
            c.auto_download_loaded = false;
            c.auto_download_presets = None;
            c.filters_loaded = false;
            c.settings_reads.reset();
        }
        crate::settings_extra::invalidate_caches(self);
        crate::themes::invalidate_cache(self);
        crate::chatlists::invalidate_cache(self);
        self.chat_helpers.state.invalidate_all();
    }

    /// Synchronizes scope defaults and per-peer mute/notification overrides into [`NotifySettingsStore`].
    pub fn sync_notify_settings_store(&self, store: &mut NotifySettingsStore, now_unix: i64) {
        crate::settings_sync::sync_notify_settings_store_live(self, store, now_unix);
    }

    /// Monotonic counter incremented whenever scope defaults or per-peer notification overrides change.
    #[must_use]
    pub fn notify_settings_version(&self) -> u64 {
        self.cache
            .lock()
            .map(|c| c.notify_settings_version)
            .unwrap_or(0)
    }

    /// Returns `true` (and resets the dirty flag) if per-account `StoredSessionSettings`
    /// fields changed since the last persistence pass.
    pub fn take_session_settings_dirty(&self) -> bool {
        self.cache
            .lock()
            .map(|mut c| std::mem::take(&mut c.session_settings_dirty))
            .unwrap_or(false)
    }

    /// Restores per-account [`tg_session::StoredSessionSettings`] and `CoreSettings::autoplay`
    /// toggles into this live session's cache on startup.
    pub fn apply_stored_session_settings(
        &mut self,
        stored: &tg_session::StoredSessionSettings,
        autoplay: (bool, bool, bool),
    ) {
        crate::settings_sync::apply_stored_session_settings_live(self, stored, autoplay);
    }

    /// Captures per-account settings from this live session into a [`tg_session::StoredSessionSettings`]
    /// snapshot for `tdata` persistence.
    #[must_use]
    pub fn collect_stored_session_settings(
        &self,
        base: &tg_session::StoredSessionSettings,
    ) -> tg_session::StoredSessionSettings {
        crate::settings_sync::collect_stored_session_settings_live(self, base)
    }

    /// Returns the server auto-download presets (`Low`, `Medium`, `High`), fetching them
    /// via `account.getAutoDownloadSettings` if not yet loaded.
    #[must_use]
    pub fn auto_download_presets(&self) -> Option<crate::settings_sync::ServerAutoDownloadPresets> {
        crate::settings_sync::fetch_auto_download_presets_live(self)
    }

    /// Applies one of the server auto-download presets (`Low`, `Medium`, `High`) and saves it
    /// via `account.saveAutoDownloadSettings`.
    pub fn apply_auto_download_preset(
        &mut self,
        preset: crate::settings_sync::AutoDownloadPreset,
    ) -> Result<AutoDownloadSettings, String> {
        crate::settings_sync::apply_auto_download_preset_live(self, preset)
    }

    /// Fetches the global default auto-delete timer (`messages.getDefaultHistoryTTL`).
    pub fn default_history_ttl(&self) -> Result<i32, String> {
        crate::settings_sync::fetch_default_history_ttl_live(self)
    }

    /// Updates the global default auto-delete timer (`messages.setDefaultHistoryTTL`).
    pub fn set_default_history_ttl(&mut self, period_seconds: i32) -> Result<(), String> {
        crate::settings_sync::set_default_history_ttl_live(self, period_seconds)
    }

    /// Fetches sensitive content settings (`account.getContentSettings`), returning
    /// `(sensitive_enabled, sensitive_can_change)`.
    pub fn content_settings(&self) -> Result<(bool, bool), String> {
        crate::settings_sync::fetch_content_settings_live(self)
    }

    /// Updates sensitive content filtering (`account.setContentSettings`).
    pub fn set_content_settings(&mut self, sensitive_enabled: bool) -> Result<(), String> {
        crate::settings_sync::set_content_settings_live(self, sensitive_enabled)
    }

    /// Clears the current primary (or public fallback) profile photo via `photos.updateProfilePhoto`.
    pub fn clear_profile_photo(&mut self, fallback: bool) -> Result<(), String> {
        crate::settings_sync::clear_profile_photo_live(self, fallback)
    }

    // -------------------------------------------------------------------------
    // Internal Session & RPC helpers
    // -------------------------------------------------------------------------

    /// Shows a text send at once and sends it in the background (TGB-2931, see
    /// [`crate::optimistic_send`]): stores the local message, queues the send in the runtime's
    /// ordered send lane and returns the first chunk as a pending message.
    pub(crate) fn send_text_optimistic(
        &self,
        data_peer: DataPeerId,
        text: &str,
        entities: &[tl_enums::MessageEntity],
        reply_to: Option<tl_enums::InputReplyTo>,
        options: SendOptions,
        extras: &crate::optimistic_send::QueuedSendExtras,
    ) -> Result<HistoryMessage, String> {
        let chunks = self.sends.prepare(data_peer, text, entities);
        if chunks.is_empty() {
            return Err("Cannot send an empty message.".to_owned());
        }
        let first = self.write_session(|s| {
            crate::optimistic_send::store_local_messages(s, data_peer, &chunks, &options, extras)
        });
        self.sends
            .queue(self, data_peer, chunks, reply_to, options, extras);
        self.bridge.state().notify_ui();
        first.ok_or_else(|| "sent message was not stored in session".to_owned())
    }

    /// Runs `f` under the session read lock when it is free. While the session actor holds the
    /// write lock, returns `last()` (the previous answer for the same read) instead of waiting
    /// and moves the view versions so the next frame reads again; waits only when there is no
    /// earlier answer (TGB-2932). The flag is `true` for a fresh read.
    pub(crate) fn read_session_or_last<R: Send>(
        &self,
        last: impl FnOnce() -> Option<R>,
        f: impl FnOnce(&Session) -> R + Send,
    ) -> (R, bool) {
        if let Ok(guard) = self.runtime.session().try_read() {
            return (f(&guard), true);
        }
        if let Some(last) = last() {
            self.bridge.state().mark_views_changed();
            self.bridge.state().notify_ui();
            return (last, false);
        }
        (self.read_session(f), true)
    }

    pub(super) fn read_session<R: Send>(&self, f: impl FnOnce(&Session) -> R + Send) -> R {
        let session_lock = self.runtime.session();
        if let Ok(guard) = session_lock.try_read() {
            return f(&guard);
        }
        let session_arc = Arc::clone(session_lock);
        block_on_runtime(self.runtime.tokio_handle(), async move {
            let guard = session_arc.read().await;
            f(&guard)
        })
    }

    #[doc(hidden)]
    pub fn write_session<R: Send>(&self, f: impl FnOnce(&mut Session) -> R + Send) -> R {
        let session_lock = self.runtime.session();
        let out = if let Ok(mut guard) = session_lock.try_write() {
            f(&mut guard)
        } else {
            let session_arc = Arc::clone(session_lock);
            block_on_runtime(self.runtime.tokio_handle(), async move {
                let mut guard = session_arc.write().await;
                f(&mut guard)
            })
        };
        // Not every session mutation emits a change event (flags set through `message_mut`), so
        // a write from here always moves the view versions.
        self.bridge.state().mark_views_changed();
        out
    }

    pub(super) fn invoke_rpc<R>(&self, req: R) -> Result<R::Return, String>
    where
        R: tg_tl::RemoteCall + Send + Sync + 'static,
        R::Return: Send + 'static,
    {
        let rpc = self.runtime.rpc().clone();
        block_on_runtime(self.runtime.tokio_handle(), async move {
            with_rpc_timeout(LIVE_RPC_TIMEOUT, async move { rpc.invoke(&req).await }).await
        })
    }

    pub(super) fn input_peer(&self, peer_id: PeerId) -> Option<tl_enums::InputPeer> {
        let data_peer = to_data_peer(peer_id);
        let peer = self.read_session(|s| input_peer_for(s, data_peer));
        if matches!(peer, tl_enums::InputPeer::Empty) {
            None
        } else {
            Some(peer)
        }
    }

    pub(super) fn input_channel(&self, peer_id: PeerId) -> Option<tl_enums::InputChannel> {
        let data_peer = to_data_peer(peer_id);
        self.read_session(|s| {
            let peer = s.peer(data_peer)?;
            let ch = peer.as_channel()?;
            Some(tl_enums::InputChannel::InputChannel(
                tl_types::InputChannel {
                    channel_id: ch.channel_id().0 as i64,
                    access_hash: ch.access_hash as i64,
                },
            ))
        })
    }

    /// Requests the first page of the main chat list on the session runtime without blocking the
    /// caller, like tdesktop's `ApiWrap::requestDialogs` (sent from `Updates::stateDone`,
    /// `api_updates.cpp:480-489`): the main window shows at once with the list's "Loading..."
    /// state and `on_loaded` (a repaint request) runs when the page has been applied. Until then
    /// [`DialogsBackend::chats_list_loaded`] is `false` and list reads do not wait for the page.
    pub fn start_chats_list_request(&self, on_loaded: impl FnOnce() + Send + 'static) {
        let Ok(mut c) = self.cache.lock() else {
            return;
        };
        if !c.dialogs_loaded_folders.insert(MAIN_FOLDER_ID) {
            return;
        }
        c.main_dialogs_pending = true;
        drop(c);
        let rt = Arc::clone(&self.runtime);
        let cache = Arc::clone(&self.cache);
        let live = self.clone();
        self.runtime.tokio_handle().spawn(async move {
            // The runtime's startup sync usually brings the first page itself
            // (`SessionRuntimeConfig::load_initial_dialogs`); request one only if it did not.
            let _ = with_rpc_timeout(LIVE_RPC_TIMEOUT, rt.wait_ready()).await;
            let has_first_page = {
                let session = rt.session().read().await;
                !session.main_dialogs().is_empty() || session.main_list().loaded()
            };
            if !has_first_page {
                let res =
                    with_rpc_timeout(LIVE_RPC_TIMEOUT, rt.load_dialogs_page(MAIN_FOLDER_ID)).await;
                if let Err(err) = res {
                    debug!(error = %err, "background load_dialogs_page returned error");
                }
            }
            if let Ok(mut c) = cache.lock() {
                c.main_dialogs_pending = false;
            }
            live.bridge.state().mark_views_changed();
            on_loaded();
            live.request_more_dialogs(false);
        });
    }

    fn ensure_dialogs_loaded(&self, folder_id: FolderId) {
        if folder_id == MAIN_FOLDER_ID {
            // The main list shows its "Loading..." state meanwhile (`chats_list_loaded`); the
            // request marks the folder as tried itself.
            let bridge = Arc::clone(self.bridge.state());
            self.start_chats_list_request(move || bridge.notify_ui());
            return;
        }
        let first_request = self
            .cache
            .lock()
            .is_ok_and(|mut c| c.dialogs_loaded_folders.insert(folder_id));
        if !first_request {
            return;
        }
        // `ApiWrap::requestDialogs` for the archive: the list shows what is loaded until the page
        // lands (its chats arrive through change events, the version moves, the UI repaints).
        let rt = Arc::clone(&self.runtime);
        let bridge = Arc::clone(self.bridge.state());
        self.runtime.tokio_handle().spawn(async move {
            let res = with_rpc_timeout(LIVE_RPC_TIMEOUT, rt.load_dialogs_page(folder_id)).await;
            if let Err(err) = res {
                debug!(folder = folder_id, error = %err, "load_dialogs_page returned error");
            }
            bridge.notify_ui();
        });
    }

    /// The `open_history` request a slice read at `around_id` in `direction` needs, or `None` when
    /// the loaded history already answers it. Marks a first open of the chat as tried (it is
    /// requested once, like before).
    fn history_request(
        &self,
        peer_id: PeerId,
        around_id: MsgId,
        direction: LoadDirection,
    ) -> Option<(DataPeerId, DataMsgId, LoadDirection)> {
        let data_peer = to_data_peer(peer_id);
        let already_loaded_initial = self
            .cache
            .lock()
            .map(|c| c.history_loaded_peers.contains(&data_peer))
            .unwrap_or(false);
        // `ShowAtUnreadMsgId`: around `History::loadAroundId` when the chat has unread messages.
        let unread_around = if around_id == MsgId::SHOW_AT_UNREAD {
            self.read_session(|s| {
                s.history(data_peer)
                    .and_then(crate::open_at_unread::load_around_id)
            })
        } else {
            None
        };
        let (needs_load, unread_missing) = self.read_session(|s| {
            let Some(h) = s.history(data_peer) else {
                return (true, false);
            };
            let unread_missing =
                unread_around.is_some_and(|id| !h.loaded_ranges.contains_range_for(id));
            let needs = match direction {
                LoadDirection::Around => crate::open_at_unread::first_open_needs_load(
                    &h.loaded_ranges,
                    already_loaded_initial,
                    unread_missing,
                ),
                LoadDirection::Before => !h.loaded_ranges.loaded_at_top(),
                LoadDirection::After => !h.loaded_ranges.loaded_at_bottom(),
            };
            (needs, unread_missing)
        });
        if !needs_load {
            return None;
        }
        if matches!(direction, LoadDirection::Around) && already_loaded_initial && !unread_missing {
            return None;
        }
        if matches!(direction, LoadDirection::Around)
            && let Ok(mut c) = self.cache.lock()
        {
            c.history_loaded_peers.insert(data_peer);
        }

        let (data_around, effective_direction) = if let Some(around) = unread_around {
            (around, direction)
        } else if around_id == MsgId::SHOW_AT_END || around_id == MsgId::SHOW_AT_UNREAD {
            // Opening at the bottom of history uses `LoadDirection::Before` with `offset_id = 0`
            // so `parse_history_result` anchors `no_skip_range.till = SERVER_MAX_MSG_ID`
            // (`loaded_at_bottom() == true`) even on a partial `messagesSlice`.
            let dir = if matches!(direction, LoadDirection::Around) {
                LoadDirection::Before
            } else {
                direction
            };
            (DataMsgId(0), dir)
        } else {
            (to_data_msg(around_id), direction)
        };
        Some((data_peer, data_around, effective_direction))
    }

    fn ensure_history_loaded(&self, peer_id: PeerId, around_id: MsgId, direction: LoadDirection) {
        let Some((data_peer, data_around, effective_direction)) =
            self.history_request(peer_id, around_id, direction)
        else {
            return;
        };
        let rt = Arc::clone(&self.runtime);
        let res = block_on_runtime(self.runtime.tokio_handle(), async move {
            with_rpc_timeout(
                LIVE_RPC_TIMEOUT,
                rt.open_history(data_peer, data_around, effective_direction),
            )
            .await
        });
        if let Err(err) = res {
            debug!(peer = peer_id.0, error = %err, "open_history returned error");
        }
    }

    /// Non-blocking counterpart of [`Self::ensure_history_loaded`]: `true` when the slice can be
    /// built now, `false` while its page is requested in the background. The finished request
    /// moves the chat's version and wakes the UI; the next call then answers from what loaded
    /// (also after a failure, like the blocking read did).
    fn history_ready_or_request(
        &self,
        peer_id: PeerId,
        around_id: MsgId,
        direction: LoadDirection,
    ) -> bool {
        let data_peer = to_data_peer(peer_id);
        let key = (data_peer, direction_code(direction), around_id);
        let state = self
            .cache
            .lock()
            .ok()
            .and_then(|c| c.history_requests.get(&key).copied());
        match state {
            Some(HistoryRequestState::InFlight) => return false,
            Some(HistoryRequestState::Done) => {
                if let Ok(mut c) = self.cache.lock() {
                    c.history_requests.remove(&key);
                }
                return true;
            }
            None => {}
        }
        let Some((data_peer, data_around, effective_direction)) =
            self.history_request(peer_id, around_id, direction)
        else {
            return true;
        };
        if let Ok(mut c) = self.cache.lock() {
            c.history_requests
                .insert(key, HistoryRequestState::InFlight);
        }
        let rt = Arc::clone(&self.runtime);
        let cache = Arc::clone(&self.cache);
        let bridge = Arc::clone(self.bridge.state());
        self.runtime.tokio_handle().spawn(async move {
            let res = with_rpc_timeout(
                LIVE_RPC_TIMEOUT,
                rt.open_history(data_peer, data_around, effective_direction),
            )
            .await;
            if let Err(err) = res {
                debug!(peer = data_peer.0, error = %err, "open_history returned error");
            }
            if let Ok(mut c) = cache.lock() {
                c.history_requests.insert(key, HistoryRequestState::Done);
            }
            bridge.notify_chat_changed(data_peer);
        });
        false
    }

    fn ensure_filters_loaded(&self) {
        let loaded = self.cache.lock().map(|c| c.filters_loaded).unwrap_or(true);
        if loaded {
            return;
        }
        let load = Arc::new(FiltersLoad::default());
        if let Ok(mut c) = self.cache.lock() {
            c.filters_loaded = true;
            c.filters_loading = Some(Arc::clone(&load));
        }
        // `Data::ChatFilters::load`: the folders arrive in the background; until then the tabs
        // show the folders the session already knows.
        let rpc = self.runtime.rpc().clone();
        let live = self.clone();
        self.runtime.tokio_handle().spawn(async move {
            let res = with_rpc_timeout(LIVE_RPC_TIMEOUT, rpc.messages_get_dialog_filters()).await;
            let finish = |live: &LiveSession| {
                if let Ok(mut c) = live.cache.lock() {
                    c.filters_loading = None;
                }
                load.finish();
            };
            let Ok(tl_filters) = res else {
                finish(&live);
                return;
            };
            {
                let mut session = live.runtime.session().write().await;
                let self_id = Some(session.self_user_id());
                let parsed: Vec<ChatFilter> = tl_filters
                    .filters
                    .iter()
                    .filter_map(|f| tg_api::convert::dialog_filter_from_tl(f, self_id))
                    .collect();
                session.sync_filters_from_list(parsed, tl_filters.tags_enabled);
            }
            finish(&live);
            live.bridge.state().notify_ui();
            live.dialog_filters_loaded(tl_filters);
        });
    }

    /// [`Self::ensure_filters_loaded`] for screens that edit folders (settings): they wait until
    /// the folders arrived, so a change made there is never overwritten by the late answer of a
    /// load that started before it.
    fn ensure_filters_loaded_now(&self) {
        self.ensure_filters_loaded();
        let pending = self
            .cache
            .lock()
            .ok()
            .and_then(|c| c.filters_loading.clone());
        if let Some(load) = pending {
            load.wait();
        }
    }

    pub(super) fn resolve_media_by_id(
        &self,
        id: MediaItemId,
    ) -> Option<(DataPeerId, DataMsgId, ResolvedMediaLocation)> {
        // The media index (TGB-2938) instead of a walk over every chat's messages.
        self.read_session(|session| {
            session
                .messages_with_media(id.0)
                .into_iter()
                .find_map(|full_id| {
                    let msg = session.message(full_id)?;
                    let media = msg.media()?;
                    let matches = match media {
                        MessageMedia::Photo(p) => p.id.0 == id.0,
                        MessageMedia::Document(d) => d.id.0 == id.0,
                        _ => false,
                    };
                    if !matches {
                        return None;
                    }
                    message_media_to_location(media).map(|loc| (full_id.peer, msg.id, loc))
                })
        })
    }

    fn toggle_peer_mute_rpc(&self, peer_id: PeerId) -> bool {
        let data_peer = to_data_peer(peer_id);
        let now = now_unix_i32();
        let currently_muted = self.read_session(|s| {
            s.peer(data_peer)
                .is_some_and(|p| p.common().notify.is_muted(now))
                || s.history(data_peer).is_some_and(|h| h.muted)
        });
        let new_muted = !currently_muted;
        let mute_until = if new_muted { i32::MAX } else { 0 };

        if let Some(input_peer) = self.input_peer(peer_id) {
            let req = tl_fns::account::UpdateNotifySettings {
                peer: tl_enums::InputNotifyPeer::InputNotifyPeer(tl_types::InputNotifyPeer {
                    peer: input_peer,
                }),
                settings: tl_enums::InputPeerNotifySettings::InputPeerNotifySettings(
                    tl_types::InputPeerNotifySettings {
                        show_previews: None,
                        silent: None,
                        mute_until: Some(mute_until),
                        sound: None,
                        stories_muted: None,
                        stories_hide_sender: None,
                        stories_sound: None,
                    },
                ),
            };
            if let Err(err) = self.invoke_rpc(req) {
                warn!(peer = peer_id.0, error = %err, "account.updateNotifySettings failed");
                return currently_muted;
            }
        }

        self.write_session(|s| {
            if let Some(peer) = s.peer_mut(data_peer) {
                peer.common_mut().notify.mute_until = Some(mute_until);
            }
            s.set_history_muted(data_peer, new_muted);
        });
        if let Ok(mut c) = self.cache.lock() {
            c.notify_settings_version = c.notify_settings_version.wrapping_add(1);
        }
        self.bridge.state().notify_ui();
        new_muted
    }

    fn toggle_peer_block_rpc(&self, peer_id: PeerId) -> bool {
        let data_peer = to_data_peer(peer_id);
        let was_blocked = self.read_session(|s| {
            s.peer(data_peer)
                .is_some_and(|p| p.common().block_status == BlockStatus::Blocked)
        });
        let Some(input_peer) = self.input_peer(peer_id) else {
            return was_blocked;
        };
        let rpc_res = if was_blocked {
            self.invoke_rpc(tl_fns::contacts::Unblock {
                my_stories_from: false,
                id: input_peer,
            })
        } else {
            self.invoke_rpc(tl_fns::contacts::Block {
                my_stories_from: false,
                id: input_peer,
            })
        };
        if let Err(err) = rpc_res {
            warn!(peer = peer_id.0, error = %err, "contacts.block/unblock failed");
            return was_blocked;
        }
        let new_blocked = !was_blocked;
        self.write_session(|s| {
            if let Some(peer) = s.peer_mut(data_peer) {
                peer.common_mut().block_status = if new_blocked {
                    BlockStatus::Blocked
                } else {
                    BlockStatus::NotBlocked
                };
            }
        });
        if let Ok(mut c) = self.cache.lock() {
            c.invalidate_blocked_users();
        }
        self.bridge.state().notify_ui();
        new_blocked
    }

    /// Requests the privacy rules of `keys` that were not requested yet (`Api::UserPrivacy::reload`),
    /// all at once: the Privacy section waits for one round trip, not one per row. A key is asked
    /// once; after a failure it shows the default until [`Self::invalidate_settings_caches`]
    /// instead of being asked again on every frame.
    fn load_privacy_rules_if_needed(&self, keys: &[PrivacyKey]) {
        let missing: Vec<PrivacyKey> = match self.cache.lock() {
            Ok(mut c) => keys
                .iter()
                .copied()
                .filter(|&key| c.privacy_loaded.insert(key))
                .collect(),
            Err(_) => return,
        };
        if missing.is_empty() {
            return;
        }
        let rpc = self.runtime.rpc().clone();
        let answers = block_on_runtime(self.runtime.tokio_handle(), async move {
            let requests: Vec<_> = missing
                .into_iter()
                .map(|key| {
                    let req = tl_fns::account::GetPrivacy {
                        key: privacy_key_to_tl(key),
                    };
                    (key, spawn_rpc(&rpc, req, LIVE_RPC_TIMEOUT))
                })
                .collect();
            let mut answers = Vec::with_capacity(requests.len());
            for (key, request) in requests {
                match joined(request).await {
                    Ok(tl_enums::account::PrivacyRules::PrivacyRules(resp)) => {
                        answers.push((key, resp));
                    }
                    Err(err) => debug!(?key, error = %err, "account.getPrivacy failed"),
                }
            }
            answers
        });
        if answers.is_empty() {
            return;
        }
        self.write_session(|s| {
            for (_, resp) in &answers {
                apply_tl_users_and_chats(s, &resp.users, &resp.chats);
            }
        });
        if let Ok(mut c) = self.cache.lock() {
            for (key, resp) in &answers {
                c.privacy_rules
                    .insert(*key, privacy_rules_from_tl(&resp.rules));
            }
        }
    }

    /// The loaded rule of `key`, or the default while it is not known.
    fn cached_privacy_rule(cache: &LiveSessionCache, key: PrivacyKey) -> PrivacyRule {
        cache.privacy_rules.get(&key).cloned().unwrap_or_default()
    }

    /// Starts `account.getAuthorizations` in the background the first time (TGB-3070).
    fn fetch_sessions_if_needed(&self) {
        if self.cache.lock().map_or(true, |c| c.sessions_loaded) {
            return;
        }
        self.spawn_settings_read(SettingsRead::ActiveSessions, Self::load_active_sessions);
    }

    fn fetch_scope_notify_if_needed(&self, scope: NotifyScope) -> ScopeNotifySettings {
        crate::settings_sync::fetch_scope_notify_live(self, scope)
    }

    /// Routes an RPC-returned TL [`tl_enums::Updates`] batch through `SessionRuntime`'s single
    /// [`tg_api::UpdatesHandler`] (`ApiWrap::applyUpdates` -> `Api::Updates::applyUpdates`,
    /// `apiwrap.cpp:2332`, `api/api_updates.cpp:1518-1560`).
    pub(crate) fn apply_updates_via_runtime(
        &self,
        updates: tl_enums::Updates,
    ) -> Result<(), String> {
        let rt = Arc::clone(&self.runtime);
        match tokio::runtime::Handle::try_current() {
            Ok(current) => match current.runtime_flavor() {
                tokio::runtime::RuntimeFlavor::MultiThread => {
                    tokio::task::block_in_place(move || rt.apply_updates_blocking(updates))
                }
                _ => std::thread::scope(|scope| {
                    scope
                        .spawn(move || rt.apply_updates_blocking(updates))
                        .join()
                        .unwrap_or_else(|_| panic!("live apply_updates helper thread panicked"))
                }),
            },
            Err(_) => rt.apply_updates_blocking(updates),
        }
        .map_err(|e| e.to_string())
    }

    /// Routes a message deletion through `SessionRuntime::delete_messages_blocking`
    /// (`Data::Histories::deleteMessages` in `data/data_histories.cpp:975-1065` +
    /// `ApiWrap::applyAffectedMessages` in `apiwrap.cpp:2350-2358`).
    pub(crate) fn delete_messages_via_runtime(
        &self,
        peer_id: PeerId,
        ids: Vec<DataMsgId>,
        revoke: bool,
    ) -> Result<tl_types::messages::AffectedMessages, String> {
        let rt = Arc::clone(&self.runtime);
        let data_peer = to_data_peer(peer_id);
        match tokio::runtime::Handle::try_current() {
            Ok(current) => match current.runtime_flavor() {
                tokio::runtime::RuntimeFlavor::MultiThread => {
                    tokio::task::block_in_place(move || {
                        rt.delete_messages_blocking(data_peer, ids, revoke)
                    })
                }
                _ => std::thread::scope(|scope| {
                    scope
                        .spawn(move || rt.delete_messages_blocking(data_peer, ids, revoke))
                        .join()
                        .unwrap_or_else(|_| panic!("live delete_messages helper thread panicked"))
                }),
            },
            Err(_) => rt.delete_messages_blocking(data_peer, ids, revoke),
        }
        .map_err(|e| e.to_string())
    }

    /// Routes `messages.editMessage` through `SessionRuntime::edit_message`
    /// (`Api::EditMessage` in `api/api_editing.cpp:356, 426`; treats `MESSAGE_NOT_MODIFIED`
    /// after a 500 retry as success and syncs difference).
    pub(crate) fn edit_message_via_runtime(
        &self,
        req: tl_fns::messages::EditMessage,
    ) -> Result<Option<tl_enums::Updates>, String> {
        let rt = Arc::clone(&self.runtime);
        block_on_runtime(self.runtime.tokio_handle(), async move {
            with_rpc_timeout(LIVE_RPC_TIMEOUT, rt.edit_message(req)).await
        })
    }

    /// Updates UI-side caches (rich messages, widgets, history sections, Layer-233 transient
    /// state) after `SessionRuntime` has already applied `updates` through `UpdatesHandler`.
    pub(crate) fn apply_updates_side_caches(&self, updates: &tl_enums::Updates) {
        let self_uid = self.read_session(|s| s.self_user_id().0 as i64);
        crate::rich_message::record_rich_messages_from_updates(
            self.runtime.session(),
            updates,
            self_uid,
        );
        crate::widgets::record_widget_updates_from_batch(self.runtime.session(), updates);
        crate::history_sections::apply_updates_for_live_session(self, updates);
        crate::chat_l233::apply_updates_for_live_session(self, updates);
        self.bridge.state().notify_ui();
    }

    pub(crate) fn apply_updates_to_session(&self, updates: &tl_enums::Updates) -> Vec<DataMsgId> {
        let new_msg_ids = extract_message_ids_from_updates(updates);
        if let Err(err) = self.apply_updates_via_runtime(updates.clone()) {
            debug!(error = %err, "SessionRuntime::apply_updates_blocking returned error");
        }
        self.apply_updates_side_caches(updates);
        new_msg_ids
    }
}

/// Pure read-only extraction of message IDs referenced by an RPC-returned [`tl_enums::Updates`]
/// batch (no `Session` mutation).
pub(crate) fn extract_message_ids_from_updates(updates: &tl_enums::Updates) -> Vec<DataMsgId> {
    let mut ids = Vec::new();
    let mut push_unique = |id: DataMsgId| {
        if !ids.contains(&id) {
            ids.push(id);
        }
    };
    let mut collect_update = |upd: &tl_enums::Update| {
        let raw_msg = match upd {
            tl_enums::Update::NewMessage(u) => Some(&u.message),
            tl_enums::Update::NewChannelMessage(u) => Some(&u.message),
            tl_enums::Update::EditMessage(u) => Some(&u.message),
            tl_enums::Update::EditChannelMessage(u) => Some(&u.message),
            _ => None,
        };
        if let Some(msg) = raw_msg {
            match msg {
                tl_enums::Message::Message(m) => push_unique(DataMsgId(i64::from(m.id))),
                tl_enums::Message::Service(s) => push_unique(DataMsgId(i64::from(s.id))),
                tl_enums::Message::Empty(_) => {}
            }
        }
    };
    match updates {
        tl_enums::Updates::Updates(u) => {
            for upd in &u.updates {
                collect_update(upd);
            }
        }
        tl_enums::Updates::Combined(u) => {
            for upd in &u.updates {
                collect_update(upd);
            }
        }
        tl_enums::Updates::UpdateShortSentMessage(short) => {
            push_unique(DataMsgId(i64::from(short.id)));
        }
        tl_enums::Updates::UpdateShortMessage(short) => {
            push_unique(DataMsgId(i64::from(short.id)));
        }
        tl_enums::Updates::UpdateShortChatMessage(short) => {
            push_unique(DataMsgId(i64::from(short.id)));
        }
        tl_enums::Updates::UpdateShort(short) => {
            collect_update(&short.update);
        }
        tl_enums::Updates::TooLong => {}
    }
    ids
}

/// Returns `true` if `updates` contains an `EditMessage` / `EditChannelMessage` for `msg_id`.
pub(crate) fn updates_contain_edit_for(updates: &tl_enums::Updates, msg_id: DataMsgId) -> bool {
    let matches_edit = |upd: &tl_enums::Update| match upd {
        tl_enums::Update::EditMessage(u) => match &u.message {
            tl_enums::Message::Message(m) => DataMsgId(i64::from(m.id)) == msg_id,
            tl_enums::Message::Service(s) => DataMsgId(i64::from(s.id)) == msg_id,
            tl_enums::Message::Empty(_) => false,
        },
        tl_enums::Update::EditChannelMessage(u) => match &u.message {
            tl_enums::Message::Message(m) => DataMsgId(i64::from(m.id)) == msg_id,
            tl_enums::Message::Service(s) => DataMsgId(i64::from(s.id)) == msg_id,
            tl_enums::Message::Empty(_) => false,
        },
        _ => false,
    };
    match updates {
        tl_enums::Updates::Updates(u) => u.updates.iter().any(matches_edit),
        tl_enums::Updates::Combined(u) => u.updates.iter().any(matches_edit),
        tl_enums::Updates::UpdateShort(s) => matches_edit(&s.update),
        _ => false,
    }
}

fn updates_contain_pin_for(updates: &tl_enums::Updates, msg_id: i32) -> bool {
    let matches_pin = |upd: &tl_enums::Update| match upd {
        tl_enums::Update::PinnedMessages(u) => u.messages.contains(&msg_id),
        tl_enums::Update::PinnedChannelMessages(u) => u.messages.contains(&msg_id),
        _ => false,
    };
    match updates {
        tl_enums::Updates::Updates(u) => u.updates.iter().any(matches_pin),
        tl_enums::Updates::Combined(u) => u.updates.iter().any(matches_pin),
        tl_enums::Updates::UpdateShort(s) => matches_pin(&s.update),
        _ => false,
    }
}

pub(crate) fn data_filter_to_tl(filter: &ChatFilter, session: &Session) -> tl_enums::DialogFilter {
    let to_peers = |ids: &[DataPeerId]| -> Vec<tl_enums::InputPeer> {
        ids.iter()
            .map(|&pid| input_peer_for(session, pid))
            .filter(|p| !matches!(p, tl_enums::InputPeer::Empty))
            .collect()
    };
    let always_vec: Vec<DataPeerId> = filter.always.iter().copied().collect();
    let never_vec: Vec<DataPeerId> = filter.never.iter().copied().collect();
    tl_enums::DialogFilter::DialogFilter(tl_types::DialogFilter {
        contacts: filter.flags.contains(ChatFilterFlags::CONTACTS),
        non_contacts: filter.flags.contains(ChatFilterFlags::NON_CONTACTS),
        groups: filter.flags.contains(ChatFilterFlags::GROUPS),
        broadcasts: filter.flags.contains(ChatFilterFlags::CHANNELS),
        bots: filter.flags.contains(ChatFilterFlags::BOTS),
        exclude_muted: filter.flags.contains(ChatFilterFlags::NO_MUTED),
        exclude_read: filter.flags.contains(ChatFilterFlags::NO_READ),
        exclude_archived: filter.flags.contains(ChatFilterFlags::NO_ARCHIVED),
        title_noanimate: false,
        id: filter.id,
        title: tl_enums::TextWithEntities::TextWithEntities(tl_types::TextWithEntities {
            text: filter.title.text.clone(),
            entities: Vec::new(),
        }),
        emoticon: (!filter.icon_emoji.is_empty()).then(|| filter.icon_emoji.clone()),
        color: filter.color_index.map(i32::from),
        pinned_peers: to_peers(&filter.pinned),
        include_peers: to_peers(&always_vec),
        exclude_peers: to_peers(&never_vec),
    })
}

fn settings_folder_to_data(folder: &SettingsChatFolder) -> ChatFilter {
    let mut flags = ChatFilterFlags::empty();
    if folder.include_contacts {
        flags |= ChatFilterFlags::CONTACTS;
    }
    if folder.include_non_contacts {
        flags |= ChatFilterFlags::NON_CONTACTS;
    }
    if folder.include_groups {
        flags |= ChatFilterFlags::GROUPS;
    }
    if folder.include_channels {
        flags |= ChatFilterFlags::CHANNELS;
    }
    if folder.include_bots {
        flags |= ChatFilterFlags::BOTS;
    }
    if folder.exclude_muted {
        flags |= ChatFilterFlags::NO_MUTED;
    }
    if folder.exclude_read {
        flags |= ChatFilterFlags::NO_READ;
    }
    if folder.exclude_archived {
        flags |= ChatFilterFlags::NO_ARCHIVED;
    }
    if folder.shareable {
        flags |= ChatFilterFlags::CHATLIST;
    }
    ChatFilter::new(
        folder.id,
        DataText::plain(folder.title.clone()),
        folder.icon.emoticon().to_owned(),
        folder.color_index,
        flags,
        folder
            .included_peers
            .iter()
            .copied()
            .map(DataPeerId)
            .collect(),
        Vec::new(),
        folder
            .excluded_peers
            .iter()
            .copied()
            .map(DataPeerId)
            .collect(),
    )
}

fn report_reason_to_tl(reason: ReportReason) -> tl_enums::ReportReason {
    match reason {
        ReportReason::Spam => tl_enums::ReportReason::InputReportReasonSpam,
        ReportReason::Violence => tl_enums::ReportReason::InputReportReasonViolence,
        ReportReason::Pornography => tl_enums::ReportReason::InputReportReasonPornography,
        ReportReason::ChildAbuse => tl_enums::ReportReason::InputReportReasonChildAbuse,
        ReportReason::Copyright => tl_enums::ReportReason::InputReportReasonCopyright,
        ReportReason::IllegalDrugs => tl_enums::ReportReason::InputReportReasonIllegalDrugs,
        ReportReason::PersonalDetails => tl_enums::ReportReason::InputReportReasonPersonalDetails,
        ReportReason::Fake => tl_enums::ReportReason::InputReportReasonFake,
        ReportReason::Other => tl_enums::ReportReason::InputReportReasonOther,
    }
}

pub(super) fn make_input_reply_to(
    msg_id: MsgId,
    quote_text: Option<String>,
) -> tl_enums::InputReplyTo {
    tl_enums::InputReplyTo::Message(tl_types::InputReplyToMessage {
        quote_rich: None,
        reply_to_msg_id: msg_id.0,
        top_msg_id: None,
        reply_to_peer_id: None,
        quote_text,
        quote_entities: None,
        quote_offset: None,
        monoforum_peer_id: None,
        todo_item_id: None,
        poll_option: None,
    })
}

// ============================================================================
// 1. stand_in::backend::Backend
// ============================================================================

impl Backend for LiveSession {
    fn current_user(&self) -> Option<&AuthUser> {
        self.cached_auth_user.as_ref()
    }

    fn send_login_code(&mut self, _phone: &str) -> Result<CodeSentInfo, String> {
        Err("Live login is driven by IntroWidget over MtprotoRpc.".to_owned())
    }

    fn verify_login_code(&mut self, _code: &str) -> Result<CodeVerifyOutcome, String> {
        Err("Live login is driven by IntroWidget over MtprotoRpc.".to_owned())
    }

    fn verify_cloud_password(&mut self, _password: &str) -> Result<AuthUser, String> {
        Err("Live login is driven by IntroWidget over MtprotoRpc.".to_owned())
    }

    fn logout(&mut self) {
        self.cached_auth_user = None;
    }

    fn folders(&self) -> Vec<FolderFilter> {
        self.ensure_filters_loaded();
        self.read_session(|s| {
            let mut out = vec![FolderFilter {
                id: FilterId::ALL,
                title: "All Chats".to_owned(),
                icon_glyph: "All".to_owned(),
                unread_count: s.total_unread_badge().unread_counter.max(0) as u32,
            }];
            for filter in s.filters().list() {
                let df = chat_filter_to_dialog_folder(filter, s);
                out.push(FolderFilter {
                    id: df.id,
                    title: df.title,
                    icon_glyph: "Fld".to_owned(),
                    unread_count: df.unread.unread_chats,
                });
            }
            out
        })
    }

    fn dialogs(&self, folder: FilterId, query: &str) -> Vec<DialogEntry> {
        self.ensure_dialogs_loaded(MAIN_FOLDER_ID);
        self.request_more_dialogs(false);
        let q = query.trim().to_ascii_lowercase();
        let now = now_unix_i32();
        self.read_session(|session| {
            let keys: &[DialogKey] = if folder == FilterId::ALL {
                session.main_dialogs()
            } else {
                session.filter_dialogs(folder.0).unwrap_or(&[])
            };
            keys.iter()
                .filter_map(|&k| {
                    let DialogKey::History(pid) = k else {
                        return None;
                    };
                    dialog_entry_from_session(session, pid, now)
                })
                .filter(|entry| {
                    q.is_empty()
                        || entry.peer.title.to_ascii_lowercase().contains(&q)
                        || entry.last_message.to_ascii_lowercase().contains(&q)
                        || entry
                            .peer
                            .username
                            .as_deref()
                            .is_some_and(|u| u.to_ascii_lowercase().contains(&q))
                })
                .collect()
        })
    }

    fn peer_summary(&self, peer_id: PeerId) -> Option<PeerSummary> {
        let data_peer = to_data_peer(peer_id);
        let now = now_unix_i32();
        self.read_session(|s| {
            let peer = s.peer(data_peer)?;
            Some(peer_to_peer_summary(
                peer,
                s.history(data_peer),
                s.self_peer_id(),
                now,
                s.send_context(),
            ))
        })
    }

    fn history_slice(&self, peer_id: PeerId) -> Vec<MessageItem> {
        self.ensure_history_loaded(peer_id, MsgId::SHOW_AT_END, LoadDirection::Around);
        let data_peer = to_data_peer(peer_id);
        self.read_session(|session| {
            history_messages_ordered(session, data_peer)
                .into_iter()
                .map(|m| message_to_stand_in_item(session, m))
                .collect()
        })
    }

    fn send_message(&mut self, peer_id: PeerId, text: &str) -> Result<MessageItem, String> {
        let sent = HistoryBackend::send_message(
            self,
            peer_id,
            SendMessageRequest {
                text: TextWithEntities::simple(text),
                reply_to: None,
                silent: false,
            },
        )?;
        let timestamp = sent.formatted_time();
        Ok(MessageItem {
            id: sent.id,
            peer_id: sent.peer_id,
            sender_name: sent.sender_name,
            outgoing: sent.outgoing,
            text: sent.text.text,
            timestamp,
            is_read: sent.is_read,
        })
    }

    fn mark_read(&mut self, peer_id: PeerId) {
        let max_id = self.read_session(|s| {
            s.history(to_data_peer(peer_id))
                .and_then(|h| h.top_message_id.or_else(|| h.loaded_ranges.max_id()))
                .map(from_data_msg)
                .unwrap_or(MsgId(0))
        });
        HistoryBackend::mark_history_read(self, peer_id, max_id);
    }

    fn toggle_mute(&mut self, peer_id: PeerId) -> bool {
        self.toggle_peer_mute_rpc(peer_id)
    }

    fn connection_status(&self) -> ConnectionStatus {
        self.bridge.state().connection_status()
    }

    fn proxy_enabled(&self) -> bool {
        self.proxy.is_proxy_enabled()
    }

    fn set_connection_status(&mut self, status: ConnectionStatus) {
        self.bridge.state().set_connection_status(status);
    }

    fn set_proxy_enabled(&mut self, enabled: bool) {
        self.proxy.set_proxy_enabled(enabled);
        let active = self.proxy.is_proxy_enabled();
        if let Ok(mut c) = self.cache.lock() {
            c.proxy_enabled = active;
        }
    }

    fn retry_connection(&mut self) {
        self.proxy.retry_connection();
        let rt = Arc::clone(&self.runtime);
        let _ = block_on_runtime(self.runtime.tokio_handle(), async move {
            with_rpc_timeout(LIVE_RPC_TIMEOUT, rt.trigger_difference()).await
        });
        let now_ms = u64::try_from(self.ch_now_ms()).unwrap_or(0);
        let _ = crate::contacts_extras::maybe_refresh_contact_statuses(self, now_ms, true);
    }
}

// ============================================================================
// 2. dialogs::backend::DialogsBackend
// ============================================================================

impl DialogsBackend for LiveSession {
    fn dialog_video_userpic(&self, peer_id: PeerId) -> Option<i64> {
        crate::video_userpic::dialog_video_userpic_live(self, peer_id)
    }

    fn video_userpic_frames(
        &self,
        peer_id: PeerId,
        photo_id: i64,
    ) -> tg_app_core::core_types::video_userpic::VideoUserpicFetch {
        crate::video_userpic::video_userpic_frames_live(self, peer_id, photo_id)
    }

    fn current_time(&self) -> LocalDateTime {
        unix_to_local_datetime(now_unix_i32())
    }

    fn chats_list_loaded(&self) -> bool {
        self.cache.lock().map_or(true, |c| !c.main_dialogs_pending)
    }

    fn dialogs_version(&self) -> u64 {
        self.bridge.state().dialogs_version()
    }

    fn dialog_slice(&self, list: DialogListQuery, offset: usize, limit: usize) -> DialogSlice {
        match list {
            DialogListQuery::Filter(_) => self.ensure_dialogs_loaded(MAIN_FOLDER_ID),
            DialogListQuery::Archive => self.ensure_dialogs_loaded(ARCHIVE_FOLDER_ID),
        }
        let now = now_unix_i32();
        let key = (list, offset, limit);
        let (slice, fresh) = self.read_session_or_last(
            || self.ui_reads.last_dialog_slice(key),
            |session| {
                let (ordered, pinned_count, complete): (&[DialogKey], usize, bool) = match list {
                    DialogListQuery::Filter(fid) if fid == FilterId::ALL => (
                        session.main_dialogs(),
                        session.main_list().pinned().order().len(),
                        session.main_list().loaded(),
                    ),
                    DialogListQuery::Filter(fid) => {
                        let keys = session.filter_dialogs(fid.0).unwrap_or(&[]);
                        let pinned = session
                            .filters()
                            .chats_list(fid.0)
                            .map_or(0, |l| l.pinned().order().len());
                        (keys, pinned, session.main_list().loaded())
                    }
                    DialogListQuery::Archive => (
                        session.archive_dialogs(),
                        session.archive().chats_list.pinned().order().len(),
                        session.archive().chats_list.loaded(),
                    ),
                };
                let history_peers: Vec<DataPeerId> = ordered
                    .iter()
                    .filter_map(|&k| match k {
                        DialogKey::History(pid) => Some(pid),
                        DialogKey::Folder(_)
                        | DialogKey::ForumTopic { .. }
                        | DialogKey::SavedSublist { .. } => None,
                    })
                    .collect();
                let total = history_peers.len();
                let entries: Vec<DialogItem> = history_peers
                    .iter()
                    .skip(offset)
                    .take(limit)
                    .filter_map(|&pid| dialog_item_from_session(session, pid, list, now))
                    .collect();
                let has_more = offset.saturating_add(entries.len()) < total || !complete;
                DialogSlice {
                    entries,
                    total,
                    pinned_count,
                    has_more,
                }
            },
        );
        if fresh {
            self.ui_reads.store_dialog_slice(key, &slice);
            crate::peer_emoji_status::request_for_rows(self, &slice.entries);
        }
        // Scrolled past the loaded chats: like `Widget::setLoadMoreCallback`
        // (`dialogs_widget.cpp:548-561`) ask for the next page of the shown list.
        let past_loaded = offset.saturating_add(limit) >= slice.total;
        self.request_more_dialogs(matches!(list, DialogListQuery::Archive) && past_loaded);
        slice
    }

    fn is_peer_blocked(&self, peer_id: PeerId) -> bool {
        self.read_session(|s| {
            s.peer(to_data_peer(peer_id))
                .is_some_and(|p| p.common().block_status == BlockStatus::Blocked)
        })
    }

    fn dialog_by_peer(&self, peer_id: PeerId) -> Option<DialogItem> {
        let data_peer = to_data_peer(peer_id);
        let now = now_unix_i32();
        self.read_session(|session| {
            let in_archive = session
                .history(data_peer)
                .is_some_and(tg_data::History::is_archived);
            let list = if in_archive {
                DialogListQuery::Archive
            } else {
                DialogListQuery::Filter(FilterId::ALL)
            };
            dialog_item_from_session(session, data_peer, list, now)
        })
    }

    fn chat_folders(&self) -> Vec<DialogChatFolder> {
        self.ensure_filters_loaded();
        self.read_session(|session| {
            let mut out = vec![DialogChatFolder {
                id: FilterId::ALL,
                title: "All Chats".to_owned(),
                icon: tg_app_core::dialogs::backend::FolderIconKind::All,
                color_index: 5,
                unread: tg_app_core::dialogs::backend::FolderUnreadState {
                    unread_chats: session.total_unread_badge().unread_counter.max(0) as u32,
                    muted_chats: 0,
                },
                locked: false,
            }];
            for filter in session.filters().list() {
                out.push(chat_filter_to_dialog_folder(filter, session));
            }
            out
        })
    }

    fn folder_tags_enabled(&self) -> bool {
        self.dialog_filter_tags_enabled()
    }

    fn reorder_folders(&mut self, order: &[FilterId]) {
        let ids: Vec<i32> = order
            .iter()
            .copied()
            .filter(|&id| id != FilterId::ALL)
            .map(|id| id.0)
            .collect();
        SettingsBackend::reorder_folders(self, &ids);
    }

    fn add_peer_to_folder(&mut self, peer_id: PeerId, filter_id: FilterId) {
        let data_peer = to_data_peer(peer_id);
        let updated = self.read_session(|s| {
            let mut f = s.filters().get(filter_id.0)?.clone();
            f.always.insert(data_peer);
            f.never.remove(&data_peer);
            Some((f.clone(), data_filter_to_tl(&f, s)))
        });
        if let Some((filter, tl_filter)) = updated {
            let req = tl_fns::messages::UpdateDialogFilter {
                id: filter.id,
                filter: Some(tl_filter),
            };
            if self.invoke_rpc(req).is_ok() {
                self.write_session(|s| s.set_filter(filter));
                self.bridge.state().notify_ui();
            }
        }
    }

    fn remove_peer_from_folder(&mut self, peer_id: PeerId, filter_id: FilterId) {
        let data_peer = to_data_peer(peer_id);
        let updated = self.read_session(|s| {
            let mut f = s.filters().get(filter_id.0)?.clone();
            f.always.remove(&data_peer);
            f.never.insert(data_peer);
            Some((f.clone(), data_filter_to_tl(&f, s)))
        });
        if let Some((filter, tl_filter)) = updated {
            let req = tl_fns::messages::UpdateDialogFilter {
                id: filter.id,
                filter: Some(tl_filter),
            };
            if self.invoke_rpc(req).is_ok() {
                self.write_session(|s| s.set_filter(filter));
                self.bridge.state().notify_ui();
            }
        }
    }

    fn mark_folder_read(&mut self, filter_id: FilterId) {
        let peers: Vec<PeerId> = self.read_session(|s| {
            let keys: &[DialogKey] = if filter_id == FilterId::ALL {
                s.main_dialogs()
            } else {
                s.filter_dialogs(filter_id.0).unwrap_or(&[])
            };
            keys.iter()
                .filter_map(|&k| {
                    let DialogKey::History(pid) = k else {
                        return None;
                    };
                    let h = s.history(pid)?;
                    if h.unread_count.unwrap_or(0) > 0 || h.unread_mark {
                        Some(from_data_peer(pid))
                    } else {
                        None
                    }
                })
                .collect()
        });
        for pid in peers {
            self.mark_dialog_read(pid);
        }
    }

    fn archive_summary(&self) -> Option<ArchiveSummary> {
        let collapsed = self.archive_collapsed();
        let now = now_unix_i32();
        self.read_session(|s| {
            let peers: Vec<DataPeerId> = s
                .archive_dialogs()
                .iter()
                .filter_map(|&k| match k {
                    DialogKey::History(pid) => Some(pid),
                    DialogKey::Folder(_)
                    | DialogKey::ForumTopic { .. }
                    | DialogKey::SavedSublist { .. } => None,
                })
                .collect();
            if peers.is_empty() {
                return None;
            }
            let mut unread_count: u32 = 0;
            let mut all_muted = true;
            let mut preview_titles = Vec::new();
            for &pid in &peers {
                if preview_titles.len() < 4 {
                    preview_titles.push(peer_display_title(s, pid));
                }
                if let Some(h) = s.history(pid) {
                    let u = h.unread_count.unwrap_or(0).max(0) as u32;
                    if u > 0 || h.unread_mark {
                        unread_count = unread_count.saturating_add(u.max(1));
                        let muted =
                            h.muted || s.peer(pid).is_some_and(|p| p.common().notify.is_muted(now));
                        if !muted {
                            all_muted = false;
                        }
                    }
                }
            }
            Some(ArchiveSummary {
                chat_count: peers.len(),
                unread_count,
                all_muted,
                preview_titles,
                collapsed,
            })
        })
    }

    fn archive_collapsed(&self) -> bool {
        self.cache
            .lock()
            .map(|c| c.archive_collapsed)
            .unwrap_or(false)
    }

    fn set_archive_collapsed(&mut self, collapsed: bool) {
        if let Ok(mut c) = self.cache.lock() {
            c.archive_collapsed = collapsed;
            c.session_settings_dirty = true;
        }
        self.bridge.state().mark_views_changed();
    }

    fn toggle_archive(&mut self, peer_id: PeerId) -> bool {
        let data_peer = to_data_peer(peer_id);
        let currently_archived = self.read_session(|s| {
            s.history(data_peer)
                .is_some_and(tg_data::History::is_archived)
        });
        let target_folder = if currently_archived {
            MAIN_FOLDER_ID
        } else {
            ARCHIVE_FOLDER_ID
        };
        let Some(input_peer) = self.input_peer(peer_id) else {
            return currently_archived;
        };
        let req = tl_fns::folders::EditPeerFolders {
            folder_peers: vec![tl_enums::InputFolderPeer::InputFolderPeer(
                tl_types::InputFolderPeer {
                    peer: input_peer,
                    folder_id: target_folder,
                },
            )],
        };
        if let Err(err) = self.invoke_rpc(req) {
            warn!(peer = peer_id.0, error = %err, "folders.editPeerFolders failed");
            return currently_archived;
        }
        self.write_session(|s| {
            s.set_history_folder(
                data_peer,
                (target_folder != MAIN_FOLDER_ID).then_some(target_folder),
            );
        });
        self.bridge.state().notify_ui();
        !currently_archived
    }

    fn mark_archive_read(&mut self) {
        let peers: Vec<PeerId> = self.read_session(|s| {
            s.archive_dialogs()
                .iter()
                .filter_map(|&k| match k {
                    DialogKey::History(pid) => Some(from_data_peer(pid)),
                    DialogKey::Folder(_)
                    | DialogKey::ForumTopic { .. }
                    | DialogKey::SavedSublist { .. } => None,
                })
                .collect()
        });
        for pid in peers {
            self.mark_dialog_read(pid);
        }
    }

    fn toggle_pin(&mut self, peer_id: PeerId, list: DialogListQuery) -> bool {
        let data_peer = to_data_peer(peer_id);
        let key = DialogKey::History(data_peer);
        let (filter_id, currently_pinned) = self.read_session(|s| match list {
            DialogListQuery::Archive => (0, s.archive().chats_list.pinned().order().contains(&key)),
            DialogListQuery::Filter(fid) if fid == FilterId::ALL => {
                (0, s.main_list().pinned().order().contains(&key))
            }
            DialogListQuery::Filter(fid) => (
                fid.0,
                s.filters()
                    .chats_list(fid.0)
                    .is_some_and(|l| l.pinned().order().contains(&key)),
            ),
        });
        let new_pinned = !currently_pinned;
        let Some(input_peer) = self.input_peer(peer_id) else {
            return currently_pinned;
        };
        let req = tl_fns::messages::ToggleDialogPin {
            pinned: new_pinned,
            peer: tl_enums::InputDialogPeer::InputDialogPeer(tl_types::InputDialogPeer {
                peer: input_peer,
            }),
        };
        if let Err(err) = self.invoke_rpc(req) {
            warn!(peer = peer_id.0, error = %err, "messages.toggleDialogPin failed");
            return currently_pinned;
        }
        self.write_session(|s| {
            s.set_pinned(key, filter_id, new_pinned);
        });
        self.bridge.state().notify_ui();
        new_pinned
    }

    fn reorder_pinned(
        &mut self,
        list: DialogListQuery,
        from_index: usize,
        to_index: usize,
    ) -> bool {
        if from_index == to_index {
            return false;
        }
        let archived = matches!(list, DialogListQuery::Archive);
        let folder_id = if archived {
            ARCHIVE_FOLDER_ID
        } else {
            MAIN_FOLDER_ID
        };
        let Some((new_order, tl_order)) = self.read_session(|s| {
            let mut order: Vec<DialogKey> = if archived {
                s.archive().chats_list.pinned().order().to_vec()
            } else {
                s.main_list().pinned().order().to_vec()
            };
            if from_index >= order.len() || to_index >= order.len() {
                return None;
            }
            let item = order.remove(from_index);
            order.insert(to_index, item);
            let tl_peers: Vec<tl_enums::InputDialogPeer> = order
                .iter()
                .filter_map(|k| match *k {
                    DialogKey::History(pid) => {
                        let ip = input_peer_for(s, pid);
                        if matches!(ip, tl_enums::InputPeer::Empty) {
                            None
                        } else {
                            Some(tl_enums::InputDialogPeer::InputDialogPeer(
                                tl_types::InputDialogPeer { peer: ip },
                            ))
                        }
                    }
                    DialogKey::Folder(fid) => Some(tl_enums::InputDialogPeer::Folder(
                        tl_types::InputDialogPeerFolder { folder_id: fid },
                    )),
                    DialogKey::ForumTopic { .. } | DialogKey::SavedSublist { .. } => None,
                })
                .collect();
            Some((order, tl_peers))
        }) else {
            return false;
        };

        let req = tl_fns::messages::ReorderPinnedDialogs {
            force: true,
            folder_id,
            order: tl_order,
        };
        if self.invoke_rpc(req).is_err() {
            return false;
        }
        self.write_session(|s| {
            s.apply_pinned_list(0, archived, new_order);
        });
        self.bridge.state().notify_ui();
        true
    }

    fn toggle_mute_dialog(&mut self, peer_id: PeerId) -> bool {
        self.toggle_peer_mute_rpc(peer_id)
    }

    fn mark_dialog_read(&mut self, peer_id: PeerId) {
        let data_peer = to_data_peer(peer_id);
        let (max_id, had_unread_mark) = self.read_session(|s| {
            let h = s.history(data_peer);
            (
                h.and_then(|h| h.top_message_id.or_else(|| h.loaded_ranges.max_id()))
                    .unwrap_or(DataMsgId(0)),
                h.is_some_and(|h| h.unread_mark),
            )
        });
        if had_unread_mark && let Some(input_peer) = self.input_peer(peer_id) {
            // `Histories::changeDialogUnreadMark`: sent without waiting, no fail handler.
            self.queue_background_rpc(
                tl_fns::messages::MarkDialogUnread {
                    unread: false,
                    parent_peer: None,
                    peer: tl_enums::InputDialogPeer::InputDialogPeer(tl_types::InputDialogPeer {
                        peer: input_peer,
                    }),
                },
                |error| debug!(%error, "messages.markDialogUnread failed"),
            );
            self.write_session(|s| {
                s.set_history_unread_mark(data_peer, false);
            });
        }
        if max_id.0 > 0 {
            HistoryBackend::mark_history_read(self, peer_id, from_data_msg(max_id));
        }
    }

    fn mark_dialog_unread(&mut self, peer_id: PeerId) {
        let Some(input_peer) = self.input_peer(peer_id) else {
            return;
        };
        let req = tl_fns::messages::MarkDialogUnread {
            unread: true,
            parent_peer: None,
            peer: tl_enums::InputDialogPeer::InputDialogPeer(tl_types::InputDialogPeer {
                peer: input_peer,
            }),
        };
        if self.invoke_rpc(req).is_ok() {
            let data_peer = to_data_peer(peer_id);
            self.write_session(|s| {
                s.set_history_unread_mark(data_peer, true);
            });
            self.bridge.state().notify_ui();
        }
    }

    fn clear_history(&mut self, peer_id: PeerId) {
        let data_peer = to_data_peer(peer_id);
        let max_id = self.read_session(|s| {
            s.history(data_peer)
                .and_then(|h| h.top_message_id.or_else(|| h.loaded_ranges.max_id()))
                .map(|m| m.0 as i32)
                .unwrap_or(0)
        });
        let ok = if let Some(ch) = self.input_channel(peer_id) {
            match self.invoke_rpc(tl_fns::channels::DeleteHistory {
                for_everyone: false,
                channel: ch,
                max_id,
            }) {
                Ok(updates) => {
                    let _ = self.apply_updates_to_session(&updates);
                    true
                }
                Err(_) => false,
            }
        } else if let Some(input_peer) = self.input_peer(peer_id) {
            let mut succeeded = false;
            for _ in 0..tg_api::DELETE_HISTORY_RETRY_LIMIT {
                match self.invoke_rpc(tl_fns::messages::DeleteHistory {
                    just_clear: true,
                    revoke: false,
                    peer: input_peer.clone(),
                    max_id,
                    min_date: None,
                    max_date: None,
                }) {
                    Ok(tl_enums::messages::AffectedHistory::AffectedHistory(aff)) => {
                        let _ = self.apply_affected_history(Some(peer_id), aff.pts, aff.pts_count);
                        succeeded = true;
                        if aff.offset <= 0 {
                            break;
                        }
                    }
                    Err(_) => {
                        succeeded = false;
                        break;
                    }
                }
            }
            succeeded
        } else {
            false
        };
        if ok {
            let ids: Vec<DataMsgId> = self.read_session(|s| {
                history_messages_ordered(s, data_peer)
                    .into_iter()
                    .map(|m| m.id)
                    .collect()
            });
            self.write_session(|s| {
                s.delete_messages(data_peer, &ids);
                s.set_history_unread_count(data_peer, 0);
                s.set_history_unread_mark(data_peer, false);
            });
            self.bridge.state().notify_ui();
        }
    }

    fn delete_dialog(&mut self, peer_id: PeerId) {
        let data_peer = to_data_peer(peer_id);
        let max_id = self.read_session(|s| {
            s.history(data_peer)
                .and_then(|h| h.top_message_id.or_else(|| h.loaded_ranges.max_id()))
                .map(|m| m.0 as i32)
                .unwrap_or(0)
        });
        let ok = if let Some(ch) = self.input_channel(peer_id) {
            match self.invoke_rpc(tl_fns::channels::LeaveChannel { channel: ch }) {
                Ok(updates) => {
                    let _ = self.apply_updates_to_session(&updates);
                    true
                }
                Err(_) => false,
            }
        } else if let Some(input_peer) = self.input_peer(peer_id) {
            let mut succeeded = false;
            for _ in 0..tg_api::DELETE_HISTORY_RETRY_LIMIT {
                match self.invoke_rpc(tl_fns::messages::DeleteHistory {
                    just_clear: false,
                    revoke: false,
                    peer: input_peer.clone(),
                    max_id,
                    min_date: None,
                    max_date: None,
                }) {
                    Ok(tl_enums::messages::AffectedHistory::AffectedHistory(aff)) => {
                        let _ = self.apply_affected_history(Some(peer_id), aff.pts, aff.pts_count);
                        succeeded = true;
                        if aff.offset <= 0 {
                            break;
                        }
                    }
                    Err(_) => {
                        succeeded = false;
                        break;
                    }
                }
            }
            succeeded
        } else {
            false
        };
        if ok {
            self.write_session(|s| {
                if let Some(h) = s.history_mut(data_peer) {
                    h.in_chat_list = false;
                }
                s.sync_dialog_lists(data_peer);
            });
            self.bridge.state().notify_ui();
        }
    }

    fn search_dialogs(&self, request: &SearchRequest) -> SearchResults {
        let hashtags = self
            .cache
            .lock()
            .map(|c| c.recent_hashtags_list.clone())
            .unwrap_or_default();
        crate::search_and_info::search_dialogs(self, request, &hashtags)
    }

    fn top_peers(&self) -> Vec<TopPeerItem> {
        crate::search_and_info::top_peers(self)
    }

    fn remove_top_peer(&mut self, peer_id: PeerId) {
        if let Ok(mut c) = self.cache.lock() {
            c.removed_top_peers.insert(peer_id);
        }
        crate::search_and_info::remove_top_peer(self, peer_id);
    }

    fn recent_search_peers(&self) -> Vec<RecentPeerItem> {
        let ids: Vec<PeerId> = self
            .cache
            .lock()
            .map(|c| c.recent_search_peers.clone())
            .unwrap_or_default();
        let now = now_unix_i32();
        self.read_session(|s| {
            ids.into_iter()
                .filter_map(|pid| {
                    let data_pid = to_data_peer(pid);
                    let peer = s.peer(data_pid)?;
                    let (status, _) = peer_status_text(peer, now);
                    let h = s.history(data_pid);
                    let verified = match peer {
                        Peer::User(u) => u.is_verified(),
                        Peer::Channel(ch) => ch.is_verified(),
                        Peer::Chat(_) => false,
                    };
                    Some(RecentPeerItem {
                        peer_id: pid,
                        kind: peer_dialog_kind(peer),
                        special: if data_pid == s.self_peer_id() {
                            tg_app_core::dialogs::backend::SpecialRowKind::SavedMessages
                        } else {
                            tg_app_core::dialogs::backend::SpecialRowKind::None
                        },
                        title: peer.name().to_owned(),
                        status,
                        color_index: peer.common().color_index % 7,
                        unread_count: h.and_then(|h| h.unread_count).unwrap_or(0).max(0) as u32,
                        muted: peer.common().notify.is_muted(now),
                        verified,
                    })
                })
                .collect()
        })
    }

    fn add_recent_search_peer(&mut self, peer_id: PeerId) {
        if let Ok(mut c) = self.cache.lock() {
            c.recent_search_peers.retain(|&p| p != peer_id);
            c.recent_search_peers.insert(0, peer_id);
            c.recent_search_peers.truncate(20);
        }
    }

    fn remove_recent_search_peer(&mut self, peer_id: PeerId) {
        if let Ok(mut c) = self.cache.lock() {
            c.recent_search_peers.retain(|&p| p != peer_id);
        }
    }

    fn clear_recent_search_peers(&mut self) {
        if let Ok(mut c) = self.cache.lock() {
            c.recent_search_peers.clear();
        }
    }

    fn recent_hashtags(&self) -> Vec<String> {
        self.cache
            .lock()
            .map(|c| c.recent_hashtags_list.clone())
            .unwrap_or_default()
    }

    fn remove_recent_hashtag(&mut self, tag: &str) {
        if let Ok(mut c) = self.cache.lock() {
            c.recent_hashtags_list.retain(|t| t != tag);
        }
    }

    fn search_tags(&self) -> Vec<SearchTagChip> {
        self.search_tags_live()
    }

    fn saved_messages_peer_id(&self) -> PeerId {
        self.read_session(|s| from_data_peer(s.self_peer_id()))
    }

    fn community_groups(&self) -> Vec<tg_app_core::core_types::misc::CommunityDialogGroup> {
        crate::communities::community_dialog_groups(self)
    }

    fn toggle_community_collapsed(&mut self, community_id: PeerId, collapsed: bool) {
        let _ = crate::communities::toggle_community_collapsed_in_dialogs(
            self,
            community_id,
            collapsed,
        );
    }

    fn promo_data(&self) -> Option<tg_app_core::core_types::misc::PromoDataSnapshot> {
        crate::suggestions::load_promo_data(self, &self.promo_state, now_unix_i32())
    }

    fn dismiss_promo_suggestion(
        &mut self,
        peer_id: Option<PeerId>,
        suggestion: &str,
    ) -> Result<bool, String> {
        crate::suggestions::dismiss_promo_suggestion_async(
            self,
            &self.promo_state,
            peer_id,
            suggestion,
        );
        Ok(true)
    }

    fn contact_birthdays(
        &self,
        force: bool,
    ) -> Result<Vec<tg_app_core::core_types::dto::contacts_extras::ContactBirthdayEntry>, String>
    {
        crate::contacts_extras::contact_birthdays(self, force)
    }

    fn poll_contact_birthdays(
        &self,
    ) -> Option<
        Result<Vec<tg_app_core::core_types::dto::contacts_extras::ContactBirthdayEntry>, String>,
    > {
        crate::contacts_extras::poll_contact_birthdays(self)
    }

    fn toggle_top_peers(&mut self, enabled: bool) -> Result<bool, String> {
        crate::contacts_extras::toggle_top_peers(self, enabled)
    }

    fn top_peers_enabled(&self) -> bool {
        crate::search_and_info::is_top_peers_enabled(self)
    }

    fn refresh_contact_statuses(
        &mut self,
    ) -> Result<Vec<tg_app_core::core_types::dto::contacts_extras::ContactStatusInfo>, String> {
        crate::contacts_extras::refresh_contact_statuses(self)
    }

    fn sponsored_peers(
        &self,
        query: &str,
    ) -> Result<Vec<tg_app_core::core_types::dto::contacts_extras::SponsoredPeerInfo>, String> {
        crate::contacts_extras::sponsored_peers(self, query)
    }

    fn hide_promo_data(&mut self, peer_id: PeerId) -> Result<bool, String> {
        crate::contacts_extras::hide_promo_data(self, peer_id)
    }

    fn mute_dialog_for(&mut self, peer_id: PeerId, seconds: i32) -> Result<bool, String> {
        crate::dialogs_actions::mute_dialog_for(self, peer_id, seconds)
    }

    fn dialog_mute_until(&self, peer_id: PeerId) -> Option<i32> {
        crate::dialogs_actions::dialog_mute_until(self, peer_id)
    }

    fn dialog_notify_sound(
        &self,
        peer_id: PeerId,
    ) -> tg_app_core::notifications::settings::NotifySound {
        crate::dialogs_actions::dialog_notify_sound(self, peer_id)
    }

    fn set_dialog_sound_enabled(&mut self, peer_id: PeerId, enabled: bool) -> Result<bool, String> {
        crate::dialogs_actions::set_dialog_sound_enabled(self, peer_id, enabled)
    }

    fn set_dialog_notify_sound(
        &mut self,
        peer_id: PeerId,
        sound: tg_app_core::notifications::settings::NotifySound,
    ) -> Result<(), String> {
        crate::dialogs_actions::set_dialog_notify_sound(self, peer_id, sound)
    }

    fn dialog_suggests_report_spam(&self, peer_id: PeerId) -> bool {
        crate::dialogs_actions::dialog_suggests_report_spam(self, peer_id)
    }

    fn report_spam_and_block(
        &mut self,
        peer_id: PeerId,
        report_spam: bool,
        delete_chat: bool,
    ) -> Result<(), String> {
        crate::dialogs_actions::report_spam_and_block(self, peer_id, report_spam, delete_chat)
    }

    fn delete_chat(&mut self, peer_id: PeerId, revoke: bool) -> Result<(), String> {
        crate::dialogs_delete::delete_conversation(self, peer_id, revoke)
    }

    fn clear_history_for(&mut self, peer_id: PeerId, revoke: bool) -> Result<(), String> {
        crate::dialogs_delete::clear_history_for(self, peer_id, revoke)
    }

    fn can_revoke_full_history(&self, peer_id: PeerId) -> bool {
        crate::dialogs_delete::can_revoke_full_history(self, peer_id)
    }

    fn peer_first_name(&self, peer_id: PeerId) -> Option<String> {
        crate::dialogs_delete::peer_first_name(self, peer_id)
    }

    fn can_delete_chat_for_everyone(&self, peer_id: PeerId) -> bool {
        crate::dialogs_delete::can_delete_chat_for_everyone(self, peer_id)
    }

    fn folders_including_peer(&self, peer_id: PeerId) -> Vec<FilterId> {
        crate::dialogs_delete::folders_including_peer(self, peer_id)
    }

    fn remove_peer_from_all_folders(&mut self, peer_id: PeerId) {
        crate::dialogs_delete::remove_peer_from_all_folders(self, peer_id);
    }

    fn unblock_and_restart_bot(&mut self, peer_id: PeerId) -> Result<(), String> {
        crate::dialogs_actions::unblock_and_restart_bot(self, peer_id)
    }
}

// ============================================================================
// 3. history::backend::HistoryBackend
// ============================================================================

impl LiveSession {
    fn slice_around_now(
        &self,
        peer_id: PeerId,
        around_id: MsgId,
        limit_before: usize,
        limit_after: usize,
    ) -> HistorySlice {
        let data_peer = to_data_peer(peer_id);
        // Clear pending deltas right before snapshotting `Session` so events already reflected
        // in the returned slice are not replayed, while events arriving afterward stay queued.
        self.bridge.state().clear_history_delta(data_peer);
        let media_cache = self.media_cache.clone();
        let mut slice = self.read_session(|s| {
            let mut slice = build_history_slice(
                s,
                data_peer,
                around_id,
                limit_before,
                limit_after,
                &media_cache,
            );
            // A window that stops short of either end of what is loaded is not at that end:
            // the view pages the rest in from the session.
            let ends = |first: bool| {
                let edge = if first {
                    s.loaded_messages(data_peer).next()
                } else {
                    s.loaded_messages(data_peer).next_back()
                };
                let shown = if first {
                    slice.messages.first()
                } else {
                    slice.messages.last()
                };
                edge.is_none_or(|edge| shown.is_some_and(|m| m.id == from_data_msg(edge.id)))
            };
            let (at_oldest, at_newest) = (ends(true), ends(false));
            slice.loaded_at_top &= at_oldest;
            slice.loaded_at_bottom &= at_newest;
            if at_newest {
                append_local_messages(s, data_peer, &mut slice);
            }
            slice
        });
        self.decorate_cached_translations(peer_id, &mut slice.messages);
        crate::rich_message::attach_cached_rich_messages(
            self.runtime.session(),
            &mut slice.messages,
        );
        crate::widgets::attach_cached_widget_states(self.runtime.session(), &mut slice.messages);
        crate::dice_pack::attach_and_prefetch_dice_lottie(self, &mut slice.messages);
        crate::reply_targets::attach_reply_targets(self, &mut slice.messages);
        crate::custom_emoji_loader::request_for_messages(self, &slice.messages);
        if slice.loaded_at_bottom {
            crate::chat_l233::append_streamed_drafts_to_slice(self, peer_id, &mut slice.messages);
        }
        slice
    }

    fn slice_before_now(&self, peer_id: PeerId, before_id: MsgId, limit: usize) -> HistorySlice {
        let data_peer = to_data_peer(peer_id);
        let media_cache = self.media_cache.clone();
        let mut slice = self.read_session(|s| {
            let Some(history) = s.history(data_peer) else {
                return HistorySlice::default();
            };
            let target = to_data_msg(before_id);
            let resolve = |doc: &tg_data::DocumentData| media_cache.cached_lottie_json_for_doc(doc);
            // A range query from `target` down, only as far as the page needs (TGB-2938).
            let mut msgs: Vec<HistoryMessage> = s
                .loaded_messages_before(data_peer, target)
                .take(limit)
                .map(|m| message_to_history_message_with_stickers(s, m, &resolve))
                .collect();
            msgs.reverse();
            HistorySlice {
                messages: msgs,
                loaded_at_top: history.loaded_ranges.loaded_at_top(),
                loaded_at_bottom: history.loaded_ranges.loaded_at_bottom(),
                total_count: loaded_id_count(history),
                first_unread_id: first_unread_in_history(s, data_peer),
                unread_count: history.unread_count.unwrap_or(0).max(0) as u32,
            }
        });
        self.decorate_cached_translations(peer_id, &mut slice.messages);
        crate::rich_message::attach_cached_rich_messages(
            self.runtime.session(),
            &mut slice.messages,
        );
        crate::widgets::attach_cached_widget_states(self.runtime.session(), &mut slice.messages);
        crate::dice_pack::attach_and_prefetch_dice_lottie(self, &mut slice.messages);
        crate::reply_targets::attach_reply_targets(self, &mut slice.messages);
        crate::custom_emoji_loader::request_for_messages(self, &slice.messages);
        slice
    }

    fn slice_after_now(&self, peer_id: PeerId, after_id: MsgId, limit: usize) -> HistorySlice {
        let data_peer = to_data_peer(peer_id);
        let media_cache = self.media_cache.clone();
        let mut slice = self.read_session(|s| {
            let Some(history) = s.history(data_peer) else {
                return HistorySlice::default();
            };
            let target = to_data_msg(after_id);
            let resolve = |doc: &tg_data::DocumentData| media_cache.cached_lottie_json_for_doc(doc);
            let msgs: Vec<HistoryMessage> = s
                .loaded_messages_after(data_peer, target)
                .take(limit)
                .map(|m| message_to_history_message_with_stickers(s, m, &resolve))
                .collect();
            let reaches_bottom = msgs.len() < limit;
            let mut slice = HistorySlice {
                messages: msgs,
                loaded_at_top: history.loaded_ranges.loaded_at_top(),
                loaded_at_bottom: history.loaded_ranges.loaded_at_bottom(),
                total_count: loaded_id_count(history),
                first_unread_id: first_unread_in_history(s, data_peer),
                unread_count: history.unread_count.unwrap_or(0).max(0) as u32,
            };
            if reaches_bottom && slice.loaded_at_bottom {
                append_local_messages(s, data_peer, &mut slice);
            }
            slice
        });
        self.decorate_cached_translations(peer_id, &mut slice.messages);
        crate::rich_message::attach_cached_rich_messages(
            self.runtime.session(),
            &mut slice.messages,
        );
        crate::widgets::attach_cached_widget_states(self.runtime.session(), &mut slice.messages);
        crate::dice_pack::attach_and_prefetch_dice_lottie(self, &mut slice.messages);
        crate::reply_targets::attach_reply_targets(self, &mut slice.messages);
        crate::custom_emoji_loader::request_for_messages(self, &slice.messages);
        slice
    }

    /// [`Self::poll_visible_slice_reactions`] without waiting: `messages.getMessagesReactions`
    /// runs on a worker thread (one per chat at a time) and the refreshed messages reach the
    /// open history as edits, like tdesktop's periodic `Data::Reactions` poll. The slice shows the
    /// reactions it was loaded with until then.
    fn poll_slice_reactions_in_background(&self, peer_id: PeerId, messages: &[HistoryMessage]) {
        let ids: Vec<MsgId> = messages
            .iter()
            .filter(|m| m.id.0 > 0)
            .map(|m| m.id)
            .collect();
        self.poll_reactions_in_background(peer_id, ids, false);
    }

    /// Polls the reactions of `ids` on a worker thread, one poll per chat at a time; the
    /// messages whose reactions changed reach the open history as edits. With `mark_due` the
    /// ids are first narrowed to those due ([`Self::due_reaction_polls`]) and marked polled, so
    /// the history can pass its painted messages every frame (TGB-3013).
    pub(crate) fn poll_reactions_in_background(
        &self,
        peer_id: PeerId,
        ids: Vec<MsgId>,
        mark_due: bool,
    ) {
        let data_peer = to_data_peer(peer_id);
        if data_peer.is_user() || ids.is_empty() {
            return;
        }
        let first = self
            .cache
            .lock()
            .is_ok_and(|mut c| c.reaction_polls.insert(data_peer));
        if !first {
            return;
        }
        let release = move |live: &Self| {
            if let Ok(mut c) = live.cache.lock() {
                c.reaction_polls.remove(&data_peer);
            }
        };
        let ids = if mark_due {
            self.due_reaction_polls(peer_id, &ids, true)
        } else {
            ids
        };
        if ids.is_empty() {
            release(self);
            return;
        }
        let live = self.clone();
        let spawned = std::thread::Builder::new()
            .name("reactions-poll".to_owned())
            .spawn(move || {
                let snapshot = |live: &Self| {
                    live.read_session(|s| {
                        ids.iter()
                            .map(|&id| {
                                s.message(DataFullMsgId::new(data_peer, to_data_msg(id)))
                                    .and_then(|m| m.reactions.clone())
                            })
                            .collect::<Vec<_>>()
                    })
                };
                let before = snapshot(&live);
                let polled = if mark_due {
                    live.poll_reactions_of_live(peer_id, &ids)
                } else {
                    live.poll_visible_messages_reactions_live(peer_id, &ids)
                };
                if polled.is_ok_and(|p| !p.is_empty()) {
                    let after = snapshot(&live);
                    let bridge = live.bridge.state();
                    let mut changed = false;
                    for ((id, was), now) in ids.iter().zip(&before).zip(&after) {
                        if was != now {
                            bridge.record_message_edited(data_peer, to_data_msg(*id));
                            changed = true;
                        }
                    }
                    if changed {
                        bridge.notify_chat_changed(data_peer);
                    }
                }
                release(&live);
            });
        if spawned.is_err() {
            release(self);
        }
    }

    fn poll_visible_slice_reactions(&self, peer_id: PeerId, messages: &mut [HistoryMessage]) {
        if to_data_peer(peer_id).is_user() {
            return;
        }
        let reacted_ids: Vec<MsgId> = messages
            .iter()
            .filter(|m| m.id.0 > 0)
            .map(|m| m.id)
            .collect();
        if reacted_ids.is_empty() {
            return;
        }
        let Ok(polled) = self.poll_visible_messages_reactions_live(peer_id, &reacted_ids) else {
            return;
        };
        if polled.is_empty() {
            return;
        }
        let data_peer = to_data_peer(peer_id);
        let media_cache = self.media_cache.clone();
        self.read_session(|s| {
            let resolve = |doc: &tg_data::DocumentData| media_cache.cached_lottie_json_for_doc(doc);
            for msg in messages.iter_mut() {
                if !polled.contains(&msg.id) {
                    continue;
                }
                let full_id = DataFullMsgId::new(data_peer, to_data_msg(msg.id));
                if let Some(updated) = s.message(full_id) {
                    msg.reactions =
                        message_to_history_message_with_stickers(s, updated, &resolve).reactions;
                }
            }
        });
    }
}

impl HistoryBackend for LiveSession {
    fn chat_version(&self, peer: PeerId) -> u64 {
        self.bridge.state().chat_version(to_data_peer(peer))
    }

    fn search_chat_messages(
        &self,
        peer_id: PeerId,
        query: &str,
        from: Option<PeerId>,
        offset_id: MsgId,
        limit: i32,
    ) -> Result<tg_app_core::history::backend::ChatSearchPage, String> {
        crate::chat_search::chat_search_page(self, peer_id, query, from, offset_id, limit)
    }

    fn self_peer(&self) -> (PeerId, String) {
        self.read_session(|s| {
            let pid = s.self_peer_id();
            (from_data_peer(pid), peer_display_title(s, pid))
        })
    }

    fn peer_info(&self, peer_id: PeerId) -> Option<HistoryPeerInfo> {
        let data_peer = to_data_peer(peer_id);
        let dismissed = self
            .cache
            .lock()
            .map(|c| c.dismissed_contact_banners.contains(&peer_id))
            .unwrap_or(false);
        let now = now_unix_i32();
        let (info, fresh) = self.read_session_or_last(
            || self.ui_reads.last_peer_info(peer_id),
            |s| {
                let mut info = peer_to_history_peer_info(s, data_peer, now)?;
                if dismissed {
                    info.contact_status = None;
                }
                Some(info)
            },
        );
        if fresh {
            self.ui_reads.store_peer_info(peer_id, &info);
        }
        info
    }

    fn load_slice_around(
        &self,
        peer_id: PeerId,
        around_id: MsgId,
        limit_before: usize,
        limit_after: usize,
    ) -> HistorySlice {
        self.ensure_history_loaded(peer_id, around_id, LoadDirection::Around);
        let mut slice = self.slice_around_now(peer_id, around_id, limit_before, limit_after);
        self.poll_visible_slice_reactions(peer_id, &mut slice.messages);
        slice
    }

    fn try_load_slice_around(
        &self,
        peer_id: PeerId,
        around_id: MsgId,
        limit_before: usize,
        limit_after: usize,
    ) -> Loadable<HistorySlice> {
        if self.history_ready_or_request(peer_id, around_id, LoadDirection::Around) {
            let slice = self.slice_around_now(peer_id, around_id, limit_before, limit_after);
            self.poll_slice_reactions_in_background(peer_id, &slice.messages);
            Loadable::Ready(slice)
        } else {
            Loadable::Loading
        }
    }

    fn load_slice_before(&self, peer_id: PeerId, before_id: MsgId, limit: usize) -> HistorySlice {
        self.ensure_history_loaded(peer_id, before_id, LoadDirection::Before);
        let mut slice = self.slice_before_now(peer_id, before_id, limit);
        self.poll_visible_slice_reactions(peer_id, &mut slice.messages);
        slice
    }

    fn try_load_slice_before(
        &self,
        peer_id: PeerId,
        before_id: MsgId,
        limit: usize,
    ) -> Loadable<HistorySlice> {
        if self.history_ready_or_request(peer_id, before_id, LoadDirection::Before) {
            let slice = self.slice_before_now(peer_id, before_id, limit);
            self.poll_slice_reactions_in_background(peer_id, &slice.messages);
            Loadable::Ready(slice)
        } else {
            Loadable::Loading
        }
    }

    fn load_slice_after(&self, peer_id: PeerId, after_id: MsgId, limit: usize) -> HistorySlice {
        self.ensure_history_loaded(peer_id, after_id, LoadDirection::After);
        let mut slice = self.slice_after_now(peer_id, after_id, limit);
        self.poll_visible_slice_reactions(peer_id, &mut slice.messages);
        slice
    }

    fn try_load_slice_after(
        &self,
        peer_id: PeerId,
        after_id: MsgId,
        limit: usize,
    ) -> Loadable<HistorySlice> {
        if self.history_ready_or_request(peer_id, after_id, LoadDirection::After) {
            let slice = self.slice_after_now(peer_id, after_id, limit);
            self.poll_slice_reactions_in_background(peer_id, &slice.messages);
            Loadable::Ready(slice)
        } else {
            Loadable::Loading
        }
    }

    fn set_history_open(&self, peer_id: PeerId, open: bool) {
        // Off the UI thread: the write lock may be held by the session actor (TGB-2938).
        let session = Arc::clone(self.runtime.session());
        let data_peer = to_data_peer(peer_id);
        self.runtime.tokio_handle().spawn(async move {
            session.write().await.set_history_open(data_peer, open);
        });
    }

    fn drain_history_updates(&mut self, peer_id: PeerId) -> Vec<HistoryLiveUpdate> {
        let resolutions = self.sends.take_resolved();
        let mut out = self.drain_history_delta(peer_id);
        if !resolutions.is_empty() {
            out.extend(self.send_resolution_updates(peer_id, &resolutions));
        }
        out
    }

    fn send_message(
        &mut self,
        peer_id: PeerId,
        request: SendMessageRequest,
    ) -> Result<HistoryMessage, String> {
        let trimmed = request.text.text.trim();
        if trimmed.is_empty() {
            return Err("Cannot send an empty message.".to_owned());
        }
        let data_peer = to_data_peer(peer_id);
        let entities = self.read_session(|s| ui_text_to_tl_entities(&request.text, s));
        let reply_to = request.reply_to.map(|mid| make_input_reply_to(mid, None));
        let options = SendOptions {
            silent: request.silent,
            ..SendOptions::default()
        };
        let extras = crate::optimistic_send::QueuedSendExtras {
            reply_to_msg_id: request.reply_to.map(to_data_msg),
            ..Default::default()
        };
        self.send_text_optimistic(
            data_peer,
            &request.text.text,
            &entities,
            reply_to,
            options,
            &extras,
        )
    }

    fn send_message_extended(
        &mut self,
        peer_id: PeerId,
        request: SendMessageExtended,
    ) -> Result<HistoryMessage, String> {
        if request.media.is_some() {
            return crate::upload::send_history_media_extended(self, peer_id, request);
        }
        if request.thread_key.is_some() || request.schedule_date.is_some() {
            return self.send_thread_or_scheduled_extended(peer_id, request);
        }
        self.send_message_extended_live(peer_id, request)
    }

    fn pending_media_uploads(&self, peer_id: PeerId) -> Vec<MediaUploadStatus> {
        crate::upload::pending_media_uploads_for(self, peer_id)
    }

    fn cancel_media_upload(&mut self, upload_id: u64) -> bool {
        crate::upload::cancel_media_upload_by_id(self, upload_id)
    }

    fn edit_message(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        text: TextWithEntities,
    ) -> Result<HistoryMessage, String> {
        let Some(input_peer) = self.input_peer(peer_id) else {
            return Err("Peer not found in session.".to_owned());
        };
        let entities = self.read_session(|s| ui_text_to_tl_entities(&text, s));
        let req = tl_fns::messages::EditMessage {
            no_webpage: false,
            invert_media: false,
            peer: input_peer,
            id: msg_id.0,
            message: Some(text.text.clone()),
            media: None,
            reply_markup: None,
            entities: (!entities.is_empty()).then_some(entities),
            schedule_date: None,
            schedule_repeat_period: None,
            quick_reply_shortcut_id: None,
            rich_message: None,
        };
        let maybe_updates = self.edit_message_via_runtime(req)?;
        let data_peer = to_data_peer(peer_id);
        let data_msg = to_data_msg(msg_id);
        if let Some(ref updates) = maybe_updates {
            self.apply_updates_side_caches(updates);
            if !updates_contain_edit_for(updates, data_msg) {
                let data_text = ui_text_to_data_text(&text);
                let edit_ts = now_unix_i32();
                self.write_session(|s| {
                    if let Some(m) = s.message_mut(DataFullMsgId::new(data_peer, data_msg)) {
                        if let MessageContent::Regular {
                            text: ref mut t, ..
                        } = m.content
                        {
                            *t = data_text;
                        }
                        m.edit_date = Some(edit_ts);
                    }
                });
            }
        }
        self.bridge.state().notify_ui();
        self.read_session(|s| {
            s.message(DataFullMsgId::new(data_peer, data_msg))
                .map(|m| message_to_history_message(s, m))
        })
        .ok_or_else(|| "Edited message not found in session.".to_owned())
    }

    fn send_rich_message(
        &mut self,
        peer_id: PeerId,
        input: tl_types::InputRichMessage,
        page: tg_iv::rich_page::RichPage,
        reply_to: Option<MsgId>,
        silent: bool,
    ) -> Result<HistoryMessage, String> {
        crate::rich_message::send_rich_message_live(self, peer_id, input, page, reply_to, silent)
    }

    fn send_saved_music(
        &mut self,
        peer_id: PeerId,
        tracks: &[tg_app_core::info::backend::SavedMusicTrack],
    ) -> Result<Vec<HistoryMessage>, String> {
        crate::music_send::send_saved_music_live(self, peer_id, tracks)
    }

    fn compose_rich_message_with_ai(
        &mut self,
        request: tg_app_core::core_types::dto::compose_ai::ComposeRichAiRequest,
    ) -> Result<tg_app_core::core_types::dto::compose_ai::ComposeRichAiResult, String> {
        <Self as tg_app_core::core_types::dto::compose_ai::ComposeAiBackend>::compose_rich_message_with_ai(
            self, request,
        )
    }

    fn translate_rich_message(
        &mut self,
        request: tg_app_core::core_types::dto::compose_ai::TranslateRichMessageRequest,
    ) -> Result<tg_app_core::core_types::dto::compose_ai::TranslateRichMessageResult, String> {
        <Self as tg_app_core::core_types::dto::compose_ai::ComposeAiBackend>::translate_rich_message(
            self, request,
        )
    }

    fn edit_rich_message(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        input: tl_types::InputRichMessage,
        page: tg_iv::rich_page::RichPage,
    ) -> Result<HistoryMessage, String> {
        crate::rich_message::edit_rich_message_live(self, peer_id, msg_id, input, page)
    }

    fn edit_message_media(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        request: tg_app_core::core_types::dto::edit_caption::EditMediaCaptionRequest,
    ) -> Result<HistoryMessage, String> {
        crate::search_and_info::edit_message_media(self, peer_id, msg_id, request)
    }

    fn query_inline_bot(
        &self,
        bot_username: &str,
        query: &str,
    ) -> Vec<tg_app_core::history::sections_model::InlineBotResult> {
        crate::inline_bot_query::query_inline_bot(self, bot_username, query)
    }

    fn contacts(&self) -> Vec<tg_app_core::history::sections_model::ContactAttachment> {
        crate::search_and_info::history_contacts(self)
    }

    fn nearby_venues(&self) -> Vec<tg_app_core::history::sections_model::VenueItem> {
        crate::search_and_info::nearby_venues(self)
    }

    fn nearby_venues_for_coords(
        &self,
        lat: f64,
        lon: f64,
        query: &str,
    ) -> Vec<tg_app_core::history::sections_model::VenueItem> {
        crate::search_and_info::nearby_venues_for_coords(self, lat, lon, query)
    }

    fn load_static_map_tile(
        &self,
        lat: f64,
        lon: f64,
        width: i32,
        height: i32,
        zoom: u8,
        scale: u8,
    ) -> Result<Vec<u8>, String> {
        crate::geo_map::load_static_map_tile(self, lat, lon, width, height, zoom, scale)
    }

    fn load_static_map_tile_async(
        &self,
        lat: f64,
        lon: f64,
        width: i32,
        height: i32,
        zoom: u8,
        scale: u8,
        repaint: Option<egui::Context>,
    ) -> tg_app_core::core_types::platform_call::PlatformCall<Result<Vec<u8>, String>> {
        crate::geo_map::load_static_map_tile_async(
            self, lat, lon, width, height, zoom, scale, repaint,
        )
    }

    fn default_location_country_iso2(&self) -> Option<String> {
        crate::geo_map::default_location_country_iso2(self)
    }

    fn search_results_calendar(
        &self,
        peer_id: PeerId,
        year: i32,
        month: u32,
    ) -> Result<Vec<tg_app_core::core_types::dto::calendar::CalendarDayInfo>, String> {
        crate::search_and_info::search_results_calendar(self, peer_id, year, month)
    }

    fn delete_messages(
        &mut self,
        peer_id: PeerId,
        msg_ids: &[MsgId],
        revoke_for_all: bool,
    ) -> Result<(), String> {
        if msg_ids.is_empty() {
            return Ok(());
        }
        let (local_ids, data_ids): (Vec<DataMsgId>, Vec<DataMsgId>) = msg_ids
            .iter()
            .copied()
            .map(to_data_msg)
            .partition(|id| id.is_client());
        if !local_ids.is_empty() {
            // A message being sent or failed exists only here (`HistoryItem::destroy`).
            let data_peer = to_data_peer(peer_id);
            self.write_session(|s| s.delete_messages(data_peer, &local_ids));
        }
        if !data_ids.is_empty() {
            self.delete_messages_via_runtime(peer_id, data_ids, revoke_for_all)?;
        }
        self.bridge.state().notify_ui();
        Ok(())
    }

    fn forward_messages(
        &mut self,
        from_peer: PeerId,
        msg_ids: &[MsgId],
        to_peer: PeerId,
    ) -> Result<Vec<HistoryMessage>, String> {
        if msg_ids.is_empty() {
            return Ok(Vec::new());
        }
        let Some(from_input) = self.input_peer(from_peer) else {
            return Err("Source peer not found.".to_owned());
        };
        let Some(to_input) = self.input_peer(to_peer) else {
            return Err("Destination peer not found.".to_owned());
        };
        let random_ids: Vec<i64> = {
            let mut c = self
                .cache
                .lock()
                .map_err(|_| "live session cache lock poisoned".to_owned())?;
            msg_ids.iter().map(|_| c.next_random_id()).collect()
        };
        let req = tl_fns::messages::ForwardMessages {
            silent: false,
            background: false,
            with_my_score: false,
            drop_author: false,
            drop_media_captions: false,
            noforwards: false,
            allow_paid_floodskip: false,
            from_ephemeral: false,
            from_peer: from_input,
            id: msg_ids.iter().map(|m| m.0).collect(),
            random_id: random_ids,
            to_peer: to_input,
            top_msg_id: None,
            reply_to: None,
            schedule_date: None,
            schedule_repeat_period: None,
            send_as: None,
            quick_reply_shortcut: None,
            effect: None,
            video_timestamp: None,
            allow_paid_stars: None,
            suggested_post: None,
        };
        let updates = self.invoke_rpc(req)?;
        let new_ids = self.apply_updates_to_session(&updates);
        let dest_data_peer = to_data_peer(to_peer);
        let forwarded = self.read_session(|s| {
            new_ids
                .into_iter()
                .filter_map(|mid| {
                    s.message(DataFullMsgId::new(dest_data_peer, mid))
                        .map(|m| message_to_history_message(s, m))
                })
                .collect()
        });
        Ok(forwarded)
    }

    fn mark_history_read(&mut self, peer_id: PeerId, max_id: MsgId) {
        let data_peer = to_data_peer(peer_id);
        let data_max = if max_id == MsgId::SHOW_AT_END || max_id.0 <= 0 {
            self.read_session(|s| {
                s.history(data_peer)
                    .and_then(|h| h.top_message_id.or_else(|| h.loaded_ranges.max_id()))
                    .unwrap_or(DataMsgId(0))
            })
        } else {
            to_data_msg(max_id)
        };
        if data_max.0 <= 0 {
            return;
        }
        // Like `Data::Histories::readInboxTill` (`data/data_histories.cpp:273-284`): the local
        // read state changes now, `messages.readHistory` goes out without the UI waiting for it.
        // The runtime applies the same read again when the request is answered.
        self.write_session(|s| {
            let still_unread = s
                .history(data_peer)
                .and_then(|h| h.top_message_id)
                .filter(|&top| data_max >= top)
                .map(|_| 0);
            s.mark_inbox_read(data_peer, data_max, still_unread);
        });
        let rt = Arc::clone(&self.runtime);
        self.ui_rpc.push(async move {
            if let Err(err) =
                with_rpc_timeout(LIVE_RPC_TIMEOUT, rt.mark_read(data_peer, data_max, true)).await
            {
                debug!(peer = data_peer.0, error = %err, "mark_read failed");
            }
        });
        self.bridge.state().notify_ui();
    }

    fn send_typing(&mut self, peer_id: PeerId, action: TypingAction) {
        // `Api::SendProgressManager` sends `messages.setTyping` without anyone waiting for it;
        // the queue keeps a cancel behind the typing it cancels.
        let data_peer = to_data_peer(peer_id);
        if let Some(kind) = history_typing_to_send_progress(action) {
            let rt = Arc::clone(&self.runtime);
            self.ui_rpc.push(async move {
                let _ =
                    with_rpc_timeout(LIVE_RPC_TIMEOUT, rt.send_typing(data_peer, None, kind, 0))
                        .await;
            });
        } else if let Some(input_peer) = self.input_peer(peer_id) {
            let rpc = self.runtime.rpc().clone();
            let req = tl_fns::messages::SetTyping {
                peer: input_peer,
                top_msg_id: None,
                action: tl_enums::SendMessageAction::SendMessageCancelAction,
            };
            self.ui_rpc.push(async move {
                let _ =
                    with_rpc_timeout(LIVE_RPC_TIMEOUT, async move { rpc.invoke(&req).await }).await;
            });
        }
    }

    fn toggle_reaction(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        emoji: &str,
    ) -> Result<Vec<ReactionCounter>, String> {
        self.toggle_reaction_live(peer_id, msg_id, emoji)
    }

    fn pin_message(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        unpin: bool,
    ) -> Result<Vec<PinnedMessageSummary>, String> {
        let Some(input_peer) = self.input_peer(peer_id) else {
            return Err("Peer not found.".to_owned());
        };
        let req = tl_fns::messages::UpdatePinnedMessage {
            silent: false,
            unpin,
            pm_oneside: false,
            peer: input_peer,
            id: msg_id.0,
        };
        let updates = self.invoke_rpc(req)?;
        let _ = self.apply_updates_to_session(&updates);

        if !updates_contain_pin_for(&updates, msg_id.0) {
            let data_peer = to_data_peer(peer_id);
            let data_msg = to_data_msg(msg_id);
            self.write_session(|s| {
                let _ = s.set_message_pinned(DataFullMsgId::new(data_peer, data_msg), !unpin);
            });
        }
        self.invalidate_pinned_cache(peer_id);
        self.bridge.state().notify_ui();
        Ok(self.pinned_messages(peer_id))
    }

    fn pinned_messages(&self, peer_id: PeerId) -> Vec<PinnedMessageSummary> {
        crate::search_and_info::pinned_messages(self, peer_id)
    }

    fn pinned_full_messages(
        &self,
        peer_id: PeerId,
        topic_root_id: Option<MsgId>,
    ) -> Vec<HistoryMessage> {
        crate::search_and_info::pinned_full_messages(self, peer_id, topic_root_id)
    }

    fn unpin_all_messages(
        &mut self,
        peer_id: PeerId,
        topic_root_id: Option<MsgId>,
    ) -> Result<(), String> {
        crate::search_and_info::unpin_all_messages(self, peer_id, topic_root_id)
    }

    fn report_messages(
        &mut self,
        peer_id: PeerId,
        msg_ids: &[MsgId],
        reason: tg_app_core::history::sections_model::MessageReportReason,
        comment: &str,
    ) -> Result<(), String> {
        crate::search_and_info::report_messages(self, peer_id, msg_ids, reason, comment)
    }

    fn vote_poll(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        option_indices: &[usize],
    ) -> Result<(), String> {
        let Some(input_peer) = self.input_peer(peer_id) else {
            return Err("Peer not found.".to_owned());
        };
        let data_peer = to_data_peer(peer_id);
        let data_msg = to_data_msg(msg_id);
        let option_bytes: Vec<Vec<u8>> = self.read_session(|s| {
            let Some(msg) = s.message(DataFullMsgId::new(data_peer, data_msg)) else {
                return Vec::new();
            };
            let Some(MessageMedia::Poll(poll)) = msg.media() else {
                return Vec::new();
            };
            option_indices
                .iter()
                .filter_map(|&idx| poll.answers.get(idx).map(|a| a.option.clone()))
                .collect()
        });
        let req = tl_fns::messages::SendVote {
            peer: input_peer,
            msg_id: msg_id.0,
            options: option_bytes,
        };
        let updates = self.invoke_rpc(req)?;
        let _ = self.apply_updates_to_session(&updates);
        Ok(())
    }

    fn save_draft(&mut self, peer_id: PeerId, draft: Option<DraftMessage>) {
        let data_peer = to_data_peer(peer_id);
        let (text, reply_to_id) = match &draft {
            Some(d) if !d.text.is_empty() || d.reply_to.is_some() => (d.text.clone(), d.reply_to),
            _ => (String::new(), None),
        };
        if let Some(input_peer) = self.input_peer(peer_id) {
            let reply_to = reply_to_id.map(|mid| make_input_reply_to(mid, None));
            let req = tl_fns::messages::SaveDraft {
                no_webpage: false,
                invert_media: false,
                reply_to,
                peer: input_peer,
                message: text.clone(),
                entities: None,
                media: None,
                effect: None,
                suggested_post: None,
                rich_message: None,
            };
            // `ApiWrap::saveDraftToCloudDelayed`: the request goes out in the background.
            let rpc = self.runtime.rpc().clone();
            self.ui_rpc.push(async move {
                let res =
                    with_rpc_timeout(LIVE_RPC_TIMEOUT, async move { rpc.invoke(&req).await }).await;
                if let Err(err) = res {
                    debug!(peer = peer_id.0, error = %err, "messages.saveDraft failed");
                }
            });
        }
        let now = now_unix_i32();
        let local_key = DraftKey::local(DataMsgId::ZERO, DataPeerId::ZERO);
        let cloud_key = DraftKey::cloud(DataMsgId::ZERO, DataPeerId::ZERO);
        self.write_session(|s| {
            if text.is_empty() && reply_to_id.is_none() {
                let _ = s.clear_draft(data_peer, local_key);
                let _ = s.clear_draft(data_peer, cloud_key);
            } else {
                let data_draft = Draft {
                    date: now,
                    text: DataText::plain(text),
                    reply: reply_to_id.map(|mid| ReplyInfo {
                        reply_to_msg_id: Some(to_data_msg(mid)),
                        ..ReplyInfo::default()
                    }),
                    webpage: WebPageDraft::default(),
                    cursor_position: 0,
                    cursor_anchor: 0,
                    effect_id: None,
                };
                s.set_draft(data_peer, local_key, data_draft.clone());
                s.set_draft(data_peer, cloud_key, data_draft);
            }
        });
        self.bridge.state().notify_ui();
    }

    fn load_draft(&self, peer_id: PeerId) -> Option<DraftMessage> {
        let data_peer = to_data_peer(peer_id);
        self.read_session(|s| s.history(data_peer).and_then(draft_from_history))
    }

    fn dismiss_contact_status(&mut self, peer_id: PeerId) {
        if let Ok(mut c) = self.cache.lock() {
            c.dismissed_contact_banners.insert(peer_id);
        }
        self.bridge.state().mark_views_changed();
    }

    fn add_contact(&mut self, peer_id: PeerId) {
        let data_peer = to_data_peer(peer_id);
        let maybe_req = self.read_session(|s| {
            let user_id = data_peer.to_user()?;
            let user = s.peer(data_peer)?.as_user()?;
            let input_user = input_user_for(s, user_id);
            if matches!(input_user, tl_enums::InputUser::Empty) {
                return None;
            }
            Some(tl_fns::contacts::AddContact {
                add_phone_privacy_exception: false,
                id: input_user,
                first_name: user.first_name.clone(),
                last_name: user.last_name.clone(),
                phone: user.phone.clone(),
                note: None,
            })
        });
        if let Some(req) = maybe_req
            && let Ok(updates) = self.invoke_rpc(req)
        {
            let _ = self.apply_updates_to_session(&updates);
        }
        self.dismiss_contact_status(peer_id);
    }

    fn toggle_block_peer(&mut self, peer_id: PeerId) {
        let _ = self.toggle_peer_block_rpc(peer_id);
    }

    fn toggle_mute(&mut self, peer_id: PeerId) -> bool {
        self.toggle_peer_mute_rpc(peer_id)
    }

    fn as_bots_backend(&self) -> Option<&dyn tg_app_core::bots::backend::BotsBackend> {
        Some(self)
    }

    fn as_bots_backend_mut(&mut self) -> Option<&mut dyn tg_app_core::bots::backend::BotsBackend> {
        Some(self)
    }

    fn as_chat_helpers_backend(
        &self,
    ) -> Option<&dyn tg_app_core::chat_helpers::backend::ChatHelpersBackend> {
        Some(self)
    }

    fn as_chat_helpers_backend_mut(
        &mut self,
    ) -> Option<&mut dyn tg_app_core::chat_helpers::backend::ChatHelpersBackend> {
        Some(self)
    }

    fn send_poll(
        &mut self,
        peer_id: PeerId,
        thread_key: Option<ThreadKey>,
        request: CreatePollRequest,
    ) -> Result<HistoryMessage, String> {
        self.send_poll_live(peer_id, thread_key, request)
    }

    fn send_poll_with_extras(
        &mut self,
        peer_id: PeerId,
        thread_key: Option<ThreadKey>,
        request: CreatePollRequest,
        extras: tg_app_core::history::sections_model::PollMessageExtras,
    ) -> Result<HistoryMessage, String> {
        self.send_poll_with_extras_live(peer_id, thread_key, request, extras)
    }

    fn add_poll_option(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        request: tg_app_core::history::sections_model::AddPollOptionRequest,
    ) -> Result<HistoryMessage, String> {
        self.add_poll_option_live(peer_id, msg_id, request)
    }

    fn delete_poll_option(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        option_bytes: &[u8],
    ) -> Result<(), String> {
        self.delete_poll_option_live(peer_id, msg_id, option_bytes)
    }

    fn mark_ttl_media_viewed(&mut self, peer_id: PeerId, msg_id: MsgId) -> Result<(), String> {
        tg_app_core::core_types::dto::self_destruct::SelfDestructBackend::mark_ttl_media_viewed(
            self, peer_id, msg_id,
        )
    }

    fn as_compose_ai_backend(
        &mut self,
    ) -> Option<&mut dyn tg_app_core::core_types::dto::compose_ai::ComposeAiBackend> {
        Some(self)
    }

    fn as_sticker_creator_backend(
        &mut self,
    ) -> Option<&mut dyn tg_app_core::core_types::dto::sticker_creator::StickerCreatorBackend> {
        Some(self)
    }

    fn as_self_destruct_backend(
        &mut self,
    ) -> Option<&mut dyn tg_app_core::core_types::dto::self_destruct::SelfDestructBackend> {
        Some(self)
    }

    fn streamed_draft_messages(
        &self,
        peer_id: PeerId,
        root_id: Option<MsgId>,
    ) -> Vec<HistoryMessage> {
        crate::chat_l233::streamed_draft_messages_live(self, peer_id, root_id)
    }

    fn has_stoppable_streamed_draft(&self, peer_id: PeerId, root_id: Option<MsgId>) -> bool {
        crate::chat_l233::has_stoppable_streamed_draft_live(self, peer_id, root_id)
    }

    fn stop_streamed_draft(
        &mut self,
        peer_id: PeerId,
        root_id: Option<MsgId>,
    ) -> Result<bool, String> {
        crate::chat_l233::stop_streamed_draft_live(self, peer_id, root_id)
    }

    fn compose_stash(
        &self,
        key: tg_app_core::core_types::dto::compose_stash::ComposeStashKey,
    ) -> Option<tg_app_core::core_types::dto::compose_stash::ComposeStash> {
        crate::chat_l233::compose_stash_live(self, key)
    }

    fn set_compose_stash(
        &mut self,
        key: tg_app_core::core_types::dto::compose_stash::ComposeStashKey,
        stash: Option<tg_app_core::core_types::dto::compose_stash::ComposeStash>,
    ) {
        crate::chat_l233::set_compose_stash_live(self, key, stash);
    }

    fn report_read_metrics(
        &mut self,
        peer_id: PeerId,
        metrics: Vec<crate::read_metrics::FinalizedReadMetric>,
    ) {
        crate::chat_l233::report_read_metrics_live(self, peer_id, metrics);
    }

    fn flush_read_metrics(&mut self, force: bool) -> usize {
        crate::chat_l233::flush_read_metrics_live(self, force)
    }

    fn send_emoji_interaction(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        emoticon: &str,
        bunch: &tg_app_core::core_types::dto::emoji_interactions::EmojiInteractionsBunch,
    ) -> Result<(), String> {
        crate::chat_l233::send_emoji_interaction_live(self, peer_id, msg_id, emoticon, bunch)
    }

    fn send_emoji_interaction_seen(
        &mut self,
        peer_id: PeerId,
        emoticon: &str,
    ) -> Result<(), String> {
        crate::chat_l233::send_emoji_interaction_seen_live(self, peer_id, emoticon)
    }

    fn drain_incoming_emoji_interactions(
        &mut self,
        peer_id: PeerId,
    ) -> Vec<tg_app_core::core_types::dto::emoji_interactions::IncomingEmojiInteraction> {
        crate::chat_l233::drain_incoming_emoji_interactions_live(self, peer_id)
    }

    fn drain_emoji_interaction_seen(&mut self, peer_id: PeerId) -> Vec<String> {
        crate::chat_l233::drain_emoji_interaction_seen_live(self, peer_id)
    }

    fn resolve_emoji_interaction_lottie(
        &mut self,
        emoticon: &str,
        variant_index: usize,
    ) -> Option<(u64, String)> {
        crate::emoji_pack::resolve_emoji_interaction_lottie_live(self, emoticon, variant_index)
    }

    fn emoji_interaction_variant_count(&self, emoticon: &str) -> Option<usize> {
        crate::emoji_pack::emoji_interaction_variant_count_live(self, emoticon)
    }

    fn peer_no_forwards_enabled(&self, peer_id: PeerId) -> bool {
        crate::chat_l233::peer_no_forwards_enabled_live(self, peer_id)
    }

    fn peer_allows_forwarding(&self, peer_id: PeerId) -> bool {
        let data_peer = to_data_peer(peer_id);
        self.read_session(|s| {
            s.peer(data_peer)
                .is_none_or(tg_data::Peer::allows_forwarding)
        })
    }

    fn is_no_forwards_request_acted(&self, peer_id: PeerId, msg_id: MsgId) -> bool {
        crate::chat_l233::is_no_forwards_request_acted_live(self, peer_id, msg_id)
    }

    fn toggle_peer_no_forwards(
        &mut self,
        peer_id: PeerId,
        enabled: bool,
        request_msg_id: Option<MsgId>,
    ) -> Result<(), String> {
        crate::chat_l233::toggle_peer_no_forwards_live(self, peer_id, enabled, request_msg_id)
    }

    fn toggle_saved_gif_from_message(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        saved: bool,
    ) -> Result<bool, String> {
        crate::toggling_media::toggle_saved_gif_from_message(self, peer_id, msg_id, saved)
    }

    fn toggle_faved_sticker_from_message(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        faved: bool,
    ) -> Result<bool, String> {
        crate::toggling_media::toggle_faved_sticker_from_message(self, peer_id, msg_id, faved)
    }

    fn toggle_recent_sticker_from_message(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        saved: bool,
    ) -> Result<bool, String> {
        crate::toggling_media::toggle_recent_sticker_from_message(self, peer_id, msg_id, saved)
    }

    fn toggle_saved_ringtone_from_message(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        saved: bool,
    ) -> Result<bool, String> {
        crate::toggling_media::toggle_saved_ringtone_from_message(self, peer_id, msg_id, saved)
    }

    fn message_summary(
        &self,
        peer_id: PeerId,
        msg_id: MsgId,
    ) -> Option<tg_app_core::history::backend::MessageSummaryEntry> {
        self.message_summary_live(peer_id, msg_id)
    }

    fn toggle_message_summary(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        to_lang: Option<&str>,
    ) -> Result<tg_app_core::history::backend::MessageSummaryEntry, String> {
        self.toggle_message_summary_live(peer_id, msg_id, to_lang)
    }

    fn apply_suggested_birthday(
        &mut self,
        day: u8,
        month: u8,
        year: Option<u16>,
    ) -> Result<(), String> {
        self.apply_suggested_birthday_live(day, month, year)
    }

    fn apply_suggested_profile_photo(
        &mut self,
        _peer_id: PeerId,
        _msg_id: MsgId,
        photo_id: i64,
        access_hash: i64,
        file_reference: &[u8],
        _is_video: bool,
    ) -> Result<(), String> {
        self.apply_suggested_profile_photo_live(photo_id, access_hash, file_reference)
    }

    fn forward_messages_with_options(
        &mut self,
        from_peer: PeerId,
        msg_ids: &[MsgId],
        to_peer: PeerId,
        options: ForwardOptions,
    ) -> Result<Vec<HistoryMessage>, String> {
        self.forward_messages_with_options_live(from_peer, msg_ids, to_peer, options)
    }

    fn scheduled_messages(
        &self,
        peer_id: PeerId,
        topic_root_id: Option<MsgId>,
    ) -> Vec<HistoryMessage> {
        self.scheduled_messages_live(peer_id, topic_root_id)
    }

    fn send_scheduled_now(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
    ) -> Result<HistoryMessage, String> {
        self.send_scheduled_now_live(peer_id, msg_id)
    }

    fn reschedule_message(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        target: ScheduleTarget,
    ) -> Result<HistoryMessage, String> {
        self.reschedule_message_live(peer_id, msg_id, target)
    }

    fn delete_scheduled_messages(
        &mut self,
        peer_id: PeerId,
        msg_ids: &[MsgId],
    ) -> Result<(), String> {
        self.delete_scheduled_messages_live(peer_id, msg_ids)
    }

    fn replies_root_and_comments(
        &self,
        peer_id: PeerId,
        root_msg_id: MsgId,
    ) -> Option<(HistoryMessage, Vec<HistoryMessage>)> {
        self.replies_root_and_comments_live(peer_id, root_msg_id)
    }

    fn forum_topics(&self, peer_id: PeerId) -> Vec<ForumTopic> {
        self.get_forum_topics_live(peer_id)
    }

    fn create_forum_topic(
        &mut self,
        peer_id: PeerId,
        title: String,
        icon_color: u32,
        custom_emoji: Option<String>,
    ) -> Result<ForumTopic, String> {
        self.create_forum_topic_live(peer_id, title, icon_color, custom_emoji)
    }

    fn toggle_forum_topic_pin(
        &mut self,
        peer_id: PeerId,
        topic_root_id: MsgId,
    ) -> Result<bool, String> {
        self.toggle_forum_topic_pin_live(peer_id, topic_root_id)
    }

    fn toggle_forum_topic_closed(
        &mut self,
        peer_id: PeerId,
        topic_root_id: MsgId,
    ) -> Result<bool, String> {
        self.toggle_forum_topic_closed_live(peer_id, topic_root_id)
    }

    fn load_topic_messages(&self, peer_id: PeerId, topic_root_id: MsgId) -> Vec<HistoryMessage> {
        self.load_topic_messages_live(peer_id, topic_root_id)
    }

    fn forum_topic_icons(&self) -> Vec<tg_app_core::history::sections_model::ForumTopicIconEntry> {
        self.forum_topic_icons_live()
    }

    fn edit_forum_topic(
        &mut self,
        peer_id: PeerId,
        topic_root_id: MsgId,
        title: Option<String>,
        icon_emoji_id: Option<i64>,
    ) -> Result<ForumTopic, String> {
        LiveSession::edit_forum_topic(
            self,
            peer_id,
            topic_root_id,
            crate::history_sections::ForumTopicEdit {
                title,
                icon_emoji_id,
                closed: None,
                hidden: None,
            },
        )
    }

    fn reorder_pinned_forum_topics(
        &mut self,
        peer_id: PeerId,
        order: &[MsgId],
    ) -> Result<(), String> {
        LiveSession::reorder_pinned_forum_topics(self, peer_id, order, false)
    }

    fn saved_sublists(&self, peer_id: PeerId) -> Vec<SavedSublist> {
        self.saved_sublists_live(peer_id)
    }

    fn load_saved_sublist_messages(
        &self,
        peer_id: PeerId,
        sublist_peer_id: PeerId,
    ) -> Vec<HistoryMessage> {
        self.load_saved_sublist_messages_live(peer_id, sublist_peer_id)
    }

    fn admin_log_entries(&self, peer_id: PeerId, filter: &AdminLogFilter) -> Vec<AdminLogEntry> {
        self.admin_log_entries_live(peer_id, filter)
    }

    fn admin_log_admins(&self, peer_id: PeerId) -> Vec<AdminLogAdmin> {
        self.admin_log_admins_live(peer_id)
    }

    fn save_thread_draft(&mut self, thread: ThreadKey, draft: Option<ThreadDraft>) {
        self.save_rich_draft_live(thread, draft.map(RichThreadDraft::from_legacy));
    }

    fn load_thread_draft(&self, thread: ThreadKey) -> Option<ThreadDraft> {
        self.load_rich_draft_live(thread).map(|d| d.to_legacy())
    }

    fn save_rich_draft(&mut self, thread: ThreadKey, draft: Option<RichThreadDraft>) {
        self.save_rich_draft_live(thread, draft);
    }

    fn load_rich_draft(&self, thread: ThreadKey) -> Option<RichThreadDraft> {
        self.load_rich_draft_live(thread)
    }

    fn send_as_peers(&self, peer_id: PeerId) -> Vec<SendAsPeer> {
        self.send_as_peers_live(peer_id)
    }

    fn default_send_as_peer(&self, peer_id: PeerId) -> Option<PeerId> {
        self.default_send_as_peer_live(peer_id)
    }

    fn save_default_send_as(&mut self, peer_id: PeerId, send_as: PeerId) -> Result<(), String> {
        self.save_default_send_as_live(peer_id, send_as)
    }

    fn slow_mode_state(&self, peer_id: PeerId) -> SlowModeState {
        self.slow_mode_state_live(peer_id)
    }

    fn record_slow_mode_send(&mut self, peer_id: PeerId, now_unix: i64) {
        self.record_slow_mode_send_live(peer_id, now_unix);
    }

    fn current_unix_time(&self) -> Option<i64> {
        Some(crate::convert::now_unix_i64())
    }

    fn resolve_link_preview(&self, url: &str) -> Option<LinkPreviewData> {
        self.resolve_link_preview_live(url)
    }

    fn translate_message(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        target_lang: &str,
    ) -> Result<String, String> {
        self.translate_message_live(peer_id, msg_id, target_lang)
    }

    fn translate_messages_batch(
        &mut self,
        peer_id: PeerId,
        msg_ids: &[MsgId],
        target_lang: &str,
    ) -> Result<Vec<(MsgId, String)>, String> {
        self.translate_messages_batch_live(peer_id, msg_ids, target_lang)
    }

    fn clear_message_translation(&mut self, peer_id: PeerId, msg_id: MsgId) {
        self.clear_message_translation_live(peer_id, msg_id);
    }

    fn peer_translations_disabled(&self, peer_id: PeerId) -> bool {
        self.peer_translations_disabled_live(peer_id)
    }

    fn toggle_peer_translations(&mut self, peer_id: PeerId, disabled: bool) -> Result<(), String> {
        self.toggle_peer_translations_live(peer_id, disabled)
    }

    fn do_not_translate_languages(&self) -> Vec<String> {
        self.do_not_translate_languages_live()
    }

    fn set_do_not_translate_languages(&mut self, langs: Vec<String>) {
        self.set_do_not_translate_languages_live(langs);
    }

    fn target_translation_language(&self) -> String {
        self.target_translation_language_live()
    }

    fn set_target_translation_language(&mut self, lang: String) {
        self.set_target_translation_language_live(lang);
    }

    fn available_reactions(&self) -> Vec<AvailableReactionItem> {
        self.available_reactions_live()
    }

    fn top_reactions(&self) -> Vec<String> {
        self.top_reactions_live()
    }

    fn reaction_strip_emojis(&self, peer_id: Option<PeerId>) -> Vec<String> {
        self.reaction_strip_emojis_live(peer_id)
    }

    fn available_effects(&self) -> Vec<AvailableMessageEffect> {
        self.available_effects_live()
    }

    fn effect_lottie_json(&self, effect_id: u64) -> Option<String> {
        self.effect_lottie_json_live(effect_id)
    }

    fn message_reactions_list(
        &self,
        peer_id: PeerId,
        msg_id: MsgId,
        reaction_filter: Option<&str>,
        offset: Option<&str>,
        limit: usize,
    ) -> Result<MessageReactionsPage, String> {
        self.message_reactions_list_live(peer_id, msg_id, reaction_filter, offset, limit)
    }

    fn default_reaction(&self) -> String {
        self.default_reaction_live()
    }

    fn set_default_reaction(&mut self, emoji: &str) -> Result<(), String> {
        self.set_default_reaction_live(emoji)
    }

    fn saved_reaction_tags(&self, peer_id: Option<PeerId>) -> Vec<SavedReactionTagItem> {
        self.saved_reaction_tags_live(peer_id)
    }

    fn update_saved_reaction_tag(
        &mut self,
        emoji: &str,
        title: Option<&str>,
    ) -> Result<bool, String> {
        LiveSession::update_saved_reaction_tag(self, emoji, title)
    }

    fn history_saved_messages_peer_id(&self) -> Option<PeerId> {
        let self_pid = self.read_session(|s| s.self_peer_id());
        (self_pid.0 != 0).then(|| from_data_peer(self_pid))
    }

    fn poll_votes(
        &self,
        peer_id: PeerId,
        msg_id: MsgId,
        option_index: Option<usize>,
        offset: Option<&str>,
        limit: usize,
    ) -> Result<PollVotesPage, String> {
        self.poll_votes_live(peer_id, msg_id, option_index, offset, limit)
    }

    fn retract_poll_vote(&mut self, peer_id: PeerId, msg_id: MsgId) -> Result<(), String> {
        self.retract_poll_vote_live(peer_id, msg_id)
    }

    fn stop_poll(&mut self, peer_id: PeerId, msg_id: MsgId) -> Result<(), String> {
        self.stop_poll_live(peer_id, msg_id)
    }

    fn reload_poll_results(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
    ) -> Result<Option<tg_app_core::history::backend::PollAttachment>, String> {
        self.reload_poll_results_live(peer_id, msg_id)
    }

    fn poll_stats(
        &self,
        peer_id: PeerId,
        msg_id: MsgId,
        dark: bool,
    ) -> Result<tg_stats::StatisticalGraph, String> {
        self.poll_stats_live(peer_id, msg_id, dark)
    }

    fn chat_mention_candidates(
        &self,
        peer_id: PeerId,
    ) -> Vec<tg_app_core::core_types::dto::rich_compose::MentionCandidate> {
        crate::chat_helpers::chat_mention_candidates(self, peer_id)
    }

    fn reply_preview_thumb(
        &self,
        peer_id: PeerId,
        msg_id: MsgId,
    ) -> tg_app_core::info::backend::SharedMediaThumb {
        crate::reply_targets::reply_preview_thumb(self, peer_id, msg_id)
    }

    fn start_media_download(&self, peer_id: PeerId, msg_id: MsgId) {
        crate::media_download::start(self, peer_id, msg_id);
    }

    fn open_media_stream(
        &self,
        peer_id: PeerId,
        msg_id: MsgId,
    ) -> Option<Arc<dyn tg_video::RangeLoader>> {
        crate::media_download::stream(self, peer_id, msg_id)
    }

    fn cancel_media_download(&self, peer_id: PeerId, msg_id: MsgId) {
        crate::media_download::cancel(self, peer_id, msg_id);
    }

    fn inline_media_descriptor(
        &self,
        peer_id: PeerId,
        msg_id: MsgId,
    ) -> Option<InlineMediaDescriptor> {
        let data_peer = to_data_peer(peer_id);
        let data_msg = to_data_msg(msg_id);
        let settings = self
            .cache
            .lock()
            .map(|c| c.auto_download.clone())
            .unwrap_or_default();
        let (msg, photo_autoload) = self.read_session(|s| {
            let msg = s
                .message(DataFullMsgId::new(data_peer, data_msg))
                .cloned()?;
            let allowed = crate::auto_download_gate::photo_autoload_allowed(s, &settings, &msg)
                && crate::auto_download_gate::round_video_autoplay_allowed(s, &settings, &msg);
            Some((msg, allowed))
        })?;
        self.media_cache.inline_media_descriptor_for_message_with(
            &msg,
            photo_autoload,
            self.runtime.rpc(),
            self.runtime.tokio_handle(),
            self.bridge.state(),
        )
    }

    fn as_layer233_extras(
        &self,
    ) -> Option<&dyn tg_app_core::history::backend::Layer233ExtrasBackend> {
        Some(self)
    }

    fn as_layer233_extras_mut(
        &mut self,
    ) -> Option<&mut dyn tg_app_core::history::backend::Layer233ExtrasBackend> {
        Some(self)
    }

    fn transcribe_audio_state(
        &self,
        peer_id: PeerId,
        msg_id: MsgId,
    ) -> Option<tg_app_core::history::backend::TranscribeUiEntry> {
        self.transcribe_entry_ui(peer_id, msg_id)
    }

    fn transcribe_limits(
        &self,
        peer_id: Option<PeerId>,
    ) -> tg_app_core::history::backend::TranscribeLimitsView {
        self.transcribe_limits_view(peer_id)
    }

    fn toggle_transcribe_audio(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        duration_secs: u32,
        is_round_video: bool,
    ) -> tg_app_core::history::backend::TranscribeToggleOutcome {
        self.toggle_transcribe_audio_live(peer_id, msg_id, duration_secs, is_round_video)
    }

    fn rate_transcribed_audio(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        good: bool,
    ) -> Result<(), String> {
        self.rate_transcribed_audio_live(peer_id, msg_id, good)
    }

    fn join_requests_bar(
        &self,
        peer_id: PeerId,
    ) -> Option<tg_app_core::info::backend::JoinRequestsSummary> {
        let summary = crate::admin::join_requests_summary(self, peer_id);
        (summary.count > 0).then_some(summary)
    }

    fn join_requests_entries(
        &self,
        peer_id: PeerId,
        query: &str,
    ) -> Vec<tg_app_core::info::backend::InviteImporterInfo> {
        crate::admin::get_chat_invite_importers(self, peer_id, None, true, query)
            .map(|r| r.importers)
            .unwrap_or_default()
    }

    fn decide_join_request(
        &mut self,
        peer_id: PeerId,
        user_id: PeerId,
        approved: bool,
    ) -> Result<(), String> {
        crate::admin::hide_chat_join_request(self, peer_id, user_id, approved)
    }

    fn decide_all_join_requests(
        &mut self,
        peer_id: PeerId,
        approved: bool,
    ) -> Result<usize, String> {
        crate::admin::hide_all_chat_join_requests(self, peer_id, None, approved)
    }

    fn widget_query(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        data: &[u8],
        with_wallet: bool,
    ) -> Result<tg_tgw::WidgetRequestOutcome, String> {
        crate::widgets::widget_query_live(self, peer_id, msg_id, data, with_wallet)
    }

    fn widget_poll_public_data(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        hash: i64,
    ) -> Result<tg_tgw::PublicDataPollResult, String> {
        crate::widgets::widget_poll_public_data_live(self, peer_id, msg_id, hash)
    }

    fn widget_edit_data(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        public_data: Option<&str>,
        chat_data: Option<&str>,
    ) -> Result<(), String> {
        crate::widgets::widget_edit_message_data_live(self, peer_id, msg_id, public_data, chat_data)
    }
    fn business_bot_bar(
        &self,
        peer_id: PeerId,
    ) -> Option<tg_app_core::history::backend::BusinessBotBarState> {
        crate::profile_extras::business_bot_bar(self, peer_id)
    }

    fn toggle_business_bot_paused(&mut self, peer_id: PeerId, paused: bool) -> Result<(), String> {
        crate::profile_extras::toggle_business_bot_paused(self, peer_id, paused)
    }

    fn disable_business_bot(&mut self, peer_id: PeerId) -> Result<(), String> {
        crate::profile_extras::disable_business_bot(self, peer_id)
    }

    // -----------------------------------------------------------------------
    // WP-53: Chat Engagement & Read-Receipt Protocol
    // -----------------------------------------------------------------------

    fn schedule_message_views_increment(&mut self, peer_id: PeerId, msg_ids: &[MsgId]) {
        crate::engagement::schedule_message_views_increment(self, peer_id, msg_ids);
    }

    fn flush_message_views(
        &mut self,
        peer_id: Option<PeerId>,
        force: bool,
    ) -> Result<Vec<tg_app_core::history::backend::MessageViewsInfo>, String> {
        crate::engagement::flush_message_views(self, peer_id, force)
    }

    fn remove_incremented_views(&mut self, peer_id: PeerId) {
        crate::engagement::remove_incremented_views(self, peer_id);
    }

    fn get_messages_views(
        &mut self,
        peer_id: PeerId,
        msg_ids: &[MsgId],
        increment: bool,
    ) -> Result<Vec<tg_app_core::history::backend::MessageViewsInfo>, String> {
        crate::engagement::get_messages_views(self, peer_id, msg_ids, increment)
    }

    fn poll_extended_media(
        &mut self,
        peer_id: PeerId,
        msg_ids: &[MsgId],
        force: bool,
    ) -> Result<Vec<tg_app_core::history::backend::ExtendedMediaRefreshEntry>, String> {
        crate::engagement::poll_extended_media(self, peer_id, msg_ids, force)
    }

    fn flush_extended_media_polls(
        &mut self,
        force: bool,
    ) -> Result<Vec<tg_app_core::history::backend::ExtendedMediaRefreshEntry>, String> {
        crate::engagement::flush_extended_media_polls(self, force)
    }

    fn poll_messages_reactions(
        &self,
        peer_id: PeerId,
        msg_ids: &[MsgId],
    ) -> Result<Vec<MsgId>, String> {
        self.poll_visible_messages_reactions_live(peer_id, msg_ids)
    }

    fn poll_painted_reactions(&self, peer_id: PeerId, msg_ids: &[MsgId]) {
        self.poll_reactions_in_background(peer_id, msg_ids.to_vec(), true);
    }

    fn load_unread_things_page(
        &mut self,
        scope: tg_app_core::history::backend::UnreadThingsScope,
        kind: tg_app_core::history::backend::UnreadThingsKind,
    ) -> Result<tg_app_core::history::backend::UnreadThingsSlice, String> {
        crate::engagement::load_unread_things_page(self, scope, kind)
    }

    fn unread_things_counts(
        &self,
        scope: tg_app_core::history::backend::UnreadThingsScope,
    ) -> tg_app_core::history::backend::UnreadThingsCounts {
        crate::engagement::unread_things_counts(self, scope)
    }

    fn read_unread_mentions(
        &mut self,
        peer_id: PeerId,
        top_msg_id: Option<MsgId>,
    ) -> Result<tg_app_core::history::backend::UnreadThingsReadResult, String> {
        crate::engagement::read_unread_mentions(self, peer_id, top_msg_id)
    }

    fn read_unread_reactions(
        &mut self,
        peer_id: PeerId,
        top_msg_id: Option<MsgId>,
        saved_peer_id: Option<PeerId>,
    ) -> Result<tg_app_core::history::backend::UnreadThingsReadResult, String> {
        crate::engagement::read_unread_reactions(self, peer_id, top_msg_id, saved_peer_id)
    }

    fn mark_visible_reactions_read(
        &mut self,
        peer_id: PeerId,
        msg_ids: &[MsgId],
    ) -> Result<Vec<MsgId>, String> {
        crate::engagement::mark_visible_reactions_read(self, peer_id, msg_ids)
    }

    fn read_unread_poll_votes(
        &mut self,
        peer_id: PeerId,
        top_msg_id: Option<MsgId>,
    ) -> Result<tg_app_core::history::backend::UnreadThingsReadResult, String> {
        crate::engagement::read_unread_poll_votes(self, peer_id, top_msg_id)
    }

    fn get_message_read_participants(
        &self,
        peer_id: PeerId,
        msg_id: MsgId,
    ) -> Result<Vec<tg_app_core::history::backend::MessageReadParticipant>, String> {
        crate::engagement::get_message_read_participants(self, peer_id, msg_id)
    }

    fn get_outbox_read_date(
        &self,
        peer_id: PeerId,
        msg_id: MsgId,
    ) -> Result<tg_app_core::history::backend::OutboxReadDateStatus, String> {
        crate::engagement::get_outbox_read_date(self, peer_id, msg_id)
    }

    fn read_discussion(
        &mut self,
        peer_id: PeerId,
        top_msg_id: MsgId,
        read_max_id: MsgId,
    ) -> Result<bool, String> {
        crate::engagement::read_discussion(self, peer_id, top_msg_id, read_max_id)
    }

    fn report_messages_delivery(
        &mut self,
        peer_id: PeerId,
        msg_ids: &[MsgId],
        push: bool,
    ) -> Result<usize, String> {
        crate::engagement::report_messages_delivery(self, peer_id, msg_ids, push)
    }

    fn ephemeral_delete_message(
        &mut self,
        peer_id: PeerId,
        receiver_user_id: PeerId,
        ephemeral_id: i32,
    ) -> Result<bool, String> {
        crate::engagement::ephemeral_delete_message(self, peer_id, receiver_user_id, ephemeral_id)
    }

    fn ephemeral_report_message(
        &mut self,
        peer_id: PeerId,
        ephemeral_id: i32,
        option: &[u8],
        comment: &str,
    ) -> Result<tg_app_core::history::backend::EphemeralReportOutcome, String> {
        crate::engagement::ephemeral_report_message(self, peer_id, ephemeral_id, option, comment)
    }

    fn export_message_link(
        &self,
        peer_id: PeerId,
        msg_id: MsgId,
        grouped: bool,
        thread: bool,
    ) -> Result<tg_app_core::history::backend::ExportedMessageLinkInfo, String> {
        crate::engagement::export_message_link(self, peer_id, msg_id, grouped, thread)
    }

    fn get_message_author(
        &self,
        peer_id: PeerId,
        msg_id: MsgId,
    ) -> Result<tg_app_core::history::backend::MessageAuthorInfo, String> {
        crate::engagement::get_message_author(self, peer_id, msg_id)
    }

    fn migrate_chat(
        &mut self,
        chat_peer_id: PeerId,
    ) -> Result<tg_app_core::history::backend::ChatMigrationResult, String> {
        crate::engagement::migrate_chat(self, chat_peer_id)
    }

    fn future_chat_creator_after_leave(
        &self,
        peer_id: PeerId,
    ) -> Result<Option<tg_app_core::history::backend::FutureChatCreatorInfo>, String> {
        crate::engagement::future_chat_creator_after_leave(self, peer_id)
    }

    fn accept_contact(&mut self, peer_id: PeerId) -> Result<(), String> {
        crate::contacts_extras::accept_contact(self, peer_id)
    }

    fn report_spam(&mut self, peer_id: PeerId) -> Result<bool, String> {
        crate::contacts_extras::report_spam(self, peer_id)
    }

    fn hide_peer_settings_bar(&mut self, peer_id: PeerId) -> Result<bool, String> {
        crate::contacts_extras::hide_peer_settings_bar(self, peer_id)
    }

    fn block_from_replies(
        &mut self,
        request: tg_app_core::core_types::dto::contacts_extras::BlockFromRepliesRequest,
    ) -> Result<(), String> {
        crate::contacts_extras::block_from_replies(self, request)
    }
}

/// The "Unread messages" bar position: the first loaded incoming message after the inbox read
/// mark while the chat has unread messages (a range query, TGB-2938).
fn first_unread_in_history(session: &Session, peer_id: DataPeerId) -> Option<MsgId> {
    session.first_unread_incoming(peer_id).map(from_data_msg)
}

/// Number of loaded message ids of `history` (`O(slices)`; every id in a loaded range has its
/// message stored, unloading shrinks the ranges with the messages).
fn loaded_id_count(history: &tg_data::History) -> usize {
    history
        .loaded_ranges
        .slices()
        .iter()
        .map(|slice| slice.messages.len())
        .sum()
}

fn build_history_slice(
    session: &Session,
    peer_id: DataPeerId,
    around_id: MsgId,
    limit_before: usize,
    limit_after: usize,
    media_cache: &LiveMediaCache,
) -> HistorySlice {
    let Some(history) = session.history(peer_id) else {
        return HistorySlice::default();
    };
    let resolve = |doc: &tg_data::DocumentData| media_cache.cached_lottie_json_for_doc(doc);
    let window = limit_before.saturating_add(limit_after).max(20);
    // Range queries over the loaded ids (TGB-2938): a page costs its own length, not the
    // whole loaded history.
    let first_unread = first_unread_in_history(session, peer_id);
    let page = if around_id == MsgId::SHOW_AT_UNREAD
        && let Some(unread) = first_unread
    {
        session.loaded_messages_around(peer_id, to_data_msg(unread), limit_before, limit_after)
    } else if around_id == MsgId::SHOW_AT_END || around_id.0 <= 0 {
        session.last_loaded_messages(peer_id, window)
    } else {
        session.loaded_messages_around(peer_id, to_data_msg(around_id), limit_before, limit_after)
    };
    let slice_msgs: Vec<HistoryMessage> = page
        .into_iter()
        .map(|m| message_to_history_message_with_stickers(session, m, &resolve))
        .collect();

    HistorySlice {
        messages: slice_msgs,
        loaded_at_top: history.loaded_ranges.loaded_at_top(),
        loaded_at_bottom: history.loaded_ranges.loaded_at_bottom(),
        total_count: loaded_id_count(history),
        first_unread_id: first_unread,
        unread_count: history.unread_count.unwrap_or(0).max(0) as u32,
    }
}

/// Appends the chat's local messages (being sent or failed) to a slice that reaches the bottom:
/// tdesktop keeps them after the newest server message (TGB-2931).
fn append_local_messages(session: &Session, peer_id: DataPeerId, slice: &mut HistorySlice) {
    if slice.loaded_at_bottom || slice.messages.is_empty() {
        slice
            .messages
            .extend(crate::optimistic_send::local_messages(session, peer_id));
    }
}

// ============================================================================
// 4. info::backend::InfoBackend
// ============================================================================

impl InfoBackend for LiveSession {
    fn peer_profile(&self, peer_id: PeerId) -> Option<InfoPeerProfile> {
        let data_peer = to_data_peer(peer_id);
        let now = now_unix_i32();
        let mut profile = self.read_session(|s| peer_to_info_profile(s, data_peer, now))?;
        if (profile.kind.is_group() || profile.kind.is_channel())
            && let Some((call_id, access_hash, can_manage)) = self.known_group_call_for(peer_id)
        {
            profile.group_call.active = true;
            if profile.group_call.call_id.is_none() {
                profile.group_call.call_id = Some((call_id, access_hash));
            }
            profile.group_call.can_manage |= can_manage;
        }
        crate::peer_emoji_status::request_for_profile(self, &profile);
        Some(profile)
    }

    fn all_peers(&self) -> Vec<InfoPeerProfile> {
        let now = now_unix_i32();
        self.read_session(|s| {
            known_peer_ids(s)
                .into_iter()
                .filter_map(|pid| peer_to_info_profile(s, pid, now))
                .collect()
        })
    }

    fn contacts(&self) -> Vec<InfoPeerProfile> {
        let now = now_unix_i32();
        self.read_session(|s| {
            let self_id = s.self_peer_id();
            known_peer_ids(s)
                .into_iter()
                .filter(|&pid| {
                    pid != self_id
                        && s.peer(pid)
                            .and_then(|p| p.as_user())
                            .is_some_and(|u| u.is_contact())
                })
                .filter_map(|pid| peer_to_info_profile(s, pid, now))
                .collect()
        })
    }

    fn peer_members(&self, peer_id: PeerId, query: &str) -> Vec<MemberEntry> {
        crate::search_and_info::peer_members(self, peer_id, query)
    }

    fn common_groups(&self, user_id: PeerId) -> Vec<CommonGroupEntry> {
        crate::search_and_info::common_groups(self, user_id)
    }

    fn shared_media(
        &self,
        peer_id: PeerId,
        tab: SharedMediaTab,
        query: &str,
    ) -> Vec<InfoMediaItem> {
        crate::search_and_info::shared_media(self, peer_id, tab, query)
    }

    fn shared_media_thumb(
        &self,
        peer_id: PeerId,
        msg_id: i32,
    ) -> tg_app_core::info::backend::SharedMediaThumb {
        let data_peer = to_data_peer(peer_id);
        let data_msg = DataMsgId(i64::from(msg_id));
        let Some(msg) =
            self.read_session(|s| s.message(DataFullMsgId::new(data_peer, data_msg)).cloned())
        else {
            return tg_app_core::info::backend::SharedMediaThumb::Unavailable;
        };
        self.media_cache.shared_media_thumb_for_message(
            &msg,
            self.runtime.rpc(),
            self.runtime.tokio_handle(),
            self.bridge.state(),
        )
    }

    fn global_media(
        &self,
        tab: SharedMediaTab,
        query: &str,
        limit: usize,
    ) -> tg_app_core::core_types::dto::global_media::GlobalMediaPage {
        crate::search_and_info::global_media(self, tab, query, limit)
    }

    fn toggle_notifications(&mut self, peer_id: PeerId) -> bool {
        self.toggle_peer_mute_rpc(peer_id)
    }

    fn toggle_block_user(&mut self, peer_id: PeerId) -> bool {
        self.toggle_peer_block_rpc(peer_id)
    }

    fn add_contact(
        &mut self,
        first_name: &str,
        last_name: &str,
        phone: &str,
    ) -> Result<PeerId, String> {
        let req = tl_fns::contacts::ImportContacts {
            contacts: vec![tl_enums::InputContact::InputPhoneContact(
                tl_types::InputPhoneContact {
                    client_id: 1,
                    phone: phone.trim().to_owned(),
                    first_name: first_name.trim().to_owned(),
                    last_name: last_name.trim().to_owned(),
                    note: None,
                },
            )],
        };
        let tl_enums::contacts::ImportedContacts::ImportedContacts(resp) = self.invoke_rpc(req)?;
        self.write_session(|s| {
            apply_tl_users_and_chats(s, &resp.users, &[]);
        });
        resp.imported
            .first()
            .map(|imp| {
                let tl_enums::ImportedContact::ImportedContact(ic) = imp;
                PeerId::from_user(ic.user_id as u64)
            })
            .ok_or_else(|| "User with that phone number was not found on Telegram.".to_owned())
    }

    fn create_group(&mut self, req: CreateGroupRequest) -> Result<PeerId, String> {
        crate::admin::create_group(self, req)
    }

    fn create_channel(&mut self, req: CreateChannelRequest) -> Result<PeerId, String> {
        crate::admin::create_channel(self, req)
    }

    fn update_peer_info(
        &mut self,
        peer_id: PeerId,
        req: UpdatePeerInfoRequest,
    ) -> Result<(), String> {
        crate::admin::update_peer_info(self, peer_id, req)
    }

    fn update_default_permissions(
        &mut self,
        peer_id: PeerId,
        rights: ChatBannedRights,
        slowmode_seconds: u32,
    ) -> Result<(), String> {
        crate::admin::update_default_permissions(self, peer_id, rights, slowmode_seconds)
    }

    fn update_admin_rights(
        &mut self,
        peer_id: PeerId,
        user_id: PeerId,
        rights: ChatAdminRights,
        custom_title: String,
    ) -> Result<(), String> {
        let res = crate::admin::update_admin_rights(self, peer_id, user_id, rights, custom_title);
        if res.is_ok() {
            self.invalidate_members_cache(peer_id);
        }
        res
    }

    fn add_members(&mut self, peer_id: PeerId, member_ids: &[PeerId]) -> Result<usize, String> {
        let res = crate::admin::add_members(self, peer_id, member_ids);
        if res.is_ok() {
            self.invalidate_members_cache(peer_id);
        }
        res
    }

    fn remove_member(&mut self, peer_id: PeerId, user_id: PeerId) -> Result<(), String> {
        let res = crate::admin::remove_member(self, peer_id, user_id);
        if res.is_ok() {
            self.invalidate_members_cache(peer_id);
        }
        res
    }

    fn member_rank(&self, peer_id: PeerId, user_id: PeerId) -> Option<String> {
        self.cached_member_rank(peer_id, user_id)
    }

    fn edit_member_rank(
        &mut self,
        peer_id: PeerId,
        user_id: PeerId,
        rank: &str,
    ) -> Result<String, String> {
        let res = self.edit_chat_participant_rank(peer_id, user_id, rank);
        if res.is_ok() {
            self.invalidate_members_cache(peer_id);
        }
        res
    }

    fn set_auto_delete_ttl(&mut self, peer_id: PeerId, ttl_seconds: u32) -> Result<(), String> {
        crate::admin::set_auto_delete_ttl(self, peer_id, ttl_seconds)
    }

    fn set_linked_discussion(
        &mut self,
        channel_id: PeerId,
        group_id: Option<PeerId>,
    ) -> Result<(), String> {
        crate::admin::set_linked_discussion(self, channel_id, group_id)
    }

    fn check_username(&self, username: &str, exclude_peer: Option<PeerId>) -> UsernameAvailability {
        crate::channel_mgmt::check_username(self, username, exclude_peer)
    }

    fn update_username(&mut self, peer_id: PeerId, username: &str) -> Result<(), String> {
        crate::admin::update_username(self, peer_id, username)
    }

    fn forward_messages(
        &mut self,
        target_peers: &[PeerId],
        message_ids: &[i32],
        comment: &str,
    ) -> Result<usize, String> {
        let mut forwarded_count = 0usize;
        let ids: Vec<MsgId> = message_ids.iter().copied().map(MsgId).collect();
        let source_peer = self.read_session(|s| {
            let first = DataMsgId(i64::from(*message_ids.first()?));
            for pid in known_peer_ids(s) {
                if s.message(DataFullMsgId::new(pid, first)).is_some() {
                    return Some(from_data_peer(pid));
                }
            }
            None
        });
        for &target in target_peers {
            if let Some(src) = source_peer {
                let res = HistoryBackend::forward_messages(self, src, &ids, target)?;
                forwarded_count += res.len();
            }
            if !comment.trim().is_empty() {
                let _ = HistoryBackend::send_message(
                    self,
                    target,
                    SendMessageRequest {
                        text: TextWithEntities::simple(comment),
                        reply_to: None,
                        silent: false,
                    },
                )?;
            }
        }
        Ok(forwarded_count)
    }

    fn delete_messages(
        &mut self,
        peer_id: PeerId,
        message_ids: &[i32],
        revoke_for_everyone: bool,
    ) -> Result<usize, String> {
        let ids: Vec<MsgId> = message_ids.iter().copied().map(MsgId).collect();
        HistoryBackend::delete_messages(self, peer_id, &ids, revoke_for_everyone)?;
        Ok(ids.len())
    }

    fn submit_report(
        &mut self,
        peer_id: PeerId,
        reason: ReportReason,
        comment: &str,
    ) -> Result<(), String> {
        let Some(input_peer) = self.input_peer(peer_id) else {
            return Err("Peer not found.".to_owned());
        };
        let req = tl_fns::account::ReportPeer {
            peer: input_peer,
            reason: report_reason_to_tl(reason),
            message: comment.to_owned(),
        };
        let _ = self.invoke_rpc(req)?;
        Ok(())
    }

    fn languages(&self) -> Vec<LanguageEntry> {
        self.langpack
            .languages()
            .into_iter()
            .map(|l| {
                let pct = if l.strings_count > 0 {
                    ((i64::from(l.translated_count) * 100) / i64::from(l.strings_count))
                        .clamp(0, 100) as u8
                } else {
                    100
                };
                LanguageEntry {
                    id: l.lang_code,
                    name: l.name,
                    native_name: l.native_name,
                    official: l.official,
                    progress_percent: pct,
                }
            })
            .collect()
    }

    fn active_language(&self) -> String {
        self.langpack.active_language_code()
    }

    fn set_language(&mut self, lang_code: &str) -> Result<(), String> {
        let _ = self.langpack.apply_language(lang_code)?;
        if let Ok(mut c) = self.cache.lock() {
            c.active_language = lang_code.to_owned();
        }
        Ok(())
    }

    fn terms_of_service(&self) -> TermsOfServiceData {
        crate::upload::fetch_terms_of_service(self)
    }

    fn accept_terms(&mut self, tos_id: &str) -> Result<(), String> {
        crate::upload::accept_terms_of_service(self, tos_id)
    }

    fn exported_chat_invites(
        &self,
        peer_id: PeerId,
        revoked: bool,
    ) -> Result<tg_app_core::info::backend::InviteLinksList, String> {
        crate::admin::get_exported_chat_invites(self, peer_id, revoked)
    }

    fn export_chat_invite(
        &mut self,
        peer_id: PeerId,
        req: tg_app_core::info::backend::InviteLinkRequest,
    ) -> Result<tg_app_core::info::backend::InviteLinkInfo, String> {
        crate::admin::export_chat_invite(self, peer_id, req)
    }

    fn edit_exported_chat_invite(
        &mut self,
        peer_id: PeerId,
        link: &str,
        req: tg_app_core::info::backend::InviteLinkRequest,
    ) -> Result<tg_app_core::info::backend::InviteLinkInfo, String> {
        crate::admin::edit_exported_chat_invite(self, peer_id, link, req)
    }

    fn revoke_exported_chat_invite(
        &mut self,
        peer_id: PeerId,
        link: &str,
    ) -> Result<
        (
            tg_app_core::info::backend::InviteLinkInfo,
            Option<tg_app_core::info::backend::InviteLinkInfo>,
        ),
        String,
    > {
        crate::admin::revoke_exported_chat_invite(self, peer_id, link)
    }

    fn delete_exported_chat_invite(&mut self, peer_id: PeerId, link: &str) -> Result<(), String> {
        crate::admin::delete_exported_chat_invite(self, peer_id, link)
    }

    fn delete_revoked_exported_chat_invites(&mut self, peer_id: PeerId) -> Result<(), String> {
        crate::admin::delete_revoked_exported_chat_invites(self, peer_id)
    }

    fn chat_invite_importers(
        &self,
        peer_id: PeerId,
        link: Option<&str>,
        requested: bool,
        query: &str,
    ) -> Result<tg_app_core::info::backend::InviteImportersList, String> {
        crate::admin::get_chat_invite_importers(self, peer_id, link, requested, query)
    }

    fn hide_chat_join_request(
        &mut self,
        peer_id: PeerId,
        user_id: PeerId,
        approved: bool,
    ) -> Result<(), String> {
        crate::admin::hide_chat_join_request(self, peer_id, user_id, approved)
    }

    fn hide_all_chat_join_requests(
        &mut self,
        peer_id: PeerId,
        link: Option<&str>,
        approved: bool,
    ) -> Result<usize, String> {
        crate::admin::hide_all_chat_join_requests(self, peer_id, link, approved)
    }

    fn join_requests_summary(
        &self,
        peer_id: PeerId,
    ) -> tg_app_core::info::backend::JoinRequestsSummary {
        crate::admin::join_requests_summary(self, peer_id)
    }

    fn peer_color_settings(
        &self,
        peer_id: PeerId,
    ) -> tg_app_core::info::backend::PeerColorSettings {
        crate::admin::peer_color_settings(self, peer_id)
    }

    fn update_peer_color(
        &mut self,
        peer_id: PeerId,
        req: tg_app_core::info::backend::UpdatePeerColorRequest,
    ) -> Result<(), String> {
        crate::admin::update_peer_color(self, peer_id, req)
    }

    fn check_ownership_transfer(
        &self,
        peer_id: PeerId,
    ) -> Result<tg_app_core::info::backend::OwnershipTransferCheck, String> {
        crate::admin::check_ownership_transfer(self, peer_id)
    }

    fn transfer_ownership(
        &mut self,
        peer_id: PeerId,
        new_owner_id: PeerId,
        password: &str,
    ) -> Result<(), tg_app_core::info::backend::OwnershipTransferError> {
        crate::admin::transfer_ownership(self, peer_id, new_owner_id, password)
    }

    fn similar_channels(
        &self,
        peer_id: PeerId,
    ) -> Result<tg_app_core::info::backend::SimilarChannelsResult, String> {
        crate::admin::get_channel_recommendations(self, peer_id)
    }

    fn downloads_list(&self, query: &str) -> Vec<tg_app_core::info::backend::DownloadEntry> {
        crate::admin::downloads_list(self, query)
    }

    fn toggle_download_pause(&mut self, download_id: u64) -> Result<(), String> {
        crate::admin::toggle_download_pause(self, download_id)
    }

    fn cancel_or_delete_download(
        &mut self,
        download_id: u64,
        delete_file: bool,
    ) -> Result<(), String> {
        crate::admin::cancel_or_delete_download(self, download_id, delete_file)
    }

    fn clear_completed_downloads(&mut self) -> Result<usize, String> {
        crate::admin::clear_completed_downloads(self)
    }

    fn delete_all_downloads(&mut self) -> Result<usize, String> {
        crate::admin::delete_all_downloads(self)
    }

    fn check_history_import(
        &self,
        import_head: &str,
    ) -> Result<tg_app_core::core_types::dto::chat_import::HistoryImportParsedInfo, String> {
        crate::chat_import::check_history_import(self, import_head)
    }

    fn check_history_import_peer(&self, peer_id: PeerId) -> Result<String, String> {
        crate::chat_import::check_history_import_peer(self, peer_id)
    }

    fn run_history_import(
        &mut self,
        peer_id: PeerId,
        archive: &tg_app_core::core_types::dto::chat_import::ParsedImportArchive,
    ) -> Result<tg_app_core::core_types::dto::chat_import::ChatImportSummary, String> {
        crate::chat_import::run_history_import(self, peer_id, archive)
    }
    fn peer_pinned_stories(
        &self,
        peer_id: PeerId,
        offset_id: i32,
        limit: i32,
    ) -> Result<tg_app_core::info::backend::ProfileStoriesPage, String> {
        crate::profile_extras::peer_pinned_stories(self, peer_id, offset_id, limit)
    }

    fn peer_stories_archive(
        &self,
        peer_id: PeerId,
        offset_id: i32,
        limit: i32,
    ) -> Result<tg_app_core::info::backend::ProfileStoriesPage, String> {
        crate::profile_extras::peer_stories_archive(self, peer_id, offset_id, limit)
    }

    fn peer_story_albums(
        &self,
        peer_id: PeerId,
    ) -> Result<Vec<tg_app_core::info::backend::ProfileStoryAlbum>, String> {
        crate::profile_extras::peer_story_albums(self, peer_id)
    }

    fn peer_album_stories(
        &self,
        peer_id: PeerId,
        album_id: i32,
        offset_id: i32,
        limit: i32,
    ) -> Result<tg_app_core::info::backend::ProfileStoriesPage, String> {
        crate::profile_extras::peer_album_stories(self, peer_id, album_id, offset_id, limit)
    }

    fn peer_saved_gifts(
        &self,
        peer_id: PeerId,
        filter: &tg_payments::star_gifts::PeerGiftsFilter,
    ) -> Result<tg_app_core::info::backend::ProfilePeerGiftsPage, String> {
        crate::profile_extras::peer_saved_gifts(self, peer_id, filter)
    }

    fn toggle_peer_gift_saved(
        &mut self,
        peer_id: PeerId,
        msg_id: i32,
        saved_id: u64,
        unsave: bool,
    ) -> Result<(), String> {
        crate::profile_extras::toggle_peer_gift_saved(self, peer_id, msg_id, saved_id, unsave)
    }

    fn peer_saved_music(
        &self,
        peer_id: PeerId,
        offset: i32,
        limit: i32,
    ) -> Result<Vec<tg_app_core::info::backend::SavedMusicTrack>, String> {
        crate::profile_extras::peer_saved_music(self, peer_id, offset, limit)
    }

    fn save_profile_music(
        &mut self,
        track: &tg_app_core::info::backend::SavedMusicTrack,
        unsave: bool,
    ) -> Result<(), String> {
        crate::profile_extras::save_profile_music(self, track, unsave)
    }

    fn upload_emoji_userpic(
        &mut self,
        peer_id: PeerId,
        req: tg_app_core::info::backend::EmojiUserpicRequest,
    ) -> Result<(), String> {
        crate::profile_extras::upload_emoji_userpic(self, peer_id, req)
    }

    fn peer_available_reactions(
        &self,
        peer_id: PeerId,
    ) -> tg_app_core::info::backend::PeerReactionsConfig {
        crate::profile_extras::peer_available_reactions(self, peer_id)
    }

    fn set_peer_available_reactions(
        &mut self,
        peer_id: PeerId,
        config: tg_app_core::info::backend::PeerReactionsConfig,
    ) -> Result<(), String> {
        crate::profile_extras::set_peer_available_reactions(self, peer_id, config)
    }

    fn toggle_peer_username(
        &mut self,
        peer_id: PeerId,
        username: &str,
        active: bool,
    ) -> Result<(), String> {
        crate::profile_extras::toggle_peer_username(self, peer_id, username, active)
    }

    fn reorder_peer_usernames(&mut self, peer_id: PeerId, order: &[String]) -> Result<(), String> {
        crate::profile_extras::reorder_peer_usernames(self, peer_id, order)
    }

    fn peer_verification_status(
        &self,
        bot_id: PeerId,
        peer_id: PeerId,
    ) -> tg_app_core::info::backend::PeerVerificationStatus {
        crate::profile_extras::peer_verification_status(self, bot_id, peer_id)
    }

    fn set_peer_custom_verification(
        &mut self,
        bot_id: PeerId,
        peer_id: PeerId,
        enabled: bool,
        custom_description: Option<&str>,
    ) -> Result<(), String> {
        crate::profile_extras::set_peer_custom_verification(
            self,
            bot_id,
            peer_id,
            enabled,
            custom_description,
        )
    }

    fn joined_communities(
        &self,
    ) -> Result<Vec<tg_app_core::core_types::dto::community::CommunityDetails>, String> {
        crate::communities::joined_communities(self)
    }

    fn community_details(
        &self,
        community_id: PeerId,
    ) -> Option<tg_app_core::core_types::dto::community::CommunityDetails> {
        crate::communities::community_details(self, community_id)
    }

    fn community_for_peer(
        &self,
        peer_id: PeerId,
    ) -> Option<tg_app_core::core_types::dto::community::CommunityDetails> {
        crate::communities::community_for_peer(self, peer_id)
    }

    fn create_community(
        &mut self,
        req: tg_app_core::core_types::dto::community::CreateCommunityRequest,
    ) -> Result<PeerId, String> {
        crate::communities::create_community(self, req)
    }

    fn toggle_community_peer_link(
        &mut self,
        community_id: PeerId,
        peer_id: PeerId,
        action: tg_app_core::core_types::dto::community::CommunityPeerLinkAction,
    ) -> Result<(), String> {
        crate::communities::toggle_community_peer_link(self, community_id, peer_id, action)
    }

    fn toggle_community_collapsed_in_dialogs(
        &mut self,
        community_id: PeerId,
        collapsed: bool,
    ) -> Result<(), String> {
        crate::communities::toggle_community_collapsed_in_dialogs(self, community_id, collapsed)
    }

    fn community_peer_link_requests(
        &self,
        community_id: PeerId,
        offset: &str,
        limit: i32,
    ) -> Result<tg_app_core::core_types::dto::community::CommunityPeerRequestsSlice, String> {
        crate::communities::community_peer_link_requests(self, community_id, offset, limit)
    }

    fn toggle_community_peer_link_request_approval(
        &mut self,
        community_id: PeerId,
        peer_id: PeerId,
        reject: bool,
    ) -> Result<(), String> {
        crate::communities::toggle_community_peer_link_request_approval(
            self,
            community_id,
            peer_id,
            reject,
        )
    }

    fn toggle_all_community_peer_link_request_approval(
        &mut self,
        community_id: PeerId,
        reject: bool,
    ) -> Result<(), String> {
        crate::communities::toggle_all_community_peer_link_request_approval(
            self,
            community_id,
            reject,
        )
    }

    fn toggle_community_participant_banned(
        &mut self,
        community_id: PeerId,
        participant_id: PeerId,
        unban: bool,
    ) -> Result<(), String> {
        crate::communities::toggle_community_participant_banned(
            self,
            community_id,
            participant_id,
            unban,
        )
    }

    fn community_participant_joined_chats(
        &self,
        community_id: PeerId,
        participant_id: PeerId,
    ) -> Result<tg_app_core::core_types::dto::community::CommunityParticipantJoinedChats, String>
    {
        crate::communities::community_participant_joined_chats(self, community_id, participant_id)
    }

    fn update_community_settings(
        &mut self,
        community_id: PeerId,
        title: &str,
        only_admins_can_add: bool,
    ) -> Result<(), String> {
        crate::communities::update_community_settings(
            self,
            community_id,
            title,
            only_admins_can_add,
        )
    }

    fn delete_community(&mut self, community_id: PeerId) -> Result<(), String> {
        crate::communities::delete_community(self, community_id)
    }

    fn check_bot_username(&self, username: &str) -> Result<bool, String> {
        crate::communities::check_bot_username(self, username)
    }

    fn create_managed_bot(
        &mut self,
        manager_id: PeerId,
        name: &str,
        username: &str,
        via_deeplink: bool,
    ) -> Result<PeerId, String> {
        crate::communities::create_managed_bot(self, manager_id, name, username, via_deeplink)
    }

    fn get_bot_info(
        &self,
        bot_id: PeerId,
        lang_code: &str,
    ) -> Result<tg_app_core::core_types::dto::managed_bot::BotInfoTexts, String> {
        crate::communities::get_bot_info(self, bot_id, lang_code)
    }

    fn set_bot_info(
        &mut self,
        bot_id: PeerId,
        lang_code: &str,
        change: &tg_app_core::core_types::dto::managed_bot::BotInfoChange,
    ) -> Result<(), String> {
        crate::communities::set_bot_info(self, bot_id, lang_code, change)
    }

    fn translate_bot_info_texts(
        &self,
        texts: &[String],
        lang_code: &str,
    ) -> Result<Vec<String>, String> {
        crate::communities::translate_bot_info_texts(self, texts, lang_code)
    }

    fn admined_bots(&self) -> Result<Vec<PeerId>, String> {
        crate::communities::admined_bots(self)
    }

    fn moderate_messages(
        &mut self,
        req: tg_app_core::core_types::dto::moderate_messages::ModerateMessagesRequest,
    ) -> Result<tg_app_core::core_types::dto::moderate_messages::ModerateMessagesResult, String>
    {
        crate::communities::moderate_messages(self, req)
    }

    fn welcome_messages(
        &self,
        peer_id: PeerId,
    ) -> Result<Vec<tg_app_core::core_types::dto::welcome_messages::WelcomeMessageItem>, String>
    {
        crate::communities::welcome_messages(self, peer_id)
    }

    fn send_welcome_message(
        &mut self,
        peer_id: PeerId,
        text: &str,
    ) -> Result<tg_app_core::core_types::dto::welcome_messages::WelcomeMessageItem, String> {
        crate::communities::send_welcome_message(self, peer_id, text)
    }

    fn edit_welcome_message(
        &mut self,
        peer_id: PeerId,
        id: i32,
        text: &str,
    ) -> Result<tg_app_core::core_types::dto::welcome_messages::WelcomeMessageItem, String> {
        crate::communities::edit_welcome_message(self, peer_id, id, text)
    }

    fn delete_welcome_message(&mut self, peer_id: PeerId, id: i32) -> Result<(), String> {
        crate::communities::delete_welcome_message(self, peer_id, id)
    }

    fn delete_all_welcome_messages(&mut self, peer_id: PeerId) -> Result<(), String> {
        crate::communities::delete_all_welcome_messages(self, peer_id)
    }

    fn channel_anti_spam_enabled(&self, peer_id: PeerId) -> bool {
        crate::communities::channel_anti_spam_enabled(self, peer_id)
    }

    fn toggle_channel_anti_spam(&mut self, peer_id: PeerId, enabled: bool) -> Result<(), String> {
        crate::communities::toggle_channel_anti_spam(self, peer_id, enabled)
    }

    fn report_antispam_false_positive(
        &mut self,
        peer_id: PeerId,
        msg_id: i32,
    ) -> Result<(), String> {
        crate::communities::report_antispam_false_positive(self, peer_id, msg_id)
    }

    fn get_collectible_info(
        &self,
        peer_id: PeerId,
        entity: &str,
    ) -> Result<tg_app_core::core_types::dto::collectible_info::CollectibleInfoData, String> {
        crate::wallet::get_collectible_info_from_session(self, peer_id, entity)
    }

    fn get_paid_messages_revenue(
        &self,
        parent_peer: Option<PeerId>,
        user_id: PeerId,
    ) -> Result<tg_app_core::info::backend::PaidMessagesRevenue, String> {
        crate::paid_messages::get_paid_messages_revenue_live(self, parent_peer, user_id)
    }

    fn toggle_no_paid_messages_exception(
        &mut self,
        parent_peer: Option<PeerId>,
        user_id: PeerId,
        refund_charged: bool,
        require_payment: bool,
    ) -> Result<bool, String> {
        crate::paid_messages::toggle_no_paid_messages_exception_live(
            self,
            parent_peer,
            user_id,
            refund_charged,
            require_payment,
        )
    }

    fn get_requirements_to_contact(
        &self,
        user_ids: &[PeerId],
    ) -> Result<Vec<tg_app_core::info::backend::UserContactRequirement>, String> {
        crate::paid_messages::get_requirements_to_contact_live(self, user_ids)
    }

    fn set_main_profile_tab(
        &mut self,
        peer_id: PeerId,
        tab: tg_app_core::info::backend::ProfileMainTab,
    ) -> Result<(), String> {
        crate::profile_extras::set_main_profile_tab(self, peer_id, tab)
    }

    fn main_profile_tab(
        &self,
        peer_id: PeerId,
    ) -> Option<tg_app_core::info::backend::ProfileMainTab> {
        crate::profile_extras::main_profile_tab(self, peer_id)
    }

    fn suggest_user_birthday(
        &mut self,
        peer_id: PeerId,
        birthday: tg_app_core::info::values::Birthday,
    ) -> Result<(), String> {
        crate::profile_extras::suggest_user_birthday(self, peer_id, birthday)
    }

    fn report_profile_photo(
        &mut self,
        peer_id: PeerId,
        photo: tg_app_core::info::backend::ProfilePhotoRef,
        reason: ReportReason,
        comment: &str,
    ) -> Result<(), String> {
        crate::profile_extras::report_profile_photo(self, peer_id, photo, reason, comment)
    }

    fn personal_channel(
        &self,
        peer_id: PeerId,
    ) -> Option<tg_app_core::info::backend::PersonalChannelOption> {
        crate::profile_info_extras::personal_channel(self, peer_id)
    }

    fn personal_channel_candidates(
        &self,
    ) -> Vec<tg_app_core::info::backend::PersonalChannelOption> {
        crate::profile_info_extras::personal_channel_candidates(self)
    }

    fn update_personal_channel(&mut self, channel_id: Option<PeerId>) -> Result<(), String> {
        SettingsBackend::update_personal_channel(self, channel_id.map(|p| p.0)).map(|_| ())
    }

    fn peer_profile_balance(
        &self,
        peer_id: PeerId,
    ) -> Option<tg_app_core::info::backend::ProfileBalanceInfo> {
        crate::profile_info_extras::peer_profile_balance(self, peer_id)
    }

    fn chat_star_gift_notifications_enabled(&self, peer_id: PeerId) -> Option<bool> {
        crate::star_gifts::info_chat_star_gift_notifications_enabled(self, peer_id)
    }

    fn toggle_chat_star_gift_notifications(
        &mut self,
        peer_id: PeerId,
        enabled: bool,
    ) -> Result<bool, String> {
        crate::star_gifts::info_toggle_chat_star_gift_notifications(self, peer_id, enabled)
    }
    fn channel_management_state(
        &self,
        peer_id: PeerId,
    ) -> Option<tg_app_core::info::backend::ChannelManagementState> {
        crate::channel_mgmt::channel_management_state(self, peer_id)
    }

    fn toggle_pre_history_hidden(&mut self, peer_id: PeerId, hidden: bool) -> Result<(), String> {
        crate::channel_mgmt::toggle_pre_history_hidden(self, peer_id, hidden)
    }

    fn groups_for_discussion(
        &self,
    ) -> Result<Vec<tg_app_core::info::backend::DiscussionGroupCandidate>, String> {
        crate::channel_mgmt::groups_for_discussion(self)
    }

    fn convert_to_gigagroup(&mut self, peer_id: PeerId) -> Result<(), String> {
        crate::channel_mgmt::convert_to_gigagroup(self, peer_id)
    }

    fn deactivate_all_channel_usernames(&mut self, peer_id: PeerId) -> Result<(), String> {
        crate::channel_mgmt::deactivate_all_channel_usernames(self, peer_id)
    }

    fn toggle_channel_forum(
        &mut self,
        peer_id: PeerId,
        enabled: bool,
        tabs: bool,
    ) -> Result<(), String> {
        crate::channel_mgmt::toggle_channel_forum(self, peer_id, enabled, tabs)
    }

    fn toggle_participants_hidden(&mut self, peer_id: PeerId, hidden: bool) -> Result<(), String> {
        crate::channel_mgmt::toggle_participants_hidden(self, peer_id, hidden)
    }

    fn toggle_view_forum_as_messages(
        &mut self,
        peer_id: PeerId,
        as_messages: bool,
    ) -> Result<(), String> {
        crate::channel_mgmt::toggle_view_forum_as_messages(self, peer_id, as_messages)
    }

    fn set_boosts_to_unblock_restrictions(
        &mut self,
        peer_id: PeerId,
        boosts: u32,
    ) -> Result<(), String> {
        crate::channel_mgmt::set_boosts_to_unblock_restrictions(self, peer_id, boosts)
    }

    fn set_channel_sticker_set(
        &mut self,
        peer_id: PeerId,
        set: tg_app_core::info::backend::ChannelStickerSetRef,
    ) -> Result<(), String> {
        crate::channel_mgmt::set_channel_sticker_set(self, peer_id, set)
    }

    fn set_channel_emoji_sticker_set(
        &mut self,
        peer_id: PeerId,
        set: tg_app_core::info::backend::ChannelStickerSetRef,
    ) -> Result<(), String> {
        crate::channel_mgmt::set_channel_emoji_sticker_set(self, peer_id, set)
    }

    fn update_channel_paid_messages_price(
        &mut self,
        peer_id: PeerId,
        broadcast_messages_allowed: bool,
        stars_per_message: i64,
    ) -> Result<(), String> {
        crate::channel_mgmt::update_channel_paid_messages_price(
            self,
            peer_id,
            broadcast_messages_allowed,
            stars_per_message,
        )
    }

    fn toggle_channel_autotranslation(
        &mut self,
        peer_id: PeerId,
        enabled: bool,
    ) -> Result<(), String> {
        crate::channel_mgmt::toggle_channel_autotranslation(self, peer_id, enabled)
    }

    fn delete_basic_chat(&mut self, peer_id: PeerId) -> Result<(), String> {
        crate::channel_mgmt::delete_basic_chat(self, peer_id)
    }

    fn admins_with_invites(
        &self,
        peer_id: PeerId,
    ) -> Result<Vec<tg_app_core::info::backend::AdminWithInvitesEntry>, String> {
        crate::channel_mgmt::admins_with_invites(self, peer_id)
    }

    fn delete_contacts(&mut self, peer_ids: &[PeerId]) -> Result<(), String> {
        crate::contacts_extras::delete_contacts(self, peer_ids)
    }

    fn export_contact_token(
        &self,
    ) -> Result<tg_app_core::core_types::dto::contacts_extras::ExportedContactTokenInfo, String>
    {
        crate::contacts_extras::export_contact_token(self)
    }

    fn import_contact_token(&mut self, token: &str) -> Result<PeerId, String> {
        crate::contacts_extras::import_contact_token(self, token)
    }

    fn export_no_paid_messages_token(
        &self,
        revoke: bool,
        temporary: bool,
    ) -> Result<
        tg_app_core::core_types::dto::contacts_extras::ExportedNoPaidMessagesTokenInfo,
        String,
    > {
        crate::contacts_extras::export_no_paid_messages_token(self, revoke, temporary)
    }

    fn import_no_paid_messages_token(
        &mut self,
        peer_id: PeerId,
        token: &str,
    ) -> Result<
        tg_app_core::core_types::dto::contacts_extras::NoPaidMessagesTokenImportResult,
        String,
    > {
        crate::contacts_extras::import_no_paid_messages_token(self, peer_id, token)
    }

    fn contact_note(&self, peer_id: PeerId) -> Option<tg_text::TextWithEntities> {
        crate::contacts_extras::contact_note(self, peer_id)
    }

    fn update_contact_note(
        &mut self,
        peer_id: PeerId,
        note: tg_text::TextWithEntities,
    ) -> Result<bool, String> {
        crate::contacts_extras::update_contact_note(self, peer_id, note)
    }

    fn active_profile_photo_ref(
        &self,
        peer_id: PeerId,
    ) -> Option<tg_app_core::info::backend::ProfilePhotoRef> {
        crate::profile_info_extras::active_profile_photo_ref(self, peer_id)
    }

    fn delete_profile_photos(
        &mut self,
        photos: &[tg_app_core::info::backend::ProfilePhotoRef],
    ) -> Result<Vec<i64>, String> {
        crate::contacts_extras::delete_profile_photos(self, photos)
    }

    fn upload_contact_profile_photo(
        &mut self,
        peer_id: PeerId,
        request: tg_app_core::core_types::dto::contacts_extras::ContactProfilePhotoUpload,
    ) -> Result<i64, String> {
        crate::contacts_extras::upload_contact_profile_photo(self, peer_id, request)
    }
}

// ============================================================================
// 5. settings::backend::SettingsBackend
// ============================================================================

impl SettingsBackend for LiveSession {
    fn dictionary_cloud_source(
        &self,
    ) -> Option<Box<dyn tg_app_core::core_types::dto::dictionaries::DictionaryCloudSource + Send>>
    {
        crate::wp40_polish::live_dictionary_source(self)
    }

    fn profile(&self) -> ProfileInfo {
        let mut info = self.read_session(self_profile_from_session);
        if let Ok(c) = self.cache.lock() {
            info.phone_hidden = c.phone_hidden;
            if let Some(idx) = c.photo_color_index {
                info.photo_color_index = idx;
            }
            if c.photo_label.is_some() {
                info.photo_label = c.photo_label.clone();
            }
        }
        info
    }

    fn update_name(&mut self, first_name: &str, last_name: &str) -> Result<(), String> {
        let first = first_name.trim();
        if first.is_empty() {
            return Err("First name cannot be empty.".to_owned());
        }
        let req = tl_fns::account::UpdateProfile {
            first_name: Some(first.to_owned()),
            last_name: Some(last_name.trim().to_owned()),
            about: None,
        };
        let user = self.invoke_rpc(req)?;
        self.write_session(|s| {
            apply_tl_users_and_chats(s, &[user], &[]);
        });
        self.refresh_cached_views();
        self.bridge.state().notify_ui();
        Ok(())
    }

    fn update_bio(&mut self, bio: &str) -> Result<(), String> {
        let req = tl_fns::account::UpdateProfile {
            first_name: None,
            last_name: None,
            about: Some(bio.trim().to_owned()),
        };
        let user = self.invoke_rpc(req)?;
        self.write_session(|s| {
            apply_tl_users_and_chats(s, &[user], &[]);
            let self_id = s.self_peer_id();
            if let Some(p) = s.peer_mut(self_id) {
                p.common_mut().about = bio.trim().to_owned();
            }
        });
        self.refresh_cached_views();
        self.bridge.state().notify_ui();
        Ok(())
    }

    fn check_username(&self, username: &str) -> UsernameStatus {
        let syntax = validate_username_syntax(username);
        if syntax != UsernameStatus::Available {
            return syntax;
        }
        let cleaned = username.trim().trim_start_matches('@');
        let current = self.profile().username;
        if cleaned.eq_ignore_ascii_case(&current) {
            return UsernameStatus::Current;
        }
        match self.invoke_rpc(tl_fns::account::CheckUsername {
            username: cleaned.to_owned(),
        }) {
            Ok(true) => UsernameStatus::Available,
            Ok(false) => UsernameStatus::Taken,
            Err(_) => UsernameStatus::Taken,
        }
    }

    fn update_username(&mut self, username: &str) -> Result<(), String> {
        let cleaned = username.trim().trim_start_matches('@').to_owned();
        let user = self.invoke_rpc(tl_fns::account::UpdateUsername { username: cleaned })?;
        self.write_session(|s| {
            apply_tl_users_and_chats(s, &[user], &[]);
        });
        self.refresh_cached_views();
        self.bridge.state().notify_ui();
        Ok(())
    }

    fn update_birthday(&mut self, birthday: Option<SettingsBirthday>) -> Result<(), String> {
        let tl_bday = birthday.map(|b| {
            tl_enums::Birthday::Birthday(tl_types::Birthday {
                day: i32::from(b.day),
                month: i32::from(b.month),
                year: b.year.map(i32::from),
            })
        });
        let _ = self.invoke_rpc(tl_fns::account::UpdateBirthday { birthday: tl_bday })?;
        self.write_session(|s| {
            let self_id = s.self_peer_id();
            if let Some(Peer::User(u)) = s.peer_mut(self_id) {
                u.birthday = birthday.map(|b| tg_data::Birthday {
                    day: b.day,
                    month: b.month,
                    year: b.year.unwrap_or(0),
                });
            }
        });
        self.bridge.state().notify_ui();
        Ok(())
    }

    fn update_profile_photo(&mut self, color_index: u8, photo_label: Option<String>) {
        crate::settings_sync::update_profile_photo_live(self, color_index, photo_label);
    }

    fn upload_profile_photo(&mut self, request: ProfilePhotoUpload) -> Result<i64, String> {
        crate::settings_sync::upload_profile_photo_live(self, request)
    }

    fn set_phone_hidden(&mut self, hidden: bool) {
        if let Ok(mut c) = self.cache.lock() {
            c.phone_hidden = hidden;
            c.session_settings_dirty = true;
        }
    }

    fn contacts(&self) -> Vec<PeerRef> {
        let now = now_unix_i32();
        self.read_session(|s| {
            let self_id = s.self_peer_id();
            known_peer_ids(s)
                .into_iter()
                .filter(|&pid| pid != self_id)
                .filter_map(|pid| s.peer(pid).map(|p| peer_to_settings_ref(p, now)))
                .collect()
        })
    }

    fn privacy_rules(&self) -> BTreeMap<PrivacyKey, PrivacyRule> {
        self.load_privacy_rules_if_needed(&PrivacyKey::ALL);
        let Ok(c) = self.cache.lock() else {
            return PrivacyKey::ALL
                .into_iter()
                .map(|key| (key, PrivacyRule::default()))
                .collect();
        };
        PrivacyKey::ALL
            .into_iter()
            .map(|key| (key, Self::cached_privacy_rule(&c, key)))
            .collect()
    }

    fn privacy_rule(&self, key: PrivacyKey) -> PrivacyRule {
        self.load_privacy_rules_if_needed(&[key]);
        self.cache
            .lock()
            .map(|c| Self::cached_privacy_rule(&c, key))
            .unwrap_or_default()
    }

    fn set_privacy_rule(&mut self, key: PrivacyKey, mut rule: PrivacyRule) {
        rule.normalize();
        let tl_rules = self.read_session(|s| privacy_rule_to_tl(&rule, s));
        let req = tl_fns::account::SetPrivacy {
            key: privacy_key_to_tl(key),
            rules: tl_rules,
        };
        match self.invoke_rpc(req) {
            Ok(tl_enums::account::PrivacyRules::PrivacyRules(resp)) => {
                self.write_session(|s| {
                    apply_tl_users_and_chats(s, &resp.users, &resp.chats);
                });
                let server_rule = privacy_rules_from_tl(&resp.rules);
                if let Ok(mut c) = self.cache.lock() {
                    c.privacy_loaded.insert(key);
                    c.privacy_rules.insert(key, server_rule);
                }
                if key == PrivacyKey::LastSeen {
                    // Port of `UserPrivacy::apply` -> `ApiWrap::updatePrivacyLastSeens`
                    // (`api/api_user_privacy.cpp:213-215`, `apiwrap.cpp:2182-2230`).
                    let now_ms = u64::try_from(self.ch_now_ms()).unwrap_or(0);
                    let _ =
                        crate::contacts_extras::maybe_refresh_contact_statuses(self, now_ms, true);
                }
                self.bridge.state().notify_ui();
            }
            Err(err) => {
                warn!(?key, error = %err, "account.setPrivacy failed");
            }
        }
    }

    fn global_privacy(&self) -> GlobalPrivacySettings {
        crate::settings_sync::fetch_global_privacy_live(self)
    }

    fn set_global_privacy(&mut self, settings: GlobalPrivacySettings) {
        crate::settings_sync::save_global_privacy_live(self, settings);
    }

    fn account_ttl_days(&self) -> i32 {
        let cached = self
            .cache
            .lock()
            .ok()
            .filter(|c| c.account_ttl_loaded)
            .map(|c| c.account_ttl_days);
        if let Some(days) = cached {
            return days;
        }
        self.spawn_settings_read(SettingsRead::AccountTtl, Self::load_account_ttl);
        self.cache.lock().map_or(180, |c| c.account_ttl_days)
    }

    fn set_account_ttl_days(&mut self, days: i32) {
        let req = tl_fns::account::SetAccountTTL {
            ttl: tl_enums::AccountDaysTTL::AccountDaysTTL(tl_types::AccountDaysTTL { days }),
        };
        if self.invoke_rpc(req).is_ok()
            && let Ok(mut c) = self.cache.lock()
        {
            c.account_ttl_loaded = true;
            c.account_ttl_days = days;
        }
    }

    fn clear_payment_info(&mut self, clear_shipping: bool, clear_credentials: bool) {
        // tdesktop sends it and closes the box at once, with no fail handler
        // (`settings_privacy_security.cpp` `ClearPaymentInfoBox`).
        self.queue_background_rpc(
            tl_fns::payments::ClearSavedInfo {
                credentials: clear_credentials,
                info: clear_shipping,
            },
            |error| debug!(%error, "payments.clearSavedInfo failed"),
        );
    }

    fn active_sessions(&self) -> Vec<ActiveSession> {
        self.fetch_sessions_if_needed();
        self.cache
            .lock()
            .map(|c| c.active_sessions.clone())
            .unwrap_or_default()
    }

    fn terminate_session(&mut self, hash: u64) -> bool {
        if hash == 0 {
            return false;
        }
        let req = tl_fns::account::ResetAuthorization { hash: hash as i64 };
        match self.invoke_rpc(req) {
            Ok(true) => {
                if let Ok(mut c) = self.cache.lock() {
                    c.active_sessions.retain(|s| s.hash != hash);
                }
                self.bridge.state().notify_ui();
                true
            }
            _ => false,
        }
    }

    fn terminate_all_other_sessions(&mut self) -> usize {
        let before = self
            .cache
            .lock()
            .map(|c| c.active_sessions.iter().filter(|s| s.hash != 0).count())
            .unwrap_or(0);
        match self.invoke_rpc(tl_fns::auth::ResetAuthorizations {}) {
            Ok(true) => {
                if let Ok(mut c) = self.cache.lock() {
                    c.active_sessions.retain(|s| s.hash == 0);
                }
                self.bridge.state().notify_ui();
                before
            }
            _ => 0,
        }
    }

    fn update_session_permissions(
        &mut self,
        hash: u64,
        accept_calls: bool,
        accept_secret_chats: bool,
    ) {
        // Match `Authorizations::toggleCallsDisabled` (`api/api_authorizations.cpp:247-266`):
        // only transmit the flag(s) that actually changed rather than always setting both.
        let cached = self
            .cache
            .lock()
            .ok()
            .and_then(|c| c.active_sessions.iter().find(|s| s.hash == hash).cloned());
        let (encrypted_requests_disabled, call_requests_disabled) = match cached {
            Some(prev) => {
                let enc = (prev.accept_secret_chats != accept_secret_chats)
                    .then_some(!accept_secret_chats);
                let calls = (prev.accept_calls != accept_calls).then_some(!accept_calls);
                if enc.is_none() && calls.is_none() {
                    return;
                }
                (enc, calls)
            }
            None => (Some(!accept_secret_chats), Some(!accept_calls)),
        };

        let rpc = self.runtime.rpc().clone();
        let ok = block_on_runtime(self.runtime.tokio_handle(), async move {
            tokio::time::timeout(LIVE_RPC_TIMEOUT, async move {
                let mut mgr = tg_api::account::AuthorizationsManager::new();
                mgr.change_settings(
                    &rpc,
                    hash as i64,
                    false,
                    encrypted_requests_disabled,
                    call_requests_disabled,
                )
                .await
            })
            .await
        })
        .ok()
        .and_then(Result::ok)
        .unwrap_or(false);

        if ok
            && let Ok(mut c) = self.cache.lock()
            && let Some(entry) = c.active_sessions.iter_mut().find(|s| s.hash == hash)
        {
            entry.accept_calls = accept_calls;
            entry.accept_secret_chats = accept_secret_chats;
        }
    }

    fn review_authorization(&mut self, hash: u64, confirm: bool) -> Result<bool, String> {
        // Match `Authorizations::reviewOne` (`api/api_authorizations.cpp:439-495`):
        // sends `account.changeAuthorizationSettings` with `confirmed: true` only (`flags = 0x08`)
        // when confirming, or `account.resetAuthorization` when rejecting; `HASH_INVALID` is benign.
        let rpc = self.runtime.rpc().clone();
        let now = now_unix_i32();
        let res = block_on_runtime(self.runtime.tokio_handle(), async move {
            tokio::time::timeout(LIVE_RPC_TIMEOUT, async move {
                let mut mgr = tg_api::account::AuthorizationsManager::new();
                mgr.review_unreviewed(&rpc, hash as i64, confirm, now).await
            })
            .await
        });
        match res {
            Ok(Ok(ok)) => {
                if !confirm && let Ok(mut c) = self.cache.lock() {
                    c.active_sessions.retain(|s| s.hash != hash);
                }
                self.bridge.state().notify_ui();
                Ok(ok)
            }
            Ok(Err(err)) => Err(err.to_string()),
            Err(_) => Err("Timed out reviewing authorization.".to_owned()),
        }
    }

    fn update_personal_channel(&mut self, channel_id: Option<i64>) -> Result<bool, String> {
        // Match `core/local_url_handlers.cpp:205-218` via
        // `tg_api::account::AccountSettingsManager::update_personal_channel`.
        let data_cid = channel_id.map(|id| {
            to_data_peer(PeerId(id))
                .to_channel()
                .unwrap_or_else(|| tg_data::ChannelId::from_tl(id))
        });
        let rpc = self.runtime.rpc().clone();
        let session_arc = Arc::clone(self.runtime.session());
        let res = block_on_runtime(self.runtime.tokio_handle(), async move {
            tokio::time::timeout(LIVE_RPC_TIMEOUT, async move {
                let mut session = session_arc.write().await;
                let mgr = tg_api::account::AccountSettingsManager::new();
                mgr.update_personal_channel(&rpc, &mut session, data_cid)
                    .await
            })
            .await
        });
        match res {
            Ok(Ok(ok)) => {
                self.bridge.state().notify_ui();
                Ok(ok)
            }
            Ok(Err(err)) => Err(err.to_string()),
            Err(_) => Err("Timed out updating personal channel.".to_owned()),
        }
    }

    fn session_ttl_days(&self) -> i32 {
        self.fetch_sessions_if_needed();
        self.cache.lock().map(|c| c.session_ttl_days).unwrap_or(180)
    }

    fn set_session_ttl_days(&mut self, days: i32) {
        let req = tl_fns::account::SetAuthorizationTTL {
            authorization_ttl_days: days,
        };
        if self.invoke_rpc(req).is_ok()
            && let Ok(mut c) = self.cache.lock()
        {
            c.session_ttl_days = days;
        }
    }

    fn blocked_users(&self) -> Vec<PeerRef> {
        let cached = self
            .cache
            .lock()
            .ok()
            .filter(|c| c.blocked_loaded)
            .map(|c| c.blocked_peers.clone());
        if let Some(list) = cached {
            return list;
        }
        // Until the answer is there the last list stays (`Api::BlockedPeers::slice`).
        self.spawn_settings_read(SettingsRead::BlockedUsers, Self::load_blocked_users);
        self.cache
            .lock()
            .map(|c| c.blocked_peers.clone())
            .unwrap_or_default()
    }

    fn block_peer(&mut self, peer_id: i64) {
        let pid = PeerId(peer_id);
        let is_blocked = self.read_session(|s| {
            s.peer(to_data_peer(pid))
                .is_some_and(|p| p.common().block_status == BlockStatus::Blocked)
        });
        if !is_blocked {
            let _ = self.toggle_peer_block_rpc(pid);
        }
    }

    fn unblock_peer(&mut self, peer_id: i64) {
        let pid = PeerId(peer_id);
        let is_blocked = self.read_session(|s| {
            s.peer(to_data_peer(pid))
                .is_some_and(|p| p.common().block_status == BlockStatus::Blocked)
        });
        if is_blocked {
            let _ = self.toggle_peer_block_rpc(pid);
        }
    }

    fn cloud_password_state(&self) -> CloudPasswordState {
        let cached = self
            .cache
            .lock()
            .ok()
            .filter(|c| c.cloud_password_loaded)
            .map(|c| c.cloud_password.clone());
        if let Some(st) = cached {
            return st;
        }
        self.spawn_settings_read(SettingsRead::CloudPassword, Self::load_cloud_password);
        self.cache
            .lock()
            .map(|c| c.cloud_password.clone())
            .unwrap_or_default()
    }

    fn settings_read_pending(&self, read: SettingsRead) -> bool {
        self.settings_read_pending_now(read)
    }

    fn verify_cloud_password(&self, password: &str) -> Result<(), CloudPasswordError> {
        crate::admin::verify_cloud_password(self, password)
    }

    fn set_cloud_password(
        &mut self,
        current_password: Option<&str>,
        new_password: &str,
        hint: &str,
        recovery_email: Option<&str>,
    ) -> Result<CloudPasswordState, CloudPasswordError> {
        let state = crate::admin::set_cloud_password(
            self,
            current_password,
            new_password,
            hint,
            recovery_email,
        )?;
        if let Ok(mut c) = self.cache.lock() {
            c.cloud_password_loaded = true;
            c.cloud_password = state.clone();
        }
        Ok(state)
    }

    fn confirm_recovery_email(
        &mut self,
        code: &str,
    ) -> Result<CloudPasswordState, CloudPasswordError> {
        let state = crate::admin::confirm_recovery_email(self, code)?;
        if let Ok(mut c) = self.cache.lock() {
            c.cloud_password_loaded = true;
            c.cloud_password = state.clone();
        }
        Ok(state)
    }

    fn cancel_recovery_email(&mut self) -> CloudPasswordState {
        let state = crate::admin::cancel_recovery_email(self);
        if let Ok(mut c) = self.cache.lock() {
            c.cloud_password_loaded = true;
            c.cloud_password = state.clone();
        }
        state
    }

    fn disable_cloud_password(
        &mut self,
        current_password: &str,
    ) -> Result<CloudPasswordState, CloudPasswordError> {
        let state = crate::admin::disable_cloud_password(self, current_password)?;
        if let Ok(mut c) = self.cache.lock() {
            c.cloud_password_loaded = true;
            c.cloud_password = state.clone();
        }
        Ok(state)
    }

    fn set_recovery_email(
        &mut self,
        current_password: &str,
        recovery_email: &str,
    ) -> Result<CloudPasswordState, CloudPasswordError> {
        let state = crate::admin::set_recovery_email(self, current_password, recovery_email)?;
        if let Ok(mut c) = self.cache.lock() {
            c.cloud_password_loaded = true;
            c.cloud_password = state.clone();
        }
        Ok(state)
    }

    fn resend_recovery_email(&mut self) -> Result<CloudPasswordState, CloudPasswordError> {
        let state = crate::admin::resend_recovery_email(self)?;
        if let Ok(mut c) = self.cache.lock() {
            c.cloud_password_loaded = true;
            c.cloud_password = state.clone();
        }
        Ok(state)
    }

    fn reset_cloud_password(&mut self) -> Result<CloudPasswordResetOutcome, CloudPasswordError> {
        let outcome = crate::admin::reset_cloud_password(self)?;
        match &outcome {
            CloudPasswordResetOutcome::ResetOk(state) => {
                if let Ok(mut c) = self.cache.lock() {
                    c.cloud_password_loaded = true;
                    c.cloud_password = state.clone();
                }
            }
            CloudPasswordResetOutcome::RequestedWait { until_date } => {
                if let Ok(mut c) = self.cache.lock() {
                    c.cloud_password.pending_reset_date = *until_date;
                }
            }
            CloudPasswordResetOutcome::FailedWait { .. } => {}
        }
        Ok(outcome)
    }

    fn decline_cloud_password_reset(&mut self) -> Result<CloudPasswordState, CloudPasswordError> {
        let state = crate::admin::decline_cloud_password_reset(self)?;
        if let Ok(mut c) = self.cache.lock() {
            c.cloud_password_loaded = true;
            c.cloud_password = state.clone();
        }
        Ok(state)
    }

    fn scope_notifications(&self, scope: NotifyScope) -> ScopeNotifySettings {
        self.fetch_scope_notify_if_needed(scope)
    }

    fn set_scope_notifications(&mut self, scope: NotifyScope, settings: ScopeNotifySettings) {
        crate::settings_sync::save_scope_notify_live(self, scope, settings);
    }

    fn notification_events(&self) -> NotificationEventsSettings {
        crate::wp40_polish::fetch_notification_events_live(self)
    }

    fn set_notification_events(&mut self, events: NotificationEventsSettings) {
        crate::wp40_polish::save_notification_events_live(self, events);
    }

    fn reset_all_notifications(&mut self) {
        if self
            .invoke_rpc(tl_fns::account::ResetNotifySettings {})
            .is_ok()
        {
            if let Ok(mut c) = self.cache.lock() {
                c.scope_notify_loaded.clear();
                c.notify_exceptions_loaded.clear();
                c.notify_exception_peers.clear();
                c.scope_notify.clear();
                for scope in NotifyScope::ALL {
                    c.settings_reads
                        .invalidate(SettingsRead::ScopeNotifications(scope));
                }
                c.notify_settings_version = c.notify_settings_version.wrapping_add(1);
            }
            self.write_session(crate::notify_sync::reset_session_notify_settings);
            self.bridge.state().notify_ui();
        }
    }

    fn auto_download(&self) -> AutoDownloadSettings {
        crate::settings_sync::fetch_auto_download_live(self)
    }

    fn set_auto_download(&mut self, settings: AutoDownloadSettings) {
        crate::settings_sync::save_auto_download_live(self, settings);
    }

    fn sticker_sets(&self) -> Vec<StickerSetInfo> {
        crate::chat_helpers::settings_sticker_sets(self)
    }

    fn set_sticker_set_archived(&mut self, id: u64, archived: bool) {
        crate::chat_helpers::settings_set_sticker_set_archived(self, id, archived);
    }

    fn remove_sticker_set(&mut self, id: u64) {
        crate::chat_helpers::settings_remove_sticker_set(self, id);
    }

    fn folders(&self) -> Vec<SettingsChatFolder> {
        self.ensure_filters_loaded_now();
        self.read_session(|s| {
            s.filters()
                .list()
                .iter()
                .map(chat_filter_to_settings_folder)
                .collect()
        })
    }

    fn suggested_folders(&self) -> Vec<SuggestedFolder> {
        crate::chatlists::suggested_folders(self)
    }

    fn save_folder(
        &mut self,
        mut folder: SettingsChatFolder,
    ) -> Result<SettingsChatFolder, String> {
        if !folder.is_valid() {
            return Err("Folder requires a title and at least one chat rule.".to_owned());
        }
        if folder.id <= 1 {
            let next_id = self.read_session(|s| {
                s.filters()
                    .list()
                    .iter()
                    .map(|f| f.id)
                    .max()
                    .unwrap_or(1)
                    .saturating_add(1)
                    .max(2)
            });
            folder.id = next_id;
        }
        let data_filter = settings_folder_to_data(&folder);
        let tl_filter = self.read_session(|s| data_filter_to_tl(&data_filter, s));
        let req = tl_fns::messages::UpdateDialogFilter {
            id: folder.id,
            filter: Some(tl_filter),
        };
        let _ = self.invoke_rpc(req)?;
        self.write_session(|s| s.set_filter(data_filter));
        self.bridge.state().notify_ui();
        Ok(folder)
    }

    fn delete_folder(&mut self, id: i32) -> bool {
        let req = tl_fns::messages::UpdateDialogFilter { id, filter: None };
        if self.invoke_rpc(req).is_ok() {
            self.write_session(|s| {
                let _ = s.remove_filter(id);
            });
            self.bridge.state().notify_ui();
            true
        } else {
            false
        }
    }

    fn reorder_folders(&mut self, ids: &[i32]) {
        let req = tl_fns::messages::UpdateDialogFiltersOrder {
            order: ids.to_vec(),
        };
        if self.invoke_rpc(req).is_ok() {
            self.write_session(|s| s.apply_filter_order(ids));
            self.bridge.state().notify_ui();
        }
    }

    fn accept_suggested_folder(&mut self, index: usize) -> Option<SettingsChatFolder> {
        crate::chatlists::accept_suggested_folder(self, index)
    }

    fn export_folder_invite_link(
        &mut self,
        folder_id: i32,
        title: &str,
        peers: &[i64],
    ) -> Result<String, String> {
        let peer_ids: Vec<PeerId> = peers.iter().map(|&raw| PeerId(raw)).collect();
        self.export_chatlist_invite(folder_id, title, &peer_ids)
            .map(|link| link.url)
    }

    fn folder_tags_enabled(&self) -> bool {
        self.ensure_filters_loaded_now();
        self.dialog_filter_tags_enabled()
    }

    fn set_folder_tags_enabled(&mut self, enabled: bool) -> Result<bool, String> {
        self.toggle_dialog_filter_tags(enabled)
    }

    fn folder_invite_links(
        &self,
        filter_id: i32,
    ) -> Vec<tg_app_core::settings::backend::FolderInviteLink> {
        crate::chatlists::settings_folder_invite_links(self, filter_id)
    }

    fn create_folder_invite_link(
        &mut self,
        filter_id: i32,
        title: &str,
        peers: &[i64],
    ) -> Result<tg_app_core::settings::backend::FolderInviteLink, String> {
        crate::chatlists::settings_create_folder_invite_link(self, filter_id, title, peers)
    }

    fn edit_folder_invite_link(
        &mut self,
        filter_id: i32,
        slug: &str,
        title: Option<&str>,
        peers: Option<&[i64]>,
    ) -> Result<tg_app_core::settings::backend::FolderInviteLink, String> {
        crate::chatlists::settings_edit_folder_invite_link(self, filter_id, slug, title, peers)
    }

    fn delete_folder_invite_link(&mut self, filter_id: i32, slug: &str) -> Result<bool, String> {
        crate::chatlists::settings_delete_folder_invite_link(self, filter_id, slug)
    }

    fn check_folder_updates(&mut self, filter_id: i32) -> Result<Vec<i64>, String> {
        crate::chatlists::settings_check_folder_updates(self, filter_id)
    }

    fn join_folder_updates(&mut self, filter_id: i32, peers: &[i64]) -> Result<(), String> {
        crate::chatlists::settings_join_folder_updates(self, filter_id, peers)
    }

    fn languages(&self) -> Vec<LangPackLanguageInfo> {
        self.langpack.languages()
    }

    fn active_language_code(&self) -> String {
        self.langpack.active_language_code()
    }

    fn recent_languages(&self) -> Vec<tg_lang::Language> {
        self.langpack.recent_languages()
    }

    fn apply_language(&mut self, lang_code: &str) -> Result<String, String> {
        let applied = self.langpack.apply_language(lang_code)?;
        if let Ok(mut c) = self.cache.lock() {
            c.active_language = applied.id.clone();
        }
        Ok(applied.name)
    }

    fn open_set_language_link(
        &mut self,
        url: &str,
    ) -> Result<Option<tg_app_core::settings::backend::LanguagePrompt>, String> {
        match self.langpack.handle_set_language_link(url)? {
            crate::langpack::SetLanguageLinkOutcome::OpenLanguageBox => Ok(None),
            crate::langpack::SetLanguageLinkOutcome::Prompt(prompt) => Ok(Some(*prompt)),
        }
    }

    fn confirm_language_prompt(&mut self) -> Result<Option<String>, String> {
        let Some(applied) = self.langpack.confirm_pending_prompt()? else {
            return Ok(None);
        };
        if let Ok(mut c) = self.cache.lock() {
            c.active_language = applied.id.clone();
        }
        Ok(Some(applied.name))
    }

    fn dismiss_language_prompt(&mut self) {
        self.langpack.dismiss_pending_prompt();
    }

    fn pending_language_prompt(&self) -> Option<tg_app_core::settings::backend::LanguagePrompt> {
        self.langpack.pending_prompt()
    }

    fn remove_recent_language(&mut self, lang_code: &str) {
        self.langpack.remove_recent_language(lang_code);
    }

    fn local_storage_stats(&self) -> LocalStorageStats {
        self.media_cache.stats()
    }

    fn clear_local_storage(&mut self) -> i64 {
        let freed = self.media_cache.clear();
        self.bridge.state().notify_ui();
        freed
    }

    fn set_local_storage_limits(&mut self, size_limit_bytes: i64, time_limit_days: i32) {
        self.media_cache
            .set_limits(size_limit_bytes, time_limit_days);
        self.bridge.state().notify_ui();
    }

    fn request_export(&mut self, request: &ExportRequest) -> String {
        crate::admin::request_export(self, request)
    }

    fn proxy_settings(&self) -> Option<tg_core::SettingsProxy> {
        Some(self.proxy.proxy_settings())
    }

    fn apply_proxy_settings(&mut self, settings: &tg_core::SettingsProxy) -> Result<(), String> {
        self.proxy.apply_proxy_settings(settings)
    }

    fn check_proxy(&mut self, proxy: &tg_core::ProxyData) {
        self.proxy.check_proxy(proxy);
    }

    fn proxy_check_status(&self, proxy: &tg_core::ProxyData) -> ProxyCheckStatus {
        self.proxy.proxy_check_status(proxy)
    }

    fn is_live_proxy_backend(&self) -> bool {
        self.proxy.is_live()
    }

    fn web_authorizations(&self) -> Vec<WebAuthorization> {
        crate::settings_extra::web_authorizations(self)
    }

    fn disconnect_website(&mut self, hash: u64, block_bot: bool) -> Result<bool, String> {
        crate::settings_extra::disconnect_website(self, hash, block_bot)
    }

    fn disconnect_all_websites(&mut self) -> Result<usize, String> {
        crate::settings_extra::disconnect_all_websites(self)
    }

    fn saved_ringtones(&self) -> Vec<SavedRingtone> {
        crate::settings_extra::saved_ringtones(self)
    }

    fn save_ringtone(&mut self, id: i64, unsave: bool) -> Result<(), String> {
        crate::settings_extra::save_ringtone(self, id, unsave)
    }

    fn upload_ringtone(
        &mut self,
        file_name: &str,
        mime_type: &str,
        content: &[u8],
    ) -> Result<SavedRingtone, String> {
        crate::settings_extra::upload_ringtone(self, file_name, mime_type, content)
    }

    fn passkeys(&self) -> Vec<tg_app_core::settings::backend::PasskeyEntry> {
        self.passkeys.passkeys()
    }

    fn refresh_passkeys(&mut self) {
        let _ = self.passkeys.refresh_list();
    }

    fn is_passkey_supported(&self) -> bool {
        self.passkeys.is_supported()
    }

    fn can_register_passkey(&self) -> bool {
        self.passkeys.can_register()
    }

    fn register_passkey(&mut self) -> Result<tg_app_core::settings::backend::PasskeyEntry, String> {
        self.passkeys.register_passkey(self.passkeys.window_hwnd())
    }

    fn start_passkey_hybrid_qr(&mut self) -> Result<String, String> {
        self.passkeys.start_hybrid_qr_registration()
    }

    fn passkey_hybrid_qr_status(&self) -> tg_app_core::settings::backend::PasskeyHybridQrStatus {
        self.passkeys.hybrid_qr_status()
    }

    fn cancel_passkey_hybrid_qr(&mut self) {
        self.passkeys.cancel_hybrid_qr();
    }

    fn delete_passkey(&mut self, id: &str) -> Result<(), String> {
        self.passkeys.delete_passkey(id)
    }

    fn download_and_switch_emoji_set(
        &mut self,
        manager: &mut tg_app_core::core_types::emoji_sets::EmojiSetsManager,
        set_id: i32,
    ) -> Result<usize, String> {
        let res = manager.load_and_switch_to(self, set_id);
        self.bridge_state().notify_ui();
        res
    }

    fn fetch_faq_suggestions(
        &self,
        faq_url: &str,
    ) -> Result<Vec<tg_app_core::settings::backend::FaqEntry>, String> {
        crate::wp52_ux_extras::fetch_faq_suggestions(self, faq_url)
    }

    fn leave_folder_suggestions(&self, filter_id: i32) -> Result<Vec<i64>, String> {
        crate::chatlists::settings_leave_folder_suggestions(self, filter_id)
    }

    fn confirm_bot_connection(&mut self, bot_id: i64) -> Result<bool, String> {
        crate::profile_extras::confirm_bot_connection(self, bot_id)
    }

    fn get_support_contact(
        &self,
    ) -> Result<tg_app_core::settings::backend::SupportContact, String> {
        crate::profile_extras::get_support_contact(self)
    }

    fn set_blocked_peers(
        &mut self,
        peer_ids: &[i64],
        my_stories_from: bool,
    ) -> Result<bool, String> {
        let data_peers: Vec<DataPeerId> = peer_ids
            .iter()
            .map(|&raw| to_data_peer(PeerId(raw)))
            .collect();
        crate::contacts_extras::set_blocked_peers(self, &data_peers, my_stories_from)
    }
}

// ============================================================================
// 6. media::backend::MediaBackend
// ============================================================================

impl MediaBackend for LiveSession {
    fn get_item(&self, id: MediaItemId) -> Option<ViewerMediaItem> {
        self.read_session(|s| {
            for pid in known_peer_ids(s) {
                for msg in history_messages_ordered(s, pid) {
                    if let Some(item) = message_to_viewer_media_item(s, msg)
                        && item.id == id
                    {
                        return Some(item);
                    }
                }
            }
            None
        })
    }

    fn siblings_in_chat(&self, peer_id: i64, _item_id: MediaItemId) -> Vec<ViewerMediaItem> {
        let data_peer = DataPeerId(peer_id);
        self.read_session(|s| {
            history_messages_ordered(s, data_peer)
                .into_iter()
                .filter_map(|m| message_to_viewer_media_item(s, m))
                .collect()
        })
    }

    fn load_bytes(&self, id: MediaItemId) -> anyhow::Result<Vec<u8>> {
        let (_, _, resolved) = self
            .resolve_media_by_id(id)
            .ok_or_else(|| anyhow!("media item {} is not loaded in session", id.0))?;
        let (cache_key, tag) = match &resolved.location {
            tl_enums::InputFileLocation::InputPhotoFileLocation(loc) => (
                Some(photo_cache_key(
                    resolved.dc_id,
                    loc.id as u64,
                    &loc.thumb_size,
                )),
                tg_storage::cache::media_cache::MediaCacheTag::Photo,
            ),
            tl_enums::InputFileLocation::InputDocumentFileLocation(loc) => (
                Some(if loc.thumb_size.is_empty() {
                    document_cache_key(resolved.dc_id, loc.id as u64)
                } else {
                    document_thumb_cache_key(resolved.dc_id, loc.id as u64, &loc.thumb_size)
                }),
                tg_storage::cache::media_cache::MediaCacheTag::Document,
            ),
            _ => (None, tg_storage::cache::media_cache::MediaCacheTag::Other),
        };
        if let Some(ref key) = cache_key
            && let Some(cached) = self.media_cache.get_blob(key)
        {
            return Ok(cached);
        }
        let bytes = download_file_bytes(
            self.runtime.rpc(),
            self.runtime.tokio_handle(),
            resolved.dc_id,
            resolved.location,
            (resolved.size > 0).then_some(resolved.size),
        )?;
        if let Some(ref key) = cache_key {
            let _ = self.media_cache.put_blob(key, tag, &bytes);
        }
        Ok(bytes)
    }

    fn open_range_loader(&self, id: MediaItemId) -> anyhow::Result<Arc<dyn RangeLoader>> {
        let (peer_id, msg_id, resolved) = self
            .resolve_media_by_id(id)
            .ok_or_else(|| anyhow!("media item {} is not loaded in session", id.0))?;
        let origin = tg_media::transfer::FileOrigin {
            peer_id: Some(peer_id.0),
            msg_id: Some(msg_id.0 as i32),
            sticker_set_id: None,
        };
        let mut loader = TelegramRangeLoader::new(
            self.runtime.rpc().clone(),
            self.runtime.tokio_handle().clone(),
            resolved,
        )
        .with_refresh_origin(Arc::clone(self.runtime.session()), origin);
        if self.media_cache.is_persistent() {
            let streamed_dir = self.media_cache.root_dir().join("streamed");
            loader = loader.with_storage_dir(streamed_dir);
        }
        Ok(Arc::new(loader))
    }

    fn save_to_disk(&self, id: MediaItemId, destination: &Path) -> anyhow::Result<PathBuf> {
        let bytes = self.load_bytes(id)?;
        let saved = tg_platform::file_safety::write_unique_file_at_destination(destination, &bytes)
            .map_err(|err| anyhow!("{err}"))?;
        mark_downloaded_file_zone_identifier(&saved);
        Ok(saved)
    }

    fn copy_item(&self, id: MediaItemId) -> anyhow::Result<()> {
        crate::upload::copy_media_item_to_clipboard(self, id)
    }

    fn forward_item(&self, id: MediaItemId) -> anyhow::Result<()> {
        crate::upload::forward_media_item(self, id, None)
    }

    fn forward_item_to(&self, id: MediaItemId, to_peer: i64) -> anyhow::Result<()> {
        crate::upload::forward_media_item(self, id, Some(PeerId(to_peer)))
    }

    fn delete_item(&self, id: MediaItemId) -> anyhow::Result<()> {
        let (peer_id, msg_id, _) = self
            .resolve_media_by_id(id)
            .ok_or_else(|| anyhow!("media item {} is not loaded in session", id.0))?;
        let mut handle = self.clone();
        HistoryBackend::delete_messages(
            &mut handle,
            from_data_peer(peer_id),
            &[from_data_msg(msg_id)],
            true,
        )
        .map_err(|e| anyhow!("{e}"))
    }

    fn show_in_chat(&self, peer_id: i64, msg_id: i32) {
        if let Ok(mut c) = self.cache.lock() {
            c.pending_chat_opens.push_back(PendingChatOpen {
                peer_id: PeerId(peer_id),
                msg_id: MsgId(msg_id),
                draft: None,
            });
        }
        self.bridge.state().notify_ui();
    }

    fn open_in_external_app(&self, id: MediaItemId) -> anyhow::Result<PathBuf> {
        let item = self
            .get_item(id)
            .ok_or_else(|| anyhow!("media item {} not found", id.0))?;
        let raw_name = if item.file_name.is_empty() {
            format!("rustgram_media_{}", id.0)
        } else {
            item.file_name
        };
        let temp_dir = self.require_external_open_temp_dir()?;
        let bytes = self.load_bytes(id)?;
        let saved =
            tg_platform::file_safety::write_unique_file_in_dir(&temp_dir, &raw_name, &bytes)
                .map_err(|err| anyhow!("{err}"))?;
        mark_downloaded_file_zone_identifier(&saved);
        crate::media::launch_external_file(&saved)?;
        Ok(saved)
    }

    fn open_in_external_app_confirmed(&self, id: MediaItemId) -> anyhow::Result<PathBuf> {
        let item = self
            .get_item(id)
            .ok_or_else(|| anyhow!("media item {} not found", id.0))?;
        let raw_name = if item.file_name.is_empty() {
            format!("rustgram_media_{}", id.0)
        } else {
            item.file_name
        };
        let temp_dir = self.require_external_open_temp_dir()?;
        let bytes = self.load_bytes(id)?;
        let saved =
            tg_platform::file_safety::write_unique_file_in_dir(&temp_dir, &raw_name, &bytes)
                .map_err(|err| anyhow!("{err}"))?;
        mark_downloaded_file_zone_identifier(&saved);
        crate::media::launch_external_file_confirmed(&saved)?;
        Ok(saved)
    }

    fn audio_track_metadata(&self, id: AudioMsgId) -> Option<AudioTrackMetadata> {
        self.read_session(|s| {
            let msg = s.message(id.context_id)?;
            message_to_audio_track_metadata(s, msg)
        })
    }

    fn audio_track_cover(&self, id: AudioMsgId) -> Option<Vec<u8>> {
        let loc = self.read_session(|s| {
            let msg = s.message(id.context_id)?;
            let MessageMedia::Document(doc) = msg.media()? else {
                return None;
            };
            crate::media::document_thumb_location(doc)
        })?;
        download_file_bytes(
            self.runtime.rpc(),
            self.runtime.tokio_handle(),
            loc.dc_id,
            loc.location,
            Some(loc.size),
        )
        .ok()
        .filter(|b| !b.is_empty())
    }

    fn report_music_listen(
        &self,
        track_id: AudioMsgId,
        duration_secs: i32,
    ) -> Result<bool, String> {
        Ok(crate::media::spawn_report_music_listen(
            self.runtime.rpc(),
            self.runtime.tokio_handle(),
            self.runtime.session(),
            track_id,
            duration_secs,
        ))
    }

    fn audio_data_source(&self) -> Arc<dyn AudioDataSource> {
        Arc::new(LiveAudioDataSource::new(
            self.runtime.rpc().clone(),
            self.runtime.tokio_handle().clone(),
            Arc::clone(self.runtime.session()),
        ))
    }

    fn playlist_provider(&self) -> Arc<dyn PlaylistProvider> {
        Arc::new(LivePlaylistProvider::with_runtime(
            Arc::clone(&self.runtime),
            Some(Arc::clone(self.bridge.state())),
        ))
    }
}

// ============================================================================
// 7. chat_helpers::backend::ChatHelpersBackend -> implemented in live/chat_helpers.rs
// ============================================================================

// ============================================================================
// 8. notifications::backend::NotifyBackend
// ============================================================================

impl NotifyBackend for LiveSession {
    fn poll_events(&mut self) -> Vec<IncomingNotifyEvent> {
        self.bridge.state().drain_notify_events()
    }

    fn mark_read(
        &mut self,
        _session_id: u64,
        peer_id: PeerId,
        _topic_root_id: MsgId,
        max_msg_id: MsgId,
    ) {
        HistoryBackend::mark_history_read(self, peer_id, max_msg_id);
    }

    fn send_quick_reply(
        &mut self,
        _session_id: u64,
        peer_id: PeerId,
        _topic_root_id: MsgId,
        reply_to_msg_id: MsgId,
        text: String,
    ) {
        let reply_to = (reply_to_msg_id.0 > 0).then_some(reply_to_msg_id);
        let _ = HistoryBackend::send_message(
            self,
            peer_id,
            SendMessageRequest {
                text: TextWithEntities::simple(text),
                reply_to,
                silent: false,
            },
        );
    }

    fn open_chat(
        &mut self,
        _session_id: u64,
        peer_id: PeerId,
        _topic_root_id: MsgId,
        msg_id: MsgId,
        draft: Option<String>,
    ) {
        if let Ok(mut c) = self.cache.lock() {
            c.pending_chat_opens.push_back(PendingChatOpen {
                peer_id,
                msg_id,
                draft,
            });
        }
        self.bridge.state().notify_ui();
    }

    fn unread_state(&self) -> UnreadBadgeState {
        let now = now_unix_i32();
        self.read_session(|s| {
            let mut state = UnreadBadgeState::default();
            for &key in s.main_dialogs() {
                let DialogKey::History(pid) = key else {
                    continue;
                };
                let Some(h) = s.history(pid) else {
                    continue;
                };
                let count = h.unread_count.unwrap_or(0).max(0) as u32;
                let muted = h.muted || s.peer(pid).is_some_and(|p| p.common().notify.is_muted(now));
                if count > 0 {
                    state.messages = state.messages.saturating_add(count);
                    state.chats = state.chats.saturating_add(1);
                    if muted {
                        state.messages_muted = state.messages_muted.saturating_add(count);
                        state.chats_muted = state.chats_muted.saturating_add(1);
                    }
                } else if h.unread_mark {
                    state.marks = state.marks.saturating_add(1);
                    if muted {
                        state.marks_muted = state.marks_muted.saturating_add(1);
                    }
                }
                if h.unread_mentions_count > 0 {
                    state.mentions = state
                        .mentions
                        .saturating_add(h.unread_mentions_count as u32);
                }
            }
            state
        })
    }

    fn peer_info(&self, peer_id: PeerId) -> Option<NotifyPeerInfo> {
        self.read_session(|s| peer_to_notify_info(s, to_data_peer(peer_id)))
    }

    fn notify_settings_versions(&self) -> Vec<(u64, u64)> {
        crate::notify_sync::request_notify_defaults(self);
        crate::notify_sync::request_wanted_peer_settings(self);
        vec![(
            self.session_id,
            crate::notify_sync::notify_settings_epoch(self),
        )]
    }

    fn sync_notify_settings(
        &self,
        session_id: u64,
        store: &mut tg_app_core::notifications::settings::NotifySettingsStore,
        now_unix: i64,
    ) {
        if session_id == self.session_id {
            crate::notify_sync::sync_store(self, store, now_unix);
        }
    }
}

/// Per-live-session pending Mini App URL overrides keyed by `(session_ptr, bot_bare_id)`.
fn live_pending_mini_app_urls() -> &'static Mutex<HashMap<(usize, u64), String>> {
    static MAP: std::sync::OnceLock<Mutex<HashMap<(usize, u64), String>>> =
        std::sync::OnceLock::new();
    MAP.get_or_init(|| Mutex::new(HashMap::new()))
}

impl tg_app_core::session_api::AppSession for LiveSession {
    fn prepare_open_chat(&self, peer_id: PeerId) {
        crate::open_chat_prefetch::start(self, to_data_peer(peer_id));
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn sync_cached_user(&mut self) {
        if Backend::current_user(self).is_none() {
            self.refresh_cached_views();
        }
    }

    fn resolve_username(&self, username: &str) -> Option<PeerId> {
        let cleaned = username.trim().trim_start_matches('@');
        if cleaned.is_empty() {
            return None;
        }
        // The session's username index (TGB-2938) instead of building every peer's profile.
        self.read_session(|s| s.peer_by_username(cleaned))
            .map(from_data_peer)
    }

    fn is_forum_peer(&self, peer_id: PeerId) -> bool {
        InfoBackend::peer_profile(self, peer_id).is_some_and(|p| p.is_forum)
    }

    fn resolve_media_item_id(
        &self,
        peer_id: PeerId,
        msg_id: MsgId,
        _album_index: Option<usize>,
    ) -> Option<MediaItemId> {
        MediaBackend::siblings_in_chat(self, peer_id.0, MediaItemId(0))
            .into_iter()
            .find(|it| it.msg_id == msg_id.0)
            .map(|it| it.id)
    }

    fn resolve_audio_msg_id(&self, peer_id: PeerId, msg_id: MsgId) -> Option<AudioMsgId> {
        let item = MediaBackend::siblings_in_chat(self, peer_id.0, MediaItemId(0))
            .into_iter()
            .find(|it| it.msg_id == msg_id.0)?;
        let candidate = AudioMsgId::new(
            tg_audio::DocumentId(item.id.0),
            tg_audio::FullMsgId::new(
                tg_audio::PeerId(peer_id.0),
                tg_audio::MsgId(i64::from(msg_id.0)),
            ),
            tg_audio::MediaKind::AudioFile,
            item.duration_ms.max(1_000),
            0,
        );
        if MediaBackend::audio_track_metadata(self, candidate).is_some() {
            Some(candidate)
        } else {
            None
        }
    }

    fn set_pending_mini_app_url(&mut self, bot_id: PeerId, url: Option<String>) {
        let key = (
            Arc::as_ptr(self.runtime().session()) as usize,
            bot_id.bare(),
        );
        if let Ok(mut guard) = live_pending_mini_app_urls().lock() {
            if let Some(u) = url {
                guard.insert(key, u);
            } else {
                guard.remove(&key);
            }
        }
    }

    fn take_pending_mini_app_url(&mut self, bot_id: PeerId) -> Option<String> {
        let key = (
            Arc::as_ptr(self.runtime().session()) as usize,
            bot_id.bare(),
        );
        live_pending_mini_app_urls()
            .lock()
            .ok()
            .and_then(|mut guard| guard.remove(&key))
    }

    fn is_mini_app_bot_trusted(&self, bot_id: PeerId) -> bool {
        self.cache
            .lock()
            .ok()
            .is_some_and(|c| c.trusted_mini_app_bots.contains(&bot_id.0))
    }

    fn trust_mini_app_bot(&mut self, bot_id: PeerId) {
        if let Ok(mut c) = self.cache.lock()
            && c.trusted_mini_app_bots.insert(bot_id.0)
        {
            c.session_settings_dirty = true;
        }
    }

    fn take_pending_chat_opens(&self) -> Vec<PendingChatOpen> {
        self.drain_pending_chat_opens()
            .into_iter()
            .map(|open| PendingChatOpen {
                peer_id: open.peer_id,
                msg_id: open.msg_id,
                draft: open.draft,
            })
            .collect()
    }

    fn set_stats_target_peer(&self, peer_id: PeerId) {
        LiveSession::set_stats_target_peer(self, peer_id);
    }

    fn send_helper_message(
        &mut self,
        peer_id: PeerId,
        text: String,
    ) -> Result<HistoryMessage, String> {
        HistoryBackend::send_message(
            self,
            peer_id,
            SendMessageRequest {
                text: TextWithEntities::simple(text),
                reply_to: None,
                silent: false,
            },
        )
    }

    fn send_sticker(
        &mut self,
        peer_id: PeerId,
        document_id: tg_chat::DocumentId,
        reply_to: Option<MsgId>,
        silent: bool,
    ) -> Result<HistoryMessage, String> {
        self.send_sticker_document(peer_id, document_id, reply_to, silent)
    }

    fn send_gif(
        &mut self,
        peer_id: PeerId,
        document_id: tg_chat::DocumentId,
        _title: &str,
        reply_to: Option<MsgId>,
        silent: bool,
    ) -> Result<HistoryMessage, String> {
        self.send_gif_document(peer_id, document_id, reply_to, silent)
    }

    fn resolve_export_peer(
        &self,
        peer_id: PeerId,
    ) -> Option<(tg_export::stand_in::InputPeer, String)> {
        LiveSession::resolve_export_peer(self, peer_id)
    }

    fn install_sticker_set_preview(
        &mut self,
        set_id: u64,
        short_name: &str,
    ) -> Result<bool, String> {
        self.install_sticker_set_by_id_or_slug(set_id, short_name)
    }

    fn remove_sticker_set_preview(
        &mut self,
        set_id: u64,
        short_name: &str,
    ) -> Result<bool, String> {
        self.remove_sticker_set_by_id_or_slug(set_id, short_name)
    }

    fn rename_sticker_set_preview(
        &mut self,
        set_id: u64,
        short_name: &str,
        new_title: &str,
    ) -> Result<Option<tg_app_core::session_api::StickerSetPreview>, String> {
        self.rename_sticker_set_owner(set_id, short_name, new_title)
    }

    fn change_sticker_position_preview(
        &mut self,
        set_id: u64,
        short_name: &str,
        document_id: u64,
        new_position: usize,
    ) -> Result<Option<tg_app_core::session_api::StickerSetPreview>, String> {
        self.change_sticker_position_owner(set_id, short_name, document_id, new_position)
    }

    fn remove_sticker_from_set_preview(
        &mut self,
        set_id: u64,
        short_name: &str,
        document_id: u64,
    ) -> Result<Option<tg_app_core::session_api::StickerSetPreview>, String> {
        self.remove_sticker_from_set_owner(set_id, short_name, document_id)
    }

    fn delete_sticker_set_preview(&mut self, set_id: u64, short_name: &str) -> Result<(), String> {
        self.delete_sticker_set_owner(set_id, short_name)
    }

    fn create_settings_backend(&self) -> Box<dyn SettingsBackend> {
        Box::new(self.clone())
    }

    fn create_stats_backend(&self) -> Box<dyn tg_app_core::statistics::backend::StatsBackend> {
        Box::new(self.clone())
    }

    fn create_notify_backend(&self) -> Box<dyn NotifyBackend> {
        Box::new(self.clone())
    }

    fn create_webview_backend(&self) -> Box<dyn tg_app_core::webview::backend::WebViewBackend> {
        Box::new(self.clone())
    }

    fn create_calls_backend(&self) -> Box<dyn tg_app_core::calls::backend::CallsBackend> {
        Box::new(self.clone())
    }

    fn create_stories_backend(&self) -> Box<dyn tg_app_core::stories::backend::StoriesBackend> {
        Box::new(self.clone())
    }

    fn create_business_backend(&self) -> Box<dyn tg_app_core::business::backend::BusinessBackend> {
        Box::new(self.clone())
    }

    fn create_payments_backend(&self) -> Box<dyn tg_app_core::payments::backend::PaymentsBackend> {
        Box::new(self.clone())
    }

    fn create_passport_backend(&self) -> Box<dyn tg_app_core::payments::backend::PassportBackend> {
        Box::new(self.clone())
    }

    fn is_live_session(&self) -> bool {
        true
    }

    fn resolve_username_remote(
        &mut self,
        username: &str,
        referer: Option<&str>,
    ) -> Result<PeerId, String> {
        self.deep_link_resolve_username(username, referer)
    }

    fn resolve_phone_remote(&mut self, phone: &str) -> Result<PeerId, String> {
        self.deep_link_resolve_phone(phone)
    }

    fn resolve_private_channel_peer(&self, raw_channel_id: u64) -> Option<PeerId> {
        self.deep_link_resolve_private_channel(raw_channel_id)
    }

    fn check_chat_invite(
        &mut self,
        hash: &str,
    ) -> Result<tg_app_core::session_api::ChatInvitePreview, String> {
        self.deep_link_check_chat_invite(hash)
    }

    fn import_chat_invite(&mut self, hash: &str) -> Result<PeerId, String> {
        self.deep_link_import_chat_invite(hash)
    }

    fn check_chatlist_invite(
        &mut self,
        slug: &str,
    ) -> Result<tg_app_core::session_api::ChatlistInvitePreview, String> {
        self.deep_link_check_chatlist_invite(slug)
    }

    fn join_chatlist_invite(&mut self, slug: &str, peers: &[PeerId]) -> Result<usize, String> {
        self.deep_link_join_chatlist_invite(slug, peers)
    }

    fn resolve_sticker_set(
        &mut self,
        short_name: &str,
        is_emoji: bool,
    ) -> Result<tg_app_core::session_api::StickerSetPreview, String> {
        self.deep_link_resolve_sticker_set(short_name, is_emoji)
    }

    fn resolve_sticker_set_by_id(
        &mut self,
        set_id: u64,
        access_hash: u64,
    ) -> Result<tg_app_core::session_api::StickerSetPreview, String> {
        self.deep_link_resolve_sticker_set_by_id(set_id, access_hash)
    }

    fn resolve_cloud_theme(
        &mut self,
        slug: &str,
    ) -> Result<tg_app_core::session_api::CloudThemePreview, String> {
        self.deep_link_resolve_cloud_theme(slug)
    }

    fn resolve_wallpaper(
        &mut self,
        slug: &str,
    ) -> Result<tg_app_core::session_api::WallPaperPreview, String> {
        self.deep_link_resolve_wallpaper(slug)
    }

    fn send_confirm_phone_code(&mut self, hash: &str) -> Result<String, String> {
        self.deep_link_send_confirm_phone_code(hash)
    }

    fn confirm_phone_code(
        &mut self,
        phone_code_hash: &str,
        phone_code: &str,
    ) -> Result<bool, String> {
        self.deep_link_confirm_phone(phone_code_hash, phone_code)
    }

    fn check_gift_code(
        &mut self,
        slug: &str,
    ) -> Result<tg_app_core::session_api::GiftCodePreview, String> {
        self.deep_link_check_gift_code(slug)
    }

    fn apply_gift_code(&mut self, slug: &str) -> Result<(), String> {
        self.deep_link_apply_gift_code(slug)
    }

    fn get_bank_card_data(
        &mut self,
        number: &str,
    ) -> Result<tg_app_core::session_api::BankCardInfo, String> {
        self.deep_link_get_bank_card_data(number)
    }

    fn resolve_unique_gift(
        &mut self,
        slug: &str,
    ) -> Result<tg_app_core::session_api::UniqueGiftPreview, String> {
        self.deep_link_resolve_unique_gift(slug)
    }

    fn resolve_business_chat_link(&mut self, slug: &str) -> Result<(PeerId, String), String> {
        self.deep_link_resolve_business_chat_link(slug)
    }

    fn request_deep_link_info(
        &mut self,
        path: &str,
    ) -> Result<Option<tg_app_core::session_api::DeepLinkInfoResult>, String> {
        self.deep_link_request_info(path)
    }
}

/// Marks a downloaded file on Windows NTFS with Mark-of-the-Web (`:Zone.Identifier` ADS with
/// `ZoneId=3`, Internet Zone), porting tdesktop's `Platform::File::PostprocessDownloaded`
/// (`Telegram/SourceFiles/platform/win/file_utilities_win.cpp:305-326`).
///
/// Silently ignores filesystems that do not support Alternate Data Streams (e.g. FAT32/exFAT).
pub fn mark_downloaded_file_zone_identifier(path: &Path) {
    #[cfg(windows)]
    {
        let mut ads = path.as_os_str().to_os_string();
        ads.push(":Zone.Identifier");
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(PathBuf::from(ads))
        {
            use std::io::Write as _;
            let _ = file.write_all(b"[ZoneTransfer]\r\nZoneId=3\r\n");
        }
    }
    #[cfg(not(windows))]
    {
        let _ = path;
    }
}

impl LiveSession {
    /// Applies the side effects of finished background sends and returns the history updates
    /// of those in `peer_id` (TGB-2931). Sends in other chats show up when those chats load.
    fn send_resolution_updates(
        &mut self,
        peer_id: PeerId,
        resolutions: &[crate::optimistic_send::SendResolution],
    ) -> Vec<HistoryLiveUpdate> {
        for resolution in resolutions {
            match &resolution.outcome {
                Ok(outcome) => {
                    for upd in &outcome.updates {
                        self.apply_updates_side_caches(upd);
                    }
                }
                Err(err) => {
                    self.apply_slowmode_wait_from_error(from_data_peer(resolution.peer), err);
                }
            }
        }
        let data_peer = to_data_peer(peer_id);
        let mut updates = self.read_session(|s| {
            resolutions
                .iter()
                .flat_map(|r| crate::optimistic_send::resolution_updates(s, data_peer, r))
                .collect::<Vec<_>>()
        });
        let mut sent: Vec<HistoryMessage> = updates
            .iter()
            .filter_map(|u| match u {
                HistoryLiveUpdate::LocalMessageSent { message, .. } => Some((**message).clone()),
                _ => None,
            })
            .collect();
        if !sent.is_empty() {
            self.decorate_live_messages(peer_id, &mut sent);
            let mut decorated = sent.into_iter();
            for upd in &mut updates {
                if let HistoryLiveUpdate::LocalMessageSent { message, .. } = upd
                    && let Some(m) = decorated.next()
                {
                    **message = m;
                }
            }
        }
        updates
    }

    /// Adds the caches a live-arrived message shows with (translations, rich messages, widgets,
    /// dice, reply targets).
    fn decorate_live_messages(&mut self, peer_id: PeerId, batch: &mut [HistoryMessage]) {
        self.decorate_cached_translations(peer_id, batch);
        crate::rich_message::attach_cached_rich_messages(self.runtime.session(), batch);
        crate::widgets::attach_cached_widget_states(self.runtime.session(), batch);
        crate::dice_pack::attach_and_prefetch_dice_lottie(self, batch);
        crate::reply_targets::attach_reply_targets(self, batch);
        crate::custom_emoji_loader::request_for_messages(self, batch);
    }

    fn drain_history_delta(&mut self, peer_id: PeerId) -> Vec<HistoryLiveUpdate> {
        let data_peer = to_data_peer(peer_id);
        let Some(delta) = self.bridge.state().take_history_delta(data_peer) else {
            return Vec::new();
        };
        if delta.reload_required {
            return vec![HistoryLiveUpdate::ReloadRequired];
        }

        let media_cache = self.media_cache.clone();
        let (mut added_msgs, mut edited_msgs, outbox_max, unread_state) = self.read_session(|s| {
            let resolve = |doc: &tg_data::DocumentData| media_cache.cached_lottie_json_for_doc(doc);
            let added: Vec<HistoryMessage> = delta
                .added
                .iter()
                .filter_map(|&mid| {
                    s.message(DataFullMsgId::new(data_peer, mid))
                        .map(|m| message_to_history_message_with_stickers(s, m, &resolve))
                })
                .collect();
            let edited: Vec<HistoryMessage> = delta
                .edited
                .iter()
                .filter_map(|&mid| {
                    s.message(DataFullMsgId::new(data_peer, mid))
                        .map(|m| message_to_history_message_with_stickers(s, m, &resolve))
                })
                .collect();
            let outbox = if delta.outbox_read_changed {
                s.history(data_peer)
                    .map(|h| from_data_msg(h.outbox_read_till_id()))
                    .filter(|id| id.0 > 0)
            } else {
                None
            };
            let unread = if delta.unread_changed {
                s.history(data_peer).map(|h| {
                    (
                        h.unread_count.unwrap_or(0).max(0) as u32,
                        from_data_msg(h.inbox_read_till_id()),
                    )
                })
            } else {
                None
            };
            (added, edited, outbox, unread)
        });

        for batch in [&mut added_msgs, &mut edited_msgs] {
            if !batch.is_empty() {
                self.decorate_live_messages(peer_id, batch);
            }
        }

        let mut out = Vec::new();
        if !delta.deleted.is_empty() {
            let ids: Vec<MsgId> = delta.deleted.into_iter().map(from_data_msg).collect();
            out.push(HistoryLiveUpdate::MessagesDeleted(ids));
        }
        for msg in added_msgs {
            out.push(HistoryLiveUpdate::MessageAdded(Box::new(msg)));
        }
        for msg in edited_msgs {
            out.push(HistoryLiveUpdate::MessageEdited(Box::new(msg)));
        }
        if let Some(max_id) = outbox_max {
            out.push(HistoryLiveUpdate::OutboxRead { max_id });
        }
        if let Some((unread_count, inbox_read_till)) = unread_state {
            out.push(HistoryLiveUpdate::UnreadChanged {
                unread_count,
                inbox_read_till,
            });
        }
        out
    }
}

#[cfg(test)]
mod sec2b_session_tests {
    use super::*;
    use tg_api::{DcId, InvocationError, RpcClient};
    use tg_data::{
        Message, MessageContent, MessageFlags, MessageMedia, MsgId as DataMsgId,
        PeerId as DataPeerId, PhotoData, PhotoSize, TextWithEntities as DataText,
        UserId as DataUserId,
    };
    use tg_session::{SessionRuntime, SessionRuntimeConfig};
    use tg_storage::cache::media_cache::MediaCacheTag;
    use tg_tl::RemoteCall;

    #[derive(Clone, Default)]
    struct NoopRpcClient;

    impl RpcClient for NoopRpcClient {
        type Error = InvocationError;

        async fn invoke<R: RemoteCall + Send + Sync>(
            &self,
            _req: &R,
        ) -> Result<R::Return, Self::Error> {
            Err(InvocationError::Dropped)
        }

        async fn invoke_in_dc<R: RemoteCall + Send + Sync>(
            &self,
            _dc_id: DcId,
            _req: &R,
        ) -> Result<R::Return, Self::Error> {
            Err(InvocationError::Dropped)
        }

        fn main_dc_id(&self) -> DcId {
            2
        }

        fn set_main_dc_id(&self, _dc_id: DcId) {}
    }

    fn make_test_session_with_cached_photo() -> (tempfile::TempDir, LiveSession, MediaItemId) {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let cfg = SessionRuntimeConfig {
            init_updates_state: false,
            run_get_difference_on_start: false,
            load_initial_dialogs: false,
            fetch_self_user_on_start: false,
            ..SessionRuntimeConfig::default()
        };
        let rt = Arc::new(
            SessionRuntime::start(DataUserId(100), ErasedRpcClient::new(NoopRpcClient), cfg)
                .expect("start runtime"),
        );
        let live = LiveSession::new(Arc::clone(&rt), 100);
        live.set_media_cache_dir(dir.path().join("media_cache"))
            .expect("set media cache");

        let photo_id = 55_501u64;
        let photo = PhotoData {
            id: tg_data::PhotoId(photo_id),
            access_hash: 1,
            file_reference: vec![1, 2, 3],
            date: 1_700_000_000,
            dc_id: 2,
            sizes: vec![PhotoSize {
                kind: "y".to_owned(),
                width: 64,
                height: 64,
                size: 21,
            }],
            ..PhotoData::default()
        };
        let cache_key = photo_cache_key(2, photo_id, "y");
        live.media_cache()
            .put_blob(&cache_key, MediaCacheTag::Photo, b"SAVED_PHOTO_BYTES_123")
            .expect("seed cache");

        let peer = DataPeerId::from_user(DataUserId(200));
        let mut msg = Message::new_text(
            peer,
            DataMsgId(77),
            Some(peer),
            1_700_000_000,
            DataText::plain(String::new()),
        );
        msg.flags = MessageFlags::empty();
        msg.content = MessageContent::Regular {
            text: DataText::plain(String::new()),
            media: MessageMedia::Photo(Box::new(photo)),
        };
        let session_arc = Arc::clone(rt.session());
        rt.tokio_handle().block_on(async move {
            session_arc.write().await.insert_message(msg);
        });

        let item_id = MediaItemId(photo_id);
        (dir, live, item_id)
    }

    #[test]
    fn d4_save_to_disk_writes_zone_identifier_mark_of_the_web() {
        let (dir, live, item_id) = make_test_session_with_cached_photo();
        let dest = dir.path().join("downloads").join("photo.jpg");

        let saved = MediaBackend::save_to_disk(&live, item_id, &dest).expect("save_to_disk");
        assert_eq!(
            std::fs::read(&saved).expect("read saved"),
            b"SAVED_PHOTO_BYTES_123"
        );

        #[cfg(windows)]
        {
            let mut ads = saved.as_os_str().to_os_string();
            ads.push(":Zone.Identifier");
            let zone_content = std::fs::read_to_string(PathBuf::from(ads))
                .expect("Zone.Identifier ADS must exist");
            assert!(
                zone_content.contains("[ZoneTransfer]") && zone_content.contains("ZoneId=3"),
                "saved file must carry Mark-of-the-Web ZoneId=3, got: {zone_content:?}"
            );
        }
    }

    #[test]
    fn d3_external_open_temp_dir_lives_under_account_folder_and_cleans_up() {
        let (dir, live, _item_id) = make_test_session_with_cached_photo();
        let acc_temp = dir.path().join("tdata").join("temp_data");
        live.set_account_temp_dir(&acc_temp);

        let ext_dir = live
            .external_open_temp_dir()
            .expect("account temp dir is attached");
        assert_eq!(ext_dir, acc_temp.join("external"));
        assert!(
            !ext_dir.starts_with(std::env::temp_dir().join("rustgram_external")),
            "external temp copy must live under account folder, not %TEMP%\\rustgram_external"
        );

        std::fs::create_dir_all(&ext_dir).expect("mkdir");
        let stale = ext_dir.join("opened.pdf");
        std::fs::write(&stale, b"temp pdf").expect("write");
        let busy = ext_dir.join("still_open.pdf");
        std::fs::write(&busy, b"busy pdf").expect("write busy");
        assert!(stale.is_file());

        #[cfg(windows)]
        let busy_guard = {
            use std::os::windows::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .share_mode(0)
                .open(&busy)
                .expect("lock busy file")
        };

        live.cleanup_account_temp_files();
        assert!(
            !stale.exists(),
            "cleanup_account_temp_files must delete unlocked external temp copies"
        );
        #[cfg(windows)]
        {
            assert!(
                busy.exists(),
                "cleanup must skip files currently held open by an external app"
            );
            drop(busy_guard);
            live.cleanup_account_temp_files();
            assert!(!busy.exists(), "once closed, temp copy must be cleaned up");
        }
    }

    #[test]
    fn sec3_b1_external_open_has_no_temp_dir_without_account_folder() {
        let (_dir, live, _item_id) = make_test_session_with_cached_photo();
        // Back to the state before an account folder is attached: memory-only cache, no temp dir.
        live.media_cache().close_to_memory();
        assert!(live.account_temp_dir().is_none());
        assert!(!live.media_cache().is_persistent());

        assert_eq!(
            live.external_open_temp_dir(),
            None,
            "without an account folder there is no place for plaintext temp copies"
        );
        let err = live
            .require_external_open_temp_dir()
            .expect_err("opening in an external app must be refused");
        assert!(err.to_string().contains("no account folder"));
        // Nothing to clean, and nothing must be created by the cleanup either.
        live.cleanup_account_temp_files();
    }
}
