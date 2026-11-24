//! TL-backed [`ExportSource`] implementation over a generic async RPC client.
//!
//! Port of `Telegram/SourceFiles/export/export_api_wrap.h` and `export_api_wrap.cpp`:
//! - Takeout session lifecycle (`users.getUsers([inputUserSelf])`, `account.initTakeoutSession`
//!   with flags derived from `Settings`, `invokeWithTakeout` wrapping, `account.finishTakeoutSession`).
//! - Message split ranges (`messages.getSplitRanges`, `invokeWithMessagesRange`).
//! - Dialog discovery (paginated `messages.getDialogs` of 100 across splits, single-peer lookup
//!   with migrated basic-chat discovery via `channels.getFullChannel`, `channels.getLeftChannels`).
//! - Paginated history (`messages.getHistory` slices of 100 with `add_offset = -100`,
//!   `messages.search` for `only_my_messages` & `CHANNEL_PRIVATE` fallback, `messages.getReplies`
//!   for forum topics, `messages.getRichMessage` hydration, `messages.getCustomEmojiDocuments`).
//! - Chunked file downloads in 128 KiB (`128 * 1024`) parts via `upload.getFile` with
//!   `FILE_MIGRATE_X` redirect handling, `FILE_REFERENCE_*` refresh & retry, `TAKEOUT_FILE_EMPTY`
//!   and `LOCATION_INVALID` handling, `LoadedFileCache` deduplication, and `MediaSettings` policy.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use tg_tl::{
    Identifiable, RemoteCall, Serializable, enums as tl_e, functions as tl_f, types as tl_t,
};

use crate::controller::{ApiError, DownloadProgress, ExportSource, StartInfo, StreamEvent};
use crate::data::{
    ContactsList, DialogInfo, DialogsInfo, Document, File, FileLocation, FileOrigin, MediaContent,
    Message, MessagesSlice, ParseMediaContext, PersonalInfo, ProfileMusicInfo, ProfileMusicSlice,
    ReactionType, RichBlock, RichMessage, RichText, RichTextType, ServiceAction, SessionsList,
    SkipReason, StoriesInfo, StoriesSlice, TextPart, TextPartType, UserpicsInfo, UserpicsSlice,
    add_migrate_from_slice, adjust_migrate_message_ids, compute_document_name, document_folder,
    finalize_dialogs_info, prepare_file_name_date_part, prepare_photo_file_name,
    refresh_file_reference, skip_message_by_date,
};
use crate::output::file::OutputFile;
use crate::output::stats::Stats;
use crate::settings::{ExportTypes, MAX_FILE_SIZE, MediaTypes, Settings};
use crate::stand_in::{ChatId, InputFileLocation, InputPeer, InputUser, PeerId};
use crate::tl_parse::{
    append_top_peers, build_sessions_list, is_dialog_included, parse_contacts_list,
    parse_dialogs_info, parse_document, parse_left_channels_info, parse_messages_slice,
    parse_personal_info, parse_rich_message, parse_single_chat_dialogs_info,
    parse_single_user_dialogs_info, parse_stories_slice, parse_user, parse_userpics_slice,
    single_message_after, single_message_before,
};

/// File download chunk size: 128 KiB (`kFileChunkSize` in `export_api_wrap.cpp`).
pub const FILE_CHUNK_SIZE: i32 = 128 * 1024;
/// Maximum messages requested per history/search/replies slice (`kMessagesSliceLimit`).
pub const MESSAGES_SLICE_LIMIT: i32 = 100;
/// Maximum dialogs requested per `messages.getDialogs` page (`kDialogsSliceLimit`).
pub const DIALOGS_SLICE_LIMIT: i32 = 100;
/// Maximum profile photos requested per `photos.getUserPhotos` page (`kUserpicsSliceLimit`).
pub const USERPICS_SLICE_LIMIT: i32 = 100;
/// Maximum stories requested per `stories.getStoriesArchive` page (`kStoriesSliceLimit`).
pub const STORIES_SLICE_LIMIT: i32 = 100;
/// Maximum profile music tracks requested per `users.getSavedMusic` page.
pub const PROFILE_MUSIC_SLICE_LIMIT: i32 = 100;
/// Maximum top peers requested per `contacts.getTopPeers` page.
pub const TOP_PEERS_SLICE_LIMIT: i32 = 100;
/// Maximum custom emoji document IDs per `messages.getCustomEmojiDocuments` batch.
pub const CUSTOM_EMOJI_BATCH_LIMIT: usize = 100;
/// Maximum `FILE_REFERENCE_*` refresh retries per file download.
const MAX_FILE_REF_RETRIES: usize = 2;
/// Maximum entries kept in [`LoadedFileCache`] (`kLocationCacheSize` in `export_api_wrap.cpp:32`).
const LOCATION_CACHE_SIZE: usize = 100_000;

/// Error returned by an [`ExportRpc`] invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RpcCallError {
    /// MTProto RPC error code (e.g. `400`, `403`, `420`, `303`).
    pub code: i32,
    /// Telegram error type token (e.g. `"FILE_REFERENCE_EXPIRED"`, `"TAKEOUT_FILE_EMPTY"`).
    pub type_str: String,
    /// Human-readable description.
    pub description: String,
}

impl RpcCallError {
    /// Creates a new RPC error.
    pub fn new(code: i32, type_str: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            code,
            type_str: type_str.into(),
            description: description.into(),
        }
    }

    /// Returns `true` if this error indicates an expired/invalid file reference
    /// (`export_api_wrap.cpp:781-783`: `code == 400 && type.startsWith("FILE_REFERENCE_")`).
    pub fn is_file_reference_error(&self) -> bool {
        self.code == 400 && self.type_str.starts_with("FILE_REFERENCE_")
    }

    /// Returns `true` if this is `TAKEOUT_FILE_EMPTY`.
    pub fn is_takeout_file_empty(&self) -> bool {
        self.type_str == "TAKEOUT_FILE_EMPTY"
    }

    /// Returns `true` if this is `TAKEOUT_INVALID`.
    pub fn is_takeout_invalid(&self) -> bool {
        self.type_str == "TAKEOUT_INVALID"
    }

    /// Extracts the delay in seconds from a `TAKEOUT_INIT_DELAY_<seconds>` error.
    pub fn parse_takeout_init_delay(&self) -> Option<i32> {
        self.type_str
            .strip_prefix("TAKEOUT_INIT_DELAY_")
            .and_then(|s| s.parse::<i32>().ok())
    }

    /// Returns `true` if this is `CHANNEL_PRIVATE` or `CHANNEL_INVALID`.
    pub fn is_channel_private(&self) -> bool {
        self.type_str == "CHANNEL_PRIVATE" || self.type_str == "CHANNEL_INVALID"
    }

    /// Returns `true` if this is an unavailable file location error
    /// (`export_api_wrap.cpp:777-780` `ApiWrap::fileRequest`: `LOCATION_INVALID`,
    /// `VERSION_INVALID`, `LOCATION_NOT_AVAILABLE`, plus legacy `FILE_ID_INVALID` /
    /// `VOLUME_LOC_NOT_FOUND` / `FILE_PART_MISSING` / `FILEREF_UPGRADE_NEEDED`).
    pub fn is_location_invalid(&self) -> bool {
        matches!(
            self.type_str.as_str(),
            "LOCATION_INVALID"
                | "VERSION_INVALID"
                | "LOCATION_NOT_AVAILABLE"
                | "FILE_ID_INVALID"
                | "VOLUME_LOC_NOT_FOUND"
                | "FILE_PART_MISSING"
                | "FILEREF_UPGRADE_NEEDED"
        )
    }

    /// Extracts the target DC ID from a `FILE_MIGRATE_<dc>` error if present.
    pub fn file_migrate_dc(&self) -> Option<i32> {
        self.type_str
            .strip_prefix("FILE_MIGRATE_")
            .and_then(|s| s.parse::<i32>().ok())
    }

    /// Converts this RPC error into the controller's [`ApiError`].
    pub fn to_api_error(&self) -> ApiError {
        ApiError {
            code: self.code,
            kind: self.type_str.clone(),
            description: self.description.clone(),
        }
    }
}

/// Generic async Telegram RPC transport trait used by [`TlExportSource`].
///
/// Production code implements this over `tg-api` / `tg-mtproto`; tests implement it with an
/// in-memory fake server to verify exact TL requests, takeout wrapping, pagination, and chunked
/// file downloads without any network I/O.
pub trait ExportRpc: Send + Sync {
    /// Invokes a TL `RemoteCall` on `dc_id` (`0` means the main account DC).
    fn invoke<R: RemoteCall + Identifiable + Send + Sync>(
        &self,
        dc_id: i32,
        request: &R,
    ) -> impl Future<Output = Result<R::Return, RpcCallError>> + Send;
}

/// Entry cached in [`LoadedFileCache`] to avoid re-downloading duplicate files.
#[derive(Debug, Clone)]
struct CachedFile {
    relative_path: String,
    size: i64,
}

/// Stable deduplication key matching `ComputeLocationKey` in `export_api_wrap.cpp:46-69`.
///
/// Keys by `(dc_id, kind_tag, thumb_first_byte, id)` — deliberately ignoring `access_hash`
/// and `file_reference` so two messages referencing the same photo/document with refreshed
/// file references hit the cache instead of downloading twice.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum LocationCacheKey {
    /// `inputDocumentFileLocation` / `inputPhotoFileLocation` / `inputTakeoutFileLocation`.
    Tagged { kind_and_dc: u64, id: u64 },
    /// Fallback for any other `InputFileLocation` variant.
    Raw(i32, Vec<u8>),
}

/// Deduplication cache keyed by [`LocationCacheKey`] with FIFO eviction at `LOCATION_CACHE_SIZE`
/// (port of `ApiWrap::LoadedFileCache` in `export_api_wrap.cpp:692-722`).
#[derive(Debug, Default)]
struct LoadedFileCache {
    entries: HashMap<LocationCacheKey, CachedFile>,
    order: VecDeque<LocationCacheKey>,
}

impl LoadedFileCache {
    fn key(loc: &FileLocation) -> Option<LocationCacheKey> {
        let data = loc.data.as_ref()?;
        let dc = loc.dc_id as u64;
        let key = match data {
            InputFileLocation::InputDocumentFileLocation(d) => {
                let letter = d.thumb_size.as_bytes().first().copied().unwrap_or(0);
                LocationCacheKey::Tagged {
                    kind_and_dc: dc | (2u64 << 24) | (u64::from(letter) << 16),
                    id: d.id as u64,
                }
            }
            InputFileLocation::InputPhotoFileLocation(p) => {
                let letter = p.thumb_size.as_bytes().first().copied().unwrap_or(0);
                LocationCacheKey::Tagged {
                    kind_and_dc: dc | (6u64 << 24) | (u64::from(letter) << 16),
                    id: p.id as u64,
                }
            }
            InputFileLocation::InputTakeoutFileLocation => LocationCacheKey::Tagged {
                kind_and_dc: dc | (5u64 << 24),
                id: 0,
            },
            other => LocationCacheKey::Raw(loc.dc_id, other.to_bytes()),
        };
        Some(key)
    }

    fn get(&self, loc: &FileLocation) -> Option<&CachedFile> {
        let k = Self::key(loc)?;
        self.entries.get(&k)
    }

    fn insert(&mut self, loc: &FileLocation, relative_path: String, size: i64) {
        if let Some(k) = Self::key(loc)
            && self
                .entries
                .insert(
                    k.clone(),
                    CachedFile {
                        relative_path,
                        size,
                    },
                )
                .is_none()
        {
            self.order.push_back(k);
            while self.order.len() > LOCATION_CACHE_SIZE {
                if let Some(oldest) = self.order.pop_front() {
                    self.entries.remove(&oldest);
                }
            }
        }
    }
}

/// Internal classification of a downloadable file for `MediaSettings` checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DownloadKind {
    /// Always included (e.g. userpics, stories, profile music, other data, custom emoji, rich media).
    Always,
    /// Photo attachment (`MediaTypes::PHOTO`).
    Photo,
    /// Video file (`MediaTypes::VIDEO`).
    Video,
    /// Voice message (`MediaTypes::VOICE_MESSAGE`).
    VoiceMessage,
    /// Round video message (`MediaTypes::VIDEO_MESSAGE`).
    VideoMessage,
    /// Sticker (`MediaTypes::STICKER`).
    Sticker,
    /// Animated GIF (`MediaTypes::GIF`).
    Gif,
    /// Generic/audio document (`MediaTypes::FILE`).
    File,
}

impl DownloadKind {
    fn from_document(doc: &Document) -> Self {
        if doc.is_sticker {
            Self::Sticker
        } else if doc.is_video_message {
            Self::VideoMessage
        } else if doc.is_voice_message {
            Self::VoiceMessage
        } else if doc.is_animated {
            Self::Gif
        } else if doc.is_video_file {
            Self::Video
        } else {
            Self::File
        }
    }

    fn media_type_flag(self) -> Option<MediaTypes> {
        match self {
            Self::Always => None,
            Self::Photo => Some(MediaTypes::PHOTO),
            Self::Video => Some(MediaTypes::VIDEO),
            Self::VoiceMessage => Some(MediaTypes::VOICE_MESSAGE),
            Self::VideoMessage => Some(MediaTypes::VIDEO_MESSAGE),
            Self::Sticker => Some(MediaTypes::STICKER),
            Self::Gif => Some(MediaTypes::GIF),
            Self::File => Some(MediaTypes::FILE),
        }
    }
}

/// TL-backed [`ExportSource`] implementation (port of `Export::ApiWrap`).
pub struct TlExportSource<C: ExportRpc> {
    rpc: C,
    settings: Settings,
    utc_offset_secs: i32,
    takeout_id: Option<i64>,
    self_id: PeerId,
    splits: Vec<tl_t::MessageRange>,
    userpics_count: i32,
    stories_count: i32,
    profile_music_count: i32,
    file_cache: LoadedFileCache,
    custom_emoji_paths: HashMap<u64, String>,
    next_random_id: AtomicU64,
    cancelled: AtomicBool,
    skipped_random_id: AtomicU64,
    stats_files: AtomicU64,
    stats_bytes: AtomicU64,
}

impl<C: ExportRpc> TlExportSource<C> {
    /// Creates a new `TlExportSource` wrapping `rpc` with UTC (`0`) filename timestamps.
    pub fn new(rpc: C) -> Self {
        Self::with_utc_offset(rpc, 0)
    }

    /// Creates a new `TlExportSource` with a specific UTC offset in seconds for filename date suffixes.
    pub fn with_utc_offset(rpc: C, utc_offset_secs: i32) -> Self {
        Self {
            rpc,
            settings: Settings::default(),
            utc_offset_secs,
            takeout_id: None,
            self_id: PeerId::ZERO,
            splits: Vec::new(),
            userpics_count: 0,
            stories_count: 0,
            profile_music_count: 0,
            file_cache: LoadedFileCache::default(),
            custom_emoji_paths: HashMap::new(),
            next_random_id: AtomicU64::new(1),
            cancelled: AtomicBool::new(false),
            skipped_random_id: AtomicU64::new(0),
            stats_files: AtomicU64::new(0),
            stats_bytes: AtomicU64::new(0),
        }
    }

    /// Returns a reference to the underlying RPC client.
    pub fn rpc(&self) -> &C {
        &self.rpc
    }

    /// Returns the active takeout session ID if initialized.
    pub fn takeout_id(&self) -> Option<i64> {
        self.takeout_id
    }

    /// Returns the discovered `self_id` of the current account.
    pub fn self_id(&self) -> PeerId {
        self.self_id
    }

    /// Builds `account.initTakeoutSession` flags matching `ApiWrap::startMainSession`
    /// (`export_api_wrap.cpp:1070-1095`).
    ///
    /// Notes:
    /// - `files` is enabled when `(!media.types.is_empty() && size_limit > 0) || Userpics || Stories`
    ///   (plus `ProfileMusic` when audio files are exported).
    /// - `file_max_size` is only included when `has_files && size_limit < MAX_FILE_SIZE` (4000 MiB).
    pub fn build_init_takeout_request(settings: &Settings) -> tl_f::account::InitTakeoutSession {
        let types = settings.types;
        let size_limit = settings.media.size_limit;
        let has_files = (!settings.media.types.is_empty() && size_limit > 0)
            || types.contains(ExportTypes::USERPICS)
            || types.contains(ExportTypes::STORIES)
            || types.contains(ExportTypes::PROFILE_MUSIC);
        let single_peer = settings.only_single_peer();
        let (message_users, message_chats, message_megagroups, message_channels) = if single_peer {
            match &settings.single_peer {
                InputPeer::Self_ | InputPeer::User(_) | InputPeer::UserFromMessage(_) => {
                    (true, false, false, false)
                }
                InputPeer::Chat(_) => (false, true, false, false),
                InputPeer::Channel(_) | InputPeer::ChannelFromMessage(_) => {
                    (false, false, true, true)
                }
                InputPeer::Empty => (false, false, false, false),
            }
        } else {
            (
                types.intersects(ExportTypes::PERSONAL_CHATS | ExportTypes::BOT_CHATS),
                types.contains(ExportTypes::PRIVATE_GROUPS),
                types.intersects(ExportTypes::PRIVATE_GROUPS | ExportTypes::PUBLIC_GROUPS),
                types.intersects(ExportTypes::PRIVATE_CHANNELS | ExportTypes::PUBLIC_CHANNELS),
            )
        };
        tl_f::account::InitTakeoutSession {
            contacts: !single_peer && types.contains(ExportTypes::CONTACTS),
            message_users,
            message_chats,
            message_megagroups,
            message_channels,
            files: has_files,
            file_max_size: (has_files && size_limit < MAX_FILE_SIZE).then_some(size_limit),
        }
    }

    /// Returns `true` when only channels/supergroups are requested and only the newest
    /// message split needs to be queried (`ApiWrap::useOnlyLastSplit`, `export_api_wrap.cpp:1029-1031`).
    fn use_only_last_split(&self) -> bool {
        !self
            .settings
            .types
            .intersects(ExportTypes::NON_CHANNEL_CHATS_MASK)
    }

    /// Finishes an active takeout session with `success = false` after cancellation
    /// (`ApiWrap::cancelExportFast`, `export_api_wrap.cpp:1944-1951`).
    pub async fn finish_takeout_cancelled(&mut self) -> Result<(), ApiError> {
        self.cancelled.store(true, Ordering::SeqCst);
        if let Some(takeout_id) = self.takeout_id.take() {
            let wrapped = tl_f::InvokeWithTakeout {
                takeout_id,
                query: tl_f::account::FinishTakeoutSession { success: false },
            };
            let _ = self
                .rpc
                .invoke(0, &wrapped)
                .await
                .map_err(|e| e.to_api_error())?;
        }
        Ok(())
    }

    /// Wraps `request` in `InvokeWithTakeout` when a takeout session is active.
    async fn invoke_takeout<R>(&self, dc_id: i32, request: R) -> Result<R::Return, RpcCallError>
    where
        R: RemoteCall + Identifiable + Send + Sync,
        R::Return: Send,
    {
        if let Some(takeout_id) = self.takeout_id {
            let wrapped = tl_f::InvokeWithTakeout {
                takeout_id,
                query: request,
            };
            self.rpc.invoke(dc_id, &wrapped).await
        } else {
            self.rpc.invoke(dc_id, &request).await
        }
    }

    /// Wraps `request` in `InvokeWithMessagesRange` (if `split_index` resolves inside `splits`)
    /// and then `InvokeWithTakeout`.
    async fn invoke_split<R>(&self, split_index: i32, request: R) -> Result<R::Return, RpcCallError>
    where
        R: RemoteCall + Identifiable + Send + Sync,
        R::Return: Send,
    {
        let resolved_idx = if split_index < 0 && !self.splits.is_empty() {
            split_index + (self.splits.len() as i32)
        } else {
            split_index
        };
        if resolved_idx >= 0
            && let Some(range) = self.splits.get(resolved_idx as usize).cloned()
        {
            let ranged = tl_f::InvokeWithMessagesRange {
                range: tl_e::MessageRange::MessageRange(range),
                query: request,
            };
            return self.invoke_takeout(0, ranged).await;
        }
        self.invoke_takeout(0, request).await
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    fn alloc_random_id(&self) -> u64 {
        self.next_random_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Checks whether a file should be skipped per `MediaSettings` before downloading.
    fn check_media_policy(&self, file: &File, kind: DownloadKind) -> SkipReason {
        if let Some(flag) = kind.media_type_flag() {
            if !self.settings.media.types.contains(flag) {
                return SkipReason::FileType;
            }
            if self.settings.media.size_limit > 0 && file.size > self.settings.media.size_limit {
                return SkipReason::FileSize;
            }
        }
        if file.content.is_empty() && !file.location.is_valid() {
            return SkipReason::Unavailable;
        }
        SkipReason::None
    }

    /// Writes inline `file.content` or downloads `file.location` in 128 KiB parts via `upload.getFile`.
    async fn download_file<F>(
        &mut self,
        file: &mut File,
        kind: DownloadKind,
        origin: &FileOrigin,
        item_index: i32,
        on_progress: &mut F,
    ) -> Result<(), ApiError>
    where
        F: FnMut(DownloadProgress) -> bool,
    {
        if !file.relative_path.is_empty() {
            return Ok(());
        }
        let policy = self.check_media_policy(file, kind);
        if policy != SkipReason::None {
            file.skip_reason = policy;
            return Ok(());
        }

        // Check deduplication cache first.
        if file.content.is_empty()
            && let Some(cached) = self.file_cache.get(&file.location)
        {
            file.relative_path = cached.relative_path.clone();
            file.size = cached.size;
            file.skip_reason = SkipReason::None;
            return Ok(());
        }

        let suggested = if file.suggested_path.is_empty() {
            format!("files/file_{}.bin", self.alloc_random_id())
        } else {
            file.suggested_path.clone()
        };
        let relative_path = OutputFile::prepare_relative_path(&self.settings.path, &suggested);
        let full_path = format!("{}{relative_path}", self.settings.path);

        // Inline/cached bytes (e.g. vCard or PhotoCachedSize): write directly without RPC.
        if !file.content.is_empty() {
            let mut out = OutputFile::new(&full_path);
            let res = out.write_block(&file.content, None);
            if res.is_error() {
                file.skip_reason = SkipReason::Unavailable;
                return Ok(());
            }
            self.stats_files.fetch_add(1, Ordering::Relaxed);
            self.stats_bytes
                .fetch_add(file.content.len() as u64, Ordering::Relaxed);
            file.relative_path = relative_path;
            file.size = file.content.len() as i64;
            file.skip_reason = SkipReason::None;
            return Ok(());
        }

        let random_id = self.alloc_random_id();
        let mut ref_retries = 0usize;
        let expected_size = file.size;

        'retry: loop {
            if self.is_cancelled() {
                return Ok(());
            }
            let Some(mut location) = file.location.data.clone() else {
                file.skip_reason = SkipReason::Unavailable;
                return Ok(());
            };
            let dc_id = file.location.dc_id;
            let mut offset: i64 = 0;
            let mut out = OutputFile::new(&full_path);
            let mut written_total: i64 = 0;

            loop {
                if self.is_cancelled() || self.skipped_random_id.load(Ordering::SeqCst) == random_id
                {
                    file.skip_reason = SkipReason::Unavailable;
                    return Ok(());
                }
                // `export_api_wrap.cpp:760-767` (`ApiWrap::fileRequest`): send `upload.getFile`
                // with `MTP_flags(0)` (`precise: false, cdn_supported: false`) and 128 KiB limit.
                let req = tl_f::upload::GetFile {
                    precise: false,
                    cdn_supported: false,
                    location: location.clone(),
                    offset,
                    limit: FILE_CHUNK_SIZE,
                };
                match self.invoke_takeout(dc_id, req).await {
                    Ok(tl_e::upload::File::File(part)) => {
                        let chunk_len = part.bytes.len() as i64;
                        if chunk_len == 0 && expected_size > 0 && written_total < expected_size {
                            // `export_api_wrap.cpp:3555-3558` (`ApiWrap::filePartDone`): empty
                            // bytes when total size is known is a protocol error.
                            return Err(ApiError::new(
                                0,
                                "API_ERROR",
                                "Empty bytes received in file part.",
                            ));
                        }
                        if chunk_len > 0 {
                            let res = out.write_block(&part.bytes, None);
                            if res.is_error() {
                                file.skip_reason = SkipReason::Unavailable;
                                return Ok(());
                            }
                            written_total += chunk_len;
                            offset += chunk_len;
                        }
                        let total = expected_size.max(written_total);
                        if !on_progress(DownloadProgress {
                            random_id,
                            path: relative_path.clone(),
                            item_index,
                            ready: written_total,
                            total,
                        }) {
                            return Ok(());
                        }
                        // `export_api_wrap.cpp:3592-3597` (`ApiWrap::filePartDone`): stop as soon
                        // as we reach a known `expected_size` (including exact multiples of 128 KiB)
                        // or receive a short/empty final chunk.
                        let reached_known_size =
                            expected_size > 0 && written_total >= expected_size;
                        if reached_known_size || chunk_len < i64::from(FILE_CHUNK_SIZE) {
                            self.stats_files.fetch_add(1, Ordering::Relaxed);
                            self.stats_bytes
                                .fetch_add(written_total as u64, Ordering::Relaxed);
                            file.relative_path = relative_path.clone();
                            file.size = written_total;
                            file.skip_reason = SkipReason::None;
                            self.file_cache
                                .insert(&file.location, relative_path, written_total);
                            return Ok(());
                        }
                    }
                    Ok(tl_e::upload::File::CdnRedirect(redirect)) => {
                        file.location.dc_id = redirect.dc_id;
                        continue 'retry;
                    }
                    Err(err) if err.file_migrate_dc().is_some() => {
                        if let Some(new_dc) = err.file_migrate_dc() {
                            file.location.dc_id = new_dc;
                            continue 'retry;
                        }
                    }
                    Err(err) if err.is_takeout_file_empty() || err.is_location_invalid() => {
                        file.skip_reason = SkipReason::Unavailable;
                        return Ok(());
                    }
                    Err(err)
                        if err.is_file_reference_error() && ref_retries < MAX_FILE_REF_RETRIES =>
                    {
                        ref_retries += 1;
                        // `export_api_wrap.cpp:3615-3629` (`filePartRetryReference`) and
                        // `3640/3657/3680/3713/3729`: retry the SAME `offset` with the refreshed
                        // location without discarding already-written chunks; if refresh fails,
                        // mark the file unavailable instead of aborting the export.
                        match self
                            .refresh_origin_file_reference(&mut file.location, origin)
                            .await
                        {
                            Ok(true) => {
                                if let Some(refreshed_loc) = file.location.data.clone() {
                                    location = refreshed_loc;
                                    continue;
                                }
                                file.skip_reason = SkipReason::Unavailable;
                                return Ok(());
                            }
                            Ok(false) | Err(_) => {
                                file.skip_reason = SkipReason::Unavailable;
                                return Ok(());
                            }
                        }
                    }
                    Err(err) => {
                        return Err(err.to_api_error());
                    }
                }
            }
        }
    }

    /// Attempts to refresh an expired `FILE_REFERENCE_*` by refetching its owning message, story,
    /// custom emoji document, or rich message (`loadFileReference`).
    async fn refresh_origin_file_reference(
        &self,
        target: &mut FileLocation,
        origin: &FileOrigin,
    ) -> Result<bool, ApiError> {
        if origin.custom_emoji_id != 0 {
            let docs = self
                .invoke_takeout(
                    0,
                    tl_f::messages::GetCustomEmojiDocuments {
                        document_id: vec![origin.custom_emoji_id as i64],
                    },
                )
                .await
                .map_err(|e| e.to_api_error())?;
            for d in &docs {
                let parsed = parse_document(d, false);
                if refresh_file_reference(target, &parsed.file.location)
                    || refresh_file_reference(target, &parsed.thumb.file.location)
                {
                    return Ok(true);
                }
            }
            return Ok(false);
        }

        if origin.story_id != 0 {
            let resp = self
                .invoke_takeout(
                    0,
                    tl_f::stories::GetStoriesByID {
                        peer: InputPeer::Self_,
                        id: vec![origin.story_id],
                    },
                )
                .await
                .map_err(|e| e.to_api_error())?;
            let tl_e::stories::Stories::Stories(s) = resp;
            let slice = parse_stories_slice(&s.stories, 0, self.utc_offset_secs);
            for story in &slice.list {
                if let Some(f) = story.file()
                    && refresh_file_reference(target, &f.location)
                {
                    return Ok(true);
                }
                if let Some(t) = story.thumb()
                    && refresh_file_reference(target, &t.file.location)
                {
                    return Ok(true);
                }
            }
            return Ok(false);
        }

        if origin.rich_message && origin.message_id != 0 {
            // `export_api_wrap.cpp:3677` (`ApiWrap::filePartRefreshReference`):
            // `messages.getRichMessage` uses `mainRequest` (`InvokeWithTakeout` only).
            let resp = self
                .invoke_takeout(
                    0,
                    tl_f::messages::GetRichMessage {
                        peer: origin.peer.clone(),
                        id: origin.message_id,
                    },
                )
                .await
                .map_err(|e| e.to_api_error())?;
            let (_, _, msgs) = unpack_messages_response(&resp);
            for raw_msg in msgs {
                if let tl_e::Message::Message(m) = raw_msg
                    && let Some(ref rich) = m.rich_message
                {
                    let parsed = parse_rich_message(rich);
                    for p in parsed.photos.values() {
                        if refresh_file_reference(target, &p.image.file.location) {
                            return Ok(true);
                        }
                    }
                    for d in parsed.documents.values() {
                        if refresh_file_reference(target, &d.file.location)
                            || refresh_file_reference(target, &d.thumb.file.location)
                        {
                            return Ok(true);
                        }
                    }
                }
            }
            return Ok(false);
        }

        if origin.message_id != 0 {
            let input_msg = vec![tl_e::InputMessage::ID(tl_t::InputMessageID {
                id: origin.message_id,
            })];
            // `export_api_wrap.cpp:3698-3737` (`ApiWrap::filePartRefreshReference`):
            // `channels.getMessages` uses `mainRequest` (no split range), while
            // `messages.getMessages` uses `splitRequest(origin.split, ...)`.
            let resp = match &origin.peer {
                InputPeer::Channel(ch) => {
                    self.invoke_takeout(
                        0,
                        tl_f::channels::GetMessages {
                            channel: tl_e::InputChannel::InputChannel(tl_t::InputChannel {
                                channel_id: ch.channel_id,
                                access_hash: ch.access_hash,
                            }),
                            id: input_msg,
                        },
                    )
                    .await
                }
                InputPeer::ChannelFromMessage(ch) => {
                    self.invoke_takeout(
                        0,
                        tl_f::channels::GetMessages {
                            channel: tl_e::InputChannel::FromMessage(
                                tl_t::InputChannelFromMessage {
                                    peer: (*ch.peer).clone(),
                                    msg_id: ch.msg_id,
                                    channel_id: ch.channel_id,
                                },
                            ),
                            id: input_msg,
                        },
                    )
                    .await
                }
                _ => {
                    self.invoke_split(origin.split, tl_f::messages::GetMessages { id: input_msg })
                        .await
                }
            }
            .map_err(|e| e.to_api_error())?;
            let (users, chats, msgs) = unpack_messages_response(&resp);
            let slice = parse_messages_slice(users, chats, msgs, self.self_id, "");
            for msg in &slice.list {
                if refresh_message_file_reference(target, msg) {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    /// Hydrates partial `RichMessage`s, resolves custom emojis, and downloads all media files
    /// belonging to a `MessagesSlice`.
    async fn process_messages_slice<F>(
        &mut self,
        slice: &mut MessagesSlice,
        dialog: &DialogInfo,
        split_index: i32,
        peer_input: &InputPeer,
        media_ctx: &mut ParseMediaContext,
        on_progress: &mut F,
    ) -> Result<(), ApiError>
    where
        F: FnMut(DownloadProgress) -> bool,
    {
        // 1. Hydrate any partial rich messages (`rich.part == true`) via `messages.getRichMessage`
        // (`export_api_wrap.cpp:2473` `ApiWrap::resumeMessagesSlice`: `mainRequest` without split).
        for msg in &mut slice.list {
            if skip_message_by_date(msg, &self.settings) {
                continue;
            }
            if msg.rich_message.as_ref().is_some_and(|r| r.part) {
                let req = tl_f::messages::GetRichMessage {
                    peer: peer_input.clone(),
                    id: msg.id,
                };
                if let Ok(full_resp) = self.invoke_takeout(0, req).await {
                    let (_, _, raw_msgs) = unpack_messages_response(&full_resp);
                    if let Some(tl_e::Message::Message(m)) = raw_msgs.first()
                        && let Some(ref full_rich) = m.rich_message
                    {
                        msg.rich_message = Some(parse_rich_message(full_rich));
                    }
                }
            }
        }

        // 2. Collect custom emoji IDs referenced in text, reactions, and rich messages.
        let mut needed_emoji_ids: Vec<u64> = Vec::new();
        let mut seen_emoji: HashSet<u64> = HashSet::new();
        for msg in &slice.list {
            if skip_message_by_date(msg, &self.settings) {
                continue;
            }
            collect_message_custom_emojis(
                msg,
                &self.custom_emoji_paths,
                &mut seen_emoji,
                &mut needed_emoji_ids,
            );
        }
        if !needed_emoji_ids.is_empty() {
            self.fetch_and_download_custom_emojis(&needed_emoji_ids, on_progress)
                .await?;
        }

        // 3. Apply resolved custom emoji relative paths into messages.
        for msg in &mut slice.list {
            apply_message_custom_emojis(msg, &self.custom_emoji_paths);
        }

        // 4. Download media files for each message in the slice.
        let utc_offset = self.utc_offset_secs;
        for (idx, msg) in slice.list.iter_mut().enumerate() {
            if self.is_cancelled() {
                return Ok(());
            }
            if skip_message_by_date(msg, &self.settings) {
                if let Some(f) = msg.media.file_mut() {
                    f.skip_reason = SkipReason::DateLimits;
                }
                if let Some(t) = msg.media.thumb_mut() {
                    t.file.skip_reason = SkipReason::DateLimits;
                }
                continue;
            }
            let origin = FileOrigin {
                split: split_index,
                peer: peer_input.clone(),
                message_id: msg.id,
                ..Default::default()
            };
            let item_index = idx as i32;

            match &mut msg.action {
                ServiceAction::ChatEditPhoto { photo }
                | ServiceAction::SuggestProfilePhoto { photo } => {
                    media_ctx.photos += 1;
                    if photo.image.file.suggested_path.is_empty() {
                        photo.image.file.suggested_path = format!(
                            "{}photos/{}",
                            dialog.relative_path,
                            prepare_photo_file_name(media_ctx.photos, photo.date, utc_offset)
                        );
                    }
                    self.download_file(
                        &mut photo.image.file,
                        DownloadKind::Photo,
                        &origin,
                        item_index,
                        on_progress,
                    )
                    .await?;
                }
                _ => {}
            }

            match &mut msg.media.content {
                MediaContent::Photo(photo) => {
                    if msg.media.ttl == 0 {
                        media_ctx.photos += 1;
                        if photo.image.file.suggested_path.is_empty() {
                            photo.image.file.suggested_path = format!(
                                "{}photos/{}",
                                dialog.relative_path,
                                prepare_photo_file_name(media_ctx.photos, photo.date, utc_offset)
                            );
                        }
                        self.download_file(
                            &mut photo.image.file,
                            DownloadKind::Photo,
                            &origin,
                            item_index,
                            on_progress,
                        )
                        .await?;
                    }
                }
                MediaContent::Document(doc) => {
                    if msg.media.ttl == 0 {
                        let kind = DownloadKind::from_document(doc);
                        if doc.file.suggested_path.is_empty() {
                            let folder = document_folder(doc);
                            let name = compute_document_name(media_ctx, doc, msg.date, utc_offset);
                            doc.file.suggested_path =
                                format!("{}{folder}/{name}", dialog.relative_path);
                        }
                        self.download_file(&mut doc.file, kind, &origin, item_index, on_progress)
                            .await?;
                        if doc.thumb.file.location.is_valid() || !doc.thumb.file.content.is_empty()
                        {
                            if doc.thumb.file.suggested_path.is_empty() {
                                doc.thumb.file.suggested_path =
                                    format!("{}_thumb.jpg", doc.file.suggested_path);
                            }
                            self.download_file(
                                &mut doc.thumb.file,
                                kind,
                                &origin,
                                item_index,
                                on_progress,
                            )
                            .await?;
                        }
                    }
                }
                MediaContent::SharedContact(contact) if !contact.vcard.content.is_empty() => {
                    self.download_file(
                        &mut contact.vcard,
                        DownloadKind::Always,
                        &origin,
                        item_index,
                        on_progress,
                    )
                    .await?;
                }
                _ => {}
            }

            if let Some(ref mut rich) = msg.rich_message {
                let rich_origin = FileOrigin {
                    rich_message: true,
                    ..origin.clone()
                };
                for photo in rich.photos.values_mut() {
                    media_ctx.photos += 1;
                    if photo.image.file.suggested_path.is_empty() {
                        photo.image.file.suggested_path = format!(
                            "{}photos/{}",
                            dialog.relative_path,
                            prepare_photo_file_name(media_ctx.photos, photo.date, utc_offset)
                        );
                    }
                    self.download_file(
                        &mut photo.image.file,
                        DownloadKind::Photo,
                        &rich_origin,
                        item_index,
                        on_progress,
                    )
                    .await?;
                }
                for doc in rich.documents.values_mut() {
                    let kind = DownloadKind::from_document(doc);
                    if doc.file.suggested_path.is_empty() {
                        let folder = document_folder(doc);
                        let name = compute_document_name(media_ctx, doc, msg.date, utc_offset);
                        doc.file.suggested_path =
                            format!("{}{folder}/{name}", dialog.relative_path);
                    }
                    self.download_file(&mut doc.file, kind, &rich_origin, item_index, on_progress)
                        .await?;
                    if doc.thumb.file.location.is_valid() || !doc.thumb.file.content.is_empty() {
                        if doc.thumb.file.suggested_path.is_empty() {
                            doc.thumb.file.suggested_path =
                                format!("{}_thumb.jpg", doc.file.suggested_path);
                        }
                        self.download_file(
                            &mut doc.thumb.file,
                            kind,
                            &rich_origin,
                            item_index,
                            on_progress,
                        )
                        .await?;
                    }
                }
            }
        }
        Ok(())
    }

    async fn fetch_and_download_custom_emojis<F>(
        &mut self,
        ids: &[u64],
        on_progress: &mut F,
    ) -> Result<(), ApiError>
    where
        F: FnMut(DownloadProgress) -> bool,
    {
        let mut emoji_ctx = ParseMediaContext::default();
        for chunk in ids.chunks(CUSTOM_EMOJI_BATCH_LIMIT) {
            let req = tl_f::messages::GetCustomEmojiDocuments {
                document_id: chunk.iter().map(|&id| id as i64).collect(),
            };
            let docs = match self.invoke_takeout(0, req).await {
                Ok(d) => d,
                Err(_) => continue,
            };
            let mut found = HashSet::new();
            for d in &docs {
                let mut parsed = parse_document(d, false);
                if parsed.id == 0 {
                    continue;
                }
                found.insert(parsed.id);
                let name = compute_document_name(
                    &mut emoji_ctx,
                    &parsed,
                    parsed.date,
                    self.utc_offset_secs,
                );
                parsed.file.suggested_path = format!("stickers/{name}");
                let origin = FileOrigin {
                    custom_emoji_id: parsed.id,
                    ..Default::default()
                };
                self.download_file(
                    &mut parsed.file,
                    DownloadKind::Sticker,
                    &origin,
                    0,
                    on_progress,
                )
                .await?;
                let stored = if !parsed.file.relative_path.is_empty() {
                    parsed.file.relative_path.clone()
                } else {
                    TextPart::UNAVAILABLE_EMOJI.to_string()
                };
                self.custom_emoji_paths.insert(parsed.id, stored);
            }
            for &id in chunk {
                if !found.contains(&id) {
                    self.custom_emoji_paths
                        .insert(id, TextPart::UNAVAILABLE_EMOJI.to_string());
                }
            }
        }
        Ok(())
    }

    async fn query_split_message_count(
        &self,
        peer: &InputPeer,
        split_index: i32,
        only_my_messages: bool,
    ) -> Result<(i32, bool), ApiError> {
        if self.settings.single_peer_from > 0 {
            let req = tl_f::messages::GetHistory {
                peer: peer.clone(),
                offset_id: 0,
                offset_date: 0,
                add_offset: 0,
                limit: 1,
                max_id: 0,
                min_id: 0,
                hash: 0,
            };
            if let Ok(resp) = self.invoke_split(split_index, req).await {
                let (_, _, msgs) = unpack_messages_response(&resp);
                if single_message_before(msgs, self.settings.single_peer_from) {
                    return Ok((0, only_my_messages));
                }
            }
        }
        if self.settings.single_peer_till > 0 {
            let req = tl_f::messages::GetHistory {
                peer: peer.clone(),
                offset_id: 1,
                offset_date: 0,
                add_offset: -1,
                limit: 1,
                max_id: 0,
                min_id: 0,
                hash: 0,
            };
            if let Ok(resp) = self.invoke_split(split_index, req).await {
                let (_, _, msgs) = unpack_messages_response(&resp);
                if single_message_after(msgs, self.settings.single_peer_till) {
                    return Ok((0, only_my_messages));
                }
            }
        }

        if only_my_messages {
            let req = make_search_self_request(peer, 1, -1, 1);
            let resp = self
                .invoke_split(split_index, req)
                .await
                .map_err(|e| e.to_api_error())?;
            return Ok((messages_response_count(&resp), true));
        }

        let req = tl_f::messages::GetHistory {
            peer: peer.clone(),
            offset_id: 1,
            offset_date: 0,
            add_offset: -1,
            limit: 1,
            max_id: 0,
            min_id: 0,
            hash: 0,
        };
        match self.invoke_split(split_index, req).await {
            Ok(resp) => Ok((messages_response_count(&resp), false)),
            Err(err) if err.is_channel_private() => {
                let search_req = make_search_self_request(peer, 1, -1, 1);
                let resp = self
                    .invoke_split(split_index, search_req)
                    .await
                    .map_err(|e| e.to_api_error())?;
                Ok((messages_response_count(&resp), true))
            }
            Err(err) => Err(err.to_api_error()),
        }
    }
}

impl<C: ExportRpc> ExportSource for TlExportSource<C> {
    async fn start_export(
        &mut self,
        settings: &Settings,
        _stats: &Stats,
    ) -> Result<StartInfo, ApiError> {
        self.settings = settings.clone();
        self.cancelled.store(false, Ordering::SeqCst);
        self.skipped_random_id.store(0, Ordering::SeqCst);
        self.splits.clear();
        self.takeout_id = None;
        self.userpics_count = 0;
        self.stories_count = 0;
        self.profile_music_count = 0;

        // 1. Fetch self user ID (`users.getUsers([inputUserSelf])`,
        // `export_api_wrap.cpp:1096-1111` `ApiWrap::startMainSession`).
        let self_users = self
            .rpc
            .invoke(
                0,
                &tl_f::users::GetUsers {
                    id: vec![InputUser::Self_],
                },
            )
            .await
            .map_err(|e| e.to_api_error())?;
        self.self_id = PeerId::ZERO;
        for u in &self_users {
            if let tl_e::User::User(data) = u
                && data.self_
            {
                self.self_id = parse_user(u).id();
                break;
            }
        }
        if self.self_id.is_zero() {
            return Err(ApiError::new(0, "API_ERROR", "Could not retrieve selfId."));
        }

        // 2. Initialize takeout session (`account.initTakeoutSession`).
        let init_req = Self::build_init_takeout_request(settings);
        let takeout = self
            .rpc
            .invoke(0, &init_req)
            .await
            .map_err(|e| e.to_api_error())?;
        let tl_e::account::Takeout::Takeout(t) = takeout;
        self.takeout_id = Some(t.id);

        // 3. Query high-level counts for enabled steps.
        if !settings.only_single_peer() && settings.types.contains(ExportTypes::USERPICS) {
            let resp = self
                .invoke_takeout(
                    0,
                    tl_f::photos::GetUserPhotos {
                        user_id: InputUser::Self_,
                        offset: 0,
                        max_id: 0,
                        limit: 0,
                    },
                )
                .await
                .map_err(|e| e.to_api_error())?;
            self.userpics_count = match resp {
                tl_e::photos::Photos::Photos(p) => p.photos.len() as i32,
                tl_e::photos::Photos::Slice(p) => p.count,
            };
        }

        if !settings.only_single_peer() && settings.types.contains(ExportTypes::STORIES) {
            let resp = self
                .invoke_takeout(
                    0,
                    tl_f::stories::GetStoriesArchive {
                        peer: InputPeer::Self_,
                        offset_id: 0,
                        limit: 0,
                    },
                )
                .await
                .map_err(|e| e.to_api_error())?;
            let tl_e::stories::Stories::Stories(s) = resp;
            self.stories_count = s.count;
        }

        if !settings.only_single_peer() && settings.types.contains(ExportTypes::PROFILE_MUSIC) {
            let resp = self
                .invoke_takeout(
                    0,
                    tl_f::users::GetSavedMusic {
                        id: InputUser::Self_,
                        offset: 0,
                        limit: 0,
                        hash: 0,
                    },
                )
                .await
                .map_err(|e| e.to_api_error())?;
            self.profile_music_count = match resp {
                tl_e::users::SavedMusic::SavedMusic(m) => m.count,
                tl_e::users::SavedMusic::NotModified(m) => m.count,
            };
        }

        // 4. Query message split ranges (`messages.getSplitRanges`).
        let mut dialogs_count = 0_i32;
        if settings.only_single_topic() {
            dialogs_count = 1;
        } else if settings.only_single_peer()
            || settings.types.intersects(ExportTypes::ANY_CHATS_MASK)
        {
            let ranges = self
                .invoke_takeout(0, tl_f::messages::GetSplitRanges)
                .await
                .map_err(|e| e.to_api_error())?;
            self.splits = ranges
                .into_iter()
                .map(|r| {
                    let tl_e::MessageRange::MessageRange(mr) = r;
                    mr
                })
                .collect();
            // `export_api_wrap.cpp:942-947` (`ApiWrap::requestSplitRanges`): synthesize a
            // full-range `[1, i32::MAX]` split when the server returns an empty vector.
            if self.splits.is_empty() {
                self.splits.push(tl_t::MessageRange {
                    min_id: 1,
                    max_id: i32::MAX,
                });
            }

            if settings.only_single_peer() {
                dialogs_count = match settings.single_peer {
                    InputPeer::Channel(_) | InputPeer::ChannelFromMessage(_) => 1,
                    _ => self.splits.len() as i32,
                };
            } else {
                // `export_api_wrap.cpp:948-1004` (`requestSplitRanges` + `requestDialogsCount`):
                // query only the newest split when `use_only_last_split()`, and sum counts across
                // queried splits.
                let start_split = if self.use_only_last_split() {
                    self.splits.len().saturating_sub(1)
                } else {
                    0
                };
                for split_idx in start_split..self.splits.len() {
                    let req = tl_f::messages::GetDialogs {
                        exclude_pinned: false,
                        folder_id: None,
                        offset_date: 0,
                        offset_id: 0,
                        offset_peer: InputPeer::Empty,
                        limit: 1,
                        hash: 0,
                    };
                    let resp = self
                        .invoke_split(split_idx as i32, req)
                        .await
                        .map_err(|e| e.to_api_error())?;
                    dialogs_count += dialogs_response_count(&resp);
                }
                if settings.types.intersects(
                    ExportTypes::PRIVATE_GROUPS
                        | ExportTypes::PUBLIC_GROUPS
                        | ExportTypes::PRIVATE_CHANNELS
                        | ExportTypes::PUBLIC_CHANNELS,
                ) && let Ok(left) = self
                    .invoke_takeout(0, tl_f::channels::GetLeftChannels { offset: 0 })
                    .await
                {
                    let left_count = match left {
                        tl_e::messages::Chats::Chats(c) => c.chats.len() as i32,
                        tl_e::messages::Chats::Slice(c) => c.count,
                    };
                    dialogs_count += left_count;
                }
            }
        }

        Ok(StartInfo {
            userpics_count: self.userpics_count,
            stories_count: self.stories_count,
            profile_music_count: self.profile_music_count,
            dialogs_count,
        })
    }

    async fn request_dialogs_list(
        &mut self,
        on_progress: &mut (dyn FnMut(i32) -> bool + Send),
    ) -> Result<DialogsInfo, ApiError> {
        let splits_count = self.splits.len().max(1) as i32;

        if self.settings.only_single_peer() {
            let mut info = match &self.settings.single_peer {
                InputPeer::Self_ => {
                    let users = self
                        .invoke_takeout(
                            0,
                            tl_f::users::GetUsers {
                                id: vec![InputUser::Self_],
                            },
                        )
                        .await
                        .map_err(|e| e.to_api_error())?;
                    users
                        .first()
                        .map(|u| parse_single_user_dialogs_info(u, splits_count))
                        .unwrap_or_default()
                }
                InputPeer::User(u) => {
                    let users = self
                        .invoke_takeout(
                            0,
                            tl_f::users::GetUsers {
                                id: vec![InputUser::InputUser(tl_t::InputUser {
                                    user_id: u.user_id,
                                    access_hash: u.access_hash,
                                })],
                            },
                        )
                        .await
                        .map_err(|e| e.to_api_error())?;
                    users
                        .first()
                        .map(|user| parse_single_user_dialogs_info(user, splits_count))
                        .unwrap_or_default()
                }
                InputPeer::Chat(c) => {
                    let resp = self
                        .invoke_takeout(
                            0,
                            tl_f::messages::GetChats {
                                id: vec![c.chat_id],
                            },
                        )
                        .await
                        .map_err(|e| e.to_api_error())?;
                    let chats = match &resp {
                        tl_e::messages::Chats::Chats(ch) => &ch.chats,
                        tl_e::messages::Chats::Slice(ch) => &ch.chats,
                    };
                    chats
                        .first()
                        .map(|ch| parse_single_chat_dialogs_info(ch, splits_count))
                        .unwrap_or_default()
                }
                InputPeer::Channel(c) => {
                    let input_ch = tl_e::InputChannel::InputChannel(tl_t::InputChannel {
                        channel_id: c.channel_id,
                        access_hash: c.access_hash,
                    });
                    let resp = self
                        .invoke_takeout(
                            0,
                            tl_f::channels::GetChannels {
                                id: vec![input_ch.clone()],
                            },
                        )
                        .await
                        .map_err(|e| e.to_api_error())?;
                    let chats = match &resp {
                        tl_e::messages::Chats::Chats(ch) => &ch.chats,
                        tl_e::messages::Chats::Slice(ch) => &ch.chats,
                    };
                    let mut dialogs = chats
                        .first()
                        .map(|ch| parse_single_chat_dialogs_info(ch, splits_count))
                        .unwrap_or_default();
                    if let Some(target) = dialogs.chats.first_mut()
                        && let Ok(tl_e::messages::ChatFull::ChatFull(full)) = self
                            .invoke_takeout(0, tl_f::channels::GetFullChannel { channel: input_ch })
                            .await
                        && let tl_e::ChatFull::ChannelFull(cf) = full.full_chat
                        && let Some(migrated_chat_id) = cf.migrated_from_chat_id
                    {
                        let from_info = DialogInfo {
                            peer_id: PeerId::from_chat(ChatId(migrated_chat_id as u64)),
                            input: InputPeer::Chat(tl_t::InputPeerChat {
                                chat_id: migrated_chat_id,
                            }),
                            ..Default::default()
                        };
                        for s in 0..splits_count {
                            add_migrate_from_slice(target, &from_info, s, splits_count);
                        }
                    }
                    dialogs
                }
                _ => DialogsInfo::default(),
            };
            finalize_dialogs_info(&mut info, &self.settings);
            let _ = on_progress(info.total_count() as i32);
            return Ok(info);
        }

        let mut by_peer: BTreeMap<PeerId, DialogInfo> = BTreeMap::new();
        let mut order: Vec<PeerId> = Vec::new();
        let mut migrated_chats: Vec<(DialogInfo, i32)> = Vec::new();

        // `export_api_wrap.cpp:2109-2158` (`ApiWrap::requestDialogsSlice`):
        // traverse splits newest-to-oldest (or only the newest split when `use_only_last_split`).
        let first_split = if self.use_only_last_split() {
            splits_count - 1
        } else {
            0
        };
        for split_idx in (first_split..splits_count).rev() {
            let mut offset_date = 0_i32;
            let mut offset_id = 0_i32;
            let mut offset_peer = InputPeer::Empty;

            loop {
                if self.is_cancelled() {
                    break;
                }
                let req = tl_f::messages::GetDialogs {
                    exclude_pinned: false,
                    folder_id: None,
                    offset_date,
                    offset_id,
                    offset_peer: offset_peer.clone(),
                    limit: DIALOGS_SLICE_LIMIT,
                    hash: 0,
                };
                let resp = self
                    .invoke_split(split_idx, req)
                    .await
                    .map_err(|e| e.to_api_error())?;
                let (dialogs, messages, users, chats, is_full) = match &resp {
                    tl_e::messages::Dialogs::Dialogs(d) => (
                        &d.dialogs[..],
                        &d.messages[..],
                        &d.users[..],
                        &d.chats[..],
                        true,
                    ),
                    tl_e::messages::Dialogs::Slice(d) => (
                        &d.dialogs[..],
                        &d.messages[..],
                        &d.users[..],
                        &d.chats[..],
                        d.dialogs.is_empty(),
                    ),
                    tl_e::messages::Dialogs::NotModified(_) => {
                        return Err(ApiError::new(
                            0,
                            "API_ERROR",
                            "Unexpected dialogsNotModified received.",
                        ));
                    }
                };
                if dialogs.is_empty() {
                    break;
                }
                let parsed = parse_dialogs_info(dialogs, messages, users, chats, split_idx);
                let last_top_date = parsed.last().map_or(0, |last| {
                    offset_date = last.top_message_date;
                    offset_id = last.top_message_id;
                    offset_peer = last.input.clone();
                    last.top_message_date
                });
                for item in parsed {
                    if !item.migrated_to_channel_id.is_zero() {
                        migrated_chats.push((item, split_idx));
                        continue;
                    }
                    if !is_dialog_included(item.kind, &self.settings) {
                        continue;
                    }
                    if let Some(existing) = by_peer.get_mut(&item.peer_id) {
                        if !existing.splits.contains(&split_idx) {
                            existing.splits.push(split_idx);
                            existing.messages_count_per_split.push(0);
                        }
                    } else {
                        order.push(item.peer_id);
                        by_peer.insert(item.peer_id, item);
                    }
                }
                if !on_progress(by_peer.len() as i32) {
                    return Ok(DialogsInfo::default());
                }
                if is_full || last_top_date <= 0 {
                    break;
                }
            }
        }

        for (mut from, split_idx) in migrated_chats {
            let target_peer_id = PeerId::from_channel(from.migrated_to_channel_id);
            if let Some(target) = by_peer.get_mut(&target_peer_id) {
                from.splits.clear();
                from.messages_count_per_split.clear();
                add_migrate_from_slice(target, &from, split_idx, splits_count);
            }
        }

        let mut info = DialogsInfo::default();
        for peer_id in order {
            if let Some(d) = by_peer.remove(&peer_id) {
                info.chats.push(d);
            }
        }

        if self.settings.types.intersects(
            ExportTypes::PRIVATE_GROUPS
                | ExportTypes::PUBLIC_GROUPS
                | ExportTypes::PRIVATE_CHANNELS
                | ExportTypes::PUBLIC_CHANNELS,
        ) {
            let mut offset = 0_i32;
            loop {
                if self.is_cancelled() {
                    break;
                }
                let resp = self
                    .invoke_takeout(0, tl_f::channels::GetLeftChannels { offset })
                    .await
                    .map_err(|e| e.to_api_error())?;
                let (chats, done) = match &resp {
                    tl_e::messages::Chats::Chats(c) => (&c.chats[..], true),
                    tl_e::messages::Chats::Slice(c) => (&c.chats[..], c.chats.is_empty()),
                };
                if chats.is_empty() {
                    break;
                }
                let parsed = parse_left_channels_info(chats);
                offset += chats.len() as i32;
                for item in parsed {
                    if is_dialog_included(item.kind, &self.settings) {
                        info.left.push(item);
                    }
                }
                if !on_progress(info.total_count() as i32) {
                    return Ok(DialogsInfo::default());
                }
                if done {
                    break;
                }
            }
        }

        finalize_dialogs_info(&mut info, &self.settings);
        Ok(info)
    }

    async fn request_personal_info(&mut self) -> Result<PersonalInfo, ApiError> {
        let resp = self
            .invoke_takeout(
                0,
                tl_f::users::GetFullUser {
                    id: InputUser::Self_,
                },
            )
            .await
            .map_err(|e| e.to_api_error())?;
        Ok(parse_personal_info(&resp))
    }

    async fn request_userpics(
        &mut self,
        on_event: &mut (dyn FnMut(StreamEvent<UserpicsInfo, UserpicsSlice>) -> bool + Send),
    ) -> Result<(), ApiError> {
        if !on_event(StreamEvent::Start(UserpicsInfo {
            count: self.userpics_count,
        })) {
            return Ok(());
        }
        if self.userpics_count == 0 {
            return Ok(());
        }
        // `export_api_wrap.cpp:1183-1274` (`requestUserpics` / `finishUserpicsSlice`):
        // paginate with `offset = 0` and `max_id = last_photo.id`.
        let mut max_id = 0_i64;
        let mut total_processed = 0usize;
        loop {
            if self.is_cancelled() {
                return Ok(());
            }
            let resp = self
                .invoke_takeout(
                    0,
                    tl_f::photos::GetUserPhotos {
                        user_id: InputUser::Self_,
                        offset: 0,
                        max_id,
                        limit: USERPICS_SLICE_LIMIT,
                    },
                )
                .await
                .map_err(|e| e.to_api_error())?;
            let (photos, done) = match &resp {
                tl_e::photos::Photos::Photos(p) => (&p.photos[..], true),
                tl_e::photos::Photos::Slice(p) => (&p.photos[..], p.photos.is_empty()),
            };
            if photos.is_empty() {
                break;
            }
            let mut slice = parse_userpics_slice(photos, total_processed, self.utc_offset_secs);
            let next_max_id = slice.list.last().map_or(0, |p| p.id as i64);
            let stalled = next_max_id == 0 || next_max_id == max_id;
            max_id = next_max_id;
            total_processed += slice.list.len();
            for (idx, photo) in slice.list.iter_mut().enumerate() {
                let origin = FileOrigin::default();
                let mut progress_cb = |p: DownloadProgress| on_event(StreamEvent::Progress(p));
                self.download_file(
                    &mut photo.image.file,
                    DownloadKind::Always,
                    &origin,
                    idx as i32,
                    &mut progress_cb,
                )
                .await?;
            }
            if !slice.list.is_empty() && !on_event(StreamEvent::Slice(slice)) {
                return Ok(());
            }
            if done || stalled {
                break;
            }
        }
        Ok(())
    }

    async fn request_stories(
        &mut self,
        on_event: &mut (dyn FnMut(StreamEvent<StoriesInfo, StoriesSlice>) -> bool + Send),
    ) -> Result<(), ApiError> {
        if !on_event(StreamEvent::Start(StoriesInfo {
            count: self.stories_count,
        })) {
            return Ok(());
        }
        if self.stories_count == 0 {
            return Ok(());
        }
        let mut offset_id = 0_i32;
        let mut total_processed = 0usize;
        loop {
            if self.is_cancelled() {
                return Ok(());
            }
            let resp = self
                .invoke_takeout(
                    0,
                    tl_f::stories::GetStoriesArchive {
                        peer: InputPeer::Self_,
                        offset_id,
                        limit: STORIES_SLICE_LIMIT,
                    },
                )
                .await
                .map_err(|e| e.to_api_error())?;
            let tl_e::stories::Stories::Stories(s) = resp;
            if s.stories.is_empty() {
                break;
            }
            let mut slice = parse_stories_slice(&s.stories, total_processed, self.utc_offset_secs);
            let done = s.stories.len() < STORIES_SLICE_LIMIT as usize || slice.last_id == offset_id;
            offset_id = slice.last_id;
            total_processed += slice.list.len();
            for (idx, story) in slice.list.iter_mut().enumerate() {
                let origin = FileOrigin {
                    story_id: story.id,
                    ..Default::default()
                };
                let mut progress_cb = |p: DownloadProgress| on_event(StreamEvent::Progress(p));
                if let Some(f) = story.media.file_mut() {
                    self.download_file(
                        f,
                        DownloadKind::Always,
                        &origin,
                        idx as i32,
                        &mut progress_cb,
                    )
                    .await?;
                }
                if let Some(thumb) = story.media.thumb_mut()
                    && (thumb.file.location.is_valid() || !thumb.file.content.is_empty())
                {
                    self.download_file(
                        &mut thumb.file,
                        DownloadKind::Always,
                        &origin,
                        idx as i32,
                        &mut progress_cb,
                    )
                    .await?;
                }
            }
            if !on_event(StreamEvent::Slice(slice)) {
                return Ok(());
            }
            if done {
                break;
            }
        }
        Ok(())
    }

    async fn request_profile_music(
        &mut self,
        on_event: &mut (dyn FnMut(StreamEvent<ProfileMusicInfo, ProfileMusicSlice>) -> bool + Send),
    ) -> Result<(), ApiError> {
        if !on_event(StreamEvent::Start(ProfileMusicInfo {
            count: self.profile_music_count,
        })) {
            return Ok(());
        }
        if self.profile_music_count == 0 {
            return Ok(());
        }
        let mut offset = 0_i32;
        let mut track_idx = 0usize;
        loop {
            if self.is_cancelled() {
                return Ok(());
            }
            let resp = self
                .invoke_takeout(
                    0,
                    tl_f::users::GetSavedMusic {
                        id: InputUser::Self_,
                        offset,
                        limit: PROFILE_MUSIC_SLICE_LIMIT,
                        hash: 0,
                    },
                )
                .await
                .map_err(|e| e.to_api_error())?;
            let tl_e::users::SavedMusic::SavedMusic(m) = resp else {
                break;
            };
            if m.documents.is_empty() {
                break;
            }
            let done = m.documents.len() < PROFILE_MUSIC_SLICE_LIMIT as usize;
            offset += m.documents.len() as i32;
            let mut list = Vec::with_capacity(m.documents.len());
            for (idx, d) in m.documents.iter().enumerate() {
                let mut doc = parse_document(d, false);
                if doc.id == 0 {
                    continue;
                }
                doc.is_audio_file = true;
                track_idx += 1;
                doc.file.suggested_path = format!(
                    "profile_music/audio_{}{}.mp3",
                    track_idx,
                    prepare_file_name_date_part(doc.date, self.utc_offset_secs)
                );
                let origin = FileOrigin::default();
                let mut progress_cb = |p: DownloadProgress| on_event(StreamEvent::Progress(p));
                self.download_file(
                    &mut doc.file,
                    DownloadKind::Always,
                    &origin,
                    idx as i32,
                    &mut progress_cb,
                )
                .await?;
                let mut msg = Message {
                    id: track_idx as i32,
                    date: doc.date,
                    self_id: self.self_id,
                    ..Default::default()
                };
                msg.media.content = MediaContent::Document(Box::new(doc));
                list.push(msg);
            }
            if !on_event(StreamEvent::Slice(ProfileMusicSlice { list, skipped: 0 })) {
                return Ok(());
            }
            if done {
                break;
            }
        }
        Ok(())
    }

    async fn request_contacts(&mut self) -> Result<ContactsList, ApiError> {
        let saved = self
            .invoke_takeout(0, tl_f::contacts::GetSaved)
            .await
            .map_err(|e| e.to_api_error())?;
        let mut contacts = parse_contacts_list(&saved);

        let mut offset = 0_i32;
        loop {
            if self.is_cancelled() {
                break;
            }
            let req = tl_f::contacts::GetTopPeers {
                correspondents: true,
                bots_pm: false,
                bots_inline: true,
                phone_calls: true,
                forward_users: false,
                forward_chats: false,
                groups: false,
                channels: false,
                bots_app: false,
                bots_guestchat: false,
                offset,
                limit: TOP_PEERS_SLICE_LIMIT,
                hash: 0,
            };
            let resp = self
                .invoke_takeout(0, req)
                .await
                .map_err(|e| e.to_api_error())?;
            // `export_api_wrap.cpp:1749-1778` (`ApiWrap::requestTopPeersSlice`): check
            // `offset + category.peers.len() >= category.count` across all categories and advance
            // `offset = max(correspondents, inline_bots, phone_calls)`.
            let loaded = match &resp {
                tl_e::contacts::TopPeers::TopPeers(tp) => tp.categories.iter().all(|c| {
                    let tl_e::TopPeerCategoryPeers::TopPeerCategoryPeers(cp) = c;
                    (offset as usize) + cp.peers.len() >= (cp.count.max(0) as usize)
                }),
                tl_e::contacts::TopPeers::NotModified | tl_e::contacts::TopPeers::Disabled => true,
            };
            let appended = append_top_peers(&mut contacts, &resp);
            if !appended || loaded {
                break;
            }
            let next_offset = contacts
                .correspondents
                .len()
                .max(contacts.inline_bots.len())
                .max(contacts.phone_calls.len()) as i32;
            if next_offset <= offset {
                break;
            }
            offset = next_offset;
        }

        Ok(contacts)
    }

    async fn request_sessions(&mut self) -> Result<SessionsList, ApiError> {
        let auths = self
            .invoke_takeout(0, tl_f::account::GetAuthorizations)
            .await
            .map_err(|e| e.to_api_error())?;
        let web_auths = self
            .invoke_takeout(0, tl_f::account::GetWebAuthorizations)
            .await
            .map_err(|e| e.to_api_error())?;
        Ok(build_sessions_list(&auths, &web_auths))
    }

    async fn request_other_data(&mut self, relative_path: &str) -> Result<File, ApiError> {
        let mut file = File {
            location: FileLocation {
                dc_id: 0,
                data: Some(InputFileLocation::InputTakeoutFileLocation),
            },
            suggested_path: relative_path.to_string(),
            ..Default::default()
        };
        let rel_final = OutputFile::prepare_relative_path(&self.settings.path, relative_path);
        let full_path = format!("{}{rel_final}", self.settings.path);
        let mut offset: i64 = 0;
        let mut total_written: i64 = 0;
        let mut out = OutputFile::new(&full_path);

        loop {
            if self.is_cancelled() {
                break;
            }
            // `export_api_wrap.cpp:762-766`: `MTP_flags(0)` (`precise: false, cdn_supported: false`).
            let req = tl_f::upload::GetFile {
                precise: false,
                cdn_supported: false,
                location: InputFileLocation::InputTakeoutFileLocation,
                offset,
                limit: FILE_CHUNK_SIZE,
            };
            match self.invoke_takeout(0, req).await {
                Ok(tl_e::upload::File::File(part)) => {
                    let len = part.bytes.len() as i64;
                    if len > 0 {
                        let res = out.write_block(&part.bytes, None);
                        if res.is_error() {
                            file.skip_reason = SkipReason::Unavailable;
                            return Ok(file);
                        }
                        total_written += len;
                        offset += len;
                    }
                    if len < i64::from(FILE_CHUNK_SIZE) {
                        file.relative_path = rel_final;
                        file.size = total_written;
                        file.skip_reason = SkipReason::None;
                        return Ok(file);
                    }
                }
                Ok(tl_e::upload::File::CdnRedirect(_)) => {
                    file.skip_reason = SkipReason::Unavailable;
                    return Ok(file);
                }
                Err(err) if err.is_takeout_file_empty() => {
                    // Matches `export_api_wrap.cpp:769-776` + `3555-3564`: treat
                    // `TAKEOUT_FILE_EMPTY` as an empty 0-byte file at `rel_final`.
                    let res = out.write_block(&[], None);
                    if res.is_error() {
                        file.skip_reason = SkipReason::Unavailable;
                        return Ok(file);
                    }
                    file.relative_path = rel_final;
                    file.size = total_written;
                    file.skip_reason = SkipReason::None;
                    return Ok(file);
                }
                Err(err) => return Err(err.to_api_error()),
            }
        }
        Ok(file)
    }

    async fn request_messages(
        &mut self,
        dialog: &DialogInfo,
        on_event: &mut (dyn FnMut(StreamEvent<DialogInfo, MessagesSlice>) -> bool + Send),
    ) -> Result<(), ApiError> {
        let mut started = dialog.clone();
        let splits: Vec<i32> = if started.splits.is_empty() {
            vec![0]
        } else {
            started.splits.clone()
        };
        let mut counts = Vec::with_capacity(splits.len());
        let mut effective_only_my = started.only_my_messages;

        for &split_idx in &splits {
            let peer = if split_idx < 0 && !matches!(started.migrated_from_input, InputPeer::Empty)
            {
                &started.migrated_from_input
            } else {
                &started.input
            };
            let (count, switched_only_my) = self
                .query_split_message_count(peer, split_idx, effective_only_my)
                .await?;
            if switched_only_my {
                effective_only_my = true;
            }
            counts.push(count);
        }
        started.only_my_messages = effective_only_my;
        started.messages_count_per_split = counts.clone();

        if !on_event(StreamEvent::Start(started.clone())) {
            return Ok(());
        }

        let mut media_ctx = ParseMediaContext {
            self_peer_id: self.self_id,
            ..Default::default()
        };

        for (&split_idx, &expected_count) in splits.iter().zip(counts.iter()) {
            if expected_count == 0 {
                continue;
            }
            let is_migrated =
                split_idx < 0 && !matches!(started.migrated_from_input, InputPeer::Empty);
            let peer = if is_migrated {
                started.migrated_from_input.clone()
            } else {
                started.input.clone()
            };
            let mut offset_id = 1_i32;
            let mut loaded_in_split = 0_i32;

            loop {
                if self.is_cancelled() {
                    return Ok(());
                }
                // `export_api_wrap.cpp:2312-2315, 2388` (`requestMessagesSlice` /
                // `requestChatMessages`): on the first slice with `single_peer_from > 0`,
                // pass `offset_date = single_peer_from - 1` and `offset_id = 0`.
                let offset_date = if offset_id == 1 && self.settings.single_peer_from > 0 {
                    self.settings.single_peer_from - 1
                } else {
                    0
                };
                let resp = if effective_only_my {
                    let req = make_search_self_request(
                        &peer,
                        offset_id,
                        -MESSAGES_SLICE_LIMIT,
                        MESSAGES_SLICE_LIMIT,
                    );
                    self.invoke_split(split_idx, req)
                        .await
                        .map_err(|e| e.to_api_error())?
                } else {
                    let req = tl_f::messages::GetHistory {
                        peer: peer.clone(),
                        offset_id: if offset_date != 0 { 0 } else { offset_id },
                        offset_date,
                        add_offset: -MESSAGES_SLICE_LIMIT,
                        limit: MESSAGES_SLICE_LIMIT,
                        max_id: 0,
                        min_id: 0,
                        hash: 0,
                    };
                    match self.invoke_split(split_idx, req).await {
                        Ok(r) => r,
                        Err(err)
                            if err.is_channel_private()
                                && matches!(peer, InputPeer::Channel(_)) =>
                        {
                            // `export_api_wrap.cpp:2398-2413` (`ApiWrap::requestChatMessages`):
                            // mid-history `CHANNEL_PRIVATE` switches to `only_my_messages` and
                            // retries via `messages.search(from_id = inputPeerSelf)`.
                            effective_only_my = true;
                            let search_req = make_search_self_request(
                                &peer,
                                offset_id,
                                -MESSAGES_SLICE_LIMIT,
                                MESSAGES_SLICE_LIMIT,
                            );
                            self.invoke_split(split_idx, search_req)
                                .await
                                .map_err(|e| e.to_api_error())?
                        }
                        Err(err) => return Err(err.to_api_error()),
                    }
                };

                let is_non_slice = matches!(resp, tl_e::messages::Messages::Messages(_));
                let (users, chats, raw_msgs) = unpack_messages_response(&resp);
                if raw_msgs.is_empty() {
                    break;
                }
                let mut slice = parse_messages_slice(
                    users,
                    chats,
                    raw_msgs,
                    self.self_id,
                    &started.relative_path,
                );
                let max_id_in_page = slice.list.iter().map(|m| m.id).max().unwrap_or(0);
                let raw_len = raw_msgs.len();
                loaded_in_split += raw_len as i32;

                if !slice.list.is_empty() {
                    let mut progress_cb = |p: DownloadProgress| on_event(StreamEvent::Progress(p));
                    self.process_messages_slice(
                        &mut slice,
                        &started,
                        split_idx,
                        &peer,
                        &mut media_ctx,
                        &mut progress_cb,
                    )
                    .await?;
                    if is_migrated {
                        slice = adjust_migrate_message_ids(slice);
                    }
                    if !on_event(StreamEvent::Slice(slice)) {
                        return Ok(());
                    }
                }

                // `export_api_wrap.cpp:2328-2330`: `messages.messages` (non-slice) is always the
                // final page; for `messagesSlice`/`channelMessages`, only stop early on a short
                // page once we have already loaded `expected_count` messages (avoiding truncation
                // when the server returns a partial slice with more pages remaining).
                let reached_expected = expected_count > 0
                    && loaded_in_split >= expected_count
                    && raw_len < MESSAGES_SLICE_LIMIT as usize;
                if is_non_slice || reached_expected || max_id_in_page < offset_id {
                    break;
                }
                offset_id = max_id_in_page + 1;
            }
        }

        Ok(())
    }

    async fn request_topic_messages(
        &mut self,
        peer_id: PeerId,
        peer: &InputPeer,
        topic_root_id: i32,
        on_event: &mut (dyn FnMut(StreamEvent<i32, MessagesSlice>) -> bool + Send),
    ) -> Result<(), ApiError> {
        let count_resp = self
            .invoke_takeout(
                0,
                tl_f::messages::GetReplies {
                    peer: peer.clone(),
                    msg_id: topic_root_id,
                    offset_id: 1,
                    offset_date: 0,
                    add_offset: -1,
                    limit: 1,
                    max_id: 0,
                    min_id: 0,
                    hash: 0,
                },
            )
            .await
            .map_err(|e| e.to_api_error())?;
        let total = messages_response_count(&count_resp) + 1;
        if !on_event(StreamEvent::Start(total)) {
            return Ok(());
        }

        let topic_dialog = DialogInfo {
            peer_id,
            input: peer.clone(),
            relative_path: String::new(),
            ..Default::default()
        };
        let mut media_ctx = ParseMediaContext {
            self_peer_id: self.self_id,
            ..Default::default()
        };

        if let InputPeer::Channel(ch) = peer {
            let root_resp = self
                .invoke_takeout(
                    0,
                    tl_f::channels::GetMessages {
                        channel: tl_e::InputChannel::InputChannel(tl_t::InputChannel {
                            channel_id: ch.channel_id,
                            access_hash: ch.access_hash,
                        }),
                        id: vec![tl_e::InputMessage::ID(tl_t::InputMessageID {
                            id: topic_root_id,
                        })],
                    },
                )
                .await
                .map_err(|e| e.to_api_error())?;
            let (users, chats, raw_msgs) = unpack_messages_response(&root_resp);
            let mut slice = parse_messages_slice(users, chats, raw_msgs, self.self_id, "");
            if !slice.list.is_empty() {
                let mut progress_cb = |p: DownloadProgress| on_event(StreamEvent::Progress(p));
                self.process_messages_slice(
                    &mut slice,
                    &topic_dialog,
                    0,
                    peer,
                    &mut media_ctx,
                    &mut progress_cb,
                )
                .await?;
                if !on_event(StreamEvent::Slice(slice)) {
                    return Ok(());
                }
            }
        }

        let mut offset_id = 1_i32;
        loop {
            if self.is_cancelled() {
                return Ok(());
            }
            let resp = self
                .invoke_takeout(
                    0,
                    tl_f::messages::GetReplies {
                        peer: peer.clone(),
                        msg_id: topic_root_id,
                        offset_id,
                        offset_date: 0,
                        add_offset: -MESSAGES_SLICE_LIMIT,
                        limit: MESSAGES_SLICE_LIMIT,
                        max_id: 0,
                        min_id: 0,
                        hash: 0,
                    },
                )
                .await
                .map_err(|e| e.to_api_error())?;
            let (users, chats, raw_msgs) = unpack_messages_response(&resp);
            if raw_msgs.is_empty() {
                break;
            }
            let mut slice = parse_messages_slice(users, chats, raw_msgs, self.self_id, "");
            let max_id = slice.list.iter().map(|m| m.id).max().unwrap_or(0);
            let raw_len = raw_msgs.len();
            if !slice.list.is_empty() {
                let mut progress_cb = |p: DownloadProgress| on_event(StreamEvent::Progress(p));
                self.process_messages_slice(
                    &mut slice,
                    &topic_dialog,
                    0,
                    peer,
                    &mut media_ctx,
                    &mut progress_cb,
                )
                .await?;
                if !on_event(StreamEvent::Slice(slice)) {
                    return Ok(());
                }
            }
            if raw_len < MESSAGES_SLICE_LIMIT as usize || max_id < offset_id {
                break;
            }
            offset_id = max_id + 1;
        }
        Ok(())
    }

    async fn finish_export(&mut self) -> Result<(), ApiError> {
        if let Some(takeout_id) = self.takeout_id.take() {
            let wrapped = tl_f::InvokeWithTakeout {
                takeout_id,
                query: tl_f::account::FinishTakeoutSession { success: true },
            };
            let _ = self
                .rpc
                .invoke(0, &wrapped)
                .await
                .map_err(|e| e.to_api_error())?;
        }
        Ok(())
    }

    async fn finish_export_cancelled(&mut self) -> Result<(), ApiError> {
        self.finish_takeout_cancelled().await
    }

    fn cancel_export_fast(&mut self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    fn skip_file(&mut self, random_id: u64) {
        self.skipped_random_id.store(random_id, Ordering::SeqCst);
    }
}

fn make_search_self_request(
    peer: &InputPeer,
    offset_id: i32,
    add_offset: i32,
    limit: i32,
) -> tl_f::messages::Search {
    tl_f::messages::Search {
        peer: peer.clone(),
        q: String::new(),
        from_id: Some(InputPeer::Self_),
        saved_peer_id: None,
        saved_reaction: None,
        top_msg_id: None,
        filter: tl_e::MessagesFilter::InputMessagesFilterEmpty,
        min_date: 0,
        max_date: 0,
        offset_id,
        add_offset,
        limit,
        max_id: 0,
        min_id: 0,
        hash: 0,
    }
}

fn unpack_messages_response(
    resp: &tl_e::messages::Messages,
) -> (&[tl_e::User], &[tl_e::Chat], &[tl_e::Message]) {
    match resp {
        tl_e::messages::Messages::Messages(m) => (&m.users, &m.chats, &m.messages),
        tl_e::messages::Messages::Slice(m) => (&m.users, &m.chats, &m.messages),
        tl_e::messages::Messages::ChannelMessages(m) => (&m.users, &m.chats, &m.messages),
        tl_e::messages::Messages::NotModified(_) => (&[], &[], &[]),
    }
}

fn messages_response_count(resp: &tl_e::messages::Messages) -> i32 {
    match resp {
        tl_e::messages::Messages::Messages(m) => m.messages.len() as i32,
        tl_e::messages::Messages::Slice(m) => m.count,
        tl_e::messages::Messages::ChannelMessages(m) => m.count,
        tl_e::messages::Messages::NotModified(m) => m.count,
    }
}

fn dialogs_response_count(resp: &tl_e::messages::Dialogs) -> i32 {
    match resp {
        tl_e::messages::Dialogs::Dialogs(d) => d.dialogs.len() as i32,
        tl_e::messages::Dialogs::Slice(d) => d.count,
        tl_e::messages::Dialogs::NotModified(d) => d.count,
    }
}

fn refresh_message_file_reference(target: &mut FileLocation, msg: &Message) -> bool {
    if let Some(f) = msg.media.file()
        && refresh_file_reference(target, &f.location)
    {
        return true;
    }
    if let Some(t) = msg.media.thumb()
        && refresh_file_reference(target, &t.file.location)
    {
        return true;
    }
    match &msg.action {
        ServiceAction::ChatEditPhoto { photo } | ServiceAction::SuggestProfilePhoto { photo }
            if refresh_file_reference(target, &photo.image.file.location) =>
        {
            return true;
        }
        _ => {}
    }
    false
}

fn collect_message_custom_emojis(
    msg: &Message,
    cache: &HashMap<u64, String>,
    seen: &mut HashSet<u64>,
    out: &mut Vec<u64>,
) {
    for part in &msg.text {
        if part.kind == TextPartType::CustomEmoji
            && let Ok(id) = part.additional.parse::<u64>()
            && !cache.contains_key(&id)
            && seen.insert(id)
        {
            out.push(id);
        }
    }
    for r in &msg.reactions {
        if r.kind == ReactionType::CustomEmoji
            && let Ok(id) = r.document_id.parse::<u64>()
            && !cache.contains_key(&id)
            && seen.insert(id)
        {
            out.push(id);
        }
    }
    if let Some(ref rich) = msg.rich_message {
        collect_rich_message_custom_emojis(rich, cache, seen, out);
    }
}

fn collect_rich_message_custom_emojis(
    rich: &RichMessage,
    cache: &HashMap<u64, String>,
    seen: &mut HashSet<u64>,
    out: &mut Vec<u64>,
) {
    for b in &rich.blocks {
        collect_rich_block_custom_emojis(b, cache, seen, out);
    }
}

fn collect_rich_block_custom_emojis(
    block: &RichBlock,
    cache: &HashMap<u64, String>,
    seen: &mut HashSet<u64>,
    out: &mut Vec<u64>,
) {
    collect_rich_text_custom_emojis(&block.text, cache, seen, out);
    collect_rich_text_custom_emojis(&block.quote_caption, cache, seen, out);
    collect_rich_text_custom_emojis(&block.caption.text, cache, seen, out);
    collect_rich_text_custom_emojis(&block.caption.credit, cache, seen, out);
    for child in &block.blocks {
        collect_rich_block_custom_emojis(child, cache, seen, out);
    }
    for item in &block.list_items {
        if let Some(ref t) = item.text {
            collect_rich_text_custom_emojis(t, cache, seen, out);
        }
        for child in &item.blocks {
            collect_rich_block_custom_emojis(child, cache, seen, out);
        }
    }
    for row in &block.table_rows {
        for cell in &row.cells {
            if let Some(ref t) = cell.text {
                collect_rich_text_custom_emojis(t, cache, seen, out);
            }
        }
    }
    for btn in &block.buttons {
        collect_rich_text_custom_emojis(btn, cache, seen, out);
    }
}

fn collect_rich_text_custom_emojis(
    text: &RichText,
    cache: &HashMap<u64, String>,
    seen: &mut HashSet<u64>,
    out: &mut Vec<u64>,
) {
    if text.kind == RichTextType::CustomEmoji
        && text.id != 0
        && !cache.contains_key(&text.id)
        && seen.insert(text.id)
    {
        out.push(text.id);
    }
    for child in &text.children {
        collect_rich_text_custom_emojis(child, cache, seen, out);
    }
    for child in &text.old_children {
        collect_rich_text_custom_emojis(child, cache, seen, out);
    }
}

fn apply_message_custom_emojis(msg: &mut Message, cache: &HashMap<u64, String>) {
    for part in &mut msg.text {
        if part.kind == TextPartType::CustomEmoji
            && let Ok(id) = part.additional.parse::<u64>()
            && let Some(path) = cache.get(&id)
        {
            part.additional = path.clone();
        }
    }
    for r in &mut msg.reactions {
        if r.kind == ReactionType::CustomEmoji
            && let Ok(id) = r.document_id.parse::<u64>()
            && let Some(path) = cache.get(&id)
        {
            r.document_id = path.clone();
        }
    }
    if let Some(ref mut rich) = msg.rich_message {
        for b in &mut rich.blocks {
            apply_rich_block_custom_emojis(b, cache);
        }
    }
}

fn apply_rich_block_custom_emojis(block: &mut RichBlock, cache: &HashMap<u64, String>) {
    apply_rich_text_custom_emojis(&mut block.text, cache);
    apply_rich_text_custom_emojis(&mut block.quote_caption, cache);
    apply_rich_text_custom_emojis(&mut block.caption.text, cache);
    apply_rich_text_custom_emojis(&mut block.caption.credit, cache);
    for child in &mut block.blocks {
        apply_rich_block_custom_emojis(child, cache);
    }
    for item in &mut block.list_items {
        if let Some(ref mut t) = item.text {
            apply_rich_text_custom_emojis(t, cache);
        }
        for child in &mut item.blocks {
            apply_rich_block_custom_emojis(child, cache);
        }
    }
    for row in &mut block.table_rows {
        for cell in &mut row.cells {
            if let Some(ref mut t) = cell.text {
                apply_rich_text_custom_emojis(t, cache);
            }
        }
    }
    for btn in &mut block.buttons {
        apply_rich_text_custom_emojis(btn, cache);
    }
}

fn apply_rich_text_custom_emojis(text: &mut RichText, cache: &HashMap<u64, String>) {
    if text.kind == RichTextType::CustomEmoji
        && text.id != 0
        && let Some(path) = cache.get(&text.id)
    {
        text.custom_emoji_data = path.clone();
    }
    for child in &mut text.children {
        apply_rich_text_custom_emojis(child, cache);
    }
    for child in &mut text.old_children {
        apply_rich_text_custom_emojis(child, cache);
    }
}
