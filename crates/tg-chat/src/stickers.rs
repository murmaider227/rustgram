//! Sticker sets, flags, installed/archived/featured ordering, recent & favorite stickers,
//! saved GIFs, featured read state, emoji-based sticker search with premium interleaving,
//! tabbed selector section/icon list building, and Lottie/sticker sizing.
//!
//! Port of:
//! - `Telegram/SourceFiles/data/stickers/data_stickers.h` and `data_stickers.cpp`
//! - `Telegram/SourceFiles/data/stickers/data_stickers_set.h` and `data_stickers_set.cpp`
//! - `Telegram/SourceFiles/data/data_premium_limits.cpp` (faved stickers & saved GIFs limits)
//! - `Telegram/SourceFiles/chat_helpers/stickers_list_widget.cpp` (model & section/icon building)
//! - `Telegram/SourceFiles/chat_helpers/stickers_lottie.cpp` (`ComputeStickerSize`, `LottieCacheKeyShift`)
//! - `Telegram/SourceFiles/history/view/media/history_view_media_common.h` (`DownscaledSize`, `NonEmptySize`)
//! - `Telegram/lib_lottie/lottie/lottie_common.cpp` (`FrameRequest::size`)
//! - `Telegram/SourceFiles/apiwrap.cpp` (`readFeaturedSetDelayed`, `readFeaturedSets`)

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ops::{BitAnd, BitAndAssign, BitOr, BitOrAssign, BitXor, Not};

use tg_data::{DocumentId, TimeId};

use crate::emoji::strip_skin_tone;
use crate::types::{Size, StickerSetId};

// --- Special sticker set IDs matching `Data::Stickers::*SetId` ---

/// Default backward-compatibility set ID (`0`).
pub const DEFAULT_SET_ID: StickerSetId = StickerSetId(0);
/// Custom / ungrouped stickers set ID (`0xFFFF_FFFF_FFFF_FFFF`).
pub const CUSTOM_SET_ID: StickerSetId = StickerSetId(0xFFFF_FFFF_FFFF_FFFF);
/// Recent stickers section ID in the stickers panel (`0xFFFF_FFFF_FFFF_FFFE`).
pub const RECENT_SET_ID: StickerSetId = StickerSetId(0xFFFF_FFFF_FFFF_FFFE);
/// Sentinel "no set" ID (`0xFFFF_FFFF_FFFF_FFFD`).
pub const NONE_SET_ID: StickerSetId = StickerSetId(0xFFFF_FFFF_FFFF_FFFD);
/// Cloud-stored recent stickers set ID (`0xFFFF_FFFF_FFFF_FFFC`).
pub const CLOUD_RECENT_SET_ID: StickerSetId = StickerSetId(0xFFFF_FFFF_FFFF_FFFC);
/// Featured/trending tab set ID (`0xFFFF_FFFF_FFFF_FFFB`).
pub const FEATURED_SET_ID: StickerSetId = StickerSetId(0xFFFF_FFFF_FFFF_FFFB);
/// Cloud-stored favorite (faved) stickers set ID (`0xFFFF_FFFF_FFFF_FFFA`).
pub const FAVED_SET_ID: StickerSetId = StickerSetId(0xFFFF_FFFF_FFFF_FFFA);
/// Cloud-stored recent attached masks set ID (`0xFFFF_FFFF_FFFF_FFF9`).
pub const CLOUD_RECENT_ATTACHED_SET_ID: StickerSetId = StickerSetId(0xFFFF_FFFF_FFFF_FFF9);
/// Collectible emoji statuses set ID (`0xFFFF_FFFF_FFFF_FFF8`).
pub const COLLECTIBLE_SET_ID: StickerSetId = StickerSetId(0xFFFF_FFFF_FFFF_FFF8);
/// Megagroup sticker set section ID (`0xFFFF_FFFF_FFFF_FFEF`).
pub const MEGAGROUP_SET_ID: StickerSetId = StickerSetId(0xFFFF_FFFF_FFFF_FFEF);

// --- Limits and constants ---

/// Number of recent stickers shown in the panel unless unlimited option is enabled,
/// matching `kRecentDisplayLimit = 20` in `chat_helpers/stickers_list_widget.cpp`.
pub const RECENT_DISPLAY_LIMIT: usize = 20;

/// Default server config `stickersRecentLimit = 30` from `mtproto/mtproto_config.h`.
pub const DEFAULT_RECENT_STICKERS_LIMIT: usize = 30;

/// Default favorite stickers limit for non-Premium users (`stickers_faved_limit_default = 5`).
pub const FAVED_LIMIT_DEFAULT: usize = 5;
/// Favorite stickers limit for Premium users (`stickers_faved_limit_premium = 10`).
pub const FAVED_LIMIT_PREMIUM: usize = 10;

/// Default saved GIFs limit for non-Premium users (`saved_gifs_limit_default = 200`).
pub const SAVED_GIFS_LIMIT_DEFAULT: usize = 200;
/// Saved GIFs limit for Premium users (`saved_gifs_limit_premium = 400`).
pub const SAVED_GIFS_LIMIT_PREMIUM: usize = 400;

/// Cache invalidation interval (1 hour in milliseconds), matching `kUpdateTimeout = 3600'000`.
pub const STICKERS_UPDATE_TIMEOUT_MS: i64 = 3_600_000;

/// Maximum pixel area for caching Lottie sticker frames, matching `kDontCacheLottieAfterArea = 512 * 512`.
pub const DONT_CACHE_LOTTIE_AFTER_AREA: i32 = 512 * 512;

/// Sort key slice size used in emoji-based sticker ranking (`kSlice = 65536` in `data_stickers.cpp`).
pub const SORT_KEY_SLICE: i32 = 65_536;

/// Sticker set category, matching `Data::StickersType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StickersType {
    /// Regular chat stickers.
    Stickers,
    /// Photo editor masks.
    Masks,
    /// Custom emoji packs.
    Emoji,
}

/// Sticker media encoding format, matching `StickerType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum StickerType {
    /// Static WebP image.
    #[default]
    Webp,
    /// Animated Lottie (`.tgs`).
    Tgs,
    /// Video WebM sticker.
    Webm,
}

impl StickerType {
    /// Returns `true` for animated (Lottie or WebM) stickers, matching `StickerData::isAnimated()`.
    pub const fn is_animated(self) -> bool {
        matches!(self, Self::Tgs | Self::Webm)
    }

    /// Returns `true` for Lottie (`.tgs`) stickers.
    pub const fn is_lottie(self) -> bool {
        matches!(self, Self::Tgs)
    }

    /// Returns `true` for WebM video stickers.
    pub const fn is_webm(self) -> bool {
        matches!(self, Self::Webm)
    }
}

/// Maps an MTProto `PhotoSize::type` first character to [`StickerType`],
/// matching `Data::ThumbnailTypeFromPhotoSize` in `data/stickers/data_stickers.cpp`.
pub const fn thumbnail_type_from_photo_size_type(type_str: &str) -> StickerType {
    match type_str.as_bytes().first() {
        Some(b's') => StickerType::Webp,
        Some(b'a') => StickerType::Tgs,
        Some(b'v') => StickerType::Webm,
        _ => StickerType::Webp,
    }
}

/// Bitflags for a sticker set, with exact bit positions matching `Data::StickersSetFlag`
/// in `Telegram/SourceFiles/data/stickers/data_stickers_set.h`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct StickersSetFlags(u16);

impl StickersSetFlags {
    /// No flags set.
    pub const EMPTY: Self = Self(0);
    /// Installed by the user (`1 << 0`).
    pub const INSTALLED: Self = Self(1 << 0);
    /// Archived by the user or server (`1 << 1`).
    pub const ARCHIVED: Self = Self(1 << 1);
    /// Mask set (`1 << 2`).
    pub const MASKS: Self = Self(1 << 2);
    /// Official Telegram set (`1 << 3`).
    pub const OFFICIAL: Self = Self(1 << 3);
    /// Sticker documents not yet fetched (`1 << 4`).
    pub const NOT_LOADED: Self = Self(1 << 4);
    /// Featured / trending set (`1 << 5`).
    pub const FEATURED: Self = Self(1 << 5);
    /// Unread featured set (`1 << 6`).
    pub const UNREAD: Self = Self(1 << 6);
    /// Synthetic/special set such as Recent, Faved, or Megagroup (`1 << 7`).
    pub const SPECIAL: Self = Self(1 << 7);
    /// Custom emoji pack (`1 << 9`).
    pub const EMOJI: Self = Self(1 << 9);
    /// Adaptive text-color emoji set (`1 << 10`).
    pub const TEXT_COLOR: Self = Self(1 << 10);
    /// Channel emoji status set (`1 << 11`).
    pub const CHANNEL_STATUS: Self = Self(1 << 11);
    /// Current user is creator of this set (`1 << 12`).
    pub const AM_CREATOR: Self = Self(1 << 12);

    /// Creates flags from raw bits.
    pub const fn from_bits(bits: u16) -> Self {
        Self(bits)
    }

    /// Returns the raw bit representation.
    pub const fn bits(self) -> u16 {
        self.0
    }

    /// Returns `true` if all bits in `other` are set.
    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    /// Returns `true` if any bit in `other` is set.
    pub const fn intersects(self, other: Self) -> bool {
        (self.0 & other.0) != 0
    }
}

impl BitOr for StickersSetFlags {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl BitOrAssign for StickersSetFlags {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

impl BitAnd for StickersSetFlags {
    type Output = Self;
    fn bitand(self, rhs: Self) -> Self {
        Self(self.0 & rhs.0)
    }
}

impl BitAndAssign for StickersSetFlags {
    fn bitand_assign(&mut self, rhs: Self) {
        self.0 &= rhs.0;
    }
}

impl BitXor for StickersSetFlags {
    type Output = Self;
    fn bitxor(self, rhs: Self) -> Self {
        Self(self.0 ^ rhs.0)
    }
}

impl Not for StickersSetFlags {
    type Output = Self;
    fn not(self) -> Self {
        Self(!self.0)
    }
}

/// Sticker or GIF document metadata needed by sticker/GIF data logic.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StickerDocument {
    /// Document ID.
    pub id: DocumentId,
    /// Owning sticker set ID (if any).
    pub set_id: Option<StickerSetId>,
    /// Sticker encoding type (WebP, TGS, WebM).
    pub sticker_type: StickerType,
    /// Fallback alt emoji from `documentAttributeSticker`.
    pub alt_emoji: String,
    /// Intrinsic pixel dimensions.
    pub dimensions: Size,
    /// Thumbnail pixel dimensions (if the document has a thumbnail).
    pub thumbnail_size: Option<Size>,
    /// Whether this document is a Premium-only sticker (e.g. has premium video effect).
    pub is_premium_sticker: bool,
    /// Whether this document is a Premium-only custom emoji.
    pub is_premium_emoji: bool,
    /// Whether this document is a valid GIF/MPEG4 animation (`isGifv()`).
    pub is_gifv: bool,
}

/// Sticker set model, matching `Data::StickersSet` in `data/stickers/data_stickers_set.h`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StickersSet {
    /// Set ID.
    pub id: StickerSetId,
    /// Access hash.
    pub access_hash: u64,
    /// Server hash for cache validation.
    pub hash: u64,
    /// Explicit thumbnail document ID, if provided by server.
    pub thumbnail_document_id: Option<DocumentId>,
    /// Set thumbnail dimensions, if the set itself has a dedicated thumbnail.
    pub thumbnail_size: Option<Size>,
    /// Set thumbnail type.
    pub thumbnail_type: StickerType,
    /// Human-readable set title.
    pub title: String,
    /// Short name slug.
    pub short_name: String,
    /// Total sticker count reported by server.
    pub count: i32,
    /// Reference count keeping this set alive from eviction.
    pub locked: i32,
    /// Set flags.
    pub flags: StickersSetFlags,
    /// Unix timestamp when this set was installed (`0` if not installed).
    pub install_date: TimeId,
    /// Preview cover stickers (for featured/search sets before full load).
    pub covers: Vec<DocumentId>,
    /// Full sticker list in order.
    pub stickers: Vec<DocumentId>,
    /// Parallel usage timestamps (populated for `CLOUD_RECENT_SET_ID` / `CLOUD_RECENT_ATTACHED_SET_ID`).
    pub dates: Vec<TimeId>,
    /// Mapping from canonical (non-colored) emoji string to matching stickers in this set.
    pub emoji: BTreeMap<String, Vec<DocumentId>>,
}

impl StickersSet {
    /// Creates a new sticker set.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: StickerSetId,
        access_hash: u64,
        hash: u64,
        title: impl Into<String>,
        short_name: impl Into<String>,
        count: i32,
        flags: StickersSetFlags,
        install_date: TimeId,
    ) -> Self {
        Self {
            id,
            access_hash,
            hash,
            thumbnail_document_id: None,
            thumbnail_size: None,
            thumbnail_type: StickerType::Webp,
            title: title.into(),
            short_name: short_name.into(),
            count,
            locked: 0,
            flags,
            install_date,
            covers: Vec::new(),
            stickers: Vec::new(),
            dates: Vec::new(),
            emoji: BTreeMap::new(),
        }
    }

    /// Returns the category of this set (`Emoji` > `Masks` > `Stickers`),
    /// matching `StickersSet::type()`.
    pub const fn kind(&self) -> StickersType {
        if self.flags.contains(StickersSetFlags::EMOJI) {
            StickersType::Emoji
        } else if self.flags.contains(StickersSetFlags::MASKS) {
            StickersType::Masks
        } else {
            StickersType::Stickers
        }
    }

    /// Returns `true` if the set is installed and not archived, matching `SetInMyList`.
    pub const fn is_in_my_list(&self) -> bool {
        self.flags.contains(StickersSetFlags::INSTALLED)
            && !self.flags.contains(StickersSetFlags::ARCHIVED)
    }

    /// Returns the document ID to use as the set's thumbnail icon,
    /// matching `StickersSet::lookupThumbnailDocument()`.
    pub fn lookup_thumbnail_document(&self) -> Option<DocumentId> {
        if let Some(thumb_id) = self.thumbnail_document_id
            && self.stickers.contains(&thumb_id)
        {
            return Some(thumb_id);
        }
        self.stickers
            .first()
            .copied()
            .or_else(|| self.covers.first().copied())
    }
}

/// Input metadata for feeding a sticker set header (equivalent to `MTPDstickerSet`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StickerSetUpdate {
    /// Set ID.
    pub id: StickerSetId,
    /// Access hash.
    pub access_hash: u64,
    /// Server hash.
    pub hash: u64,
    /// Raw title.
    pub title: String,
    /// Short name slug.
    pub short_name: String,
    /// Total sticker count.
    pub count: i32,
    /// Parsed server flags (archived, official, masks, emoji, installed, text_color, etc.).
    pub flags: StickersSetFlags,
    /// Optional install date (`Some(0)` or `Some(ts)` when installed, `None` when not installed).
    pub install_date: Option<TimeId>,
    /// Optional thumbnail document ID.
    pub thumbnail_document_id: Option<DocumentId>,
}

/// Configuration for [`StickersStore::get_list_by_emoji`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmojiSearchOptions {
    /// Deterministic shuffle seed for tie-breaking sort keys.
    pub seed: u64,
    /// Whether the current user has Telegram Premium.
    pub is_premium: bool,
    /// Number of non-premium stickers interleaved before each premium sticker for Premium users
    /// (`stickers_normal_by_emoji_per_premium_num`, default `2`).
    pub normals_per_premium: usize,
    /// Number of premium stickers appended at the end for non-Premium users
    /// (`stickers_premium_by_emoji_num`, default `0`).
    pub premiums_to_end_for_free: usize,
}

impl Default for EmojiSearchOptions {
    fn default() -> Self {
        Self {
            seed: 0,
            is_premium: false,
            normals_per_premium: 2,
            premiums_to_end_for_free: 0,
        }
    }
}

/// Limits configuration for favorite stickers and saved GIFs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StickerLimits {
    /// Non-premium faved stickers limit (default 5).
    pub faved_default: usize,
    /// Premium faved stickers limit (default 10).
    pub faved_premium: usize,
    /// Non-premium saved GIFs limit (default 200).
    pub saved_gifs_default: usize,
    /// Premium saved GIFs limit (default 400).
    pub saved_gifs_premium: usize,
    /// Server `stickersRecentLimit` (default 30).
    pub recent_stickers_limit: usize,
}

impl Default for StickerLimits {
    fn default() -> Self {
        Self {
            faved_default: FAVED_LIMIT_DEFAULT,
            faved_premium: FAVED_LIMIT_PREMIUM,
            saved_gifs_default: SAVED_GIFS_LIMIT_DEFAULT,
            saved_gifs_premium: SAVED_GIFS_LIMIT_PREMIUM,
            recent_stickers_limit: DEFAULT_RECENT_STICKERS_LIMIT,
        }
    }
}

impl StickerLimits {
    /// Returns the effective faved stickers limit.
    pub const fn faved_current(&self, is_premium: bool) -> usize {
        if is_premium {
            self.faved_premium
        } else {
            self.faved_default
        }
    }

    /// Returns the effective saved GIFs limit.
    pub const fn saved_gifs_current(&self, is_premium: bool) -> usize {
        if is_premium {
            self.saved_gifs_premium
        } else {
            self.saved_gifs_default
        }
    }
}

/// Entry produced by [`StickersStore::collect_recent_stickers`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecentStickerEntry {
    /// Sticker document ID.
    pub document_id: DocumentId,
    /// Whether this sticker came from the legacy `CUSTOM_SET_ID` pack.
    pub is_custom: bool,
}

/// Section model produced for the tabbed stickers selector (`StickersListWidget::Set`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StickerPanelSection {
    /// Section / set ID (`FAVED_SET_ID`, `RECENT_SET_ID`, `MEGAGROUP_SET_ID`, or real set ID).
    pub id: StickerSetId,
    /// Flags on this section.
    pub flags: StickersSetFlags,
    /// Display title.
    pub title: String,
    /// Short name slug.
    pub short_name: String,
    /// Reported count.
    pub count: i32,
    /// `true` for featured/external sets with an "Add" button layout.
    pub external_layout: bool,
    /// Filtered sticker document IDs in display order.
    pub stickers: Vec<DocumentId>,
    /// Thumbnail document ID for footer tab icons.
    pub thumbnail_document_id: Option<DocumentId>,
}

/// Footer tab icon descriptor produced for the tabbed selector (`StickersListWidget::fillIcons`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StickerTabIcon {
    /// Target set ID (`FAVED_SET_ID`, `RECENT_SET_ID`, `MEGAGROUP_SET_ID`, or real set ID).
    pub set_id: StickerSetId,
    /// Thumbnail document ID (for regular installed sets).
    pub thumbnail_document_id: Option<DocumentId>,
    /// Computed icon dimensions scaled into the footer icon frame.
    pub icon_size: Size,
}

/// Parameters controlling section list building for the tabbed selector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PanelBuildOptions {
    /// Whether building the Masks list instead of regular Stickers.
    pub is_masks: bool,
    /// Whether Premium stickers may be shown (`session.premiumPossible()`).
    pub premium_possible: bool,
    /// Whether to ignore `RECENT_DISPLAY_LIMIT` (20) when collecting recent stickers.
    pub unlimited_recent: bool,
    /// Optional set ID to exclude from the list.
    pub exclude_set_id: Option<StickerSetId>,
    /// Optional megagroup sticker set configuration.
    pub megagroup_set: Option<MegagroupSetConfig>,
}

/// Megagroup sticker set display state for the panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MegagroupSetConfig {
    /// Configured group sticker set ID (`None` if no set chosen yet).
    pub set_id: Option<StickerSetId>,
    /// Whether current user can edit group stickers.
    pub can_edit_stickers: bool,
    /// Whether the user hid the group stickers section to the bottom.
    pub section_hidden: bool,
}

/// Outcome of modifying favorite stickers or saved GIFs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LimitMutationResult {
    /// Whether the underlying list changed.
    pub changed: bool,
    /// Whether an oldest item was evicted due to hitting the limit (triggering a Premium toast
    /// for non-Premium users).
    pub limit_reached: bool,
}

/// In-memory store managing sticker sets, orders, recent/faved stickers, saved GIFs,
/// featured unread tracking, emoji search, and selector section building.
///
/// Direct port of `Data::Stickers` (`data/stickers/data_stickers.cpp`) and the model parts of
/// `ChatHelpers::StickersListWidget` (`chat_helpers/stickers_list_widget.cpp`).
#[derive(Debug, Clone, Default)]
pub struct StickersStore {
    /// All known sticker sets by ID.
    pub sets: BTreeMap<StickerSetId, StickersSet>,
    /// Known sticker/GIF documents by ID.
    pub documents: HashMap<DocumentId, StickerDocument>,
    /// Installed sticker sets order.
    pub sets_order: Vec<StickerSetId>,
    /// Installed mask sets order.
    pub mask_sets_order: Vec<StickerSetId>,
    /// Installed custom emoji sets order.
    pub emoji_sets_order: Vec<StickerSetId>,
    /// Featured sticker sets order.
    pub featured_sets_order: Vec<StickerSetId>,
    /// Featured custom emoji sets order.
    pub featured_emoji_sets_order: Vec<StickerSetId>,
    /// Archived sticker sets order.
    pub archived_sets_order: Vec<StickerSetId>,
    /// Archived mask sets order.
    pub archived_mask_sets_order: Vec<StickerSetId>,
    /// Saved GIF document IDs (most recently saved first).
    pub saved_gifs: Vec<DocumentId>,
    /// Legacy local recent sticker pack `(doc_id, rating)`.
    pub old_recent_pack: Vec<(DocumentId, u16)>,
    /// Count of unread featured sticker sets (non-emoji).
    pub featured_sets_unread_count: usize,
    /// Featured set IDs queued to be marked read via `messages.readFeaturedStickers`.
    pub pending_featured_reads: BTreeSet<StickerSetId>,
    /// Sets installed locally during the current panel session (kept visible in Featured tab).
    pub locally_installed_sets: BTreeSet<StickerSetId>,
    /// Configurable account limits.
    pub limits: StickerLimits,
}

impl StickersStore {
    /// Creates an empty store with default limits.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers or updates a document in the store.
    pub fn insert_document(&mut self, doc: StickerDocument) {
        self.documents.insert(doc.id, doc);
    }

    /// Returns whether a cached timestamp needs refreshing (1 hour timeout).
    pub const fn update_needed(last_ms: i64, now_ms: i64) -> bool {
        last_ms == 0 || now_ms >= last_ms + STICKERS_UPDATE_TIMEOUT_MS
    }

    /// Returns the order vector for the given [`StickersType`].
    pub fn order_for(&self, kind: StickersType) -> &[StickerSetId] {
        match kind {
            StickersType::Stickers => &self.sets_order,
            StickersType::Masks => &self.mask_sets_order,
            StickersType::Emoji => &self.emoji_sets_order,
        }
    }

    /// Returns a mutable reference to the order vector for the given [`StickersType`].
    fn order_for_mut(&mut self, kind: StickersType) -> &mut Vec<StickerSetId> {
        match kind {
            StickersType::Stickers => &mut self.sets_order,
            StickersType::Masks => &mut self.mask_sets_order,
            StickersType::Emoji => &mut self.emoji_sets_order,
        }
    }

    /// Feeds a sticker set header into the store, preserving client flags and updating
    /// archived order when the `ARCHIVED` flag transitions.
    ///
    /// Direct port of `Stickers::feedSet(const MTPStickerSet&)` in `data_stickers.cpp`.
    pub fn feed_set(&mut self, info: StickerSetUpdate, now: TimeId) -> StickerSetId {
        let id = info.id;
        let old_flags = self
            .sets
            .get(&id)
            .map(|s| s.flags)
            .unwrap_or(StickersSetFlags::EMPTY);

        if let Some(set) = self.sets.get_mut(&id) {
            set.access_hash = info.access_hash;
            set.title = info.title;
            set.short_name = info.short_name;
            let client_flags = set.flags
                & (StickersSetFlags::FEATURED
                    | StickersSetFlags::UNREAD
                    | StickersSetFlags::NOT_LOADED
                    | StickersSetFlags::SPECIAL
                    | StickersSetFlags::INSTALLED);
            set.flags = info.flags | client_flags;
            set.install_date = match info.install_date {
                Some(d) if d != 0 => d,
                Some(_) => now,
                None => 0,
            };
            if set.count != info.count || set.hash != info.hash || set.emoji.is_empty() {
                set.count = info.count;
                set.hash = info.hash;
                set.flags |= StickersSetFlags::NOT_LOADED;
            }
            set.thumbnail_document_id = info.thumbnail_document_id;
        } else {
            let install_date = info.install_date.unwrap_or(0);
            let mut set = StickersSet::new(
                id,
                info.access_hash,
                info.hash,
                info.title,
                info.short_name,
                info.count,
                info.flags | StickersSetFlags::NOT_LOADED,
                install_date,
            );
            set.thumbnail_document_id = info.thumbnail_document_id;
            self.sets.insert(id, set);
        }

        if let Some(set) = self.sets.get(&id) {
            let changed_flags = old_flags ^ set.flags;
            if changed_flags.contains(StickersSetFlags::ARCHIVED) {
                let is_archived = set.flags.contains(StickersSetFlags::ARCHIVED);
                let is_masks = set.kind() == StickersType::Masks;
                let archived_order = if is_masks {
                    &mut self.archived_mask_sets_order
                } else {
                    &mut self.archived_sets_order
                };
                if is_archived {
                    if !archived_order.contains(&id) {
                        archived_order.insert(0, id);
                    }
                } else {
                    archived_order.retain(|&x| x != id);
                }
            }
        }
        id
    }

    /// Populates a set's stickers and emoji-to-stickers map, clearing `NOT_LOADED` and
    /// cleaning up `CUSTOM_SET_ID` and obsolete entries in `old_recent_pack`.
    ///
    /// Direct port of `Stickers::feedSetStickers` in `data_stickers.cpp`.
    pub fn feed_set_stickers(
        &mut self,
        set_id: StickerSetId,
        stickers: Vec<DocumentId>,
        emoji_packs: &[(String, Vec<DocumentId>)],
    ) {
        let Some(old_stickers) = self.sets.get(&set_id).map(|s| s.stickers.clone()) else {
            return;
        };

        // Remove from CUSTOM_SET_ID
        if let Some(custom) = self.sets.get_mut(&CUSTOM_SET_ID) {
            custom.stickers.retain(|d| !stickers.contains(d));
            if custom.stickers.is_empty() {
                self.sets.remove(&CUSTOM_SET_ID);
            }
        }

        // Prune stickers removed from this set out of old_recent_pack
        self.old_recent_pack
            .retain(|(doc_id, _)| !(old_stickers.contains(doc_id) && !stickers.contains(doc_id)));

        let mut emoji_map: BTreeMap<String, Vec<DocumentId>> = BTreeMap::new();
        for (raw_emoji, docs) in emoji_packs {
            let canonical = strip_skin_tone(raw_emoji);
            if !canonical.is_empty() {
                emoji_map.insert(canonical, docs.clone());
            }
        }

        if let Some(set) = self.sets.get_mut(&set_id) {
            set.flags &= !StickersSetFlags::NOT_LOADED;
            set.stickers = stickers;
            set.emoji = emoji_map;
        }
    }

    /// Processes a full installed sets list from the server (`setsReceived` / `masksReceived` / `emojiReceived`).
    ///
    /// Returns the IDs of sets that still need their stickers fetched (`NOT_LOADED` or empty).
    pub fn sets_received(
        &mut self,
        kind: StickersType,
        list: Vec<StickerSetUpdate>,
        now: TimeId,
    ) -> Vec<StickerSetId> {
        self.order_for_mut(kind).clear();

        for set in self.sets.values_mut() {
            if !set.flags.contains(StickersSetFlags::ARCHIVED) && set.kind() == kind {
                set.flags &= !StickersSetFlags::INSTALLED;
                set.install_date = 0;
            }
        }

        let mut to_request = Vec::new();
        for info in list {
            let id = self.feed_set(info, now);
            if let Some(set) = self.sets.get(&id) {
                let keep = !set.flags.contains(StickersSetFlags::ARCHIVED)
                    || set.flags.contains(StickersSetFlags::OFFICIAL);
                if keep {
                    let needs_load =
                        set.stickers.is_empty() || set.flags.contains(StickersSetFlags::NOT_LOADED);
                    self.order_for_mut(kind).push(id);
                    if needs_load {
                        to_request.push(id);
                    }
                }
            }
        }

        // Remove obsolete uninstalled sets and prune their stickers from old_recent_pack.
        let mut removed_docs = BTreeSet::new();
        let mut to_erase = Vec::new();
        for (&id, set) in &self.sets {
            let installed = set.flags.contains(StickersSetFlags::INSTALLED);
            let featured = set.flags.contains(StickersSetFlags::FEATURED);
            let special = set.flags.contains(StickersSetFlags::SPECIAL);
            let archived = set.flags.contains(StickersSetFlags::ARCHIVED);
            let emoji = set.flags.contains(StickersSetFlags::EMOJI);
            let locked = set.locked > 0;
            if !installed {
                for &d in &set.stickers {
                    removed_docs.insert(d);
                }
            }
            if !(installed || featured || special || archived || emoji || locked) {
                to_erase.push(id);
            }
        }
        if !removed_docs.is_empty() {
            self.old_recent_pack
                .retain(|(d, _)| !removed_docs.contains(d));
        }
        for id in to_erase {
            self.sets.remove(&id);
        }

        to_request
    }

    /// Installs a sticker/mask/emoji set locally at index 0 of its order list.
    ///
    /// Exact port of `Stickers::installLocally` in `data/stickers/data_stickers.cpp`.
    pub fn install_locally(&mut self, set_id: StickerSetId, now: TimeId) -> bool {
        let Some(set) = self.sets.get_mut(&set_id) else {
            return false;
        };
        let old_flags = set.flags;
        set.flags &= !(StickersSetFlags::ARCHIVED | StickersSetFlags::UNREAD);
        set.flags |= StickersSetFlags::INSTALLED;
        set.install_date = now;
        let changed_flags = old_flags ^ set.flags;
        let kind = set.kind();
        let was_unread_featured_sticker = old_flags.contains(StickersSetFlags::FEATURED)
            && changed_flags.contains(StickersSetFlags::UNREAD)
            && kind != StickersType::Emoji;
        let set_stickers = set.stickers.clone();

        let order = self.order_for_mut(kind);
        if order.first() != Some(&set_id) {
            order.retain(|&id| id != set_id);
            order.insert(0, set_id);
        }

        if let Some(custom) = self.sets.get_mut(&CUSTOM_SET_ID) {
            custom.stickers.retain(|d| !set_stickers.contains(d));
            if custom.stickers.is_empty() {
                self.sets.remove(&CUSTOM_SET_ID);
            }
        }

        if kind != StickersType::Emoji && changed_flags.contains(StickersSetFlags::ARCHIVED) {
            let archived_order = if kind == StickersType::Masks {
                &mut self.archived_mask_sets_order
            } else {
                &mut self.archived_sets_order
            };
            archived_order.retain(|&id| id != set_id);
        }

        if was_unread_featured_sticker && self.featured_sets_unread_count > 0 {
            self.featured_sets_unread_count -= 1;
        }
        self.locally_installed_sets.insert(set_id);
        true
    }

    /// Undoes a local installation (e.g. when the server returns STICKERSET_INVALID).
    ///
    /// Exact port of `Stickers::undoInstallLocally` in `data/stickers/data_stickers.cpp`.
    pub fn undo_install_locally(&mut self, set_id: StickerSetId) -> bool {
        let Some(set) = self.sets.get_mut(&set_id) else {
            return false;
        };
        let kind = set.kind();
        set.flags &= !StickersSetFlags::INSTALLED;
        set.install_date = 0;
        self.order_for_mut(kind).retain(|&id| id != set_id);
        self.locally_installed_sets.remove(&set_id);
        true
    }

    /// Applies an archive result returned after installing a set when at the server cap.
    ///
    /// Port of `Stickers::applyArchivedResult` in `data/stickers/data_stickers.cpp`.
    pub fn apply_archived_result(&mut self, archived: Vec<StickerSetUpdate>, now: TimeId) {
        for info in archived {
            let id = self.feed_set(info, now);
            let Some(kind) = self.sets.get(&id).map(StickersSet::kind) else {
                continue;
            };
            if kind == StickersType::Emoji {
                continue;
            }
            self.order_for_mut(kind).retain(|&x| x != id);
        }
    }

    /// Looks up all canonical emojis associated with `doc_id` in its owning sticker set.
    ///
    /// Direct port of `Stickers::getEmojiListFromSet` in `data_stickers.cpp`.
    pub fn get_emoji_list_from_set(&self, doc_id: DocumentId) -> Option<Vec<String>> {
        let doc = self.documents.get(&doc_id)?;
        let set_id = doc.set_id?;
        let set = self.sets.get(&set_id)?;
        let result: Vec<String> = set
            .emoji
            .iter()
            .filter_map(|(emoji, docs)| {
                if docs.contains(&doc_id) {
                    Some(emoji.clone())
                } else {
                    None
                }
            })
            .collect();
        if result.is_empty() {
            None
        } else {
            Some(result)
        }
    }

    /// Records usage of a sticker, moving/inserting it at the front of `CLOUD_RECENT_SET_ID`,
    /// updating dates and emoji index, trimming `old_recent_pack` against `recent_stickers_limit`,
    /// and removing it from `CUSTOM_SET_ID`.
    ///
    /// Direct port of `Stickers::incrementSticker` in `data/stickers/data_stickers.cpp`.
    pub fn increment_sticker(&mut self, doc_id: DocumentId, now: TimeId) -> bool {
        let Some(doc) = self.documents.get(&doc_id) else {
            return false;
        };
        if doc.set_id.is_none() {
            return false;
        }
        let emojis_from_set = self.get_emoji_list_from_set(doc_id);

        let set = self.sets.entry(CLOUD_RECENT_SET_ID).or_insert_with(|| {
            StickersSet::new(
                CLOUD_RECENT_SET_ID,
                0,
                0,
                "Recent",
                "",
                0,
                StickersSetFlags::SPECIAL,
                0,
            )
        });

        let mut removed_from_emoji = Vec::new();
        let existing_index = set.stickers.iter().position(|&d| d == doc_id);
        if let Some(idx) = existing_index
            && idx > 0
        {
            if !set.dates.is_empty() && idx < set.dates.len() {
                set.dates.remove(idx);
            }
            set.stickers.remove(idx);
            let mut empty_keys = Vec::new();
            for (emoji, list) in &mut set.emoji {
                if let Some(pos) = list.iter().position(|&d| d == doc_id) {
                    removed_from_emoji.push(emoji.clone());
                    list.remove(pos);
                    if list.is_empty() {
                        empty_keys.push(emoji.clone());
                    }
                }
            }
            for k in empty_keys {
                set.emoji.remove(&k);
            }
        }

        let moved_or_added = existing_index != Some(0);
        if moved_or_added {
            if set.dates.len() == set.stickers.len() {
                set.dates.insert(0, now);
            }
            set.stickers.insert(0, doc_id);
            if let Some(list) = emojis_from_set {
                for emoji in list {
                    set.emoji.entry(emoji).or_default().insert(0, doc_id);
                }
            } else {
                for emoji in removed_from_emoji {
                    set.emoji.entry(emoji).or_default().insert(0, doc_id);
                }
            }
        }

        let cloud_len = set.stickers.len();

        // Remove from old_recent_pack and enforce stickersRecentLimit
        self.old_recent_pack.retain(|(d, _)| *d != doc_id);
        while !self.old_recent_pack.is_empty()
            && cloud_len + self.old_recent_pack.len() > self.limits.recent_stickers_limit
        {
            self.old_recent_pack.pop();
        }

        // Remove from CUSTOM_SET_ID
        if let Some(custom) = self.sets.get_mut(&CUSTOM_SET_ID) {
            custom.stickers.retain(|&d| d != doc_id);
            if custom.stickers.is_empty() {
                self.sets.remove(&CUSTOM_SET_ID);
            }
        }

        moved_or_added
    }

    /// Removes `doc_id` from `CLOUD_RECENT_SET_ID`.
    pub fn remove_from_recent_set(&mut self, doc_id: DocumentId) -> bool {
        self.remove_from_set(doc_id, CLOUD_RECENT_SET_ID)
    }

    fn remove_from_set(&mut self, doc_id: DocumentId, set_id: StickerSetId) -> bool {
        let Some(set) = self.sets.get_mut(&set_id) else {
            return false;
        };
        let Some(idx) = set.stickers.iter().position(|&d| d == doc_id) else {
            return false;
        };
        set.stickers.remove(idx);
        if idx < set.dates.len() {
            set.dates.remove(idx);
        }
        let mut empty_keys = Vec::new();
        for (emoji, list) in &mut set.emoji {
            list.retain(|&d| d != doc_id);
            if list.is_empty() {
                empty_keys.push(emoji.clone());
            }
        }
        for k in empty_keys {
            set.emoji.remove(&k);
        }
        if set.stickers.is_empty() {
            self.sets.remove(&set_id);
        }
        true
    }

    /// Returns `true` if `doc_id` is currently in `FAVED_SET_ID`.
    pub fn is_faved(&self, doc_id: DocumentId) -> bool {
        self.sets
            .get(&FAVED_SET_ID)
            .is_some_and(|set| set.stickers.contains(&doc_id))
    }

    /// Adds or removes a sticker from favorites (`FAVED_SET_ID`), moving it to front if already
    /// faved and enforcing the default (5) or Premium (10) limit.
    ///
    /// Direct port of `Stickers::setFaved`, `setIsFaved`, `pushFavedToFront`, `moveFavedToFront`,
    /// `checkFavedLimit`, and `setIsNotFaved` in `data/stickers/data_stickers.cpp`.
    pub fn set_faved(
        &mut self,
        doc_id: DocumentId,
        faved: bool,
        emoji_override: Option<Vec<String>>,
        is_premium: bool,
    ) -> LimitMutationResult {
        if !faved {
            let changed = self.remove_from_set(doc_id, FAVED_SET_ID);
            return LimitMutationResult {
                changed,
                limit_reached: false,
            };
        }

        let resolved_emojis = emoji_override
            .or_else(|| self.get_emoji_list_from_set(doc_id))
            .unwrap_or_else(|| {
                self.documents
                    .get(&doc_id)
                    .map(|d| strip_skin_tone(&d.alt_emoji))
                    .filter(|e| !e.is_empty())
                    .into_iter()
                    .collect()
            });

        let limit = self.limits.faved_current(is_premium);
        let set = self.sets.entry(FAVED_SET_ID).or_insert_with(|| {
            StickersSet::new(
                FAVED_SET_ID,
                0,
                0,
                "Favorites",
                "",
                0,
                StickersSetFlags::SPECIAL,
                0,
            )
        });

        if let Some(idx) = set.stickers.iter().position(|&d| d == doc_id) {
            if idx == 0 {
                return LimitMutationResult {
                    changed: false,
                    limit_reached: false,
                };
            }
            set.stickers[..=idx].rotate_right(1);
            for list in set.emoji.values_mut() {
                if let Some(pos) = list.iter().position(|&d| d == doc_id)
                    && pos > 0
                {
                    list[..=pos].rotate_right(1);
                }
            }
            return LimitMutationResult {
                changed: true,
                limit_reached: false,
            };
        }

        set.stickers.insert(0, doc_id);
        for emoji in resolved_emojis {
            let canonical = strip_skin_tone(&emoji);
            if !canonical.is_empty() {
                set.emoji.entry(canonical).or_default().insert(0, doc_id);
            }
        }

        let mut limit_reached = false;
        if set.stickers.len() > limit
            && let Some(removing) = set.stickers.pop()
        {
            limit_reached = true;
            let mut empty_keys = Vec::new();
            for (emoji, list) in &mut set.emoji {
                list.retain(|&d| d != removing);
                if list.is_empty() {
                    empty_keys.push(emoji.clone());
                }
            }
            for k in empty_keys {
                set.emoji.remove(&k);
            }
        }

        LimitMutationResult {
            changed: true,
            limit_reached,
        }
    }

    /// Adds or moves `doc_id` to the front of `saved_gifs`, enforcing the 200 / 400 limit.
    ///
    /// Direct port of `Stickers::addSavedGif` in `data/stickers/data_stickers.cpp`.
    pub fn add_saved_gif(&mut self, doc_id: DocumentId, is_premium: bool) -> LimitMutationResult {
        if self.saved_gifs.first() == Some(&doc_id) {
            return LimitMutationResult {
                changed: false,
                limit_reached: false,
            };
        }
        self.saved_gifs.retain(|&d| d != doc_id);
        self.saved_gifs.insert(0, doc_id);

        let limit = self.limits.saved_gifs_current(is_premium);
        let limit_reached = if self.saved_gifs.len() > limit {
            self.saved_gifs.pop();
            true
        } else {
            false
        };
        LimitMutationResult {
            changed: true,
            limit_reached,
        }
    }

    /// Removes `doc_id` from `saved_gifs`.
    pub fn remove_saved_gif(&mut self, doc_id: DocumentId) -> bool {
        let before = self.saved_gifs.len();
        self.saved_gifs.retain(|&d| d != doc_id);
        self.saved_gifs.len() != before
    }

    /// Replaces `saved_gifs` from a server `messages.savedGifs` response, filtering to `is_gifv`.
    ///
    /// Direct port of `Stickers::gifsReceived` in `data/stickers/data_stickers.cpp`.
    pub fn gifs_received(&mut self, doc_ids: &[DocumentId]) {
        self.saved_gifs.clear();
        self.saved_gifs.reserve(doc_ids.len());
        for &id in doc_ids {
            if self.documents.get(&id).is_some_and(|d| d.is_gifv) {
                self.saved_gifs.push(id);
            }
        }
    }

    /// Processes a featured sticker or custom emoji sets response, updating `FEATURED` and `UNREAD`
    /// flags, pruning obsolete sets, and recomputing `featured_sets_unread_count` (which counts
    /// non-emoji featured unread sets only).
    ///
    /// Direct port of `Stickers::featuredReceived` in `data/stickers/data_stickers.cpp`.
    pub fn featured_sets_received(
        &mut self,
        kind: StickersType,
        list: Vec<StickerSetUpdate>,
        unread_ids: &BTreeSet<StickerSetId>,
    ) -> Vec<StickerSetId> {
        let is_emoji = kind == StickersType::Emoji;
        if is_emoji {
            self.featured_emoji_sets_order.clear();
        } else {
            self.featured_sets_order.clear();
        }

        for set in self.sets.values_mut() {
            if set.kind() == kind {
                set.flags &= !StickersSetFlags::FEATURED;
            }
        }

        let mut to_request = Vec::new();
        for info in list {
            let id = info.id;
            let flags = StickersSetFlags::FEATURED
                | if unread_ids.contains(&id) {
                    StickersSetFlags::UNREAD
                } else {
                    StickersSetFlags::EMPTY
                }
                | info.flags;
            let install_date = info.install_date.unwrap_or(0);

            if let Some(set) = self.sets.get_mut(&id) {
                set.access_hash = info.access_hash;
                set.title = info.title;
                set.short_name = info.short_name;
                set.flags = flags
                    | (set.flags & (StickersSetFlags::NOT_LOADED | StickersSetFlags::SPECIAL));
                set.install_date = install_date;
                if set.count != info.count || set.hash != info.hash || set.emoji.is_empty() {
                    set.count = info.count;
                    set.hash = info.hash;
                    set.flags |= StickersSetFlags::NOT_LOADED;
                }
                set.thumbnail_document_id = info.thumbnail_document_id;
            } else {
                let mut set = StickersSet::new(
                    id,
                    info.access_hash,
                    info.hash,
                    info.title,
                    info.short_name,
                    info.count,
                    flags | StickersSetFlags::NOT_LOADED,
                    install_date,
                );
                set.thumbnail_document_id = info.thumbnail_document_id;
                self.sets.insert(id, set);
            }

            if is_emoji {
                self.featured_emoji_sets_order.push(id);
            } else {
                self.featured_sets_order.push(id);
            }
            if let Some(set) = self.sets.get(&id)
                && (set.stickers.is_empty() || set.flags.contains(StickersSetFlags::NOT_LOADED))
            {
                to_request.push(id);
            }
        }

        let mut unread_count = 0usize;
        let mut to_erase = Vec::new();
        for (&id, set) in &self.sets {
            let keep = set.flags.contains(StickersSetFlags::INSTALLED)
                || set.flags.contains(StickersSetFlags::FEATURED)
                || set.flags.contains(StickersSetFlags::SPECIAL)
                || set.flags.contains(StickersSetFlags::ARCHIVED)
                || set.flags.contains(StickersSetFlags::EMOJI)
                || set.locked > 0;
            if keep {
                if set.flags.contains(StickersSetFlags::FEATURED)
                    && set.flags.contains(StickersSetFlags::UNREAD)
                    && !set.flags.contains(StickersSetFlags::EMOJI)
                {
                    unread_count += 1;
                }
            } else {
                to_erase.push(id);
            }
        }
        for id in to_erase {
            self.sets.remove(&id);
        }
        self.featured_sets_unread_count = unread_count;
        to_request
    }

    /// Queues a featured set ID to be marked read, matching `ApiWrap::readFeaturedSetDelayed`.
    pub fn queue_read_featured_set(&mut self, set_id: StickerSetId) -> bool {
        self.pending_featured_reads.insert(set_id)
    }

    /// Flushes queued featured read IDs, clearing `UNREAD` on each set, decrementing
    /// `featured_sets_unread_count`, and returning the IDs to send to the server.
    ///
    /// Exact port of `ApiWrap::readFeaturedSets` in `Telegram/SourceFiles/apiwrap.cpp`.
    pub fn flush_read_featured_sets(&mut self) -> Vec<StickerSetId> {
        let pending = std::mem::take(&mut self.pending_featured_reads);
        let mut read_ids = Vec::with_capacity(pending.len());
        let mut count = self.featured_sets_unread_count;

        for set_id in pending {
            if let Some(set) = self.sets.get_mut(&set_id) {
                set.flags &= !StickersSetFlags::UNREAD;
                read_ids.push(set_id);
                count = count.saturating_sub(1);
            }
        }
        if !read_ids.is_empty() {
            self.featured_sets_unread_count = count;
        }
        read_ids
    }

    /// Collects recent stickers for display by merging `CLOUD_RECENT_SET_ID` (or
    /// `CLOUD_RECENT_ATTACHED_SET_ID` for masks), `old_recent_pack`, and `CUSTOM_SET_ID`,
    /// excluding any stickers in `faved`, deduplicating, and capping at `RECENT_DISPLAY_LIMIT` (20)
    /// unless `unlimited` is true.
    ///
    /// Direct port of `StickersListWidget::collectRecentStickers` in `stickers_list_widget.cpp`.
    pub fn collect_recent_stickers(
        &self,
        is_masks: bool,
        faved: &BTreeSet<DocumentId>,
        unlimited: bool,
    ) -> Vec<RecentStickerEntry> {
        let cloud_id = if is_masks {
            CLOUD_RECENT_ATTACHED_SET_ID
        } else {
            CLOUD_RECENT_SET_ID
        };
        let cloud_stickers = self
            .sets
            .get(&cloud_id)
            .map(|s| s.stickers.as_slice())
            .unwrap_or(&[]);
        let custom_stickers = if is_masks {
            &[][..]
        } else {
            self.sets
                .get(&CUSTOM_SET_ID)
                .map(|s| s.stickers.as_slice())
                .unwrap_or(&[])
        };
        let old_recent = if is_masks {
            &[][..]
        } else {
            self.old_recent_pack.as_slice()
        };

        let cloud_count = cloud_stickers.len();
        let mut result: Vec<RecentStickerEntry> = Vec::new();

        let mut add = |doc_id: DocumentId, custom: bool| {
            if result.len() >= RECENT_DISPLAY_LIMIT && !unlimited {
                return;
            }
            if let Some(pos) = result.iter().position(|e| e.document_id == doc_id) {
                if pos >= cloud_count && custom {
                    result[pos].is_custom = true;
                }
            } else if !faved.contains(&doc_id) {
                result.push(RecentStickerEntry {
                    document_id: doc_id,
                    is_custom: custom,
                });
            }
        };

        for &doc_id in cloud_stickers {
            add(doc_id, false);
        }
        for &(doc_id, _) in old_recent {
            add(doc_id, false);
        }
        for &doc_id in custom_stickers {
            add(doc_id, true);
        }

        result
    }

    /// Searches stickers matching one or more `emojis` across cloud recent stickers,
    /// installed non-archived sets, and optional remote `other_stickers`, ranking with
    /// tdesktop's exact sort keys and applying Premium / non-Premium interleaving.
    ///
    /// Direct port of `Stickers::getListByEmoji` in `data/stickers/data_stickers.cpp`.
    pub fn get_list_by_emoji(
        &self,
        emojis: &[&str],
        other_stickers: Option<&[DocumentId]>,
        options: EmojiSearchOptions,
    ) -> Vec<DocumentId> {
        let mut all: BTreeSet<String> = BTreeSet::new();
        for &one in emojis {
            let canonical = strip_skin_tone(one);
            if !canonical.is_empty() {
                all.insert(canonical);
            }
        }
        if all.is_empty() {
            return Vec::new();
        }
        let single = if all.len() == 1 {
            all.iter().next().cloned()
        } else {
            None
        };

        let mut result: Vec<(DocumentId, i32)> = Vec::new();
        let mut my_counter = 0i32;

        // 1. Cloud recent stickers
        if let Some(recent) = self.sets.get(&CLOUD_RECENT_SET_ID) {
            let list: Option<&[DocumentId]> = if let Some(ref s) = single {
                recent.emoji.get(s).map(Vec::as_slice)
            } else {
                Some(recent.stickers.as_slice())
            };
            if let Some(list) = list {
                for (i, &doc_id) in list.iter().enumerate() {
                    let Some(doc) = self.documents.get(&doc_id) else {
                        continue;
                    };
                    let idx = if single.is_none() {
                        let main = strip_skin_tone(&doc.alt_emoji);
                        if main.is_empty() || !all.contains(&main) {
                            continue;
                        }
                        Some(i)
                    } else {
                        recent.stickers.iter().position(|&d| d == doc_id)
                    };
                    let usage_date = idx
                        .and_then(|pos| recent.dates.get(pos).copied())
                        .unwrap_or(0);
                    let date = if usage_date != 0 {
                        usage_date
                    } else {
                        let install = self.recent_install_date(doc);
                        if install != 0 {
                            install
                        } else {
                            create_sort_key(doc, SORT_KEY_SLICE * 6, options.seed)
                        }
                    };
                    result.push((doc_id, date));
                }
            }
        }

        // 2. Installed non-archived sets
        for &set_id in &self.sets_order {
            let Some(set) = self.sets.get(&set_id) else {
                continue;
            };
            if set.flags.contains(StickersSetFlags::ARCHIVED) || set.emoji.is_empty() {
                continue;
            }
            let my = set.flags.contains(StickersSetFlags::INSTALLED);
            let list: Option<&[DocumentId]> = if let Some(ref s) = single {
                set.emoji.get(s).map(Vec::as_slice)
            } else {
                Some(set.stickers.as_slice())
            };
            if let Some(list) = list {
                for &doc_id in list {
                    let Some(doc) = self.documents.get(&doc_id) else {
                        continue;
                    };
                    if single.is_none() {
                        let main = strip_skin_tone(&doc.alt_emoji);
                        if main.is_empty() || !all.contains(&main) {
                            continue;
                        }
                    }
                    let install_date = if my { set.install_date } else { 0 };
                    let date = if install_date > 1 {
                        install_date_adjusted(install_date, doc)
                    } else if my {
                        create_my_sort_key(doc, &mut my_counter)
                    } else {
                        create_sort_key(doc, SORT_KEY_SLICE * 2, options.seed)
                    };
                    if !result.iter().any(|&(d, _)| d == doc_id) {
                        result.push((doc_id, date));
                    }
                }
            }
        }

        // 3. Remote stickers by emoji (if provided)
        if let Some(others) = other_stickers {
            for &doc_id in others {
                let Some(doc) = self.documents.get(&doc_id) else {
                    continue;
                };
                if !result.iter().any(|&(d, _)| d == doc_id) {
                    result.push((doc_id, create_sort_key(doc, 0, options.seed)));
                }
            }
        }

        // Sort descending by sort key
        result.sort_by_key(|a| std::cmp::Reverse(a.1));

        // Interleave premium and non-premium stickers
        interleave_premium_stickers(&result, &self.documents, options)
    }

    /// Collects all Premium stickers from cloud recent, installed non-archived sets,
    /// and non-installed featured sets, sorted descending by activity/install sort key.
    ///
    /// Direct port of `Stickers::getPremiumList` in `data/stickers/data_stickers.cpp`.
    pub fn get_premium_list(&self, seed: u64) -> Vec<DocumentId> {
        let mut result: Vec<(DocumentId, i32)> = Vec::new();
        let mut my_counter = 0i32;

        if let Some(recent) = self.sets.get(&CLOUD_RECENT_SET_ID) {
            for (i, &doc_id) in recent.stickers.iter().enumerate() {
                let Some(doc) = self.documents.get(&doc_id) else {
                    continue;
                };
                if !doc.is_premium_sticker {
                    continue;
                }
                let usage_date = recent.dates.get(i).copied().unwrap_or(0);
                let date = if usage_date != 0 {
                    usage_date
                } else {
                    let install = self.recent_install_date(doc);
                    if install != 0 {
                        install
                    } else {
                        create_sort_key(doc, SORT_KEY_SLICE * 6, seed)
                    }
                };
                result.push((doc_id, date));
            }
        }

        let mut add_order = |order: &[StickerSetId], skip_flag: StickersSetFlags| {
            for &set_id in order {
                let Some(set) = self.sets.get(&set_id) else {
                    continue;
                };
                if set.flags.intersects(skip_flag) || set.emoji.is_empty() {
                    continue;
                }
                let my = set.flags.contains(StickersSetFlags::INSTALLED);
                for &doc_id in &set.stickers {
                    let Some(doc) = self.documents.get(&doc_id) else {
                        continue;
                    };
                    if !doc.is_premium_sticker {
                        continue;
                    }
                    let install_date = if my { set.install_date } else { 0 };
                    let date = if install_date > 1 {
                        install_date_adjusted(install_date, doc)
                    } else if my {
                        create_my_sort_key(doc, &mut my_counter)
                    } else {
                        create_sort_key(doc, SORT_KEY_SLICE * 2, seed)
                    };
                    if !result.iter().any(|&(d, _)| d == doc_id) {
                        result.push((doc_id, date));
                    }
                }
            }
        };

        add_order(&self.sets_order, StickersSetFlags::ARCHIVED);
        add_order(&self.featured_sets_order, StickersSetFlags::INSTALLED);

        result.sort_by_key(|a| std::cmp::Reverse(a.1));
        result.into_iter().map(|(d, _)| d).collect()
    }

    fn recent_install_date(&self, doc: &StickerDocument) -> i32 {
        if let Some(set_id) = doc.set_id
            && let Some(set) = self.sets.get(&set_id)
        {
            return install_date_adjusted(set.install_date, doc);
        }
        0
    }

    fn prepare_stickers(&self, pack: &[DocumentId], skip_premium: bool) -> Vec<DocumentId> {
        pack.iter()
            .copied()
            .filter(|doc_id| {
                !skip_premium
                    || !self
                        .documents
                        .get(doc_id)
                        .is_some_and(|d| d.is_premium_sticker)
            })
            .collect()
    }

    /// Builds the ordered sections for the "My Stickers" tab:
    /// Favorites -> Recent (excluding favorites) -> Visible Megagroup set -> Installed sets -> Hidden Megagroup set.
    ///
    /// Direct port of `StickersListWidget::refreshMySets`, `refreshFavedStickers`,
    /// `refreshRecentStickers`, and `refreshMegagroupStickers` in `stickers_list_widget.cpp`.
    pub fn build_my_sections(&self, opts: PanelBuildOptions) -> Vec<StickerPanelSection> {
        let skip_premium = !opts.premium_possible;
        let mut sections = Vec::new();
        let mut faved_map: BTreeSet<DocumentId> = BTreeSet::new();

        // 1. Favorites (not in Masks mode)
        if !opts.is_masks
            && let Some(faved_set) = self.sets.get(&FAVED_SET_ID)
        {
            let elements = self.prepare_stickers(&faved_set.stickers, skip_premium);
            if !elements.is_empty() {
                for &d in &faved_set.stickers {
                    faved_map.insert(d);
                }
                sections.push(StickerPanelSection {
                    id: FAVED_SET_ID,
                    flags: StickersSetFlags::OFFICIAL | StickersSetFlags::SPECIAL,
                    title: "Favorites".to_string(),
                    short_name: String::new(),
                    count: faved_set.count,
                    external_layout: false,
                    stickers: elements,
                    thumbnail_document_id: None,
                });
            }
        }

        // 2. Recent (excluding favorites)
        let recent_entries =
            self.collect_recent_stickers(opts.is_masks, &faved_map, opts.unlimited_recent);
        if !recent_entries.is_empty() {
            let recent_docs: Vec<DocumentId> =
                recent_entries.into_iter().map(|e| e.document_id).collect();
            let count = recent_docs.len() as i32;
            sections.push(StickerPanelSection {
                id: RECENT_SET_ID,
                flags: StickersSetFlags::OFFICIAL | StickersSetFlags::SPECIAL,
                title: "Recent".to_string(),
                short_name: String::new(),
                count,
                external_layout: false,
                stickers: recent_docs,
                thumbnail_document_id: None,
            });
        }

        // Helper for megagroup section placement (visible vs hidden)
        let append_megagroup = |sections: &mut Vec<StickerPanelSection>, hidden_place: bool| {
            let Some(mg) = opts.megagroup_set else {
                return;
            };
            if opts.is_masks {
                return;
            }
            let effective_hidden = if mg.set_id.is_some() && mg.can_edit_stickers {
                false
            } else {
                mg.section_hidden
            };
            if effective_hidden != hidden_place {
                return;
            }
            match mg.set_id {
                None => {
                    if mg.can_edit_stickers {
                        sections.push(StickerPanelSection {
                            id: MEGAGROUP_SET_ID,
                            flags: StickersSetFlags::SPECIAL,
                            title: "Group Stickers".to_string(),
                            short_name: String::new(),
                            count: 0,
                            external_layout: false,
                            stickers: Vec::new(),
                            thumbnail_document_id: None,
                        });
                    }
                }
                Some(mg_set_id) => {
                    if let Some(set) = self.sets.get(&mg_set_id) {
                        let is_installed = set.is_in_my_list();
                        if is_installed && !mg.can_edit_stickers {
                            return;
                        }
                        let elements = self.prepare_stickers(&set.stickers, skip_premium);
                        if !elements.is_empty() {
                            sections.push(StickerPanelSection {
                                id: MEGAGROUP_SET_ID,
                                flags: StickersSetFlags::SPECIAL,
                                title: "Group Stickers".to_string(),
                                short_name: String::new(),
                                count: set.count,
                                external_layout: false,
                                stickers: elements,
                                thumbnail_document_id: set.lookup_thumbnail_document(),
                            });
                        }
                    }
                }
            }
        };

        // 3. Visible megagroup set
        append_megagroup(&mut sections, false);

        // 4. Installed sets (skip archived & excluded)
        let order = if opts.is_masks {
            &self.mask_sets_order
        } else {
            &self.sets_order
        };
        for &set_id in order {
            if opts.exclude_set_id == Some(set_id) {
                continue;
            }
            let Some(set) = self.sets.get(&set_id) else {
                continue;
            };
            if set.stickers.is_empty() || set.flags.contains(StickersSetFlags::ARCHIVED) {
                continue;
            }
            let elements = self.prepare_stickers(&set.stickers, skip_premium);
            if elements.is_empty() {
                continue;
            }
            sections.push(StickerPanelSection {
                id: set.id,
                flags: set.flags,
                title: set.title.clone(),
                short_name: set.short_name.clone(),
                count: set.count,
                external_layout: false,
                stickers: elements,
                thumbnail_document_id: set.lookup_thumbnail_document(),
            });
        }

        // 5. Hidden megagroup set at the bottom
        append_megagroup(&mut sections, true);

        sections
    }

    /// Builds the ordered sections for the Featured/Trending tab, skipping sets already
    /// installed (unless installed locally during the current session).
    ///
    /// Direct port of `StickersListWidget::refreshFeaturedSets` and `appendSet(..., AppendSkip::Installed)`.
    pub fn build_featured_sections(&self, opts: PanelBuildOptions) -> Vec<StickerPanelSection> {
        let skip_premium = !opts.premium_possible;
        let mut sections = Vec::new();
        for &set_id in &self.featured_sets_order {
            if opts.exclude_set_id == Some(set_id) {
                continue;
            }
            let Some(set) = self.sets.get(&set_id) else {
                continue;
            };
            if set.is_in_my_list() && !self.locally_installed_sets.contains(&set_id) {
                continue;
            }
            let source = if set.stickers.is_empty() {
                &set.covers
            } else {
                &set.stickers
            };
            let elements = self.prepare_stickers(source, skip_premium);
            if elements.is_empty() {
                continue;
            }
            sections.push(StickerPanelSection {
                id: set.id,
                flags: set.flags,
                title: set.title.clone(),
                short_name: set.short_name.clone(),
                count: set.count,
                external_layout: true,
                stickers: elements,
                thumbnail_document_id: set.lookup_thumbnail_document(),
            });
        }
        sections
    }

    /// Builds the footer tab icon strip from `my_sections`.
    ///
    /// Direct port of `StickersListWidget::fillIcons` in `stickers_list_widget.cpp`:
    /// - Emits `FAVED_SET_ID` if present first.
    /// - Emits `RECENT_SET_ID` ONLY if `FAVED_SET_ID` was not emitted (they share one slot).
    /// - Emits `MEGAGROUP_SET_ID` and each installed set with aspect-scaled thumbnail dimensions.
    pub fn build_tab_icons(
        &self,
        my_sections: &[StickerPanelSection],
        icon_frame_side: i32,
    ) -> Vec<StickerTabIcon> {
        let mut result = Vec::with_capacity(my_sections.len() + 1);
        let mut i = 0usize;

        if i < my_sections.len() && my_sections[i].id == FAVED_SET_ID {
            i += 1;
            result.push(StickerTabIcon {
                set_id: FAVED_SET_ID,
                thumbnail_document_id: None,
                icon_size: Size::new(icon_frame_side, icon_frame_side),
            });
        }
        if i < my_sections.len() && my_sections[i].id == RECENT_SET_ID {
            i += 1;
            if result.last().is_none_or(|t| t.set_id != FAVED_SET_ID) {
                result.push(StickerTabIcon {
                    set_id: RECENT_SET_ID,
                    thumbnail_document_id: None,
                    icon_size: Size::new(icon_frame_side, icon_frame_side),
                });
            }
        }

        let frame = Size::new(icon_frame_side, icon_frame_side);
        for section in &my_sections[i..] {
            if section.id == MEGAGROUP_SET_ID {
                result.push(StickerTabIcon {
                    set_id: MEGAGROUP_SET_ID,
                    thumbnail_document_id: None,
                    icon_size: frame,
                });
                continue;
            }
            let set_thumb = self.sets.get(&section.id).and_then(|s| s.thumbnail_size);
            let doc_thumb = section
                .thumbnail_document_id
                .and_then(|id| self.documents.get(&id))
                .and_then(|d| d.thumbnail_size);
            let raw_size = set_thumb.or(doc_thumb).unwrap_or_default();
            let pix = raw_size.scaled_keep_aspect(frame);
            result.push(StickerTabIcon {
                set_id: section.id,
                thumbnail_document_id: section.thumbnail_document_id,
                icon_size: pix,
            });
        }
        result
    }
}

fn create_sort_key(doc: &StickerDocument, mut base: i32, seed: u64) -> i32 {
    if doc.sticker_type.is_animated() {
        base += SORT_KEY_SLICE;
    }
    base + ((doc.id.0 ^ seed) % (SORT_KEY_SLICE as u64)) as i32
}

fn create_my_sort_key(doc: &StickerDocument, my_counter: &mut i32) -> i32 {
    let mut base = SORT_KEY_SLICE * 6;
    if !doc.sticker_type.is_animated() {
        base -= SORT_KEY_SLICE;
    }
    *my_counter += 1;
    base - *my_counter
}

fn install_date_adjusted(date: i32, doc: &StickerDocument) -> i32 {
    if doc.sticker_type.is_animated() {
        date
    } else {
        date / 2
    }
}

fn interleave_premium_stickers(
    sorted: &[(DocumentId, i32)],
    docs: &HashMap<DocumentId, StickerDocument>,
    options: EmojiSearchOptions,
) -> Vec<DocumentId> {
    let mut mixed = Vec::with_capacity(sorted.len());
    let mut premium_idx = 0usize;
    let mut normal_idx = 0usize;

    let is_prem = |idx: usize| -> bool {
        docs.get(&sorted[idx].0)
            .is_some_and(|d| d.is_premium_sticker)
    };

    let skip_to_next = |index: &mut usize, want_premium: bool| {
        while *index < sorted.len() && is_prem(*index) != want_premium {
            *index += 1;
        }
    };

    let take_one = |index: &mut usize, want_premium: bool, out: &mut Vec<DocumentId>| -> bool {
        skip_to_next(index, want_premium);
        if *index >= sorted.len() {
            return false;
        }
        out.push(sorted[*index].0);
        *index += 1;
        true
    };

    if options.is_premium {
        loop {
            for _ in 0..options.normals_per_premium {
                if !take_one(&mut normal_idx, false, &mut mixed) {
                    break;
                }
            }
            skip_to_next(&mut normal_idx, false);
            if normal_idx >= sorted.len() {
                break;
            }
            if !take_one(&mut premium_idx, true, &mut mixed) {
                break;
            }
        }
        while take_one(&mut normal_idx, false, &mut mixed) {}
        while take_one(&mut premium_idx, true, &mut mixed) {}
    } else {
        while take_one(&mut normal_idx, false, &mut mixed) {}
        for _ in 0..options.premiums_to_end_for_free {
            if !take_one(&mut premium_idx, true, &mut mixed) {
                break;
            }
        }
    }

    mixed
}

// --- Sticker & Lottie Sizing (`chat_helpers/stickers_lottie.cpp` + `lottie_common.cpp`) ---

/// Lottie sticker rendering size tag used in cache key computation,
/// matching `ChatHelpers::StickerLottieSize` in `chat_helpers/stickers_lottie.h`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum StickerLottieSize {
    /// Sticker rendered inside message history.
    MessageHistory = 0,
    /// Sticker set preview / forum topic cover.
    StickerSet = 1,
    /// Tabbed stickers panel grid.
    StickersPanel = 2,
    /// Stickers panel footer tab icon.
    StickersFooter = 3,
    /// Sticker sets manager list thumbnail.
    SetsListThumbnail = 4,
    /// Inline results / field autocomplete sticker suggestion.
    InlineResults = 5,
    /// Emoji interaction animation.
    EmojiInteraction = 6,
    /// Reserved emoji interaction slot 1.
    EmojiInteractionReserved1 = 7,
    /// Reserved emoji interaction slot 2.
    EmojiInteractionReserved2 = 8,
    /// Reserved emoji interaction slot 3.
    EmojiInteractionReserved3 = 9,
    /// Reserved emoji interaction slot 4.
    EmojiInteractionReserved4 = 10,
    /// Reserved emoji interaction slot 5.
    EmojiInteractionReserved5 = 11,
    /// Reserved emoji interaction slot 6.
    EmojiInteractionReserved6 = 12,
    /// Reserved emoji interaction slot 7.
    EmojiInteractionReserved7 = 13,
    /// Chat intro hello sticker.
    ChatIntroHelloSticker = 14,
    /// Large animated emoji sticker.
    StickerEmojiSize = 15,
    /// Pinned profile unique gift size.
    PinnedProfileUniqueGiftSize = 16,
}

/// Computes the 8-bit cache key shift for a Lottie sticker frame cache entry.
///
/// Exact port of `LottieCacheKeyShift` in `chat_helpers/stickers_lottie.cpp`:
/// `((replacementsTag << 4) & 0xF0) | (uint8(sizeTag) & 0x0F)`.
pub const fn lottie_cache_key_shift(replacements_tag: u8, size_tag: StickerLottieSize) -> u8 {
    ((replacements_tag << 4) & 0xF0) | ((size_tag as u8) & 0x0F)
}

/// Returns `true` if Lottie frames at `box_size` should be cached to disk
/// (`width * height <= 512 * 512`).
pub const fn should_cache_lottie_frames(box_size: Size) -> bool {
    let area = (box_size.width as i64) * (box_size.height as i64);
    area <= (DONT_CACHE_LOTTIE_AFTER_AREA as i64)
}

/// Clamps both dimensions to at least `1`, matching `HistoryView::NonEmptySize`.
pub const fn non_empty_size(size: Size) -> Size {
    Size {
        width: if size.width > 1 { size.width } else { 1 },
        height: if size.height > 1 { size.height } else { 1 },
    }
}

/// Downscales `size` to fit inside `box_size` preserving aspect ratio (without upscaling),
/// clamped to at least `1x1`, matching `HistoryView::DownscaledSize`.
pub fn downscaled_size(size: Size, box_size: Size) -> Size {
    let fitted = if size.width > box_size.width || size.height > box_size.height {
        size.scaled_keep_aspect(box_size)
    } else {
        size
    };
    non_empty_size(fitted)
}

/// Computes the Lottie rasterization target size rounded down to a multiple of `size_rounding`
/// (with minimum `size_rounding`), matching `Lottie::FrameRequest::size` in `lottie_common.cpp`.
pub fn lottie_frame_size(original: Size, box_size: Size, size_rounding: i32) -> Size {
    if box_size.is_empty() || size_rounding <= 0 {
        return Size::new(0, 0);
    }
    let result = original.scaled_keep_aspect(box_size);
    let skip_w = result.width % size_rounding;
    let skip_h = result.height % size_rounding;
    Size::new(
        (result.width - skip_w).max(size_rounding),
        (result.height - skip_h).max(size_rounding),
    )
}

/// Computes the logical display size for a sticker inside `box_size` at `device_pixel_ratio`.
///
/// Exact port of `ChatHelpers::ComputeStickerSize` in `chat_helpers/stickers_lottie.cpp`:
/// - Non-Lottie or empty dimensions -> `downscaled_size(dimensions, box_size)`.
/// - Lottie -> `non_empty_size(lottie_frame_size(dimensions, box_size * ratio, 8) / ratio)`.
pub fn compute_sticker_size(
    dimensions: Size,
    sticker_type: StickerType,
    box_size: Size,
    device_pixel_ratio: i32,
) -> Size {
    if !sticker_type.is_lottie() || dimensions.is_empty() {
        return downscaled_size(dimensions, box_size);
    }
    let ratio = device_pixel_ratio.max(1);
    let scaled_box = Size::new(box_size.width * ratio, box_size.height * ratio);
    let frame = lottie_frame_size(dimensions, scaled_box, 8);
    non_empty_size(Size::new(frame.width / ratio, frame.height / ratio))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(
        id: u64,
        set_id: Option<u64>,
        sticker_type: StickerType,
        alt: &str,
        premium: bool,
    ) -> StickerDocument {
        StickerDocument {
            id: DocumentId(id),
            set_id: set_id.map(StickerSetId),
            sticker_type,
            alt_emoji: alt.to_string(),
            dimensions: Size::new(512, 512),
            thumbnail_size: Some(Size::new(128, 128)),
            is_premium_sticker: premium,
            is_premium_emoji: false,
            is_gifv: false,
        }
    }

    #[test]
    fn flags_install_undo_and_featured_read_state() {
        let mut store = StickersStore::new();
        let set_a = StickerSetId(100);
        let set_b = StickerSetId(200);
        let emoji_set = StickerSetId(300);

        let mut unread = BTreeSet::new();
        unread.insert(set_a);
        unread.insert(set_b);
        store.featured_sets_received(
            StickersType::Stickers,
            vec![
                StickerSetUpdate {
                    id: set_a,
                    access_hash: 1,
                    hash: 11,
                    title: "Featured A".into(),
                    short_name: "fa".into(),
                    count: 2,
                    flags: StickersSetFlags::OFFICIAL,
                    install_date: None,
                    thumbnail_document_id: None,
                },
                StickerSetUpdate {
                    id: set_b,
                    access_hash: 2,
                    hash: 22,
                    title: "Featured B".into(),
                    short_name: "fb".into(),
                    count: 2,
                    flags: StickersSetFlags::EMPTY,
                    install_date: None,
                    thumbnail_document_id: None,
                },
            ],
            &unread,
        );
        // Featured emoji unread must NOT increment featured_sets_unread_count
        let mut unread_emoji = BTreeSet::new();
        unread_emoji.insert(emoji_set);
        store.featured_sets_received(
            StickersType::Emoji,
            vec![StickerSetUpdate {
                id: emoji_set,
                access_hash: 3,
                hash: 33,
                title: "Emoji Pack".into(),
                short_name: "ep".into(),
                count: 5,
                flags: StickersSetFlags::EMOJI,
                install_date: None,
                thumbnail_document_id: None,
            }],
            &unread_emoji,
        );
        assert_eq!(store.featured_sets_unread_count, 2);

        // Installing set_a locally clears UNREAD & decrements count to 1, puts set_a at index 0
        assert!(store.install_locally(set_a, 1000));
        assert_eq!(store.sets_order, vec![set_a]);
        assert!(
            store.sets[&set_a]
                .flags
                .contains(StickersSetFlags::INSTALLED)
        );
        assert!(!store.sets[&set_a].flags.contains(StickersSetFlags::UNREAD));
        assert_eq!(store.featured_sets_unread_count, 1);

        // Queuing and flushing featured read for set_b clears UNREAD and drops count to 0
        assert!(store.queue_read_featured_set(set_b));
        let flushed = store.flush_read_featured_sets();
        assert_eq!(flushed, vec![set_b]);
        assert!(!store.sets[&set_b].flags.contains(StickersSetFlags::UNREAD));
        assert_eq!(store.featured_sets_unread_count, 0);

        // Undo local install removes from sets_order and clears INSTALLED
        assert!(store.undo_install_locally(set_a));
        assert!(store.sets_order.is_empty());
        assert!(
            !store.sets[&set_a]
                .flags
                .contains(StickersSetFlags::INSTALLED)
        );
    }

    #[test]
    fn faved_stickers_and_saved_gifs_limits() {
        let mut store = StickersStore::new();
        for i in 1..=7 {
            store.insert_document(doc(i, Some(10), StickerType::Webp, "\u{1F525}", false));
        }

        // Non-premium faved limit is 5
        for i in 1..=5 {
            let res = store.set_faved(DocumentId(i), true, None, false);
            assert!(res.changed);
            assert!(!res.limit_reached);
        }
        // 6th evicts oldest (doc 1)
        let res = store.set_faved(DocumentId(6), true, None, false);
        assert!(res.changed);
        assert!(res.limit_reached);
        assert!(!store.is_faved(DocumentId(1)));
        assert!(store.is_faved(DocumentId(6)));

        // Re-faving an existing sticker moves it to front without eviction
        let res = store.set_faved(DocumentId(2), true, None, false);
        assert!(res.changed);
        assert!(!res.limit_reached);
        assert_eq!(store.sets[&FAVED_SET_ID].stickers[0], DocumentId(2));

        // Premium allows up to 10 without eviction
        let res = store.set_faved(DocumentId(7), true, None, true);
        assert!(res.changed);
        assert!(!res.limit_reached);
        assert_eq!(store.sets[&FAVED_SET_ID].stickers.len(), 6);

        // Saved GIFs limit (200 default, 400 premium)
        store.limits.saved_gifs_default = 3;
        store.limits.saved_gifs_premium = 5;
        for i in 1..=3 {
            let r = store.add_saved_gif(DocumentId(i), false);
            assert!(r.changed && !r.limit_reached);
        }
        let r = store.add_saved_gif(DocumentId(4), false);
        assert!(r.changed && r.limit_reached);
        assert_eq!(
            store.saved_gifs,
            vec![DocumentId(4), DocumentId(3), DocumentId(2)]
        );
        // Premium accepts 4th without evicting
        let r = store.add_saved_gif(DocumentId(5), true);
        assert!(r.changed && !r.limit_reached);
        assert_eq!(store.saved_gifs.len(), 4);
    }

    #[test]
    fn emoji_sticker_search_ranking_and_premium_interleaving() {
        let mut store = StickersStore::new();
        let set_id = StickerSetId(50);

        // 4 normal + 2 premium stickers in an installed set
        let d_norm_1 = doc(1, Some(50), StickerType::Tgs, "\u{1F525}", false);
        let d_norm_2 = doc(2, Some(50), StickerType::Tgs, "\u{1F525}", false);
        let d_norm_3 = doc(3, Some(50), StickerType::Tgs, "\u{1F525}", false);
        let d_norm_4 = doc(4, Some(50), StickerType::Webp, "\u{1F525}", false);
        let d_prem_1 = doc(5, Some(50), StickerType::Tgs, "\u{1F525}", true);
        let d_prem_2 = doc(6, Some(50), StickerType::Tgs, "\u{1F525}", true);
        for d in [d_norm_1, d_norm_2, d_norm_3, d_norm_4, d_prem_1, d_prem_2] {
            store.insert_document(d);
        }

        store.sets.insert(
            set_id,
            StickersSet::new(
                set_id,
                1,
                1,
                "Fire Pack",
                "firepack",
                6,
                StickersSetFlags::INSTALLED,
                1, // installDate == 1 triggers CreateMySortKey
            ),
        );
        store.sets_order.push(set_id);
        store.feed_set_stickers(
            set_id,
            vec![
                DocumentId(1),
                DocumentId(2),
                DocumentId(3),
                DocumentId(4),
                DocumentId(5),
                DocumentId(6),
            ],
            &[(
                "\u{1F525}".into(),
                vec![
                    DocumentId(1),
                    DocumentId(2),
                    DocumentId(3),
                    DocumentId(4),
                    DocumentId(5),
                    DocumentId(6),
                ],
            )],
        );

        // Premium user: 2 normal then 1 premium, repeated
        let prem_res = store.get_list_by_emoji(
            &["\u{1F525}"],
            None,
            EmojiSearchOptions {
                seed: 42,
                is_premium: true,
                normals_per_premium: 2,
                premiums_to_end_for_free: 0,
            },
        );
        assert_eq!(
            prem_res,
            vec![
                DocumentId(1),
                DocumentId(2),
                DocumentId(5),
                DocumentId(3),
                DocumentId(4),
                DocumentId(6),
            ]
        );

        // Non-premium user with premiums_to_end_for_free = 1: all 4 normal, then 1 premium
        let free_res = store.get_list_by_emoji(
            &["\u{1F525}"],
            None,
            EmojiSearchOptions {
                seed: 42,
                is_premium: false,
                normals_per_premium: 2,
                premiums_to_end_for_free: 1,
            },
        );
        assert_eq!(
            free_res,
            vec![
                DocumentId(1),
                DocumentId(2),
                DocumentId(3),
                DocumentId(4),
                DocumentId(5),
            ]
        );
    }

    #[test]
    fn tabbed_selector_sections_and_footer_icons() {
        let mut store = StickersStore::new();
        let set_id = StickerSetId(77);
        for i in 1..=25 {
            store.insert_document(doc(i, Some(77), StickerType::Webp, "\u{1F600}", i == 25));
        }

        store.sets.insert(
            set_id,
            StickersSet::new(
                set_id,
                1,
                1,
                "My Set",
                "myset",
                25,
                StickersSetFlags::INSTALLED,
                100,
            ),
        );
        store.sets_order.push(set_id);
        let all_docs: Vec<DocumentId> = (1..=25).map(DocumentId).collect();
        store.feed_set_stickers(set_id, all_docs, &[]);

        // Mark doc 1 as faved, and increment docs 1..=24 into cloud recent
        store.set_faved(DocumentId(1), true, None, false);
        for i in 1..=24 {
            store.increment_sticker(DocumentId(i), 1000 + i as i32);
        }

        let sections = store.build_my_sections(PanelBuildOptions {
            is_masks: false,
            premium_possible: false, // filters out doc 25 (premium)
            unlimited_recent: false,
            exclude_set_id: None,
            megagroup_set: None,
        });
        assert_eq!(sections.len(), 3);
        assert_eq!(sections[0].id, FAVED_SET_ID);
        assert_eq!(sections[1].id, RECENT_SET_ID);
        // Recent excludes faved (doc 1) and is capped at RECENT_DISPLAY_LIMIT (20)
        assert_eq!(sections[1].stickers.len(), RECENT_DISPLAY_LIMIT);
        assert!(!sections[1].stickers.contains(&DocumentId(1)));
        assert_eq!(sections[2].id, set_id);
        // Doc 25 (premium) skipped when premium_possible == false
        assert_eq!(sections[2].stickers.len(), 24);

        // Footer icons: Faved and Recent share the first icon slot!
        let icons = store.build_tab_icons(&sections, 32);
        assert_eq!(icons.len(), 2);
        assert_eq!(icons[0].set_id, FAVED_SET_ID);
        assert_eq!(icons[1].set_id, set_id);
        assert_eq!(icons[1].icon_size, Size::new(32, 32));
    }

    #[test]
    fn sticker_and_lottie_sizing_rules() {
        assert_eq!(
            lottie_cache_key_shift(0x03, StickerLottieSize::InlineResults),
            0x35
        );
        assert!(should_cache_lottie_frames(Size::new(512, 512)));
        assert!(!should_cache_lottie_frames(Size::new(513, 512)));

        // Non-Lottie downscaled (no upscaling)
        assert_eq!(
            compute_sticker_size(
                Size::new(512, 256),
                StickerType::Webp,
                Size::new(100, 100),
                1
            ),
            Size::new(100, 50)
        );
        assert_eq!(
            compute_sticker_size(Size::new(40, 40), StickerType::Webp, Size::new(100, 100), 1),
            Size::new(40, 40)
        );

        // Lottie rounded down to multiple of 8 in device pixels, then divided by ratio
        // Box 66x66 at 1x -> 512x512 scaled to 66x66 -> floor to multiple of 8 => 64x64
        assert_eq!(
            compute_sticker_size(Size::new(512, 512), StickerType::Tgs, Size::new(66, 66), 1),
            Size::new(64, 64)
        );
        // Box 66x66 at 2x -> scaled_box 132x132 -> floor to multiple of 8 => 128x128 -> /2 => 64x64
        assert_eq!(
            compute_sticker_size(Size::new(512, 512), StickerType::Tgs, Size::new(66, 66), 2),
            Size::new(64, 64)
        );
        // Thumbnail type mapping
        assert_eq!(thumbnail_type_from_photo_size_type("s"), StickerType::Webp);
        assert_eq!(thumbnail_type_from_photo_size_type("a"), StickerType::Tgs);
        assert_eq!(thumbnail_type_from_photo_size_type("v"), StickerType::Webm);
    }

    #[test]
    fn featured_sections_skip_installed_unless_local_and_sets_received_prunes() {
        let mut store = StickersStore::new();
        store.insert_document(doc(10, Some(1), StickerType::Tgs, "\u{1F525}", true));
        store.insert_document(doc(20, Some(2), StickerType::Webp, "\u{1F525}", false));

        let mut unread = BTreeSet::new();
        unread.insert(StickerSetId(1));
        store.featured_sets_received(
            StickersType::Stickers,
            vec![
                StickerSetUpdate {
                    id: StickerSetId(1),
                    access_hash: 1,
                    hash: 1,
                    title: "One".into(),
                    short_name: "one".into(),
                    count: 1,
                    flags: StickersSetFlags::EMPTY,
                    install_date: None,
                    thumbnail_document_id: None,
                },
                StickerSetUpdate {
                    id: StickerSetId(2),
                    access_hash: 2,
                    hash: 2,
                    title: "Two".into(),
                    short_name: "two".into(),
                    count: 1,
                    flags: StickersSetFlags::INSTALLED,
                    install_date: Some(50),
                    thumbnail_document_id: None,
                },
            ],
            &unread,
        );
        store.feed_set_stickers(
            StickerSetId(1),
            vec![DocumentId(10)],
            &[("\u{1F525}".into(), vec![DocumentId(10)])],
        );
        store.feed_set_stickers(
            StickerSetId(2),
            vec![DocumentId(20)],
            &[("\u{1F525}".into(), vec![DocumentId(20)])],
        );

        // Set 2 is already installed (not locally), so featured skips it; set 1 is shown
        let featured = store.build_featured_sections(PanelBuildOptions {
            premium_possible: true,
            ..PanelBuildOptions::default()
        });
        assert_eq!(featured.len(), 1);
        assert_eq!(featured[0].id, StickerSetId(1));

        // Locally installing set 1 keeps it visible in Featured during the current session
        store.install_locally(StickerSetId(1), 500);
        let featured_after = store.build_featured_sections(PanelBuildOptions {
            premium_possible: true,
            ..PanelBuildOptions::default()
        });
        assert_eq!(featured_after.len(), 1);
        assert_eq!(featured_after[0].id, StickerSetId(1));

        // get_premium_list returns only the premium sticker (doc 10)
        assert_eq!(store.get_premium_list(0), vec![DocumentId(10)]);

        // sets_received with empty list marks non-archived sticker sets uninstalled
        // (featured sets stay in `sets`, non-featured uninstalled sets get pruned)
        let extra = StickerSetId(99);
        store.sets.insert(
            extra,
            StickersSet::new(
                extra,
                9,
                9,
                "Old",
                "old",
                0,
                StickersSetFlags::INSTALLED,
                10,
            ),
        );
        store.sets_received(StickersType::Stickers, vec![], 600);
        assert!(!store.sets.contains_key(&extra));
        assert!(store.sets.contains_key(&StickerSetId(1)));
    }
}
