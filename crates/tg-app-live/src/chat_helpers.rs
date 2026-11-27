//! Live implementation of [`ChatHelpersBackend`], `SettingsBackend` sticker-set
//! management, and `HistoryBackend::chat_mention_candidates` on [`LiveSession`].
//!
//! Port origin:
//! - `Telegram/SourceFiles/data/stickers/data_stickers.cpp`
//! - `Telegram/SourceFiles/data/stickers/data_custom_emoji.cpp`
//! - `Telegram/SourceFiles/api/api_hash.cpp`
//! - `Telegram/SourceFiles/apiwrap.cpp` (stickers, GIFs, featured & custom-emoji RPCs)
//! - `Telegram/SourceFiles/chat_helpers/stickers_list_widget.cpp`
//! - `Telegram/SourceFiles/chat_helpers/gifs_list_widget.cpp`
//! - `Telegram/SourceFiles/chat_helpers/field_autocomplete.cpp`

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tg_api::RpcClient;
use tg_api::convert::{apply_tl_chat_full, apply_tl_users_and_chats, input_peer_for};
use tg_chat::autocomplete::RecentHashtags;
use tg_chat::emoji::{EmojiKeywordResult, EmojiVariants, RecentEmojiId, SkinTone, strip_skin_tone};
use tg_chat::stickers::{
    CLOUD_RECENT_ATTACHED_SET_ID, CLOUD_RECENT_SET_ID, CUSTOM_SET_ID, DEFAULT_SET_ID,
    EmojiSearchOptions, FAVED_SET_ID, PanelBuildOptions, STICKERS_UPDATE_TIMEOUT_MS,
    StickerDocument, StickerPanelSection, StickerSetUpdate, StickerTabIcon, StickerType,
    StickersSet, StickersSetFlags, StickersStore, StickersType,
    thumbnail_type_from_photo_size_type,
};
use tg_chat::{
    BotCommand, DocumentId as ChatDocId, Size, StickerSetId, UserId as ChatUserId, UserInfo,
};
use tg_data::{ChannelId, ChatId, PeerId as DataPeerId, Session, UserId as DataUserId};
use tg_text::emoji as tg_emoji_db;
use tg_tl::{enums as tl_enums, functions as tl_fns, types as tl_types};
use tracing::debug;

use crate::bridge::LiveBridgeState;
use crate::convert::{
    avatar_rgb_for_color_index, history_messages_ordered, known_peer_ids, now_unix_i32,
    to_data_peer,
};
use crate::media::{block_on_runtime, download_file_bytes_async, with_rpc_timeout};
use crate::session::LiveSession;
use tg_app_core::chat_helpers::backend::{
    AttachedStickerSetSummary, AttachedStickersMedia, ChatHelpersBackend, DefaultEmojiCatalog,
    DefaultEmojiCatalogKind, DefaultTagReactionsResult, EmojiGroupInfo, GifCategory, GifItem,
    OldFeaturedStickersPage, PaidReactionPrivacy, SearchStickerSetsResult, SearchStickersPage,
    SearchStickersRequest, StickerMedia,
};
use tg_app_core::core_types::dto::rich_compose::MentionCandidate;
use tg_app_core::core_types::erased_rpc::ErasedRpcClient;
use tg_app_core::dialogs::backend::DialogsBackend;
use tg_app_core::settings::backend::StickerSetInfo;
use tg_app_core::stand_in::types::{MsgId, PeerId};
use tg_session::SessionRuntime;

/// Default RPC timeout for chat-helper requests.
const CHAT_HELPERS_RPC_TIMEOUT: Duration = Duration::from_secs(10);

/// Default inline-bot username for GIF search (`mtproto_config.h`: `gifSearchUsername`).
pub const DEFAULT_GIF_SEARCH_USERNAME: &str = "gif";

/// Maximum document IDs per `messages.getCustomEmojiDocuments` batch (`data_custom_emoji.cpp`: `kMaxPerRequest = 100`).
pub const CUSTOM_EMOJI_MAX_PER_REQUEST: usize = 100;

/// Cache TTL for `messages.getStickers` by emoji (`kStickersByEmojiInvalidateTimeout` = 1 hour).
pub const STICKERS_BY_EMOJI_TIMEOUT_MS: i64 = STICKERS_UPDATE_TIMEOUT_MS;

/// Cache TTL for `channels.getParticipants` mention candidates (5 minutes).
pub const PARTICIPANTS_CACHE_TTL_MS: i64 = 300_000;

/// Default cache TTL for `@gif` inline query results when server returns `cache_time <= 0` (60s).
const DEFAULT_INLINE_GIF_CACHE_TTL_MS: i64 = 60_000;

/// Retry backoff after a failed sticker/GIF refresh RPC (30s) to avoid per-frame RPC storms.
const FAILED_REFRESH_BACKOFF_MS: i64 = 30_000;

fn failed_refresh_mark(now_ms: i64) -> i64 {
    (now_ms - STICKERS_UPDATE_TIMEOUT_MS + FAILED_REFRESH_BACKOFF_MS).max(1)
}

/// Maximum number of `StickerDocument` entries stored in [`StickerDocPool`] (256 * 128 = 32,768).
const DOC_POOL_BUCKETS: usize = 256;
const DOC_POOL_BUCKET_SIZE: usize = 128;

// =============================================================================
// 1. Telegram 64-bit List Hash Functions (`api/api_hash.h` / `api/api_hash.cpp`)
//    Reuses `tg_api::hash` primitives directly.
// =============================================================================

/// Initializes a Telegram 64-bit rolling list hash (`Api::HashInit`).
#[inline]
#[must_use]
pub const fn hash_init() -> u64 {
    tg_api::hash::hash_init()
}

/// Updates a Telegram 64-bit rolling hash with an unsigned 64-bit value (`Api::HashUpdate`).
#[inline]
pub fn hash_update_u64(already: &mut u64, value: u64) {
    tg_api::hash::hash_update_u64(already, value);
}

/// Updates a Telegram 64-bit rolling hash with a signed 64-bit value.
#[inline]
pub fn hash_update_i64(already: &mut u64, value: i64) {
    tg_api::hash::hash_update_i64(already, value);
}

/// Updates a Telegram 64-bit rolling hash with a signed 32-bit value (sign-extended to 64-bit).
#[inline]
pub fn hash_update_i32(already: &mut u64, value: i32) {
    tg_api::hash::hash_update_i32(already, value);
}

/// Computes the 64-bit rolling hash over a slice of document IDs (`CountDocumentVectorHash`).
#[must_use]
pub fn count_document_vector_hash(docs: &[ChatDocId]) -> u64 {
    let raw: Vec<u64> = docs.iter().map(|d| d.0).collect();
    tg_api::hash::count_document_vector_hash(&raw)
}

/// Computes the 64-bit hash over a sticker-set order list (`CountStickersOrderHash`).
#[must_use]
pub fn count_stickers_order_hash(
    store: &StickersStore,
    order: &[StickerSetId],
    check_outdated_info: bool,
) -> u64 {
    let mut result = hash_init();
    let mut found_outdated = false;
    for &set_id in order {
        if let Some(set) = store.sets.get(&set_id) {
            if set.id == DEFAULT_SET_ID {
                found_outdated = true;
            } else if !set.flags.contains(StickersSetFlags::SPECIAL)
                && !set.flags.contains(StickersSetFlags::ARCHIVED)
            {
                hash_update_u64(&mut result, set.hash);
            }
        }
    }
    if !check_outdated_info || !found_outdated {
        result
    } else {
        0
    }
}

/// Computes the hash for `messages.getAllStickers` (`Api::CountStickersHash`).
#[must_use]
pub fn count_stickers_hash(store: &StickersStore, check_outdated_info: bool) -> u64 {
    count_stickers_order_hash(store, &store.sets_order, check_outdated_info)
}

/// Computes the hash for `messages.getMaskStickers` (`Api::CountMasksHash`).
#[must_use]
pub fn count_masks_hash(store: &StickersStore, check_outdated_info: bool) -> u64 {
    count_stickers_order_hash(store, &store.mask_sets_order, check_outdated_info)
}

/// Computes the hash for `messages.getEmojiStickers` (`Api::CountCustomEmojiHash`).
#[must_use]
pub fn count_custom_emoji_hash(store: &StickersStore, check_outdated_info: bool) -> u64 {
    count_stickers_order_hash(store, &store.emoji_sets_order, check_outdated_info)
}

/// Computes the hash for `messages.getRecentStickers` (`Api::CountRecentStickersHash`).
#[must_use]
pub fn count_recent_stickers_hash(store: &StickersStore, attached: bool) -> u64 {
    let set_id = if attached {
        CLOUD_RECENT_ATTACHED_SET_ID
    } else {
        CLOUD_RECENT_SET_ID
    };
    store
        .sets
        .get(&set_id)
        .map_or(0, |s| count_document_vector_hash(&s.stickers))
}

/// Computes the hash for `messages.getFavedStickers` (`Api::CountFavedStickersHash`).
#[must_use]
pub fn count_faved_stickers_hash(store: &StickersStore) -> u64 {
    store
        .sets
        .get(&FAVED_SET_ID)
        .map_or(0, |s| count_document_vector_hash(&s.stickers))
}

fn count_featured_hash(store: &StickersStore, order: &[StickerSetId]) -> u64 {
    let mut result = hash_init();
    for &set_id in order {
        hash_update_u64(&mut result, set_id.0);
        if store
            .sets
            .get(&set_id)
            .is_some_and(|s| s.flags.contains(StickersSetFlags::UNREAD))
        {
            hash_update_u64(&mut result, 1);
        }
    }
    result
}

/// Computes the hash for `messages.getFeaturedStickers` (`Api::CountFeaturedStickersHash`).
#[must_use]
pub fn count_featured_stickers_hash(store: &StickersStore) -> u64 {
    count_featured_hash(store, &store.featured_sets_order)
}

/// Computes the hash for `messages.getFeaturedEmojiStickers` (`Api::CountFeaturedEmojiHash`).
#[must_use]
pub fn count_featured_emoji_hash(store: &StickersStore) -> u64 {
    count_featured_hash(store, &store.featured_emoji_sets_order)
}

/// Computes the hash for `messages.getSavedGifs` (`Api::CountSavedGifsHash`).
#[must_use]
pub fn count_saved_gifs_hash(store: &StickersStore) -> u64 {
    count_document_vector_hash(&store.saved_gifs)
}

/// Computes a 64-bit hash over a slice of user IDs (for `channels.getParticipants`).
#[must_use]
pub fn count_participant_ids_hash(user_ids: &[u64]) -> u64 {
    let mut result = hash_init();
    for &uid in user_ids {
        hash_update_u64(&mut result, uid);
    }
    result
}

// =============================================================================
// 2. Append-only Safe Pool for `&StickerDocument` Borrowed Lookups
// =============================================================================

struct DocPoolBucket {
    slots: [OnceLock<StickerDocument>; DOC_POOL_BUCKET_SIZE],
}

impl DocPoolBucket {
    fn new() -> Self {
        Self {
            slots: std::array::from_fn(|_| OnceLock::new()),
        }
    }
}

struct DocPoolIndex {
    map: HashMap<ChatDocId, usize>,
    next_slot: usize,
}

/// Thread-safe, leak-free append-only pool enabling `fn sticker_document(&self, id) -> Option<&StickerDocument>`
/// even when sticker documents are lazily fetched through `&self`.
struct StickerDocPool {
    index: Mutex<DocPoolIndex>,
    buckets: Box<[OnceLock<Box<DocPoolBucket>>]>,
}

impl StickerDocPool {
    fn new() -> Self {
        let buckets = std::iter::repeat_with(OnceLock::new)
            .take(DOC_POOL_BUCKETS)
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Self {
            index: Mutex::new(DocPoolIndex {
                map: HashMap::new(),
                next_slot: 0,
            }),
            buckets,
        }
    }

    fn slot_ref(&self, slot: usize) -> Option<&StickerDocument> {
        let bucket = self.buckets.get(slot / DOC_POOL_BUCKET_SIZE)?.get()?;
        bucket.slots[slot % DOC_POOL_BUCKET_SIZE].get()
    }

    fn upsert(&self, doc: StickerDocument) {
        let Ok(mut guard) = self.index.lock() else {
            return;
        };
        if let Some(&existing_slot) = guard.map.get(&doc.id)
            && self.slot_ref(existing_slot) == Some(&doc)
        {
            return;
        }
        let max_slots = DOC_POOL_BUCKETS * DOC_POOL_BUCKET_SIZE;
        if guard.next_slot >= max_slots {
            return;
        }
        let slot = guard.next_slot;
        guard.next_slot += 1;
        let bucket = self.buckets[slot / DOC_POOL_BUCKET_SIZE]
            .get_or_init(|| Box::new(DocPoolBucket::new()));
        let doc_id = doc.id;
        let _ = bucket.slots[slot % DOC_POOL_BUCKET_SIZE].set(doc);
        guard.map.insert(doc_id, slot);
    }

    fn get(&self, id: ChatDocId) -> Option<&StickerDocument> {
        let slot = self.index.lock().ok()?.map.get(&id).copied()?;
        self.slot_ref(slot)
    }
}

// =============================================================================
// 3. Cached Live State (`LiveChatHelpersState`)
// =============================================================================

/// Download and RPC metadata retained for each sticker, custom emoji, or GIF document.
#[derive(Debug, Clone)]
pub struct LiveDocumentMeta {
    /// Document identifier.
    pub id: ChatDocId,
    /// Access hash required for `InputDocument` / `upload.getFile`.
    pub access_hash: i64,
    /// File reference bytes required for `InputDocument` / `upload.getFile`.
    pub file_reference: Vec<u8>,
    /// Hosting datacenter ID.
    pub dc_id: i32,
    /// MIME type string (e.g. `application/x-tgsticker`, `image/webp`, `video/mp4`, `image/gif`).
    pub mime_type: String,
    /// Document byte size.
    pub size: u64,
    /// Sticker format classification.
    pub sticker_type: StickerType,
    /// Whether this document is an animated GIF/MP4 loop.
    pub is_gifv: bool,
    /// Whether this document is a custom emoji.
    pub is_custom_emoji: bool,
    /// Intrinsic width in pixels.
    pub width: u32,
    /// Intrinsic height in pixels.
    pub height: u32,
    /// Duration in seconds (for videos/GIFs).
    pub duration_secs: f64,
    /// File name attribute, if any.
    pub file_name: String,
    /// Preferred thumbnail size letter (e.g. `"m"`, `"s"`, `"i"`).
    pub thumb_size_type: Option<String>,
    /// Inline cached thumbnail bytes from `PhotoCachedSize`, if provided by server.
    pub cached_thumb_bytes: Option<Vec<u8>>,
    /// Stripped JPEG thumbnail bytes from `PhotoStrippedSize`, if provided by server.
    pub stripped_thumb: Option<Vec<u8>>,
}

impl LiveDocumentMeta {
    /// Builds a TL [`tl_enums::InputDocument`] for RPCs (`saveGif`, `faveSticker`, `saveRecentSticker`).
    #[must_use]
    pub fn input_document(&self) -> tl_enums::InputDocument {
        tl_enums::InputDocument::InputDocument(tl_types::InputDocument {
            id: self.id.0 as i64,
            access_hash: self.access_hash,
            file_reference: self.file_reference.clone(),
        })
    }
}

#[derive(Debug, Clone)]
struct StickersByEmojiCacheEntry {
    docs: Vec<ChatDocId>,
    hash: i64,
    received_ms: i64,
}

#[derive(Debug, Clone)]
struct SearchStickerSetsCacheEntry {
    set_ids: Vec<StickerSetId>,
    hash: i64,
    received_ms: i64,
}

#[derive(Debug, Clone)]
struct InlineGifCacheEntry {
    items: Vec<GifItem>,
    next_offset: String,
    fetched_at_ms: i64,
    ttl_ms: i64,
}

#[derive(Debug, Clone)]
struct ResolvedGifBot {
    user_id: i64,
    access_hash: i64,
}

#[derive(Debug, Clone)]
struct ParticipantsCacheEntry {
    user_ids: Vec<u64>,
    hash: i64,
    fetched_at_ms: i64,
}

/// Decoded RGBA8 animation frame `(width, height, rgba_bytes)`.
type GifFrameRgba = (u32, u32, Vec<u8>);

struct LiveChatHelpersInner {
    store: StickersStore,
    doc_meta: HashMap<ChatDocId, LiveDocumentMeta>,
    sticker_keywords: HashMap<ChatDocId, Vec<String>>,
    gif_items: HashMap<ChatDocId, GifItem>,
    sticker_media_cache: HashMap<ChatDocId, Option<StickerMedia>>,
    sticker_media_in_flight: HashSet<ChatDocId>,
    gif_frames_cache: HashMap<ChatDocId, Option<Vec<GifFrameRgba>>>,
    gif_frames_in_flight: HashSet<ChatDocId>,
    panel_refreshes_in_flight: HashSet<PanelRefresh>,
    // Bumped by `invalidate_all`; a panel refresh that started before the bump re-marks its
    // timestamps stale when it finishes, so the invalidation is not lost.
    invalidation_epoch: u64,
    emoji_statuses_sync: tg_api::cached_lists::EmojiStatusesSync,
    stickers_by_emoji: HashMap<String, StickersByEmojiCacheEntry>,
    search_sets_cache: HashMap<(String, bool), SearchStickerSetsCacheEntry>,
    inline_gif_cache: HashMap<String, InlineGifCacheEntry>,
    gif_search_username: String,
    gif_search_bot: Option<ResolvedGifBot>,
    gif_bot_resolve_failed_at_ms: i64,
    custom_emoji_docs: HashSet<ChatDocId>,
    participants_cache: HashMap<(PeerId, String), ParticipantsCacheEntry>,
    full_chat_fetched: HashSet<PeerId>,
    chat_bot_commands: HashMap<PeerId, HashMap<ChatUserId, Vec<BotCommand>>>,
    is_premium: bool,
    // Refresh timestamps in milliseconds since Unix epoch (0 = never fetched).
    last_stickers_update_ms: i64,
    last_masks_update_ms: i64,
    last_custom_emoji_update_ms: i64,
    last_recent_update_ms: i64,
    last_recent_attached_update_ms: i64,
    last_faved_update_ms: i64,
    last_featured_update_ms: i64,
    last_featured_emoji_update_ms: i64,
    last_archived_stickers_update_ms: i64,
    last_archived_emoji_update_ms: i64,
    last_saved_gifs_update_ms: i64,
    last_emoji_groups_update_ms: i64,
    // Optional synthetic clock override for deterministic cache-expiry tests.
    now_ms_override: Option<i64>,
}

impl LiveChatHelpersInner {
    fn new() -> Self {
        Self {
            store: StickersStore::new(),
            doc_meta: HashMap::new(),
            sticker_keywords: HashMap::new(),
            gif_items: HashMap::new(),
            sticker_media_cache: HashMap::new(),
            sticker_media_in_flight: HashSet::new(),
            gif_frames_cache: HashMap::new(),
            gif_frames_in_flight: HashSet::new(),
            panel_refreshes_in_flight: HashSet::new(),
            invalidation_epoch: 0,
            emoji_statuses_sync: tg_api::cached_lists::EmojiStatusesSync::default(),
            stickers_by_emoji: HashMap::new(),
            search_sets_cache: HashMap::new(),
            inline_gif_cache: HashMap::new(),
            gif_search_username: DEFAULT_GIF_SEARCH_USERNAME.to_owned(),
            gif_search_bot: None,
            gif_bot_resolve_failed_at_ms: 0,
            custom_emoji_docs: HashSet::new(),
            participants_cache: HashMap::new(),
            full_chat_fetched: HashSet::new(),
            chat_bot_commands: HashMap::new(),
            is_premium: false,
            last_stickers_update_ms: 0,
            last_masks_update_ms: 0,
            last_custom_emoji_update_ms: 0,
            last_recent_update_ms: 0,
            last_recent_attached_update_ms: 0,
            last_faved_update_ms: 0,
            last_featured_update_ms: 0,
            last_featured_emoji_update_ms: 0,
            last_archived_stickers_update_ms: 0,
            last_archived_emoji_update_ms: 0,
            last_saved_gifs_update_ms: 0,
            last_emoji_groups_update_ms: 0,
            now_ms_override: None,
        }
    }

    fn now_ms(&self) -> i64 {
        self.now_ms_override.unwrap_or_else(system_now_ms)
    }
}

/// Shared live cache state for stickers, custom emoji, GIFs, and mention participants.
pub(super) struct LiveChatHelpersState {
    inner: Mutex<LiveChatHelpersInner>,
    doc_pool: StickerDocPool,
    /// Signalled whenever a panel refresh or a GIF frame download finishes.
    background_idle: Condvar,
}

impl LiveChatHelpersState {
    /// Creates a fresh empty live chat-helpers state.
    pub(super) fn new() -> Self {
        Self {
            inner: Mutex::new(LiveChatHelpersInner::new()),
            doc_pool: StickerDocPool::new(),
            background_idle: Condvar::new(),
        }
    }

    /// Updates cached Premium status.
    pub(super) fn set_is_premium(&self, premium: bool) {
        if let Ok(mut g) = self.inner.lock() {
            g.is_premium = premium;
        }
    }

    /// Marks all cached sticker/GIF/participant timestamps stale so subsequent reads re-fetch with hashes.
    pub(super) fn invalidate_all(&self) {
        if let Ok(mut g) = self.inner.lock() {
            g.invalidation_epoch = g.invalidation_epoch.wrapping_add(1);
            g.last_stickers_update_ms = 0;
            g.last_masks_update_ms = 0;
            g.last_custom_emoji_update_ms = 0;
            g.last_recent_update_ms = 0;
            g.last_recent_attached_update_ms = 0;
            g.last_faved_update_ms = 0;
            g.last_featured_update_ms = 0;
            g.last_featured_emoji_update_ms = 0;
            g.last_archived_stickers_update_ms = 0;
            g.last_archived_emoji_update_ms = 0;
            g.last_saved_gifs_update_ms = 0;
            g.stickers_by_emoji.clear();
            g.search_sets_cache.clear();
            g.inline_gif_cache.clear();
            g.participants_cache.clear();
            g.full_chat_fetched.clear();
        }
    }
}

/// A server refresh the sticker, GIF and emoji panels start from their per-frame getters.
///
/// It runs on its own thread, so a cold or expired cache never stalls a frame; until it lands
/// the getters return what is cached, the same content the panels show before the first load.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum PanelRefresh {
    /// `messages.getFavedStickers`, `messages.getRecentStickers`, `messages.getAllStickers`
    /// and `messages.getStickerSet` for new or changed sets.
    MyStickers,
    /// `messages.getFeaturedStickers`.
    Featured,
    /// `messages.getSavedGifs`.
    SavedGifs,
    /// `messages.getEmojiGroups`.
    EmojiGroups,
}

impl PanelRefresh {
    /// Whether the refresh functions would contact the server now (same checks they make).
    fn is_due(self, g: &LiveChatHelpersInner, now_ms: i64) -> bool {
        let needed = |last_ms| StickersStore::update_needed(last_ms, now_ms);
        match self {
            Self::MyStickers => {
                needed(g.last_faved_update_ms)
                    || needed(g.last_recent_update_ms)
                    || needed(g.last_stickers_update_ms)
            }
            Self::Featured => needed(g.last_featured_update_ms),
            Self::SavedGifs => needed(g.last_saved_gifs_update_ms),
            Self::EmojiGroups => {
                g.last_emoji_groups_update_ms == 0
                    || now_ms.saturating_sub(g.last_emoji_groups_update_ms)
                        >= STICKERS_UPDATE_TIMEOUT_MS
            }
        }
    }

    /// Marks the timestamps this refresh wrote stale again after a racing `invalidate_all`.
    fn mark_stale(self, g: &mut LiveChatHelpersInner) {
        match self {
            Self::MyStickers => {
                g.last_faved_update_ms = 0;
                g.last_recent_update_ms = 0;
                g.last_stickers_update_ms = 0;
            }
            Self::Featured => g.last_featured_update_ms = 0,
            Self::SavedGifs => g.last_saved_gifs_update_ms = 0,
            Self::EmojiGroups => {}
        }
    }
}

/// Clears a panel refresh's in-flight mark when its thread ends, even by a panic.
struct PanelRefreshDone {
    state: Arc<LiveChatHelpersState>,
    job: PanelRefresh,
    epoch: u64,
}

impl Drop for PanelRefreshDone {
    fn drop(&mut self) {
        if let Ok(mut g) = self.state.inner.lock() {
            g.panel_refreshes_in_flight.remove(&self.job);
            if g.invalidation_epoch != self.epoch {
                self.job.mark_stale(&mut g);
            }
        }
        self.state.background_idle.notify_all();
    }
}

fn pick_gif_frame(frames: Option<&Vec<GifFrameRgba>>, frame_index: usize) -> Option<GifFrameRgba> {
    let frames = frames?;
    if frames.is_empty() {
        None
    } else {
        Some(frames[frame_index % frames.len()].clone())
    }
}

fn system_now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis().min(i64::MAX as u128) as i64)
}

// =============================================================================
// 4. TL -> `tg-chat` / `GifItem` Converters
// =============================================================================

fn parse_sticker_set_flags(set: &tl_types::StickerSet) -> StickersSetFlags {
    let mut flags = StickersSetFlags::EMPTY;
    if set.installed_date.is_some() {
        flags |= StickersSetFlags::INSTALLED;
    }
    if set.archived {
        flags |= StickersSetFlags::ARCHIVED;
    }
    if set.official {
        flags |= StickersSetFlags::OFFICIAL;
    }
    if set.masks {
        flags |= StickersSetFlags::MASKS;
    }
    if set.emojis {
        flags |= StickersSetFlags::EMOJI;
    }
    if set.text_color {
        flags |= StickersSetFlags::TEXT_COLOR;
    }
    if set.channel_emoji_status {
        flags |= StickersSetFlags::CHANNEL_STATUS;
    }
    if set.creator {
        flags |= StickersSetFlags::AM_CREATOR;
    }
    flags
}

fn sticker_set_update_from_tl(
    set: &tl_types::StickerSet,
) -> (StickerSetUpdate, Option<Size>, StickerType) {
    let mut thumb_size = None;
    let mut thumb_type = StickerType::Webp;
    if let Some(thumbs) = &set.thumbs {
        for thumb in thumbs {
            match thumb {
                tl_enums::PhotoSize::PhotoSize(s) => {
                    thumb_size = Some(Size::new(s.w, s.h));
                    thumb_type = thumbnail_type_from_photo_size_type(&s.r#type);
                    break;
                }
                tl_enums::PhotoSize::PhotoCachedSize(s) => {
                    thumb_size = Some(Size::new(s.w, s.h));
                    thumb_type = thumbnail_type_from_photo_size_type(&s.r#type);
                    break;
                }
                tl_enums::PhotoSize::Progressive(s) => {
                    thumb_size = Some(Size::new(s.w, s.h));
                    thumb_type = thumbnail_type_from_photo_size_type(&s.r#type);
                    break;
                }
                _ => {}
            }
        }
    }
    let update = StickerSetUpdate {
        id: StickerSetId(set.id as u64),
        access_hash: set.access_hash as u64,
        hash: i64::from(set.hash) as u64,
        title: set.title.clone(),
        short_name: set.short_name.clone(),
        count: set.count,
        flags: parse_sticker_set_flags(set),
        install_date: set.installed_date,
        thumbnail_document_id: set.thumb_document_id.map(|id| ChatDocId(id as u64)),
    };
    (update, thumb_size, thumb_type)
}

fn parse_tl_document(
    doc: &tl_enums::Document,
    fallback_set_id: Option<StickerSetId>,
) -> Option<(StickerDocument, LiveDocumentMeta)> {
    let tl_enums::Document::Document(d) = doc else {
        return None;
    };
    let doc_id = ChatDocId(d.id as u64);

    let mut width: i32 = 0;
    let mut height: i32 = 0;
    let mut duration_secs: f64 = 0.0;
    let mut alt_emoji = String::new();
    let mut set_id = fallback_set_id;
    let mut is_video = false;
    let mut is_animated_attr = false;
    let mut is_custom_emoji = false;
    let mut is_premium_emoji = false;
    let mut file_name = String::new();

    for attr in &d.attributes {
        match attr {
            tl_enums::DocumentAttribute::ImageSize(s) => {
                width = s.w;
                height = s.h;
            }
            tl_enums::DocumentAttribute::Video(v) => {
                is_video = true;
                width = v.w;
                height = v.h;
                duration_secs = v.duration;
            }
            tl_enums::DocumentAttribute::Animated => {
                is_animated_attr = true;
            }
            tl_enums::DocumentAttribute::Sticker(s) => {
                alt_emoji = s.alt.clone();
                if let tl_enums::InputStickerSet::ID(id) = &s.stickerset {
                    set_id = Some(StickerSetId(id.id as u64));
                }
            }
            tl_enums::DocumentAttribute::CustomEmoji(c) => {
                alt_emoji = c.alt.clone();
                is_custom_emoji = true;
                is_premium_emoji = !c.free;
                if let tl_enums::InputStickerSet::ID(id) = &c.stickerset {
                    set_id = Some(StickerSetId(id.id as u64));
                }
            }
            tl_enums::DocumentAttribute::Filename(f) => {
                file_name = f.file_name.clone();
            }
            tl_enums::DocumentAttribute::Audio(_) | tl_enums::DocumentAttribute::HasStickers => {}
        }
    }

    let sticker_type = if d.mime_type == "application/x-tgsticker" {
        StickerType::Tgs
    } else if d.mime_type == "video/webm" {
        StickerType::Webm
    } else {
        StickerType::Webp
    };

    let is_premium_sticker = d
        .video_thumbs
        .as_deref()
        .unwrap_or_default()
        .iter()
        .any(|vt| matches!(vt, tl_enums::VideoSize::VideoSize(vs) if vs.r#type == "f"));

    let is_gifv = d.mime_type == "image/gif"
        || (is_animated_attr && (is_video || d.mime_type == "video/mp4"));

    let mut thumbnail_size = None;
    let mut thumb_size_type = None;
    let mut cached_thumb_bytes = None;
    let mut stripped_thumb = None;
    if let Some(thumbs) = &d.thumbs {
        for thumb in thumbs {
            match thumb {
                tl_enums::PhotoSize::PhotoSize(s) => {
                    if thumbnail_size.is_none() {
                        thumbnail_size = Some(Size::new(s.w, s.h));
                        thumb_size_type = Some(s.r#type.clone());
                    }
                }
                tl_enums::PhotoSize::PhotoCachedSize(s) => {
                    thumbnail_size = Some(Size::new(s.w, s.h));
                    if thumb_size_type.is_none() {
                        thumb_size_type = Some(s.r#type.clone());
                    }
                    cached_thumb_bytes = Some(s.bytes.clone());
                }
                tl_enums::PhotoSize::Progressive(s) => {
                    if thumbnail_size.is_none() {
                        thumbnail_size = Some(Size::new(s.w, s.h));
                        thumb_size_type = Some(s.r#type.clone());
                    }
                }
                tl_enums::PhotoSize::PhotoStrippedSize(s) => {
                    stripped_thumb = Some(s.bytes.clone());
                }
                tl_enums::PhotoSize::Empty(_) | tl_enums::PhotoSize::PhotoPathSize(_) => {}
            }
        }
    }

    let sticker_doc = StickerDocument {
        id: doc_id,
        set_id,
        sticker_type,
        alt_emoji,
        dimensions: Size::new(width.max(0), height.max(0)),
        thumbnail_size,
        is_premium_sticker,
        is_premium_emoji,
        is_gifv,
    };

    let meta = LiveDocumentMeta {
        id: doc_id,
        access_hash: d.access_hash,
        file_reference: d.file_reference.clone(),
        dc_id: d.dc_id,
        mime_type: d.mime_type.clone(),
        size: u64::try_from(d.size).unwrap_or(0),
        sticker_type,
        is_gifv,
        is_custom_emoji,
        width: u32::try_from(width).unwrap_or(0),
        height: u32::try_from(height).unwrap_or(0),
        duration_secs,
        file_name,
        thumb_size_type,
        cached_thumb_bytes,
        stripped_thumb,
    };

    Some((sticker_doc, meta))
}

fn gif_item_from_meta(
    meta: &LiveDocumentMeta,
    title: Option<String>,
    tags: Vec<String>,
    saved: bool,
) -> GifItem {
    let resolved_title = title
        .filter(|t| !t.trim().is_empty())
        .or_else(|| {
            let stem = std::path::Path::new(&meta.file_name)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("");
            if stem.is_empty() {
                None
            } else {
                Some(stem.to_owned())
            }
        })
        .unwrap_or_else(|| format!("GIF {}", meta.id.0));
    GifItem {
        id: meta.id,
        title: resolved_title,
        width: if meta.width > 0 { meta.width } else { 160 },
        height: if meta.height > 0 { meta.height } else { 120 },
        tags,
        saved,
        frame_count: 1,
        fps: 12.0,
    }
}

fn packs_from_tl(packs: &[tl_enums::StickerPack]) -> Vec<(String, Vec<ChatDocId>)> {
    packs
        .iter()
        .map(|pack| {
            let tl_enums::StickerPack::StickerPack(p) = pack;
            let docs = p.documents.iter().map(|&id| ChatDocId(id as u64)).collect();
            (p.emoticon.clone(), docs)
        })
        .collect()
}

fn ingest_document(
    inner: &mut LiveChatHelpersInner,
    pool: &StickerDocPool,
    doc: &tl_enums::Document,
    fallback_set_id: Option<StickerSetId>,
) -> Option<ChatDocId> {
    let (sticker_doc, meta) = parse_tl_document(doc, fallback_set_id)?;
    let id = sticker_doc.id;
    if meta.is_custom_emoji {
        inner.custom_emoji_docs.insert(id);
    }
    if meta.is_gifv && !inner.gif_items.contains_key(&id) {
        let saved = inner.store.saved_gifs.contains(&id);
        let item = gif_item_from_meta(&meta, None, Vec::new(), saved);
        inner.gif_items.insert(id, item);
    }
    inner.doc_meta.insert(id, meta);
    inner.store.insert_document(sticker_doc.clone());
    pool.upsert(sticker_doc);
    Some(id)
}

/// Registers Star Gift sticker documents (`starGift.sticker`) with the sticker media cache.
pub(crate) fn ingest_gift_sticker_documents(
    state: &LiveChatHelpersState,
    docs: &[tl_enums::Document],
) {
    let Ok(mut inner) = state.inner.lock() else {
        return;
    };
    for doc in docs {
        let _ = ingest_document(&mut inner, &state.doc_pool, doc, None);
    }
}

/// Returns cached media for a registered gift sticker, or starts its download on `handle`.
pub(crate) fn gift_sticker_media<C: RpcClient + Clone + 'static>(
    state: &Arc<LiveChatHelpersState>,
    rpc: &C,
    handle: &tokio::runtime::Handle,
    bridge: std::sync::Weak<crate::bridge::LiveBridgeState>,
    id: ChatDocId,
) -> Option<StickerMedia> {
    let meta = {
        let mut g = state.inner.lock().ok()?;
        if let Some(cached) = g.sticker_media_cache.get(&id) {
            return cached.clone();
        }
        if g.sticker_media_in_flight.contains(&id) {
            return None;
        }
        let meta = g.doc_meta.get(&id)?.clone();
        g.sticker_media_in_flight.insert(id);
        meta
    };
    let state = Arc::clone(state);
    let rpc = rpc.clone();
    handle.spawn(async move {
        let resolved = fetch_sticker_media_async(&rpc, &meta).await;
        if let Ok(mut g) = state.inner.lock() {
            g.sticker_media_in_flight.remove(&id);
            g.sticker_media_cache.insert(id, resolved);
        }
        if let Some(b) = bridge.upgrade() {
            b.notify_ui();
        }
    });
    None
}

fn ingest_sticker_set_full(
    inner: &mut LiveChatHelpersInner,
    pool: &StickerDocPool,
    full: &tl_types::messages::StickerSet,
    now: i32,
) -> StickerSetId {
    let tl_enums::StickerSet::StickerSet(ref raw_set) = full.set;
    let (update, thumb_size, thumb_type) = sticker_set_update_from_tl(raw_set);
    let set_id = inner.store.feed_set(update, now);
    if let Some(set) = inner.store.sets.get_mut(&set_id) {
        set.thumbnail_size = thumb_size;
        set.thumbnail_type = thumb_type;
    }

    let mut doc_ids = Vec::with_capacity(full.documents.len());
    for doc in &full.documents {
        if let Some(id) = ingest_document(inner, pool, doc, Some(set_id)) {
            doc_ids.push(id);
        }
    }
    let emoji_packs = packs_from_tl(&full.packs);
    inner.store.feed_set_stickers(set_id, doc_ids, &emoji_packs);

    for kw in &full.keywords {
        let tl_enums::StickerKeyword::StickerKeyword(k) = kw;
        let doc_id = ChatDocId(k.document_id as u64);
        inner.sticker_keywords.insert(doc_id, k.keyword.clone());
    }

    set_id
}

fn ingest_covered_set(
    inner: &mut LiveChatHelpersInner,
    pool: &StickerDocPool,
    covered: &tl_enums::StickerSetCovered,
    now: i32,
) -> StickerSetId {
    match covered {
        tl_enums::StickerSetCovered::StickerSetCovered(c) => {
            let tl_enums::StickerSet::StickerSet(ref raw_set) = c.set;
            let (update, thumb_size, thumb_type) = sticker_set_update_from_tl(raw_set);
            let set_id = inner.store.feed_set(update, now);
            let cover_id = ingest_document(inner, pool, &c.cover, Some(set_id));
            if let Some(set) = inner.store.sets.get_mut(&set_id) {
                set.thumbnail_size = thumb_size;
                set.thumbnail_type = thumb_type;
                set.covers = cover_id.into_iter().collect();
            }
            set_id
        }
        tl_enums::StickerSetCovered::StickerSetMultiCovered(c) => {
            let tl_enums::StickerSet::StickerSet(ref raw_set) = c.set;
            let (update, thumb_size, thumb_type) = sticker_set_update_from_tl(raw_set);
            let set_id = inner.store.feed_set(update, now);
            let mut covers = Vec::with_capacity(c.covers.len());
            for doc in &c.covers {
                if let Some(id) = ingest_document(inner, pool, doc, Some(set_id)) {
                    covers.push(id);
                }
            }
            if let Some(set) = inner.store.sets.get_mut(&set_id) {
                set.thumbnail_size = thumb_size;
                set.thumbnail_type = thumb_type;
                set.covers = covers;
            }
            set_id
        }
        tl_enums::StickerSetCovered::StickerSetFullCovered(c) => {
            let tl_enums::StickerSet::StickerSet(ref raw_set) = c.set;
            let (update, thumb_size, thumb_type) = sticker_set_update_from_tl(raw_set);
            let set_id = inner.store.feed_set(update, now);
            if let Some(set) = inner.store.sets.get_mut(&set_id) {
                set.thumbnail_size = thumb_size;
                set.thumbnail_type = thumb_type;
            }
            let mut doc_ids = Vec::with_capacity(c.documents.len());
            for doc in &c.documents {
                if let Some(id) = ingest_document(inner, pool, doc, Some(set_id)) {
                    doc_ids.push(id);
                }
            }
            let emoji_packs = packs_from_tl(&c.packs);
            inner.store.feed_set_stickers(set_id, doc_ids, &emoji_packs);
            for kw in &c.keywords {
                let tl_enums::StickerKeyword::StickerKeyword(k) = kw;
                inner
                    .sticker_keywords
                    .insert(ChatDocId(k.document_id as u64), k.keyword.clone());
            }
            set_id
        }
        tl_enums::StickerSetCovered::StickerSetNoCovered(c) => {
            let tl_enums::StickerSet::StickerSet(ref raw_set) = c.set;
            let (update, thumb_size, thumb_type) = sticker_set_update_from_tl(raw_set);
            let set_id = inner.store.feed_set(update, now);
            if let Some(set) = inner.store.sets.get_mut(&set_id) {
                set.thumbnail_size = thumb_size;
                set.thumbnail_type = thumb_type;
            }
            set_id
        }
    }
}

fn build_input_sticker_set(
    store: &StickersStore,
    set_id: StickerSetId,
) -> Option<tl_enums::InputStickerSet> {
    let set = store.sets.get(&set_id)?;
    if set.access_hash != 0 || set.short_name.is_empty() {
        Some(tl_enums::InputStickerSet::ID(tl_types::InputStickerSetID {
            id: set.id.0 as i64,
            access_hash: set.access_hash as i64,
        }))
    } else {
        Some(tl_enums::InputStickerSet::ShortName(
            tl_types::InputStickerSetShortName {
                short_name: set.short_name.clone(),
            },
        ))
    }
}

fn sticker_set_info_from_store(store: &StickersStore, set: &StickersSet) -> StickerSetInfo {
    let animated = set.thumbnail_type.is_animated()
        || set.stickers.iter().any(|doc_id| {
            store
                .documents
                .get(doc_id)
                .is_some_and(|d| d.sticker_type.is_animated())
        })
        || set.covers.iter().any(|doc_id| {
            store
                .documents
                .get(doc_id)
                .is_some_and(|d| d.sticker_type.is_animated())
        });
    StickerSetInfo {
        id: set.id.0,
        title: set.title.clone(),
        short_name: set.short_name.clone(),
        count: u32::try_from(set.count.max(0)).unwrap_or(0),
        is_emoji: set.kind() == StickersType::Emoji,
        archived: set.flags.contains(StickersSetFlags::ARCHIVED),
        animated,
    }
}

// =============================================================================
// 5. `LiveSession` RPC & Cache Operations
// =============================================================================

impl LiveSession {
    pub(crate) fn ch_invoke_rpc<R>(&self, req: R) -> Result<R::Return, String>
    where
        R: tg_tl::RemoteCall + Send + Sync + 'static,
        R::Return: Send + 'static,
    {
        let rpc = self.runtime().rpc().clone();
        block_on_runtime(self.runtime().tokio_handle(), async move {
            with_rpc_timeout(
                CHAT_HELPERS_RPC_TIMEOUT,
                async move { rpc.invoke(&req).await },
            )
            .await
        })
    }

    fn ch_read_session<R: Send>(&self, f: impl FnOnce(&Session) -> R + Send) -> R {
        let session_lock = self.runtime().session();
        if let Ok(guard) = session_lock.try_read() {
            return f(&guard);
        }
        let session_arc = Arc::clone(session_lock);
        block_on_runtime(self.runtime().tokio_handle(), async move {
            let guard = session_arc.read().await;
            f(&guard)
        })
    }

    fn ch_write_session<R: Send>(&self, f: impl FnOnce(&mut Session) -> R + Send) -> R {
        let session_lock = self.runtime().session();
        if let Ok(mut guard) = session_lock.try_write() {
            return f(&mut guard);
        }
        let session_arc = Arc::clone(session_lock);
        block_on_runtime(self.runtime().tokio_handle(), async move {
            let mut guard = session_arc.write().await;
            f(&mut guard)
        })
    }

    fn ch_state(&self) -> &Arc<LiveChatHelpersState> {
        &self.chat_helpers.state
    }

    /// Starts `job` on a background thread when its cache is due and it is not already running.
    /// The panels' getters call this every frame; when nothing is due it only checks the state.
    fn start_panel_refresh(&self, job: PanelRefresh) {
        let state = Arc::clone(self.ch_state());
        let epoch = {
            let Ok(mut g) = state.inner.lock() else {
                return;
            };
            let now_ms = g.now_ms();
            if g.panel_refreshes_in_flight.contains(&job) || !job.is_due(&g, now_ms) {
                return;
            }
            g.panel_refreshes_in_flight.insert(job);
            g.invalidation_epoch
        };
        let live = self.clone();
        let done = PanelRefreshDone { state, job, epoch };
        let spawned = std::thread::Builder::new()
            .name("tg-panel-refresh".to_owned())
            .spawn(move || {
                {
                    let _done = done;
                    live.run_panel_refresh(job);
                }
                live.bridge_state().notify_ui();
            });
        if let Err(err) = spawned {
            debug!(error = %err, ?job, "panel refresh thread failed to start");
        }
    }

    fn run_panel_refresh(&self, job: PanelRefresh) {
        let result = match job {
            PanelRefresh::MyStickers => {
                let faved = self.refresh_faved_stickers(false);
                let recent = self.refresh_recent_stickers(false, false);
                let all = self.refresh_all_stickers(false);
                faved.and(recent).and(all)
            }
            PanelRefresh::Featured => self.refresh_featured_stickers(false),
            PanelRefresh::SavedGifs => self.refresh_saved_gifs(false),
            PanelRefresh::EmojiGroups => self.refresh_emoji_groups(false).map(|_| ()),
        };
        if let Err(err) = result {
            debug!(error = %err, ?job, "panel refresh failed");
        }
    }

    /// Blocks until no panel refresh or GIF frame download started by the panel getters is
    /// running, or `timeout` passes. Returns `true` when idle (tests and headless captures).
    pub fn wait_panel_background_idle(&self, timeout: Duration) -> bool {
        let state = self.ch_state();
        let Ok(g) = state.inner.lock() else {
            return false;
        };
        state
            .background_idle
            .wait_timeout_while(g, timeout, |g| {
                !g.panel_refreshes_in_flight.is_empty() || !g.gif_frames_in_flight.is_empty()
            })
            .is_ok_and(|(_, res)| !res.timed_out())
    }

    /// Sets the `@gif` inline bot username used for GIF search (default `"gif"`).
    pub fn set_gif_search_username(&self, username: impl Into<String>) {
        if let Ok(mut g) = self.ch_state().inner.lock() {
            let next = username.into().trim().trim_start_matches('@').to_owned();
            if !next.is_empty() && g.gif_search_username != next {
                g.gif_search_username = next;
                g.gif_search_bot = None;
                g.gif_bot_resolve_failed_at_ms = 0;
            }
        }
    }

    /// Overrides the internal cache clock (milliseconds) for deterministic tests.
    #[cfg(test)]
    pub(crate) fn set_chat_helpers_now_ms(&self, now_ms: Option<i64>) {
        if let Ok(mut g) = self.ch_state().inner.lock() {
            g.now_ms_override = now_ms;
        }
    }

    pub(crate) fn ch_now_ms(&self) -> i64 {
        self.ch_state()
            .inner
            .lock()
            .map(|g| g.now_ms())
            .unwrap_or_else(|_| system_now_ms())
    }

    pub(crate) fn ch_ingest_documents(
        &self,
        docs: &[tl_enums::Document],
        fallback_set_id: Option<StickerSetId>,
    ) -> Vec<ChatDocId> {
        let state = self.ch_state();
        let Ok(mut g) = state.inner.lock() else {
            return Vec::new();
        };
        let mut ids = Vec::with_capacity(docs.len());
        for doc in docs {
            if let Some(id) = ingest_document(&mut g, &state.doc_pool, doc, fallback_set_id) {
                ids.push(id);
            }
        }
        ids
    }

    pub(crate) fn ch_ingest_covered_sets(
        &self,
        sets: &[tl_enums::StickerSetCovered],
        mark_emoji: bool,
    ) -> Vec<StickerSetId> {
        let state = self.ch_state();
        let Ok(mut g) = state.inner.lock() else {
            return Vec::new();
        };
        let now_secs = (g.now_ms() / 1000) as i32;
        let mut ids = Vec::with_capacity(sets.len());
        for covered in sets {
            let set_id = ingest_covered_set(&mut g, &state.doc_pool, covered, now_secs);
            if mark_emoji && let Some(set) = g.store.sets.get_mut(&set_id) {
                set.flags |= StickersSetFlags::EMOJI;
            }
            ids.push(set_id);
        }
        ids
    }

    pub(crate) fn ch_ingest_old_featured_sets(
        &self,
        featured: &tl_types::messages::FeaturedStickers,
    ) -> Vec<StickerSetId> {
        let state = self.ch_state();
        let Ok(mut g) = state.inner.lock() else {
            return Vec::new();
        };
        let now_secs = (g.now_ms() / 1000) as i32;
        let unread: BTreeSet<StickerSetId> = featured
            .unread
            .iter()
            .map(|&id| StickerSetId(id as u64))
            .collect();
        let mut ids = Vec::with_capacity(featured.sets.len());
        for covered in &featured.sets {
            let set_id = ingest_covered_set(&mut g, &state.doc_pool, covered, now_secs);
            if let Some(set) = g.store.sets.get_mut(&set_id) {
                set.flags |= StickersSetFlags::FEATURED;
                if unread.contains(&set_id) {
                    set.flags |= StickersSetFlags::UNREAD;
                }
            }
            if !g.store.featured_sets_order.contains(&set_id) {
                g.store.featured_sets_order.push(set_id);
            }
            ids.push(set_id);
        }
        ids
    }

    pub(crate) fn ch_clear_recent_stickers_local(&self, attached: bool) {
        if let Ok(mut g) = self.ch_state().inner.lock() {
            let now_ms = g.now_ms();
            if attached {
                g.store.sets.remove(&CLOUD_RECENT_ATTACHED_SET_ID);
                g.last_recent_attached_update_ms = now_ms;
            } else {
                g.store.sets.remove(&CLOUD_RECENT_SET_ID);
                g.store.sets.remove(&CUSTOM_SET_ID);
                g.store.old_recent_pack.clear();
                g.last_recent_update_ms = now_ms;
            }
        }
    }

    /// Refreshes installed sticker sets via `messages.getAllStickers` + `messages.getStickerSet`
    /// using Telegram's 64-bit hash and 1-hour update interval.
    pub fn refresh_all_stickers(&self, force: bool) -> Result<(), String> {
        let (should_fetch, hash, now_ms) = {
            let g = self
                .ch_state()
                .inner
                .lock()
                .map_err(|_| "chat helpers lock poisoned".to_owned())?;
            let now_ms = g.now_ms();
            let needed = force || StickersStore::update_needed(g.last_stickers_update_ms, now_ms);
            let hash = if force && g.last_stickers_update_ms == 0 {
                0
            } else {
                count_stickers_hash(&g.store, true) as i64
            };
            (needed, hash, now_ms)
        };
        if !should_fetch {
            return Ok(());
        }

        let resp = self.ch_invoke_rpc(tl_fns::messages::GetAllStickers { hash });
        let to_load: Vec<(StickerSetId, tl_enums::InputStickerSet, i32)> = {
            let mut g = self
                .ch_state()
                .inner
                .lock()
                .map_err(|_| "chat helpers lock poisoned".to_owned())?;
            match resp {
                Ok(tl_enums::messages::AllStickers::NotModified) => {
                    g.last_stickers_update_ms = now_ms;
                    Vec::new()
                }
                Ok(tl_enums::messages::AllStickers::AllStickers(all)) => {
                    g.last_stickers_update_ms = now_ms;
                    let now_secs = (now_ms / 1000) as i32;
                    let mut updates = Vec::with_capacity(all.sets.len());
                    let mut thumb_info = Vec::with_capacity(all.sets.len());
                    for raw in &all.sets {
                        let tl_enums::StickerSet::StickerSet(s) = raw;
                        let (upd, tsz, ttype) = sticker_set_update_from_tl(s);
                        thumb_info.push((upd.id, tsz, ttype));
                        updates.push(upd);
                    }
                    let req_ids = g
                        .store
                        .sets_received(StickersType::Stickers, updates, now_secs);
                    for (id, tsz, ttype) in thumb_info {
                        if let Some(set) = g.store.sets.get_mut(&id) {
                            set.thumbnail_size = tsz;
                            set.thumbnail_type = ttype;
                        }
                    }
                    req_ids
                        .into_iter()
                        .filter_map(|id| {
                            let input = build_input_sticker_set(&g.store, id)?;
                            let req_hash = g.store.sets.get(&id).map_or(0, |s| {
                                if s.stickers.is_empty()
                                    || s.flags.contains(StickersSetFlags::NOT_LOADED)
                                {
                                    0
                                } else {
                                    s.hash as i32
                                }
                            });
                            Some((id, input, req_hash))
                        })
                        .collect()
                }
                Err(err) => {
                    g.last_stickers_update_ms = failed_refresh_mark(now_ms);
                    debug!(error = %err, "messages.getAllStickers failed");
                    return Err(err);
                }
            }
        };

        for (_id, input, set_hash) in to_load {
            let _ = self.fetch_sticker_set_internal(input, set_hash);
        }
        self.bridge_state().notify_ui();
        Ok(())
    }

    /// Fetches a single sticker or custom-emoji set via `messages.getStickerSet` and caches it.
    pub fn fetch_sticker_set(
        &self,
        stickerset: tl_enums::InputStickerSet,
        hash: i32,
    ) -> Result<Option<StickerSetId>, String> {
        self.fetch_sticker_set_internal(stickerset, hash)
    }

    fn fetch_sticker_set_internal(
        &self,
        stickerset: tl_enums::InputStickerSet,
        hash: i32,
    ) -> Result<Option<StickerSetId>, String> {
        let resp = self.ch_invoke_rpc(tl_fns::messages::GetStickerSet { stickerset, hash })?;
        match resp {
            tl_enums::messages::StickerSet::NotModified => Ok(None),
            tl_enums::messages::StickerSet::StickerSet(full) => {
                let state = self.ch_state();
                let mut g = state
                    .inner
                    .lock()
                    .map_err(|_| "chat helpers lock poisoned".to_owned())?;
                let now_secs = (g.now_ms() / 1000) as i32;
                let set_id = ingest_sticker_set_full(&mut g, &state.doc_pool, &full, now_secs);
                Ok(Some(set_id))
            }
        }
    }

    /// Refreshes favorite stickers via `messages.getFavedStickers` with hash caching.
    pub fn refresh_faved_stickers(&self, force: bool) -> Result<(), String> {
        let (should_fetch, hash, now_ms) = {
            let g = self
                .ch_state()
                .inner
                .lock()
                .map_err(|_| "chat helpers lock poisoned".to_owned())?;
            let now_ms = g.now_ms();
            let needed = force || StickersStore::update_needed(g.last_faved_update_ms, now_ms);
            let hash = if force && g.last_faved_update_ms == 0 {
                0
            } else {
                count_faved_stickers_hash(&g.store) as i64
            };
            (needed, hash, now_ms)
        };
        if !should_fetch {
            return Ok(());
        }

        let resp = self.ch_invoke_rpc(tl_fns::messages::GetFavedStickers { hash });
        let state = self.ch_state();
        let mut g = state
            .inner
            .lock()
            .map_err(|_| "chat helpers lock poisoned".to_owned())?;
        match resp {
            Ok(tl_enums::messages::FavedStickers::NotModified) => {
                g.last_faved_update_ms = now_ms;
                Ok(())
            }
            Ok(tl_enums::messages::FavedStickers::FavedStickers(faved)) => {
                g.last_faved_update_ms = now_ms;
                let mut docs = Vec::with_capacity(faved.stickers.len());
                for doc in &faved.stickers {
                    if let Some(id) = ingest_document(&mut g, &state.doc_pool, doc, None) {
                        docs.push(id);
                    }
                }
                if docs.is_empty() {
                    g.store.sets.remove(&FAVED_SET_ID);
                } else {
                    let emoji_packs = packs_from_tl(&faved.packs);
                    let count = docs.len() as i32;
                    let set = g.store.sets.entry(FAVED_SET_ID).or_insert_with(|| {
                        StickersSet::new(
                            FAVED_SET_ID,
                            0,
                            faved.hash as u64,
                            "Favorites",
                            "",
                            count,
                            StickersSetFlags::SPECIAL,
                            0,
                        )
                    });
                    set.hash = faved.hash as u64;
                    set.count = count;
                    g.store.feed_set_stickers(FAVED_SET_ID, docs, &emoji_packs);
                }
                Ok(())
            }
            Err(err) => {
                g.last_faved_update_ms = failed_refresh_mark(now_ms);
                debug!(error = %err, "messages.getFavedStickers failed");
                Err(err)
            }
        }
    }

    /// Refreshes cloud recent stickers via `messages.getRecentStickers` with hash caching.
    pub fn refresh_recent_stickers(&self, attached: bool, force: bool) -> Result<(), String> {
        let (should_fetch, hash, now_ms) = {
            let g = self
                .ch_state()
                .inner
                .lock()
                .map_err(|_| "chat helpers lock poisoned".to_owned())?;
            let now_ms = g.now_ms();
            let last_ms = if attached {
                g.last_recent_attached_update_ms
            } else {
                g.last_recent_update_ms
            };
            let needed = force || StickersStore::update_needed(last_ms, now_ms);
            let hash = if force && last_ms == 0 {
                0
            } else {
                count_recent_stickers_hash(&g.store, attached) as i64
            };
            (needed, hash, now_ms)
        };
        if !should_fetch {
            return Ok(());
        }

        let resp = self.ch_invoke_rpc(tl_fns::messages::GetRecentStickers { attached, hash });
        let state = self.ch_state();
        let mut g = state
            .inner
            .lock()
            .map_err(|_| "chat helpers lock poisoned".to_owned())?;
        let set_last = |g: &mut LiveChatHelpersInner, ts: i64| {
            if attached {
                g.last_recent_attached_update_ms = ts;
            } else {
                g.last_recent_update_ms = ts;
            }
        };
        match resp {
            Ok(tl_enums::messages::RecentStickers::NotModified) => {
                set_last(&mut g, now_ms);
                Ok(())
            }
            Ok(tl_enums::messages::RecentStickers::RecentStickers(recent)) => {
                set_last(&mut g, now_ms);
                let target_set_id = if attached {
                    CLOUD_RECENT_ATTACHED_SET_ID
                } else {
                    CLOUD_RECENT_SET_ID
                };
                let mut docs = Vec::with_capacity(recent.stickers.len());
                for doc in &recent.stickers {
                    if let Some(id) = ingest_document(&mut g, &state.doc_pool, doc, None) {
                        docs.push(id);
                    }
                }
                if docs.is_empty() {
                    g.store.sets.remove(&target_set_id);
                } else {
                    let emoji_packs = packs_from_tl(&recent.packs);
                    let count = docs.len() as i32;
                    let set = g.store.sets.entry(target_set_id).or_insert_with(|| {
                        StickersSet::new(
                            target_set_id,
                            0,
                            recent.hash as u64,
                            "Recent",
                            "",
                            count,
                            StickersSetFlags::SPECIAL,
                            0,
                        )
                    });
                    set.hash = recent.hash as u64;
                    set.count = count;
                    set.dates = recent.dates.clone();
                    g.store.feed_set_stickers(target_set_id, docs, &emoji_packs);
                }
                Ok(())
            }
            Err(err) => {
                set_last(&mut g, failed_refresh_mark(now_ms));
                debug!(error = %err, attached, "messages.getRecentStickers failed");
                Err(err)
            }
        }
    }

    /// Refreshes featured/trending sticker sets via `messages.getFeaturedStickers` with hash caching.
    pub fn refresh_featured_stickers(&self, force: bool) -> Result<(), String> {
        let (should_fetch, hash, now_ms) = {
            let g = self
                .ch_state()
                .inner
                .lock()
                .map_err(|_| "chat helpers lock poisoned".to_owned())?;
            let now_ms = g.now_ms();
            let needed = force || StickersStore::update_needed(g.last_featured_update_ms, now_ms);
            let hash = if force && g.last_featured_update_ms == 0 {
                0
            } else {
                count_featured_stickers_hash(&g.store) as i64
            };
            (needed, hash, now_ms)
        };
        if !should_fetch {
            return Ok(());
        }

        let resp = self.ch_invoke_rpc(tl_fns::messages::GetFeaturedStickers { hash });
        let state = self.ch_state();
        let mut g = state
            .inner
            .lock()
            .map_err(|_| "chat helpers lock poisoned".to_owned())?;
        match resp {
            Ok(tl_enums::messages::FeaturedStickers::NotModified(_)) => {
                g.last_featured_update_ms = now_ms;
                Ok(())
            }
            Ok(tl_enums::messages::FeaturedStickers::FeaturedStickers(featured)) => {
                g.last_featured_update_ms = now_ms;
                let now_secs = (now_ms / 1000) as i32;
                let unread: BTreeSet<StickerSetId> = featured
                    .unread
                    .iter()
                    .map(|&id| StickerSetId(id as u64))
                    .collect();
                let mut updates = Vec::with_capacity(featured.sets.len());
                for covered in &featured.sets {
                    let raw_set = match covered {
                        tl_enums::StickerSetCovered::StickerSetCovered(c) => &c.set,
                        tl_enums::StickerSetCovered::StickerSetMultiCovered(c) => &c.set,
                        tl_enums::StickerSetCovered::StickerSetFullCovered(c) => &c.set,
                        tl_enums::StickerSetCovered::StickerSetNoCovered(c) => &c.set,
                    };
                    let tl_enums::StickerSet::StickerSet(s) = raw_set;
                    let (upd, _, _) = sticker_set_update_from_tl(s);
                    updates.push(upd);
                }
                let _ = g
                    .store
                    .featured_sets_received(StickersType::Stickers, updates, &unread);
                for covered in &featured.sets {
                    let _ = ingest_covered_set(&mut g, &state.doc_pool, covered, now_secs);
                }
                Ok(())
            }
            Err(err) => {
                g.last_featured_update_ms = failed_refresh_mark(now_ms);
                debug!(error = %err, "messages.getFeaturedStickers failed");
                Err(err)
            }
        }
    }

    /// Queues a featured sticker set ID to be marked read on the next flush.
    pub fn queue_read_featured_sticker_set(&self, set_id: StickerSetId) -> bool {
        self.ch_state()
            .inner
            .lock()
            .map(|mut g| g.store.queue_read_featured_set(set_id))
            .unwrap_or(false)
    }

    /// Flushes queued featured sticker set IDs via `messages.readFeaturedStickers`
    /// and clears their `UNREAD` flag + unread counter only after the RPC succeeds.
    pub fn flush_read_featured_sticker_sets(&self) -> Result<Vec<StickerSetId>, String> {
        let pending_ids: Vec<i64> = {
            let g = self
                .ch_state()
                .inner
                .lock()
                .map_err(|_| "chat helpers lock poisoned".to_owned())?;
            g.store
                .pending_featured_reads
                .iter()
                .filter(|id| g.store.sets.contains_key(id))
                .map(|id| id.0 as i64)
                .collect()
        };
        if pending_ids.is_empty() {
            return Ok(Vec::new());
        }
        let _ = self.ch_invoke_rpc(tl_fns::messages::ReadFeaturedStickers { id: pending_ids })?;
        let flushed = {
            let mut g = self
                .ch_state()
                .inner
                .lock()
                .map_err(|_| "chat helpers lock poisoned".to_owned())?;
            g.store.flush_read_featured_sets()
        };
        self.bridge_state().notify_ui();
        Ok(flushed)
    }

    /// Marks specific featured sticker sets as read immediately via `messages.readFeaturedStickers`.
    pub fn read_featured_stickers(
        &self,
        set_ids: &[StickerSetId],
    ) -> Result<Vec<StickerSetId>, String> {
        {
            let mut g = self
                .ch_state()
                .inner
                .lock()
                .map_err(|_| "chat helpers lock poisoned".to_owned())?;
            for &id in set_ids {
                g.store.queue_read_featured_set(id);
            }
        }
        self.flush_read_featured_sticker_sets()
    }

    /// Returns the current count of unread featured sticker sets.
    #[must_use]
    pub fn featured_stickers_unread_count(&self) -> usize {
        self.ch_state()
            .inner
            .lock()
            .map(|g| g.store.featured_sets_unread_count)
            .unwrap_or(0)
    }

    /// Refreshes installed custom emoji sets via `messages.getEmojiStickers` + `messages.getStickerSet`.
    pub fn refresh_custom_emoji_sets(&self, force: bool) -> Result<(), String> {
        let (should_fetch, hash, now_ms) = {
            let g = self
                .ch_state()
                .inner
                .lock()
                .map_err(|_| "chat helpers lock poisoned".to_owned())?;
            let now_ms = g.now_ms();
            let needed =
                force || StickersStore::update_needed(g.last_custom_emoji_update_ms, now_ms);
            let hash = if force && g.last_custom_emoji_update_ms == 0 {
                0
            } else {
                count_custom_emoji_hash(&g.store, true) as i64
            };
            (needed, hash, now_ms)
        };
        if !should_fetch {
            return Ok(());
        }

        let resp = self.ch_invoke_rpc(tl_fns::messages::GetEmojiStickers { hash });
        let to_load: Vec<(StickerSetId, tl_enums::InputStickerSet, i32)> = {
            let mut g = self
                .ch_state()
                .inner
                .lock()
                .map_err(|_| "chat helpers lock poisoned".to_owned())?;
            match resp {
                Ok(tl_enums::messages::AllStickers::NotModified) => {
                    g.last_custom_emoji_update_ms = now_ms;
                    Vec::new()
                }
                Ok(tl_enums::messages::AllStickers::AllStickers(all)) => {
                    g.last_custom_emoji_update_ms = now_ms;
                    let now_secs = (now_ms / 1000) as i32;
                    let mut updates = Vec::with_capacity(all.sets.len());
                    let mut thumb_info = Vec::with_capacity(all.sets.len());
                    for raw in &all.sets {
                        let tl_enums::StickerSet::StickerSet(s) = raw;
                        let (mut upd, tsz, ttype) = sticker_set_update_from_tl(s);
                        upd.flags |= StickersSetFlags::EMOJI;
                        thumb_info.push((upd.id, tsz, ttype));
                        updates.push(upd);
                    }
                    let req_ids = g
                        .store
                        .sets_received(StickersType::Emoji, updates, now_secs);
                    for (id, tsz, ttype) in thumb_info {
                        if let Some(set) = g.store.sets.get_mut(&id) {
                            set.thumbnail_size = tsz;
                            set.thumbnail_type = ttype;
                        }
                    }
                    req_ids
                        .into_iter()
                        .filter_map(|id| {
                            let input = build_input_sticker_set(&g.store, id)?;
                            let req_hash = g.store.sets.get(&id).map_or(0, |s| {
                                if s.stickers.is_empty()
                                    || s.flags.contains(StickersSetFlags::NOT_LOADED)
                                {
                                    0
                                } else {
                                    s.hash as i32
                                }
                            });
                            Some((id, input, req_hash))
                        })
                        .collect()
                }
                Err(err) => {
                    g.last_custom_emoji_update_ms = failed_refresh_mark(now_ms);
                    debug!(error = %err, "messages.getEmojiStickers failed");
                    return Err(err);
                }
            }
        };

        for (_id, input, set_hash) in to_load {
            let _ = self.fetch_sticker_set_internal(input, set_hash);
        }
        self.bridge_state().notify_ui();
        Ok(())
    }

    /// Refreshes featured custom emoji sets via `messages.getFeaturedEmojiStickers` with hash caching.
    pub fn refresh_featured_custom_emoji_sets(&self, force: bool) -> Result<(), String> {
        let (should_fetch, hash, now_ms) = {
            let g = self
                .ch_state()
                .inner
                .lock()
                .map_err(|_| "chat helpers lock poisoned".to_owned())?;
            let now_ms = g.now_ms();
            let needed =
                force || StickersStore::update_needed(g.last_featured_emoji_update_ms, now_ms);
            let hash = if force && g.last_featured_emoji_update_ms == 0 {
                0
            } else {
                count_featured_emoji_hash(&g.store) as i64
            };
            (needed, hash, now_ms)
        };
        if !should_fetch {
            return Ok(());
        }

        let resp = self.ch_invoke_rpc(tl_fns::messages::GetFeaturedEmojiStickers { hash });
        let state = self.ch_state();
        let mut g = state
            .inner
            .lock()
            .map_err(|_| "chat helpers lock poisoned".to_owned())?;
        match resp {
            Ok(tl_enums::messages::FeaturedStickers::NotModified(_)) => {
                g.last_featured_emoji_update_ms = now_ms;
                Ok(())
            }
            Ok(tl_enums::messages::FeaturedStickers::FeaturedStickers(featured)) => {
                g.last_featured_emoji_update_ms = now_ms;
                let now_secs = (now_ms / 1000) as i32;
                let unread: BTreeSet<StickerSetId> = featured
                    .unread
                    .iter()
                    .map(|&id| StickerSetId(id as u64))
                    .collect();
                let mut updates = Vec::with_capacity(featured.sets.len());
                for covered in &featured.sets {
                    let raw_set = match covered {
                        tl_enums::StickerSetCovered::StickerSetCovered(c) => &c.set,
                        tl_enums::StickerSetCovered::StickerSetMultiCovered(c) => &c.set,
                        tl_enums::StickerSetCovered::StickerSetFullCovered(c) => &c.set,
                        tl_enums::StickerSetCovered::StickerSetNoCovered(c) => &c.set,
                    };
                    let tl_enums::StickerSet::StickerSet(s) = raw_set;
                    let (mut upd, _, _) = sticker_set_update_from_tl(s);
                    upd.flags |= StickersSetFlags::EMOJI;
                    updates.push(upd);
                }
                let _ = g
                    .store
                    .featured_sets_received(StickersType::Emoji, updates, &unread);
                for covered in &featured.sets {
                    let _ = ingest_covered_set(&mut g, &state.doc_pool, covered, now_secs);
                }
                Ok(())
            }
            Err(err) => {
                g.last_featured_emoji_update_ms = failed_refresh_mark(now_ms);
                debug!(error = %err, "messages.getFeaturedEmojiStickers failed");
                Err(err)
            }
        }
    }

    /// Returns installed custom emoji set IDs in display order (fetching via `messages.getEmojiStickers` if needed).
    #[must_use]
    pub fn custom_emoji_set_ids(&self) -> Vec<StickerSetId> {
        let _ = self.refresh_custom_emoji_sets(false);
        self.ch_state()
            .inner
            .lock()
            .map(|g| g.store.emoji_sets_order.clone())
            .unwrap_or_default()
    }

    /// Returns featured custom emoji set IDs in display order.
    #[must_use]
    pub fn featured_custom_emoji_set_ids(&self) -> Vec<StickerSetId> {
        let _ = self.refresh_featured_custom_emoji_sets(false);
        self.ch_state()
            .inner
            .lock()
            .map(|g| g.store.featured_emoji_sets_order.clone())
            .unwrap_or_default()
    }

    /// Resolves custom emoji documents by ID via `messages.getCustomEmojiDocuments`
    /// in batches of at most [`CUSTOM_EMOJI_MAX_PER_REQUEST`] (100), reusing cached documents.
    pub fn get_custom_emoji_documents(
        &self,
        doc_ids: &[ChatDocId],
    ) -> Result<Vec<StickerDocument>, String> {
        let missing: Vec<ChatDocId> = {
            let g = self
                .ch_state()
                .inner
                .lock()
                .map_err(|_| "chat helpers lock poisoned".to_owned())?;
            let mut seen = HashSet::new();
            doc_ids
                .iter()
                .copied()
                .filter(|id| seen.insert(*id) && !g.store.documents.contains_key(id))
                .collect()
        };

        for chunk in missing.chunks(CUSTOM_EMOJI_MAX_PER_REQUEST) {
            let req = tl_fns::messages::GetCustomEmojiDocuments {
                document_id: chunk.iter().map(|id| id.0 as i64).collect(),
            };
            let docs = self.ch_invoke_rpc(req)?;
            let state = self.ch_state();
            let mut g = state
                .inner
                .lock()
                .map_err(|_| "chat helpers lock poisoned".to_owned())?;
            for doc in &docs {
                if let Some(id) = ingest_document(&mut g, &state.doc_pool, doc, None) {
                    g.custom_emoji_docs.insert(id);
                }
            }
        }

        let g = self
            .ch_state()
            .inner
            .lock()
            .map_err(|_| "chat helpers lock poisoned".to_owned())?;
        Ok(doc_ids
            .iter()
            .filter_map(|id| g.store.documents.get(id).cloned())
            .collect())
    }

    /// Refreshes archived sticker or custom emoji sets via `messages.getArchivedStickers`.
    pub fn refresh_archived_stickers(&self, emojis: bool, force: bool) -> Result<(), String> {
        let (should_fetch, now_ms) = {
            let g = self
                .ch_state()
                .inner
                .lock()
                .map_err(|_| "chat helpers lock poisoned".to_owned())?;
            let now_ms = g.now_ms();
            let last_ms = if emojis {
                g.last_archived_emoji_update_ms
            } else {
                g.last_archived_stickers_update_ms
            };
            (
                force || StickersStore::update_needed(last_ms, now_ms),
                now_ms,
            )
        };
        if !should_fetch {
            return Ok(());
        }

        let resp = self.ch_invoke_rpc(tl_fns::messages::GetArchivedStickers {
            masks: false,
            emojis,
            offset_id: 0,
            limit: 100,
        });
        let state = self.ch_state();
        let mut g = state
            .inner
            .lock()
            .map_err(|_| "chat helpers lock poisoned".to_owned())?;
        let set_last = |g: &mut LiveChatHelpersInner, ts: i64| {
            if emojis {
                g.last_archived_emoji_update_ms = ts;
            } else {
                g.last_archived_stickers_update_ms = ts;
            }
        };
        match resp {
            Ok(tl_enums::messages::ArchivedStickers::ArchivedStickers(archived)) => {
                set_last(&mut g, now_ms);
                let now_secs = (now_ms / 1000) as i32;
                for covered in &archived.sets {
                    let set_id = ingest_covered_set(&mut g, &state.doc_pool, covered, now_secs);
                    if let Some(set) = g.store.sets.get_mut(&set_id) {
                        set.flags |= StickersSetFlags::ARCHIVED;
                        if emojis {
                            set.flags |= StickersSetFlags::EMOJI;
                        }
                    }
                    if !g.store.archived_sets_order.contains(&set_id) {
                        g.store.archived_sets_order.push(set_id);
                    }
                }
                Ok(())
            }
            Err(err) => {
                set_last(&mut g, failed_refresh_mark(now_ms));
                debug!(error = %err, emojis, "messages.getArchivedStickers failed");
                Err(err)
            }
        }
    }

    /// Reorders installed sticker, mask, or custom emoji sets via `messages.reorderStickerSets`.
    pub fn reorder_sticker_sets(
        &mut self,
        kind: StickersType,
        order: &[StickerSetId],
    ) -> Result<(), String> {
        let req = tl_fns::messages::ReorderStickerSets {
            masks: kind == StickersType::Masks,
            emojis: kind == StickersType::Emoji,
            order: order.iter().map(|id| id.0 as i64).collect(),
        };
        let ok = self.ch_invoke_rpc(req)?;
        if !ok {
            return Err("Server rejected sticker set reorder.".to_owned());
        }
        let mut g = self
            .ch_state()
            .inner
            .lock()
            .map_err(|_| "chat helpers lock poisoned".to_owned())?;
        let target = match kind {
            StickersType::Stickers => &mut g.store.sets_order,
            StickersType::Masks => &mut g.store.mask_sets_order,
            StickersType::Emoji => &mut g.store.emoji_sets_order,
        };
        *target = order.to_vec();
        self.bridge_state().notify_ui();
        Ok(())
    }

    /// Searches remote sticker sets via `messages.searchStickerSets` with hash caching.
    pub fn search_sticker_sets(
        &self,
        query: &str,
        exclude_featured: bool,
    ) -> Result<Vec<StickerSetId>, String> {
        let q = query.trim();
        if q.is_empty() {
            return Ok(Vec::new());
        }
        let cache_key = (q.to_lowercase(), exclude_featured);
        let (should_fetch, prev_hash, cached, now_ms) = {
            let g = self
                .ch_state()
                .inner
                .lock()
                .map_err(|_| "chat helpers lock poisoned".to_owned())?;
            let now_ms = g.now_ms();
            if let Some(entry) = g.search_sets_cache.get(&cache_key) {
                let stale = StickersStore::update_needed(entry.received_ms, now_ms);
                (stale, entry.hash, Some(entry.set_ids.clone()), now_ms)
            } else {
                (true, 0, None, now_ms)
            }
        };
        if !should_fetch {
            return Ok(cached.unwrap_or_default());
        }

        let resp = self.ch_invoke_rpc(tl_fns::messages::SearchStickerSets {
            exclude_featured,
            q: q.to_owned(),
            hash: prev_hash,
        })?;
        let state = self.ch_state();
        let mut g = state
            .inner
            .lock()
            .map_err(|_| "chat helpers lock poisoned".to_owned())?;
        match resp {
            tl_enums::messages::FoundStickerSets::NotModified => {
                if let Some(entry) = g.search_sets_cache.get_mut(&cache_key) {
                    entry.received_ms = now_ms;
                    return Ok(entry.set_ids.clone());
                }
                Ok(cached.unwrap_or_default())
            }
            tl_enums::messages::FoundStickerSets::FoundStickerSets(found) => {
                let now_secs = (now_ms / 1000) as i32;
                let mut set_ids = Vec::with_capacity(found.sets.len());
                for covered in &found.sets {
                    let set_id = ingest_covered_set(&mut g, &state.doc_pool, covered, now_secs);
                    set_ids.push(set_id);
                }
                g.search_sets_cache.insert(
                    cache_key,
                    SearchStickerSetsCacheEntry {
                        set_ids: set_ids.clone(),
                        hash: found.hash,
                        received_ms: now_ms,
                    },
                );
                Ok(set_ids)
            }
        }
    }

    /// Fetches remote stickers matching `emoticon` via `messages.getStickers` with hash caching.
    pub fn fetch_stickers_by_emoji(&self, emoticon: &str) -> Result<Vec<ChatDocId>, String> {
        let canonical = strip_skin_tone(emoticon.trim());
        if canonical.is_empty() {
            return Ok(Vec::new());
        }
        let (should_fetch, prev_hash, cached, now_ms) = {
            let g = self
                .ch_state()
                .inner
                .lock()
                .map_err(|_| "chat helpers lock poisoned".to_owned())?;
            let now_ms = g.now_ms();
            if let Some(entry) = g.stickers_by_emoji.get(&canonical) {
                let stale = entry.received_ms == 0
                    || now_ms >= entry.received_ms + STICKERS_BY_EMOJI_TIMEOUT_MS;
                (stale, entry.hash, Some(entry.docs.clone()), now_ms)
            } else {
                (true, 0, None, now_ms)
            }
        };
        if !should_fetch {
            return Ok(cached.unwrap_or_default());
        }

        let resp = self.ch_invoke_rpc(tl_fns::messages::GetStickers {
            emoticon: canonical.clone(),
            hash: prev_hash,
        })?;
        let state = self.ch_state();
        let mut g = state
            .inner
            .lock()
            .map_err(|_| "chat helpers lock poisoned".to_owned())?;
        match resp {
            tl_enums::messages::Stickers::NotModified => {
                if let Some(entry) = g.stickers_by_emoji.get_mut(&canonical) {
                    entry.received_ms = now_ms;
                    return Ok(entry.docs.clone());
                }
                Ok(cached.unwrap_or_default())
            }
            tl_enums::messages::Stickers::Stickers(stickers) => {
                let mut docs = Vec::with_capacity(stickers.stickers.len());
                for doc in &stickers.stickers {
                    if let Some(id) = ingest_document(&mut g, &state.doc_pool, doc, None) {
                        docs.push(id);
                    }
                }
                g.stickers_by_emoji.insert(
                    canonical,
                    StickersByEmojiCacheEntry {
                        docs: docs.clone(),
                        hash: stickers.hash,
                        received_ms: now_ms,
                    },
                );
                Ok(docs)
            }
        }
    }

    /// Refreshes saved GIFs via `messages.getSavedGifs` with hash caching.
    pub fn refresh_saved_gifs(&self, force: bool) -> Result<(), String> {
        let (should_fetch, hash, now_ms) = {
            let g = self
                .ch_state()
                .inner
                .lock()
                .map_err(|_| "chat helpers lock poisoned".to_owned())?;
            let now_ms = g.now_ms();
            let needed = force || StickersStore::update_needed(g.last_saved_gifs_update_ms, now_ms);
            let hash = if force && g.last_saved_gifs_update_ms == 0 {
                0
            } else {
                count_saved_gifs_hash(&g.store) as i64
            };
            (needed, hash, now_ms)
        };
        if !should_fetch {
            return Ok(());
        }

        let resp = self.ch_invoke_rpc(tl_fns::messages::GetSavedGifs { hash });
        let state = self.ch_state();
        let mut g = state
            .inner
            .lock()
            .map_err(|_| "chat helpers lock poisoned".to_owned())?;
        match resp {
            Ok(tl_enums::messages::SavedGifs::NotModified) => {
                g.last_saved_gifs_update_ms = now_ms;
                Ok(())
            }
            Ok(tl_enums::messages::SavedGifs::SavedGifs(saved)) => {
                g.last_saved_gifs_update_ms = now_ms;
                let mut ids = Vec::with_capacity(saved.gifs.len());
                for doc in &saved.gifs {
                    if let Some(id) = ingest_document(&mut g, &state.doc_pool, doc, None) {
                        // Mark every returned saved GIF as gifv even if synthetic test doc omitted Animated attr.
                        if let Some(meta) = g.doc_meta.get_mut(&id) {
                            meta.is_gifv = true;
                            let item = gif_item_from_meta(meta, None, Vec::new(), true);
                            g.gif_items.insert(id, item);
                        }
                        if let Some(sdoc) = g.store.documents.get_mut(&id) {
                            sdoc.is_gifv = true;
                            state.doc_pool.upsert(sdoc.clone());
                        }
                        ids.push(id);
                    }
                }
                g.store.gifs_received(&ids);
                Ok(())
            }
            Err(err) => {
                g.last_saved_gifs_update_ms = failed_refresh_mark(now_ms);
                debug!(error = %err, "messages.getSavedGifs failed");
                Err(err)
            }
        }
    }

    fn resolve_gif_search_bot(&self) -> Result<tl_enums::InputUser, String> {
        let (username, cached, failed_recently, now_ms) = {
            let g = self
                .ch_state()
                .inner
                .lock()
                .map_err(|_| "chat helpers lock poisoned".to_owned())?;
            let now_ms = g.now_ms();
            let failed_recently = g.gif_bot_resolve_failed_at_ms > 0
                && now_ms < g.gif_bot_resolve_failed_at_ms + 30_000;
            (
                g.gif_search_username.clone(),
                g.gif_search_bot.clone(),
                failed_recently,
                now_ms,
            )
        };
        if let Some(bot) = cached {
            return Ok(tl_enums::InputUser::InputUser(tl_types::InputUser {
                user_id: bot.user_id,
                access_hash: bot.access_hash,
            }));
        }

        // Check if already resolved in `tg_data::Session`.
        let from_session = self.ch_read_session(|session| {
            for pid in known_peer_ids(session) {
                if let Some(user) = session.peer(pid).and_then(|p| p.as_user())
                    && user.usernames.username().eq_ignore_ascii_case(&username)
                {
                    return Some(ResolvedGifBot {
                        user_id: user.user_id().0 as i64,
                        access_hash: user.access_hash as i64,
                    });
                }
            }
            None
        });
        if let Some(bot) = from_session {
            if let Ok(mut g) = self.ch_state().inner.lock() {
                g.gif_search_bot = Some(bot.clone());
            }
            return Ok(tl_enums::InputUser::InputUser(tl_types::InputUser {
                user_id: bot.user_id,
                access_hash: bot.access_hash,
            }));
        }

        if failed_recently {
            return Err(format!("@{username} resolution recently failed"));
        }

        let resolved = self.ch_invoke_rpc(tl_fns::contacts::ResolveUsername {
            username: username.clone(),
            referer: None,
        });
        match resolved {
            Ok(tl_enums::contacts::ResolvedPeer::ResolvedPeer(rp)) => {
                self.ch_write_session(|session| {
                    apply_tl_users_and_chats(session, &rp.users, &rp.chats);
                });
                let bot_user = match &rp.peer {
                    tl_enums::Peer::User(pu) => rp.users.iter().find_map(|u| match u {
                        tl_enums::User::User(user) if user.id == pu.user_id => {
                            Some(ResolvedGifBot {
                                user_id: user.id,
                                access_hash: user.access_hash.unwrap_or(0),
                            })
                        }
                        _ => None,
                    }),
                    _ => None,
                };
                if let Some(bot) = bot_user {
                    if let Ok(mut g) = self.ch_state().inner.lock() {
                        g.gif_search_bot = Some(bot.clone());
                        g.gif_bot_resolve_failed_at_ms = 0;
                    }
                    Ok(tl_enums::InputUser::InputUser(tl_types::InputUser {
                        user_id: bot.user_id,
                        access_hash: bot.access_hash,
                    }))
                } else {
                    if let Ok(mut g) = self.ch_state().inner.lock() {
                        g.gif_bot_resolve_failed_at_ms = now_ms;
                    }
                    Err(format!("@{username} did not resolve to a user"))
                }
            }
            Err(err) => {
                if let Ok(mut g) = self.ch_state().inner.lock() {
                    g.gif_bot_resolve_failed_at_ms = now_ms;
                }
                Err(err)
            }
        }
    }

    /// Queries the `@gif` inline bot via `messages.getInlineBotResults` (with `cache_time` caching),
    /// matching `GifsListWidget::sendInlineRequest` in `chat_helpers/gifs_list_widget.cpp`.
    pub fn search_gifs_page(
        &self,
        query: &str,
        offset: &str,
    ) -> Result<(Vec<GifItem>, String), String> {
        let q = query.trim();
        let cache_key = if offset.is_empty() {
            q.to_owned()
        } else {
            format!("{q}\0{offset}")
        };
        let now_ms = self
            .ch_state()
            .inner
            .lock()
            .map(|g| g.now_ms())
            .unwrap_or_else(|_| system_now_ms());

        if let Ok(g) = self.ch_state().inner.lock()
            && let Some(entry) = g.inline_gif_cache.get(&cache_key)
            && now_ms < entry.fetched_at_ms + entry.ttl_ms
        {
            let items = entry
                .items
                .iter()
                .cloned()
                .map(|mut item| {
                    item.saved = g.store.saved_gifs.contains(&item.id);
                    item
                })
                .collect();
            return Ok((items, entry.next_offset.clone()));
        }

        let bot = self.resolve_gif_search_bot()?;
        let peer = self.ch_read_session(|session| {
            let self_peer = input_peer_for(session, session.self_peer_id());
            if matches!(self_peer, tl_enums::InputPeer::Empty) {
                tl_enums::InputPeer::Self_
            } else {
                self_peer
            }
        });

        let resp = self.ch_invoke_rpc(tl_fns::messages::GetInlineBotResults {
            bot,
            peer,
            geo_point: None,
            query: q.to_owned(),
            offset: offset.to_owned(),
        })?;
        let tl_enums::messages::BotResults::BotResults(results) = resp;
        if !results.users.is_empty() {
            self.ch_write_session(|session| {
                apply_tl_users_and_chats(session, &results.users, &[]);
            });
        }

        let state = self.ch_state();
        let mut g = state
            .inner
            .lock()
            .map_err(|_| "chat helpers lock poisoned".to_owned())?;
        let mut out_items = Vec::with_capacity(results.results.len());
        let query_tags: Vec<String> = if q.is_empty() {
            Vec::new()
        } else {
            vec![q.to_owned()]
        };

        for res in &results.results {
            match res {
                tl_enums::BotInlineResult::BotInlineMediaResult(media_res) => {
                    if let Some(doc) = &media_res.document
                        && let Some(doc_id) = ingest_document(&mut g, &state.doc_pool, doc, None)
                    {
                        let saved = g.store.saved_gifs.contains(&doc_id);
                        if let Some(meta) = g.doc_meta.get_mut(&doc_id) {
                            meta.is_gifv = true;
                            let item = gif_item_from_meta(
                                meta,
                                media_res.title.clone(),
                                query_tags.clone(),
                                saved,
                            );
                            g.gif_items.insert(doc_id, item.clone());
                            out_items.push(item);
                        }
                        if let Some(sdoc) = g.store.documents.get_mut(&doc_id) {
                            sdoc.is_gifv = true;
                            state.doc_pool.upsert(sdoc.clone());
                        }
                    }
                }
                tl_enums::BotInlineResult::BotInlineResult(web_res) => {
                    // Synthesize a deterministic ID for web-document GIF results if no Telegram Document was attached.
                    let mut id_hash = hash_init();
                    for &b in web_res.id.as_bytes() {
                        hash_update_u64(&mut id_hash, u64::from(b));
                    }
                    let doc_id = ChatDocId(id_hash | (1u64 << 62));
                    let (w, h) = extract_web_document_dims(web_res.content.as_ref())
                        .or_else(|| extract_web_document_dims(web_res.thumb.as_ref()))
                        .unwrap_or((160, 120));
                    let saved = g.store.saved_gifs.contains(&doc_id);
                    let item = GifItem {
                        id: doc_id,
                        title: web_res
                            .title
                            .clone()
                            .filter(|t| !t.is_empty())
                            .unwrap_or_else(|| format!("GIF {}", web_res.id)),
                        width: w,
                        height: h,
                        tags: query_tags.clone(),
                        saved,
                        frame_count: 1,
                        fps: 12.0,
                    };
                    g.gif_items.insert(doc_id, item.clone());
                    out_items.push(item);
                }
            }
        }

        let next_offset = results.next_offset.unwrap_or_default();
        let ttl_ms = if results.cache_time > 0 {
            i64::from(results.cache_time) * 1000
        } else {
            DEFAULT_INLINE_GIF_CACHE_TTL_MS
        };
        g.inline_gif_cache.insert(
            cache_key,
            InlineGifCacheEntry {
                items: out_items.clone(),
                next_offset: next_offset.clone(),
                fetched_at_ms: now_ms,
                ttl_ms,
            },
        );
        Ok((out_items, next_offset))
    }

    /// Resolves a cached sticker document's [`tl_enums::InputDocument`] (including `access_hash`
    /// and `file_reference`) and sends it via `messages.sendMedia(InputMediaDocument)`.
    pub fn send_sticker_document(
        &mut self,
        peer_id: PeerId,
        doc_id: ChatDocId,
        reply_to: Option<tg_app_core::stand_in::types::MsgId>,
        silent: bool,
    ) -> Result<tg_app_core::history::backend::HistoryMessage, String> {
        let input_doc = match self.resolve_input_document(doc_id) {
            Some(doc) => doc,
            None => {
                let _ = self.refresh_all_stickers(false);
                self.resolve_input_document(doc_id).ok_or_else(|| {
                    format!(
                        "Sticker document {} metadata or file reference is not loaded.",
                        doc_id.0
                    )
                })?
            }
        };
        let options = crate::upload::SendMediaOptions {
            reply_to,
            silent,
            ..Default::default()
        };
        let msg = self.send_existing_document(peer_id, input_doc, true, options)?;
        let now_secs = self
            .ch_state()
            .inner
            .lock()
            .map(|g| (g.now_ms() / 1000) as i32)
            .unwrap_or_else(|_| now_unix_i32());
        if let Ok(mut g) = self.ch_state().inner.lock() {
            g.store.increment_sticker(doc_id, now_secs);
        }
        Ok(msg)
    }

    /// Resolves a cached GIF document's [`tl_enums::InputDocument`] (including `access_hash`
    /// and `file_reference`) and sends it via `messages.sendMedia(InputMediaDocument)`.
    pub fn send_gif_document(
        &mut self,
        peer_id: PeerId,
        doc_id: ChatDocId,
        reply_to: Option<tg_app_core::stand_in::types::MsgId>,
        silent: bool,
    ) -> Result<tg_app_core::history::backend::HistoryMessage, String> {
        let input_doc = match self.resolve_input_document(doc_id) {
            Some(doc) => doc,
            None => {
                let _ = self.refresh_saved_gifs(false);
                self.resolve_input_document(doc_id).ok_or_else(|| {
                    format!(
                        "GIF document {} metadata or file reference is not loaded.",
                        doc_id.0
                    )
                })?
            }
        };
        let options = crate::upload::SendMediaOptions {
            reply_to,
            silent,
            ..Default::default()
        };
        self.send_existing_document(peer_id, input_doc, false, options)
    }

    /// Ingests a full `messages.stickerSet` response into [`LiveChatHelpersState`] and builds
    /// a rich [`tg_app_core::session_api::StickerSetPreview`] for [`crate::boxes::sticker_set::StickerSetBoxState`].
    pub(crate) fn ingest_sticker_set_preview(
        &self,
        full: &tl_types::messages::StickerSet,
        is_emoji_hint: bool,
    ) -> tg_app_core::session_api::StickerSetPreview {
        let tl_enums::StickerSet::StickerSet(ref meta) = full.set;
        let state = self.ch_state();
        let mut items = Vec::with_capacity(full.documents.len());
        if let Ok(mut g) = state.inner.lock() {
            let now_secs = (g.now_ms() / 1000) as i32;
            let set_id = ingest_sticker_set_full(&mut g, &state.doc_pool, full, now_secs);
            let doc_ids: Vec<ChatDocId> = g
                .store
                .sets
                .get(&set_id)
                .map(|s| s.stickers.clone())
                .unwrap_or_default();
            for doc_id in doc_ids {
                let sdoc = g.store.documents.get(&doc_id);
                let lmeta = g.doc_meta.get(&doc_id);
                let thumb_rgba = lmeta.and_then(|m| {
                    m.cached_thumb_bytes
                        .as_deref()
                        .and_then(decode_rgba_image_bytes)
                        .or_else(|| m.stripped_thumb.as_deref().and_then(decode_stripped_thumb))
                });
                items.push(tg_app_core::session_api::StickerSetPreviewItem {
                    document_id: doc_id.0,
                    alt_emoji: sdoc.map(|d| d.alt_emoji.clone()).unwrap_or_default(),
                    is_animated: sdoc.is_some_and(|d| d.sticker_type == StickerType::Tgs),
                    is_video: sdoc.is_some_and(|d| d.sticker_type == StickerType::Webm),
                    is_premium: sdoc.is_some_and(|d| d.is_premium_sticker || d.is_premium_emoji),
                    thumb_rgba,
                });
            }
        }
        tg_app_core::session_api::StickerSetPreview {
            set_id: meta.id as u64,
            access_hash: meta.access_hash,
            title: meta.title.clone(),
            short_name: meta.short_name.clone(),
            count: meta.count,
            is_emoji: is_emoji_hint || meta.emojis,
            installed: meta.installed_date.is_some(),
            archived: meta.archived,
            official: meta.official,
            items,
        }
    }

    /// Installs a sticker or custom-emoji set by numeric ID (via cached `StickersStore`) or
    /// short name fallback (`messages.installStickerSet`).
    pub(crate) fn install_sticker_set_by_id_or_slug(
        &mut self,
        set_id: u64,
        short_name: &str,
    ) -> Result<bool, String> {
        if set_id != 0 && ChatHelpersBackend::install_sticker_set(self, StickerSetId(set_id)) {
            return Ok(true);
        }
        if short_name.is_empty() {
            return Ok(false);
        }
        let input = tl_enums::InputStickerSet::ShortName(tl_types::InputStickerSetShortName {
            short_name: short_name.to_owned(),
        });
        let _ = self.ch_invoke_rpc(tl_fns::messages::InstallStickerSet {
            stickerset: input,
            archived: false,
        })?;
        self.bridge_state().notify_ui();
        Ok(true)
    }

    /// Uninstalls a sticker or custom-emoji set by numeric ID (via cached `StickersStore`) or
    /// short name fallback (`messages.uninstallStickerSet`).
    pub(crate) fn remove_sticker_set_by_id_or_slug(
        &mut self,
        set_id: u64,
        short_name: &str,
    ) -> Result<bool, String> {
        if set_id != 0 && ChatHelpersBackend::remove_sticker_set(self, StickerSetId(set_id)) {
            return Ok(true);
        }
        if short_name.is_empty() {
            return Ok(false);
        }
        let input = tl_enums::InputStickerSet::ShortName(tl_types::InputStickerSetShortName {
            short_name: short_name.to_owned(),
        });
        let ok = self.ch_invoke_rpc(tl_fns::messages::UninstallStickerSet { stickerset: input })?;
        if ok {
            self.bridge_state().notify_ui();
        }
        Ok(ok)
    }

    pub(crate) fn resolve_input_sticker_set_cached(
        &self,
        set_id: u64,
        short_name: &str,
    ) -> Option<tl_enums::InputStickerSet> {
        if let Ok(g) = self.ch_state().inner.lock() {
            if set_id != 0
                && let Some(input) = build_input_sticker_set(&g.store, StickerSetId(set_id))
            {
                return Some(input);
            }
            if !short_name.is_empty()
                && let Some(set) = g
                    .store
                    .sets
                    .values()
                    .find(|s| s.short_name.eq_ignore_ascii_case(short_name))
                && let Some(input) = build_input_sticker_set(&g.store, set.id)
            {
                return Some(input);
            }
        }
        None
    }

    pub(crate) fn is_sticker_set_emoji_cached(&self, set_id: u64, short_name: &str) -> bool {
        let Ok(g) = self.ch_state().inner.lock() else {
            return false;
        };
        if set_id != 0
            && let Some(set) = g.store.sets.get(&StickerSetId(set_id))
        {
            return set.flags.contains(StickersSetFlags::EMOJI);
        }
        if !short_name.is_empty()
            && let Some(set) = g
                .store
                .sets
                .values()
                .find(|s| s.short_name.eq_ignore_ascii_case(short_name))
        {
            return set.flags.contains(StickersSetFlags::EMOJI);
        }
        false
    }

    pub(crate) fn evict_deleted_sticker_set_locally(&self, set_id: u64, short_name: &str) {
        if let Ok(mut g) = self.ch_state().inner.lock() {
            let mut ids_to_remove: Vec<StickerSetId> = Vec::new();
            if set_id != 0 {
                ids_to_remove.push(StickerSetId(set_id));
            }
            if !short_name.is_empty() {
                for (&id, set) in &g.store.sets {
                    if set.short_name.eq_ignore_ascii_case(short_name)
                        && !ids_to_remove.contains(&id)
                    {
                        ids_to_remove.push(id);
                    }
                }
            }
            for id in ids_to_remove {
                g.store.undo_install_locally(id);
                g.store.sets_order.retain(|&x| x != id);
                g.store.emoji_sets_order.retain(|&x| x != id);
                g.store.mask_sets_order.retain(|&x| x != id);
                g.store.archived_sets_order.retain(|&x| x != id);
                g.store.archived_mask_sets_order.retain(|&x| x != id);
                g.store.featured_sets_order.retain(|&x| x != id);
                g.store.featured_emoji_sets_order.retain(|&x| x != id);
                g.store.sets.remove(&id);
            }
        }
    }

    pub(crate) fn resolve_input_document(&self, id: ChatDocId) -> Option<tl_enums::InputDocument> {
        if let Ok(g) = self.ch_state().inner.lock()
            && let Some(meta) = g.doc_meta.get(&id)
        {
            return Some(meta.input_document());
        }
        // Fallback: look up document in loaded session histories.
        self.ch_read_session(|session| {
            for pid in known_peer_ids(session) {
                for msg in history_messages_ordered(session, pid) {
                    if let Some(tg_data::MessageMedia::Document(doc)) = msg.media()
                        && doc.id.0 == id.0
                    {
                        return Some(tl_enums::InputDocument::InputDocument(
                            tl_types::InputDocument {
                                id: doc.id.0 as i64,
                                access_hash: doc.access_hash as i64,
                                file_reference: doc.file_reference.clone(),
                            },
                        ));
                    }
                }
            }
            None
        })
    }

    /// Fetches and caches emoji mood/category groups via `messages.getEmojiGroups` with 32-bit
    /// hash caching (`Data::EmojiStatuses::refreshGroups`, `data_emoji_statuses.cpp:202-207`).
    pub fn refresh_emoji_groups(&self, force: bool) -> Result<Vec<EmojiGroupInfo>, String> {
        let (now_ms, cached_sync) = {
            let Ok(g) = self.ch_state().inner.lock() else {
                return Err("chat helpers state lock poisoned".to_owned());
            };
            let now_ms = g.now_ms();
            if !force
                && g.last_emoji_groups_update_ms > 0
                && now_ms.saturating_sub(g.last_emoji_groups_update_ms) < STICKERS_UPDATE_TIMEOUT_MS
            {
                return Ok(emoji_groups_to_dto(
                    &g.emoji_statuses_sync.emoji_groups.groups,
                ));
            }
            (now_ms, g.emoji_statuses_sync.clone())
        };

        let rpc = self.runtime().rpc().clone();
        let mut sync = cached_sync;
        let fetched = block_on_runtime(self.runtime().tokio_handle(), async move {
            with_rpc_timeout(CHAT_HELPERS_RPC_TIMEOUT, async move {
                sync.refresh_emoji_groups(&rpc)
                    .await
                    .map(|()| sync.emoji_groups)
            })
            .await
        });

        match fetched {
            Ok(updated) => {
                let out = emoji_groups_to_dto(&updated.groups);
                if let Ok(mut g) = self.ch_state().inner.lock() {
                    g.emoji_statuses_sync.emoji_groups = updated;
                    g.last_emoji_groups_update_ms = now_ms;
                }
                Ok(out)
            }
            Err(err) => {
                debug!(error = %err, "messages.getEmojiGroups failed");
                if !force && let Ok(mut g) = self.ch_state().inner.lock() {
                    g.last_emoji_groups_update_ms = failed_refresh_mark(now_ms);
                    if !g.emoji_statuses_sync.emoji_groups.groups.is_empty() {
                        return Ok(emoji_groups_to_dto(
                            &g.emoji_statuses_sync.emoji_groups.groups,
                        ));
                    }
                }
                Err(err)
            }
        }
    }

    fn load_sticker_media_inner(&self, id: ChatDocId) -> Option<StickerMedia> {
        load_sticker_media(self.ch_state(), self.runtime(), self.bridge_state(), id)
    }

    /// Returns frame `frame_index` of GIF `id`. Frames that need a download are fetched and
    /// decoded in the background (`None` until then, as for a GIF that failed to load); local
    /// thumbnails decode inline.
    fn load_gif_frame_inner(&self, id: ChatDocId, frame_index: usize) -> Option<GifFrameRgba> {
        let meta = {
            let Ok(mut g) = self.ch_state().inner.lock() else {
                return None;
            };
            if let Some(cached) = g.gif_frames_cache.get(&id) {
                return pick_gif_frame(cached.as_ref(), frame_index);
            }
            if g.gif_frames_in_flight.contains(&id) {
                return None;
            }
            let meta = g.doc_meta.get(&id)?.clone();
            if gif_frame_download(&meta).is_some() {
                g.gif_frames_in_flight.insert(id);
            }
            meta
        };

        let Some((loc, expected_size)) = gif_frame_download(&meta) else {
            let frames = match meta.cached_thumb_bytes.as_deref() {
                Some(cached) => decode_rgba_image_bytes(cached),
                None => meta
                    .stripped_thumb
                    .as_deref()
                    .and_then(decode_stripped_thumb),
            }
            .map(|f| vec![f]);
            let result = pick_gif_frame(frames.as_ref(), frame_index);
            if let Ok(mut g) = self.ch_state().inner.lock() {
                g.gif_frames_cache.insert(id, frames);
            }
            return result;
        };

        let state = Arc::clone(&self.chat_helpers.state);
        let bridge = Arc::downgrade(self.bridge_state());
        let rpc = self.runtime().rpc().clone();
        self.runtime().tokio_handle().spawn(async move {
            let bytes = download_file_bytes_async(&rpc, meta.dc_id, loc, expected_size)
                .await
                .ok();
            let frames = tokio::task::spawn_blocking(move || {
                bytes
                    .as_deref()
                    .and_then(decode_rgba_image_bytes)
                    .or_else(|| {
                        meta.thumb_size_type.as_ref()?;
                        meta.stripped_thumb
                            .as_deref()
                            .and_then(decode_stripped_thumb)
                    })
                    .map(|f| vec![f])
            })
            .await
            .ok()
            .flatten();
            if let Ok(mut g) = state.inner.lock() {
                g.gif_frames_in_flight.remove(&id);
                g.gif_frames_cache.insert(id, frames);
            }
            state.background_idle.notify_all();
            if let Some(b) = bridge.upgrade() {
                b.notify_ui();
            }
        });
        None
    }

    /// Searches `@mention` candidates for `peer_id` using `channels.getParticipants`
    /// with [`tl_types::ChannelParticipantsSearch`], merged with local members, recent authors,
    /// and chat bots/commands.
    pub fn search_chat_mention_candidates(
        &self,
        peer_id: PeerId,
        query: &str,
    ) -> Vec<MentionCandidate> {
        chat_mention_candidates_filtered(self, peer_id, query)
    }

    /// Returns cached bot commands discovered for bots participating in `peer_id`
    /// (from `tg_data::User::bot_info` and `messages.getFullChat` / `channels.getFullChannel`).
    #[must_use]
    pub fn chat_bot_commands(&self, peer_id: PeerId) -> HashMap<ChatUserId, Vec<BotCommand>> {
        let mut combined = self
            .ch_state()
            .inner
            .lock()
            .ok()
            .and_then(|g| g.chat_bot_commands.get(&peer_id).cloned())
            .unwrap_or_default();
        self.ch_read_session(|session| {
            for pid in known_peer_ids(session) {
                if let Some(user) = session.peer(pid).and_then(|p| p.as_user())
                    && let Some(bot) = &user.bot_info
                    && !bot.commands.is_empty()
                {
                    let cmds: Vec<BotCommand> = bot
                        .commands
                        .iter()
                        .map(|c| BotCommand {
                            command: c.command.clone(),
                            description: c.description.clone(),
                        })
                        .collect();
                    combined.entry(user.user_id()).or_insert(cmds);
                }
            }
        });
        combined
    }

    /// Synchronizes borrowed autocomplete slices (`autocomplete_users`, `autocomplete_last_authors`,
    /// `autocomplete_bot_commands`) for the active chat `peer_id`.
    pub fn prepare_chat_autocomplete(&mut self, peer_id: PeerId) {
        let _ = chat_mention_candidates_filtered(self, peer_id, "");
        let data_peer = to_data_peer(peer_id);
        let (users, last_authors, bot_commands, is_premium) = self.ch_read_session(|session| {
            let self_id = session.self_peer_id();
            let premium = session
                .peer(self_id)
                .and_then(|p| p.as_user())
                .is_some_and(|u| u.is_premium());

            let mut seen_authors = HashSet::new();
            let mut last_authors = Vec::new();
            for msg in history_messages_ordered(session, data_peer)
                .into_iter()
                .rev()
            {
                if let Some(uid) = msg.from_id.and_then(DataPeerId::to_user)
                    && seen_authors.insert(uid)
                {
                    last_authors.push(uid);
                }
            }

            let mut users = Vec::new();
            let mut bot_commands: HashMap<ChatUserId, Vec<BotCommand>> = HashMap::new();
            for pid in known_peer_ids(session) {
                if let Some(user) = session.peer(pid).and_then(|p| p.as_user()) {
                    let uid = user.user_id();
                    if let Some(bot) = &user.bot_info
                        && !bot.commands.is_empty()
                    {
                        bot_commands.insert(
                            uid,
                            bot.commands
                                .iter()
                                .map(|c| BotCommand {
                                    command: c.command.clone(),
                                    description: c.description.clone(),
                                })
                                .collect(),
                        );
                    }
                    users.push(user_to_chat_user_info(user, pid == self_id));
                }
            }
            (users, last_authors, bot_commands, premium)
        });

        let mut merged_commands = bot_commands;
        if let Ok(g) = self.ch_state().inner.lock()
            && let Some(extra) = g.chat_bot_commands.get(&peer_id)
        {
            for (uid, cmds) in extra {
                merged_commands.entry(*uid).or_insert_with(|| cmds.clone());
            }
        }

        self.chat_helpers.autocomplete_users = users;
        self.chat_helpers.autocomplete_last_authors = last_authors;
        self.chat_helpers.autocomplete_bot_commands = merged_commands;
        self.chat_helpers.is_premium = is_premium;
        self.ch_state().set_is_premium(is_premium);
    }
}

/// The file a GIF frame is decoded from when it is not cached locally: the thumbnail, or the
/// whole file for a plain `image/gif`. `None` when only local bytes are available.
fn gif_frame_download(
    meta: &LiveDocumentMeta,
) -> Option<(tl_enums::InputFileLocation, Option<u64>)> {
    if meta.cached_thumb_bytes.is_some() {
        return None;
    }
    let (thumb_size, expected_size) = if let Some(thumb_type) = &meta.thumb_size_type {
        (thumb_type.clone(), None)
    } else if meta.mime_type == "image/gif" {
        (String::new(), (meta.size > 0).then_some(meta.size))
    } else {
        return None;
    };
    let loc = tl_enums::InputFileLocation::InputDocumentFileLocation(
        tl_types::InputDocumentFileLocation {
            id: meta.id.0 as i64,
            access_hash: meta.access_hash,
            file_reference: meta.file_reference.clone(),
            thumb_size,
        },
    );
    Some((loc, expected_size))
}

fn extract_web_document_dims(doc: Option<&tl_enums::WebDocument>) -> Option<(u32, u32)> {
    let attrs = match doc? {
        tl_enums::WebDocument::WebDocument(d) => &d.attributes,
        tl_enums::WebDocument::NoProxy(d) => &d.attributes,
    };
    for attr in attrs {
        match attr {
            tl_enums::DocumentAttribute::ImageSize(s) if s.w > 0 && s.h > 0 => {
                return Some((s.w as u32, s.h as u32));
            }
            tl_enums::DocumentAttribute::Video(v) if v.w > 0 && v.h > 0 => {
                return Some((v.w as u32, v.h as u32));
            }
            _ => {}
        }
    }
    None
}

fn decode_rgba_image_bytes(bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let decoded = tg_media::image::read_image(tg_media::image::ReadArgs {
        content: bytes.to_vec(),
        ..Default::default()
    })
    .ok()?;
    let (w, h) = decoded.image.dimensions();
    if w == 0 || h == 0 {
        return None;
    }
    Some((w, h, decoded.image.into_raw()))
}

fn decode_stripped_thumb(stripped: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let rgba = tg_media::image::inline::from_inline_bytes(stripped).ok()?;
    let (w, h) = rgba.dimensions();
    if w == 0 || h == 0 {
        return None;
    }
    Some((w, h, rgba.into_raw()))
}

fn decode_fallback_static_thumb(meta: &LiveDocumentMeta) -> Option<StickerMedia> {
    if let Some(bytes) = meta.cached_thumb_bytes.as_deref()
        && let Some((width, height, rgba)) = decode_rgba_image_bytes(bytes)
    {
        return Some(StickerMedia::StaticRgba {
            width,
            height,
            rgba,
        });
    }
    if let Some(stripped) = meta.stripped_thumb.as_deref()
        && let Some((width, height, rgba)) = decode_stripped_thumb(stripped)
    {
        return Some(StickerMedia::StaticRgba {
            width,
            height,
            rgba,
        });
    }
    None
}

/// The media of sticker `id` (a document the session knows from a pack, `doc_meta`): cached, or
/// downloaded in the background with the pack's file reference (`None` until then; the UI is
/// woken when it lands).
pub(crate) fn load_sticker_media(
    state: &Arc<LiveChatHelpersState>,
    runtime: &Arc<SessionRuntime<ErasedRpcClient>>,
    bridge: &Arc<LiveBridgeState>,
    id: ChatDocId,
) -> Option<StickerMedia> {
    let meta = {
        let Ok(mut g) = state.inner.lock() else {
            return None;
        };
        if let Some(cached) = g.sticker_media_cache.get(&id) {
            return cached.clone();
        }
        if g.sticker_media_in_flight.contains(&id) {
            return None;
        }
        let meta = g.doc_meta.get(&id)?.clone();
        g.sticker_media_in_flight.insert(id);
        meta
    };

    let state = Arc::clone(state);
    let bridge = Arc::downgrade(bridge);
    let rpc = runtime.rpc().clone();
    runtime.tokio_handle().spawn(async move {
        let resolved = fetch_sticker_media_async(&rpc, &meta).await;
        if let Ok(mut g) = state.inner.lock() {
            g.sticker_media_in_flight.remove(&id);
            g.sticker_media_cache.insert(id, resolved);
        }
        if let Some(b) = bridge.upgrade() {
            b.notify_ui();
        }
    });
    None
}

async fn fetch_sticker_media_async<C: RpcClient + Clone + 'static>(
    rpc: &C,
    meta: &LiveDocumentMeta,
) -> Option<StickerMedia> {
    match meta.sticker_type {
        StickerType::Tgs => {
            let loc = tl_enums::InputFileLocation::InputDocumentFileLocation(
                tl_types::InputDocumentFileLocation {
                    id: meta.id.0 as i64,
                    access_hash: meta.access_hash,
                    file_reference: meta.file_reference.clone(),
                    thumb_size: String::new(),
                },
            );
            download_file_bytes_async(rpc, meta.dc_id, loc, (meta.size > 0).then_some(meta.size))
                .await
                .ok()
                .map(|bytes| {
                    if bytes.starts_with(b"{") {
                        match String::from_utf8(bytes) {
                            Ok(json) => StickerMedia::LottieJson(json),
                            Err(e) => StickerMedia::LottieTgs(e.into_bytes()),
                        }
                    } else {
                        StickerMedia::LottieTgs(bytes)
                    }
                })
                .or_else(|| decode_fallback_static_thumb(meta))
        }
        StickerType::Webp => {
            let loc = tl_enums::InputFileLocation::InputDocumentFileLocation(
                tl_types::InputDocumentFileLocation {
                    id: meta.id.0 as i64,
                    access_hash: meta.access_hash,
                    file_reference: meta.file_reference.clone(),
                    thumb_size: String::new(),
                },
            );
            download_file_bytes_async(rpc, meta.dc_id, loc, (meta.size > 0).then_some(meta.size))
                .await
                .ok()
                .and_then(|bytes| decode_rgba_image_bytes(&bytes))
                .map(|(width, height, rgba)| StickerMedia::StaticRgba {
                    width,
                    height,
                    rgba,
                })
                .or_else(|| decode_fallback_static_thumb(meta))
        }
        StickerType::Webm => {
            if let Some(thumb) = decode_fallback_static_thumb(meta) {
                return Some(thumb);
            }
            let thumb_type = meta.thumb_size_type.clone()?;
            let loc = tl_enums::InputFileLocation::InputDocumentFileLocation(
                tl_types::InputDocumentFileLocation {
                    id: meta.id.0 as i64,
                    access_hash: meta.access_hash,
                    file_reference: meta.file_reference.clone(),
                    thumb_size: thumb_type,
                },
            );
            download_file_bytes_async(rpc, meta.dc_id, loc, None)
                .await
                .ok()
                .and_then(|bytes| decode_rgba_image_bytes(&bytes))
                .map(|(width, height, rgba)| StickerMedia::StaticRgba {
                    width,
                    height,
                    rgba,
                })
        }
    }
}

fn emoji_groups_to_dto(groups: &[tg_api::cached_lists::CachedEmojiGroup]) -> Vec<EmojiGroupInfo> {
    groups
        .iter()
        .map(|g| EmojiGroupInfo {
            icon_emoji_id: g.icon_emoji_id,
            title: g.title.clone(),
            emoticons: g.emoticons.clone(),
            is_premium: g.is_premium,
            is_greeting: g.is_greeting,
        })
        .collect()
}

pub(super) fn user_to_chat_user_info(user: &tg_data::User, is_self: bool) -> UserInfo {
    let uname = user.usernames.username().to_owned();
    let usernames = user.usernames.usernames().to_vec();
    let inline_placeholder = user
        .bot_info
        .as_ref()
        .map(|b| b.inline_placeholder.clone())
        .unwrap_or_default();
    UserInfo {
        id: user.user_id(),
        access_hash: user.access_hash,
        first_name: user.first_name.clone(),
        display_name: user.common.name.clone(),
        username: uname,
        usernames,
        name_words: user.common.name_words.iter().cloned().collect(),
        is_bot: user.is_bot(),
        is_service_user: user.is_service_user(),
        is_notifications_user: user.is_notifications_user(),
        is_self,
        is_inaccessible: user.is_inaccessible(),
        lastseen: user.lastseen,
        inline_placeholder,
        stars_per_message: user.stars_per_message.max(0) as u64,
    }
}

// =============================================================================
// 6. `ChatHelpersBackend` Implementation for `LiveSession`
// =============================================================================

impl ChatHelpersBackend for LiveSession {
    fn recent_emoji(&mut self) -> Vec<String> {
        self.chat_helpers
            .recent_emoji
            .lock()
            .map(|mut r| {
                r.list()
                    .iter()
                    .filter_map(|e| match &e.id {
                        RecentEmojiId::Unicode(s) => Some(s.clone()),
                        RecentEmojiId::Document(_) => None,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn record_emoji_used(&mut self, emoji: &str) {
        if let Ok(mut r) = self.chat_helpers.recent_emoji.lock() {
            r.increment(RecentEmojiId::Unicode(emoji.to_owned()));
        }
    }

    fn hide_recent_emoji(&mut self, emoji: &str) {
        if let Ok(mut r) = self.chat_helpers.recent_emoji.lock() {
            r.hide(&RecentEmojiId::Unicode(emoji.to_owned()));
        }
    }

    fn emoji_variants(&self) -> &EmojiVariants {
        &self.chat_helpers.emoji_variants
    }

    fn save_emoji_variant(&mut self, base_emoji: &str, tone: SkinTone) {
        self.chat_helpers
            .emoji_variants
            .save_variant(base_emoji, tone);
    }

    fn save_all_emoji_variants(&mut self, tone: SkinTone) {
        self.chat_helpers.emoji_variants.save_all_variants(tone);
    }

    fn search_emoji(&mut self, query: &str) -> Vec<EmojiKeywordResult> {
        self.ensure_emoji_keywords_languages_refreshed();
        self.chat_helpers
            .emoji_keywords
            .lock()
            .map(|kw| kw.query(query, false))
            .unwrap_or_default()
    }

    fn emoji_groups(&self) -> Result<Vec<EmojiGroupInfo>, String> {
        self.start_panel_refresh(PanelRefresh::EmojiGroups);
        self.ch_state()
            .inner
            .lock()
            .map(|g| emoji_groups_to_dto(&g.emoji_statuses_sync.emoji_groups.groups))
            .map_err(|_| "chat helpers state lock poisoned".to_owned())
    }

    fn is_premium(&self) -> bool {
        self.chat_helpers.is_premium
    }

    fn set_premium(&mut self, premium: bool) {
        self.chat_helpers.is_premium = premium;
        self.ch_state().set_is_premium(premium);
    }

    fn sticker_sections(&self) -> Vec<StickerPanelSection> {
        self.start_panel_refresh(PanelRefresh::MyStickers);
        self.ch_state()
            .inner
            .lock()
            .map(|g| {
                g.store.build_my_sections(PanelBuildOptions {
                    is_masks: false,
                    premium_possible: true,
                    unlimited_recent: false,
                    exclude_set_id: None,
                    megagroup_set: None,
                })
            })
            .unwrap_or_default()
    }

    fn featured_sticker_sections(&self) -> Vec<StickerPanelSection> {
        self.start_panel_refresh(PanelRefresh::Featured);
        self.ch_state()
            .inner
            .lock()
            .map(|g| {
                g.store.build_featured_sections(PanelBuildOptions {
                    is_masks: false,
                    premium_possible: true,
                    unlimited_recent: false,
                    exclude_set_id: None,
                    megagroup_set: None,
                })
            })
            .unwrap_or_default()
    }

    fn sticker_tab_icons(
        &self,
        sections: &[StickerPanelSection],
        icon_side: i32,
    ) -> Vec<StickerTabIcon> {
        self.ch_state()
            .inner
            .lock()
            .map(|g| g.store.build_tab_icons(sections, icon_side))
            .unwrap_or_default()
    }

    fn sticker_document(&self, id: ChatDocId) -> Option<&StickerDocument> {
        self.ch_state().doc_pool.get(id)
    }

    fn sticker_media(&self, id: ChatDocId) -> Option<StickerMedia> {
        self.load_sticker_media_inner(id)
    }

    fn search_stickers(&self, query: &str) -> Vec<ChatDocId> {
        let q = query.trim();
        if q.is_empty() {
            return Vec::new();
        }
        let _ = self.refresh_all_stickers(false);

        // 1. Direct emoji or keyword -> emojis -> `messages.getStickers` + local `get_list_by_emoji`.
        let mut emojis: Vec<String> = Vec::new();
        if let Some((er, _)) = tg_emoji_db::find_emoji(q, 0) {
            emojis.push(er.non_colored_id().to_owned());
        } else if let Ok(kw) = self.chat_helpers.emoji_keywords.lock() {
            for hit in kw.query(q, false).into_iter().take(4) {
                let canon = strip_skin_tone(&hit.emoji);
                if !canon.is_empty() && !emojis.contains(&canon) {
                    emojis.push(canon);
                }
            }
        }

        let mut remote_emoji_docs: Vec<ChatDocId> = Vec::new();
        if let Some(first_emoji) = emojis.first()
            && let Ok(fetched) = self.fetch_stickers_by_emoji(first_emoji)
        {
            remote_emoji_docs = fetched;
        }

        // 2. Remote set search via `messages.searchStickerSets`.
        let remote_set_ids = self.search_sticker_sets(q, false).unwrap_or_default();

        let Ok(g) = self.ch_state().inner.lock() else {
            return Vec::new();
        };

        let mut results = if !emojis.is_empty() {
            let refs: Vec<&str> = emojis.iter().map(String::as_str).collect();
            let others = (!remote_emoji_docs.is_empty()).then_some(remote_emoji_docs.as_slice());
            g.store.get_list_by_emoji(
                &refs,
                others,
                EmojiSearchOptions {
                    seed: 42,
                    is_premium: self.chat_helpers.is_premium,
                    normals_per_premium: 2,
                    premiums_to_end_for_free: 2,
                },
            )
        } else {
            Vec::new()
        };

        // 3. Local set titles/short names and sticker keywords.
        let ql = q.to_lowercase();
        for set in g.store.sets.values() {
            if set.title.to_lowercase().contains(&ql) || set.short_name.to_lowercase().contains(&ql)
            {
                let source = if set.stickers.is_empty() {
                    &set.covers
                } else {
                    &set.stickers
                };
                for &doc_id in source {
                    if !results.contains(&doc_id) {
                        results.push(doc_id);
                    }
                }
            }
        }
        for (&doc_id, kws) in &g.sticker_keywords {
            if kws.iter().any(|k| k.to_lowercase().contains(&ql)) && !results.contains(&doc_id) {
                results.push(doc_id);
            }
        }

        // 4. Stickers/covers from remote `messages.searchStickerSets` hits.
        for set_id in remote_set_ids {
            if let Some(set) = g.store.sets.get(&set_id) {
                let source = if set.stickers.is_empty() {
                    &set.covers
                } else {
                    &set.stickers
                };
                for &doc_id in source {
                    if !results.contains(&doc_id) {
                        results.push(doc_id);
                    }
                }
            }
        }

        results
    }

    fn record_sticker_used(&mut self, id: ChatDocId) {
        let now_secs = self
            .ch_state()
            .inner
            .lock()
            .map(|g| (g.now_ms() / 1000) as i32)
            .unwrap_or_else(|_| now_unix_i32());
        if let Some(input_doc) = self.resolve_input_document(id) {
            // Sent without waiting and without a fail handler, like tdesktop's `.send()`.
            self.queue_background_rpc(
                tl_fns::messages::SaveRecentSticker {
                    attached: false,
                    id: input_doc,
                    unsave: false,
                },
                |error| debug!(%error, "messages.saveRecentSticker failed"),
            );
        }
        if let Ok(mut g) = self.ch_state().inner.lock() {
            g.store.increment_sticker(id, now_secs);
        }
        self.bridge_state().notify_ui();
    }

    fn is_sticker_faved(&self, id: ChatDocId) -> bool {
        let _ = self.refresh_faved_stickers(false);
        self.ch_state()
            .inner
            .lock()
            .map(|g| g.store.is_faved(id))
            .unwrap_or(false)
    }

    fn toggle_faved_sticker(&mut self, id: ChatDocId) -> bool {
        let _ = self.refresh_faved_stickers(false);
        let currently_faved = self
            .ch_state()
            .inner
            .lock()
            .map(|g| g.store.is_faved(id))
            .unwrap_or(false);
        let next = !currently_faved;

        if let Some(input_doc) = self.resolve_input_document(id) {
            if let Err(err) = self.ch_invoke_rpc(tl_fns::messages::FaveSticker {
                id: input_doc,
                unfave: !next,
            }) {
                debug!(doc_id = id.0, error = %err, "messages.faveSticker failed");
                return currently_faved;
            }
        } else {
            return currently_faved;
        }

        let is_premium = self.chat_helpers.is_premium;
        if let Ok(mut g) = self.ch_state().inner.lock() {
            g.store.set_faved(id, next, None, is_premium);
        }
        self.bridge_state().notify_ui();
        next
    }

    fn install_sticker_set(&mut self, set_id: StickerSetId) -> bool {
        let (input, now_secs, needs_full_load) = {
            let Ok(g) = self.ch_state().inner.lock() else {
                return false;
            };
            let Some(input) = build_input_sticker_set(&g.store, set_id) else {
                return false;
            };
            let needs_load = g.store.sets.get(&set_id).is_some_and(|s| {
                s.stickers.is_empty() || s.flags.contains(StickersSetFlags::NOT_LOADED)
            });
            (input, (g.now_ms() / 1000) as i32, needs_load)
        };

        let resp = match self.ch_invoke_rpc(tl_fns::messages::InstallStickerSet {
            stickerset: input.clone(),
            archived: false,
        }) {
            Ok(r) => r,
            Err(err) => {
                debug!(set_id = set_id.0, error = %err, "messages.installStickerSet failed");
                return false;
            }
        };

        let state = self.ch_state();
        let installed = if let Ok(mut g) = state.inner.lock() {
            let ok = g.store.install_locally(set_id, now_secs);
            if let tl_enums::messages::StickerSetInstallResult::Archive(archived_res) = resp {
                let mut archived_updates = Vec::with_capacity(archived_res.sets.len());
                for covered in &archived_res.sets {
                    let _ = ingest_covered_set(&mut g, &state.doc_pool, covered, now_secs);
                    let raw_set = match covered {
                        tl_enums::StickerSetCovered::StickerSetCovered(c) => &c.set,
                        tl_enums::StickerSetCovered::StickerSetMultiCovered(c) => &c.set,
                        tl_enums::StickerSetCovered::StickerSetFullCovered(c) => &c.set,
                        tl_enums::StickerSetCovered::StickerSetNoCovered(c) => &c.set,
                    };
                    let tl_enums::StickerSet::StickerSet(s) = raw_set;
                    let (mut upd, _, _) = sticker_set_update_from_tl(s);
                    upd.flags |= StickersSetFlags::ARCHIVED;
                    archived_updates.push(upd);
                }
                g.store.apply_archived_result(archived_updates, now_secs);
            }
            ok
        } else {
            false
        };

        if installed && needs_full_load {
            let _ = self.fetch_sticker_set_internal(input, 0);
        }
        if installed {
            self.bridge_state().notify_ui();
        }
        installed
    }

    fn remove_sticker_set(&mut self, set_id: StickerSetId) -> bool {
        let input = {
            let Ok(g) = self.ch_state().inner.lock() else {
                return false;
            };
            match build_input_sticker_set(&g.store, set_id) {
                Some(i) => i,
                None => return false,
            }
        };

        match self.ch_invoke_rpc(tl_fns::messages::UninstallStickerSet { stickerset: input }) {
            Ok(true) => {
                if let Ok(mut g) = self.ch_state().inner.lock() {
                    g.store.undo_install_locally(set_id);
                    g.store.archived_sets_order.retain(|&id| id != set_id);
                    g.store.archived_mask_sets_order.retain(|&id| id != set_id);
                    if let Some(set) = g.store.sets.get_mut(&set_id) {
                        set.flags &= !(StickersSetFlags::INSTALLED | StickersSetFlags::ARCHIVED);
                    }
                }
                self.bridge_state().notify_ui();
                true
            }
            Ok(false) => false,
            Err(err) => {
                debug!(set_id = set_id.0, error = %err, "messages.uninstallStickerSet failed");
                false
            }
        }
    }

    fn is_set_installed(&self, set_id: StickerSetId) -> bool {
        self.ch_state()
            .inner
            .lock()
            .map(|g| {
                g.store
                    .sets
                    .get(&set_id)
                    .is_some_and(StickersSet::is_in_my_list)
            })
            .unwrap_or(false)
    }

    fn sticker_set_section(&self, set_id: StickerSetId) -> Option<StickerPanelSection> {
        let g = self.ch_state().inner.lock().ok()?;
        let set = g.store.sets.get(&set_id)?;
        Some(StickerPanelSection {
            id: set.id,
            flags: set.flags,
            title: set.title.clone(),
            short_name: set.short_name.clone(),
            count: set.count,
            external_layout: !set.is_in_my_list(),
            stickers: set.stickers.clone(),
            thumbnail_document_id: set
                .thumbnail_document_id
                .or_else(|| set.stickers.first().copied()),
        })
    }

    fn saved_gifs(&self) -> Vec<GifItem> {
        self.start_panel_refresh(PanelRefresh::SavedGifs);
        self.ch_state()
            .inner
            .lock()
            .map(|g| {
                g.store
                    .saved_gifs
                    .iter()
                    .filter_map(|id| {
                        let mut item = g.gif_items.get(id).cloned().or_else(|| {
                            g.doc_meta
                                .get(id)
                                .map(|meta| gif_item_from_meta(meta, None, Vec::new(), true))
                        })?;
                        item.saved = true;
                        Some(item)
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn gif_categories(&self) -> Vec<GifCategory> {
        vec![
            GifCategory {
                id: "recent".into(),
                emoji: String::new(),
                label: "Saved GIFs".into(),
                query: String::new(),
            },
            GifCategory {
                id: "trending".into(),
                emoji: "🔥".into(),
                label: "Trending".into(),
                query: "trending".into(),
            },
            GifCategory {
                id: "joy".into(),
                emoji: "😂".into(),
                label: "Funny".into(),
                query: "😂".into(),
            },
            GifCategory {
                id: "love".into(),
                emoji: "❤️".into(),
                label: "Love".into(),
                query: "❤️".into(),
            },
            GifCategory {
                id: "thumbs".into(),
                emoji: "👍".into(),
                label: "Agree".into(),
                query: "👍".into(),
            },
            GifCategory {
                id: "party".into(),
                emoji: "🎉".into(),
                label: "Celebrate".into(),
                query: "🎉".into(),
            },
        ]
    }

    fn search_gifs(&self, query: &str) -> Vec<GifItem> {
        let q = query.trim();
        if q.is_empty() {
            return self.saved_gifs();
        }
        self.search_gifs_page(q, "")
            .map(|(items, _)| items)
            .unwrap_or_default()
    }

    fn toggle_saved_gif(&mut self, id: ChatDocId) -> bool {
        let _ = self.refresh_saved_gifs(false);
        let currently_saved = self
            .ch_state()
            .inner
            .lock()
            .map(|g| g.store.saved_gifs.contains(&id))
            .unwrap_or(false);
        let next = !currently_saved;

        let Some(input_doc) = self.resolve_input_document(id) else {
            return currently_saved;
        };
        match self.ch_invoke_rpc(tl_fns::messages::SaveGif {
            id: input_doc,
            unsave: currently_saved,
        }) {
            Ok(true) => {
                let is_premium = self.chat_helpers.is_premium;
                if let Ok(mut g) = self.ch_state().inner.lock() {
                    if next {
                        g.store.add_saved_gif(id, is_premium);
                    } else {
                        g.store.remove_saved_gif(id);
                    }
                    if let Some(item) = g.gif_items.get_mut(&id) {
                        item.saved = next;
                    }
                }
                self.bridge_state().notify_ui();
                next
            }
            Ok(false) => currently_saved,
            Err(err) => {
                debug!(doc_id = id.0, error = %err, "messages.saveGif failed");
                currently_saved
            }
        }
    }

    fn gif_frame_rgba(&self, id: ChatDocId, frame_index: usize) -> Option<(u32, u32, Vec<u8>)> {
        self.load_gif_frame_inner(id, frame_index)
    }

    fn self_user_id(&self) -> ChatUserId {
        self.ch_read_session(|s| ChatUserId(s.self_user_id().0))
    }

    fn autocomplete_users(&self) -> &[UserInfo] {
        &self.chat_helpers.autocomplete_users
    }

    fn autocomplete_last_authors(&self) -> &[ChatUserId] {
        &self.chat_helpers.autocomplete_last_authors
    }

    fn autocomplete_bot_commands(&self) -> &HashMap<ChatUserId, Vec<BotCommand>> {
        &self.chat_helpers.autocomplete_bot_commands
    }

    fn recent_inline_bot_ids(&self) -> &[ChatUserId] {
        &self.chat_helpers.recent_inline_bots
    }

    fn remove_recent_inline_bot(&mut self, id: ChatUserId) -> bool {
        let before = self.chat_helpers.recent_inline_bots.len();
        self.chat_helpers.recent_inline_bots.retain(|&b| b != id);
        self.chat_helpers.recent_inline_bots.len() != before
    }

    fn recent_hashtags(&self) -> &RecentHashtags {
        &self.chat_helpers.recent_hashtags
    }

    fn record_hashtag(&mut self, tag: &str) {
        self.chat_helpers.recent_hashtags.increment(tag);
        self.record_recent_hashtag_cache(tag);
    }

    fn remove_hashtag(&mut self, tag: &str) -> bool {
        DialogsBackend::remove_recent_hashtag(self, tag.trim_start_matches('#'));
        self.chat_helpers.recent_hashtags.remove(tag)
    }

    fn search_emoji_suggestions(&mut self, keyword: &str, exact: bool) -> Vec<EmojiKeywordResult> {
        self.ensure_emoji_keywords_languages_refreshed();
        self.chat_helpers
            .emoji_keywords
            .lock()
            .map(|kw| kw.query(keyword, exact))
            .unwrap_or_default()
    }

    fn stickers_for_emoji(&self, emoji: &str) -> Vec<ChatDocId> {
        let canonical = strip_skin_tone(emoji.trim());
        if canonical.is_empty() {
            return Vec::new();
        }
        let _ = self.refresh_all_stickers(false);
        let _ = self.refresh_recent_stickers(false, false);
        let remote = self.fetch_stickers_by_emoji(&canonical).ok();
        self.ch_state()
            .inner
            .lock()
            .map(|g| {
                g.store.get_list_by_emoji(
                    &[canonical.as_str()],
                    remote.as_deref(),
                    EmojiSearchOptions {
                        seed: 7,
                        is_premium: self.chat_helpers.is_premium,
                        normals_per_premium: 2,
                        premiums_to_end_for_free: 2,
                    },
                )
            })
            .unwrap_or_default()
    }

    fn search_stickers_remote(
        &self,
        req: &SearchStickersRequest,
    ) -> Result<SearchStickersPage, String> {
        self.search_stickers_remote_live(req)
    }

    fn search_emoji_sticker_sets(
        &self,
        query: &str,
        exclude_featured: bool,
    ) -> Result<SearchStickerSetsResult, String> {
        self.search_emoji_sticker_sets_live(query, exclude_featured)
    }

    fn load_old_featured_stickers(
        &self,
        offset: i32,
        limit: i32,
    ) -> Result<OldFeaturedStickersPage, String> {
        self.load_old_featured_stickers_live(offset, limit)
    }

    fn clear_recent_stickers(&mut self, attached: bool) -> Result<bool, String> {
        self.clear_recent_stickers_live(attached)
    }

    fn get_attached_stickers(
        &self,
        media: &AttachedStickersMedia,
    ) -> Result<Vec<AttachedStickerSetSummary>, String> {
        self.get_attached_stickers_live(media)
    }

    fn emoji_keywords_languages(&self, lang_codes: &[String]) -> Result<Vec<String>, String> {
        self.emoji_keywords_languages_live(lang_codes, false)
    }

    fn default_emoji_catalog(
        &self,
        kind: DefaultEmojiCatalogKind,
        force: bool,
    ) -> Result<DefaultEmojiCatalog, String> {
        self.default_emoji_catalog_live(kind, force)
    }

    fn refresh_messages_reactions(&self, peer_id: PeerId, msg_ids: &[MsgId]) -> Result<(), String> {
        self.refresh_messages_reactions_live(peer_id, msg_ids)
    }

    fn default_tag_reactions(&self, force: bool) -> Result<DefaultTagReactionsResult, String> {
        self.default_tag_reactions_live(force)
    }

    fn paid_reaction_privacy(&self, force: bool) -> Result<PaidReactionPrivacy, String> {
        self.paid_reaction_privacy_live(force)
    }

    fn toggle_paid_reaction_privacy(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        privacy: PaidReactionPrivacy,
    ) -> Result<bool, String> {
        self.toggle_paid_reaction_privacy_live(peer_id, msg_id, privacy)
    }

    fn report_reaction(
        &self,
        peer_id: PeerId,
        msg_id: MsgId,
        reaction_peer_id: PeerId,
    ) -> Result<bool, String> {
        self.report_reaction_live(peer_id, msg_id, reaction_peer_id)
    }

    fn delete_participant_reaction(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        participant_id: PeerId,
    ) -> Result<(), String> {
        self.delete_participant_reaction_live(peer_id, msg_id, participant_id)
    }

    fn delete_participant_reactions(
        &mut self,
        peer_id: PeerId,
        participant_id: PeerId,
    ) -> Result<bool, String> {
        self.delete_participant_reactions_live(peer_id, participant_id)
    }
}

// =============================================================================
// 7. `SettingsBackend` Sticker Sets Wiring
// =============================================================================

/// Live implementation of [`tg_app_core::settings::backend::SettingsBackend::sticker_sets`].
pub(super) fn settings_sticker_sets(live: &LiveSession) -> Vec<StickerSetInfo> {
    let _ = live.refresh_all_stickers(false);
    let _ = live.refresh_custom_emoji_sets(false);
    let _ = live.refresh_archived_stickers(false, false);
    let _ = live.refresh_archived_stickers(true, false);

    let Ok(g) = live.ch_state().inner.lock() else {
        return Vec::new();
    };
    let mut seen = HashSet::new();
    let mut out = Vec::new();

    for &set_id in g
        .store
        .sets_order
        .iter()
        .chain(g.store.emoji_sets_order.iter())
        .chain(g.store.archived_sets_order.iter())
    {
        if seen.insert(set_id)
            && let Some(set) = g.store.sets.get(&set_id)
            && !set.flags.contains(StickersSetFlags::SPECIAL)
        {
            out.push(sticker_set_info_from_store(&g.store, set));
        }
    }
    out
}

/// Live implementation of [`tg_app_core::settings::backend::SettingsBackend::set_sticker_set_archived`].
pub(super) fn settings_set_sticker_set_archived(live: &mut LiveSession, id: u64, archived: bool) {
    let set_id = StickerSetId(id);
    let input = {
        let Ok(g) = live.ch_state().inner.lock() else {
            return;
        };
        match build_input_sticker_set(&g.store, set_id) {
            Some(i) => i,
            None => return,
        }
    };

    let req = tl_fns::messages::ToggleStickerSets {
        uninstall: false,
        archive: archived,
        unarchive: !archived,
        stickersets: vec![input],
    };
    match live.ch_invoke_rpc(req) {
        Ok(true) => {
            if let Ok(mut g) = live.ch_state().inner.lock() {
                let kind = g.store.sets.get(&set_id).map(StickersSet::kind);
                if let Some(set) = g.store.sets.get_mut(&set_id) {
                    if archived {
                        set.flags |= StickersSetFlags::ARCHIVED;
                        set.flags &= !StickersSetFlags::INSTALLED;
                    } else {
                        set.flags &= !StickersSetFlags::ARCHIVED;
                        set.flags |= StickersSetFlags::INSTALLED;
                    }
                }
                if let Some(kind) = kind {
                    if archived {
                        match kind {
                            StickersType::Stickers => g.store.sets_order.retain(|&x| x != set_id),
                            StickersType::Masks => g.store.mask_sets_order.retain(|&x| x != set_id),
                            StickersType::Emoji => {
                                g.store.emoji_sets_order.retain(|&x| x != set_id)
                            }
                        }
                        if !g.store.archived_sets_order.contains(&set_id) {
                            g.store.archived_sets_order.insert(0, set_id);
                        }
                    } else {
                        g.store.archived_sets_order.retain(|&x| x != set_id);
                        let target = match kind {
                            StickersType::Stickers => &mut g.store.sets_order,
                            StickersType::Masks => &mut g.store.mask_sets_order,
                            StickersType::Emoji => &mut g.store.emoji_sets_order,
                        };
                        if !target.contains(&set_id) {
                            target.insert(0, set_id);
                        }
                    }
                }
            }
            live.bridge_state().notify_ui();
        }
        Ok(false) => {}
        Err(err) => {
            debug!(set_id = id, archived, error = %err, "messages.toggleStickerSets failed");
        }
    }
}

/// Live implementation of [`tg_app_core::settings::backend::SettingsBackend::remove_sticker_set`].
pub(super) fn settings_remove_sticker_set(live: &mut LiveSession, id: u64) {
    let _ = ChatHelpersBackend::remove_sticker_set(live, StickerSetId(id));
}

// =============================================================================
// 8. `HistoryBackend::chat_mention_candidates` (`channels.getParticipants` + local + bots)
// =============================================================================

/// Live implementation of [`tg_app_core::history::backend::HistoryBackend::chat_mention_candidates`].
pub(super) fn chat_mention_candidates(
    live: &LiveSession,
    peer_id: PeerId,
) -> Vec<MentionCandidate> {
    chat_mention_candidates_filtered(live, peer_id, "")
}

fn chat_mention_candidates_filtered(
    live: &LiveSession,
    peer_id: PeerId,
    query: &str,
) -> Vec<MentionCandidate> {
    let data_peer = to_data_peer(peer_id);
    let q = query.trim();
    let cache_key = (peer_id, q.to_lowercase());

    enum PeerFetchKind {
        Channel(ChannelId, u64, bool),
        BasicChat(ChatId),
        Other,
    }

    let peer_kind = live.ch_read_session(|session| match session.peer(data_peer) {
        Some(tg_data::Peer::Channel(ch)) => PeerFetchKind::Channel(
            ch.channel_id(),
            ch.access_hash,
            ch.common.loaded_status != tg_data::LoadedStatus::Full,
        ),
        Some(tg_data::Peer::Chat(chat)) => {
            if chat.participants.is_empty() {
                PeerFetchKind::BasicChat(chat.chat_id())
            } else {
                PeerFetchKind::Other
            }
        }
        _ => PeerFetchKind::Other,
    });

    // 1. Fetch remote participants for channels/supergroups via `channels.getParticipants`
    //    using `ChannelParticipantsSearch { q }`, cached with hash + TTL, plus `channels.getFullChannel`
    //    once when full info (including `ChannelFull.bot_info` commands) is not yet loaded.
    match peer_kind {
        PeerFetchKind::Channel(channel_id, access_hash, needs_full_channel) => {
            let (should_fetch, prev_hash, now_ms) = live
                .ch_state()
                .inner
                .lock()
                .map(|g| {
                    let now_ms = g.now_ms();
                    if let Some(entry) = g.participants_cache.get(&cache_key) {
                        let stale = now_ms >= entry.fetched_at_ms + PARTICIPANTS_CACHE_TTL_MS;
                        (stale, entry.hash, now_ms)
                    } else {
                        (true, 0, now_ms)
                    }
                })
                .unwrap_or((false, 0, system_now_ms()));

            if should_fetch {
                let req = tl_fns::channels::GetParticipants {
                    channel: tl_enums::InputChannel::InputChannel(tl_types::InputChannel {
                        channel_id: channel_id.0 as i64,
                        access_hash: access_hash as i64,
                    }),
                    filter: tl_enums::ChannelParticipantsFilter::ChannelParticipantsSearch(
                        tl_types::ChannelParticipantsSearch { q: q.to_owned() },
                    ),
                    offset: 0,
                    limit: 200,
                    hash: prev_hash,
                };
                match live.ch_invoke_rpc(req) {
                    Ok(tl_enums::channels::ChannelParticipants::NotModified) => {
                        if let Ok(mut g) = live.ch_state().inner.lock()
                            && let Some(entry) = g.participants_cache.get_mut(&cache_key)
                        {
                            entry.fetched_at_ms = now_ms;
                        }
                    }
                    Ok(tl_enums::channels::ChannelParticipants::ChannelParticipants(resp)) => {
                        live.ch_write_session(|session| {
                            apply_tl_users_and_chats(session, &resp.users, &resp.chats);
                        });
                        let mut uids = Vec::with_capacity(resp.participants.len());
                        let mut seen = HashSet::new();
                        for p in &resp.participants {
                            if let Some(uid) = channel_participant_user_id(p)
                                && seen.insert(uid)
                            {
                                uids.push(uid);
                            }
                        }
                        for u in &resp.users {
                            if let tl_enums::User::User(user) = u
                                && !user.deleted
                                && seen.insert(user.id as u64)
                            {
                                uids.push(user.id as u64);
                            }
                        }
                        let new_hash = count_participant_ids_hash(&uids) as i64;
                        if let Ok(mut g) = live.ch_state().inner.lock() {
                            g.participants_cache.insert(
                                cache_key.clone(),
                                ParticipantsCacheEntry {
                                    user_ids: uids,
                                    hash: new_hash,
                                    fetched_at_ms: now_ms,
                                },
                            );
                        }
                    }
                    Err(err) => {
                        debug!(peer = peer_id.0, query = q, error = %err, "channels.getParticipants failed");
                        // Record negative cache timestamp to avoid spamming failed RPC every frame.
                        if let Ok(mut g) = live.ch_state().inner.lock() {
                            g.participants_cache
                                .entry(cache_key.clone())
                                .and_modify(|e| e.fetched_at_ms = now_ms)
                                .or_insert(ParticipantsCacheEntry {
                                    user_ids: Vec::new(),
                                    hash: 0,
                                    fetched_at_ms: now_ms,
                                });
                        }
                    }
                }
            }

            if needs_full_channel {
                let should_fetch_full = live
                    .ch_state()
                    .inner
                    .lock()
                    .map(|mut g| g.full_chat_fetched.insert(peer_id))
                    .unwrap_or(false);
                if should_fetch_full {
                    let full_req = tl_fns::channels::GetFullChannel {
                        channel: tl_enums::InputChannel::InputChannel(tl_types::InputChannel {
                            channel_id: channel_id.0 as i64,
                            access_hash: access_hash as i64,
                        }),
                    };
                    if let Ok(tl_enums::messages::ChatFull::ChatFull(full)) =
                        live.ch_invoke_rpc(full_req)
                    {
                        let extracted_bots = extract_bot_commands_from_chat_full(&full.full_chat);
                        live.ch_write_session(|session| {
                            apply_tl_users_and_chats(session, &full.users, &full.chats);
                            apply_tl_chat_full(session, &full.full_chat);
                            apply_bot_commands_to_session(session, &extracted_bots);
                        });
                        if !extracted_bots.is_empty()
                            && let Ok(mut g) = live.ch_state().inner.lock()
                        {
                            g.chat_bot_commands.insert(peer_id, extracted_bots);
                        }
                    }
                }
            }
        }
        PeerFetchKind::BasicChat(chat_id) => {
            let should_fetch = live
                .ch_state()
                .inner
                .lock()
                .map(|mut g| g.full_chat_fetched.insert(peer_id))
                .unwrap_or(false);
            if should_fetch {
                let req = tl_fns::messages::GetFullChat {
                    chat_id: chat_id.0 as i64,
                };
                if let Ok(tl_enums::messages::ChatFull::ChatFull(full)) = live.ch_invoke_rpc(req) {
                    let extracted_bots = extract_bot_commands_from_chat_full(&full.full_chat);
                    live.ch_write_session(|session| {
                        apply_tl_users_and_chats(session, &full.users, &full.chats);
                        apply_tl_chat_full(session, &full.full_chat);
                        apply_bot_commands_to_session(session, &extracted_bots);
                    });
                    if !extracted_bots.is_empty()
                        && let Ok(mut g) = live.ch_state().inner.lock()
                    {
                        g.chat_bot_commands.insert(peer_id, extracted_bots);
                    }
                }
            }
        }
        PeerFetchKind::Other => {}
    }

    // 2. Collect cached remote participants for `(peer_id, query)` (and fallback to `""` cache if present).
    let remote_uids: Vec<u64> = live
        .ch_state()
        .inner
        .lock()
        .map(|g| {
            let mut out = Vec::new();
            if let Some(entry) = g.participants_cache.get(&cache_key) {
                out.extend_from_slice(&entry.user_ids);
            }
            if !q.is_empty()
                && let Some(base) = g.participants_cache.get(&(peer_id, String::new()))
            {
                for &uid in &base.user_ids {
                    if !out.contains(&uid) {
                        out.push(uid);
                    }
                }
            }
            out
        })
        .unwrap_or_default();

    // 3. Merge recent authors + remote participants + local members + chat bots into MentionCandidates.
    let ql = q.to_lowercase();
    live.ch_read_session(|session| {
        let mut ordered_uids: Vec<u64> = Vec::new();
        let mut seen: HashSet<u64> = HashSet::new();
        let mut push_uid = |uid: u64| {
            if uid != 0 && seen.insert(uid) {
                ordered_uids.push(uid);
            }
        };

        // (a) Recent message senders in this chat (newest first).
        for msg in history_messages_ordered(session, data_peer)
            .into_iter()
            .rev()
        {
            if let Some(uid) = msg.from_id.and_then(DataPeerId::to_user) {
                push_uid(uid.0);
            }
        }

        // (b) Participants from `channels.getParticipants`.
        for uid in remote_uids {
            push_uid(uid);
        }

        // (c) Local peer members / admins / direct DM peer.
        match session.peer(data_peer) {
            Some(tg_data::Peer::Chat(chat)) => {
                for &uid in &chat.participants {
                    push_uid(uid.0);
                }
                for &uid in &chat.admins {
                    push_uid(uid.0);
                }
            }
            Some(tg_data::Peer::Channel(ch)) => {
                if let Some(mg) = &ch.mg_info {
                    if let Some(creator) = mg.creator {
                        push_uid(creator.0);
                    }
                    for &uid in &mg.admins {
                        push_uid(uid.0);
                    }
                    for &uid in mg.member_ranks.keys() {
                        push_uid(uid.0);
                    }
                }
            }
            Some(tg_data::Peer::User(user)) => {
                push_uid(user.user_id().0);
            }
            None => {}
        }

        // (d) Also include any known bots with commands registered for this chat.
        if let Ok(g) = live.ch_state().inner.lock()
            && let Some(bot_map) = g.chat_bot_commands.get(&peer_id)
        {
            for &bot_uid in bot_map.keys() {
                push_uid(bot_uid.0);
            }
        }

        // Convert to `MentionCandidate` and apply query filter if non-empty.
        let mut candidates = Vec::with_capacity(ordered_uids.len());
        for uid in ordered_uids {
            let Some(user) = session.user(DataUserId(uid)) else {
                continue;
            };
            if user.is_inaccessible() {
                continue;
            }
            let display_name = if !user.common.name.trim().is_empty() {
                user.common.name.clone()
            } else if !user.first_name.trim().is_empty() {
                user.first_name.clone()
            } else {
                format!("User {uid}")
            };
            let username = {
                let u = user.usernames.username();
                if u.is_empty() {
                    None
                } else {
                    Some(u.to_owned())
                }
            };
            if !ql.is_empty() {
                let name_hit = display_name.to_lowercase().contains(&ql)
                    || user.first_name.to_lowercase().contains(&ql)
                    || user.last_name.to_lowercase().contains(&ql);
                let user_hit = username
                    .as_deref()
                    .is_some_and(|u| u.to_lowercase().contains(&ql));
                if !name_hit && !user_hit {
                    continue;
                }
            }
            candidates.push(MentionCandidate {
                user_id: uid,
                access_hash: user.access_hash,
                display_name,
                username,
                avatar_rgb: avatar_rgb_for_color_index(user.common.color_index % 7),
            });
        }
        candidates
    })
}

fn channel_participant_user_id(p: &tl_enums::ChannelParticipant) -> Option<u64> {
    match p {
        tl_enums::ChannelParticipant::ChannelParticipant(m) => Some(m.user_id as u64),
        tl_enums::ChannelParticipant::Self_(m) => Some(m.user_id as u64),
        tl_enums::ChannelParticipant::Creator(m) => Some(m.user_id as u64),
        tl_enums::ChannelParticipant::Admin(m) => Some(m.user_id as u64),
        tl_enums::ChannelParticipant::Banned(m) => match &m.peer {
            tl_enums::Peer::User(u) => Some(u.user_id as u64),
            _ => None,
        },
        tl_enums::ChannelParticipant::Left(_) => None,
    }
}

fn extract_bot_commands_from_chat_full(
    full: &tl_enums::ChatFull,
) -> HashMap<ChatUserId, Vec<BotCommand>> {
    let mut out = HashMap::new();
    let bot_infos: &[tl_enums::BotInfo] = match full {
        tl_enums::ChatFull::ChatFull(c) => c.bot_info.as_deref().unwrap_or(&[]),
        tl_enums::ChatFull::ChannelFull(c) => &c.bot_info,
        tl_enums::ChatFull::CommunityFull(_) => &[],
    };
    for bi in bot_infos {
        let tl_enums::BotInfo::BotInfo(info) = bi;
        let Some(uid) = info.user_id else {
            continue;
        };
        let cmds: Vec<BotCommand> = info
            .commands
            .as_deref()
            .unwrap_or(&[])
            .iter()
            .map(|c| {
                let tl_enums::BotCommand::BotCommand(cmd) = c;
                BotCommand {
                    command: cmd.command.clone(),
                    description: cmd.description.clone(),
                }
            })
            .collect();
        if !cmds.is_empty() {
            out.insert(ChatUserId(uid as u64), cmds);
        }
    }
    out
}

fn apply_bot_commands_to_session(
    session: &mut Session,
    commands: &HashMap<ChatUserId, Vec<BotCommand>>,
) {
    for (&uid, cmds) in commands {
        let peer_id = DataPeerId::from_user(DataUserId(uid.0));
        if let Some(tg_data::Peer::User(user)) = session.peer_mut(peer_id) {
            let bot = user.bot_info.get_or_insert_with(tg_data::BotInfo::default);
            bot.commands = cmds
                .iter()
                .map(|c| tg_data::BotCommand {
                    command: c.command.clone(),
                    description: c.description.clone(),
                })
                .collect();
        }
    }
}

#[cfg(test)]
mod tests_image_limits {
    use super::decode_rgba_image_bytes;

    fn encode_1bit_gray_png(width: u32, height: u32) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, width, height);
            enc.set_color(png::ColorType::Grayscale);
            enc.set_depth(png::BitDepth::One);
            enc.set_compression(png::Compression::Fast);
            let mut writer = enc.write_header().expect("write png header");
            let row_bytes = (width as usize).div_ceil(8);
            let row = vec![0u8; row_bytes];
            let mut stream = writer.stream_writer().expect("png stream writer");
            use std::io::Write as _;
            for _ in 0..height {
                stream.write_all(&row).expect("write png row");
            }
            stream.finish().expect("finish png stream");
        }
        out
    }

    #[test]
    fn chat_helpers_decode_rgba_rejects_oversized_pixel_area() {
        let small = encode_1bit_gray_png(16, 12);
        let (w, h, rgba) = decode_rgba_image_bytes(&small).expect("small PNG should decode");
        assert_eq!((w, h), (16, 12));
        assert_eq!(rgba.len(), 16 * 12 * 4);

        // 11_000 x 11_000 = 121_000_000 > READ_MAX_AREA (108_576_768).
        let oversized = encode_1bit_gray_png(11_000, 11_000);
        assert!(
            decode_rgba_image_bytes(&oversized).is_none(),
            "oversized PNG exceeding READ_MAX_AREA must be rejected"
        );
    }
}
