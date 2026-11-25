//! Protocol-level synchronization for sticker sets, masks, custom emoji sets,
//! featured sets, recent/faved stickers, and saved GIFs.
//!
//! Port origin:
//! - `Telegram/SourceFiles/api/api_hash.cpp` (`CountStickersHash`, `CountMasksHash`,
//!   `CountCustomEmojiHash`, `CountRecentStickersHash`, `CountFavedStickersHash`,
//!   `CountFeaturedStickersHash`, `CountFeaturedEmojiHash`, `CountSavedGifsHash`)
//! - `Telegram/SourceFiles/data/stickers/data_stickers.h` & `data_stickers.cpp`
//! - `Telegram/SourceFiles/data/stickers/data_stickers_set.h` & `data_stickers_set.cpp`
//! - `Telegram/SourceFiles/apiwrap.cpp` (`updateStickers`, `updateMasks`, `updateCustomEmoji`,
//!   `updateSavedGifs`, `requestStickers`, `requestMasks`, `requestCustomEmoji`,
//!   `requestRecentStickers`, `requestFavedStickers`, `requestFeaturedStickers`,
//!   `requestFeaturedEmoji`, `requestSavedGifs`, `requestStickerSets`, `saveStickerSets`)
//! - `Telegram/SourceFiles/api/api_updates.cpp` (`updateNewStickerSet`,
//!   `updateStickerSetsOrder`, `updateMoveStickerSetToTop`, `updateStickerSets`,
//!   `updateRecentStickers`, `updateFavedStickers`, `updateReadFeaturedStickers`,
//!   `updateSavedGifs`)

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ops::{BitAnd, BitAndAssign, BitOr, BitOrAssign, BitXor, Not};

use tg_tl::{enums as tl_enums, functions as tl_fns, types as tl_types};
use tracing::warn;

use crate::error::ApiError;
use crate::hash::{count_document_vector_hash, hash_finalize, hash_init, hash_update_u64};
use crate::rpc::RpcClient;

/// Default backward-compatibility set ID (`Data::Stickers::DefaultSetId = 0`).
pub const DEFAULT_SET_ID: u64 = 0;
/// Custom / ungrouped stickers set ID (`Data::Stickers::CustomSetId`).
pub const CUSTOM_SET_ID: u64 = 0xFFFF_FFFF_FFFF_FFFF;
/// Recent stickers section ID (`Data::Stickers::RecentSetId`).
pub const RECENT_SET_ID: u64 = 0xFFFF_FFFF_FFFF_FFFE;
/// Sentinel "no set" ID (`Data::Stickers::NoneSetId`).
pub const NONE_SET_ID: u64 = 0xFFFF_FFFF_FFFF_FFFD;
/// Cloud-stored recent stickers set ID (`Data::Stickers::CloudRecentSetId`).
pub const CLOUD_RECENT_SET_ID: u64 = 0xFFFF_FFFF_FFFF_FFFC;
/// Featured/trending tab set ID (`Data::Stickers::FeaturedSetId`).
pub const FEATURED_SET_ID: u64 = 0xFFFF_FFFF_FFFF_FFFB;
/// Cloud-stored favorite (faved) stickers set ID (`Data::Stickers::FavedSetId`).
pub const FAVED_SET_ID: u64 = 0xFFFF_FFFF_FFFF_FFFA;
/// Cloud-stored recent attached masks set ID (`Data::Stickers::CloudRecentAttachedSetId`).
pub const CLOUD_RECENT_ATTACHED_SET_ID: u64 = 0xFFFF_FFFF_FFFF_FFF9;
/// Collectible emoji statuses set ID (`Data::Stickers::CollectibleSetId`).
pub const COLLECTIBLE_SET_ID: u64 = 0xFFFF_FFFF_FFFF_FFF8;

/// Cache invalidation interval (1 hour in milliseconds), matching
/// `kUpdateTimeout = crl::time(3600'000)` in `data/stickers/data_stickers.h:272`.
pub const STICKERS_UPDATE_TIMEOUT_MS: u64 = 3_600_000;

/// Maximum character length for a sticker/custom-emoji pack title
/// (`kMaxSetNameLength = 50` in `boxes/sticker_set_box.cpp:327`).
pub const MAX_STICKER_SET_TITLE_LEN: usize = 50;

/// Sticker set category (`Data::StickersType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StickersType {
    /// Regular chat stickers.
    Stickers,
    /// Photo editor masks.
    Masks,
    /// Custom emoji packs.
    Emoji,
}

/// Bitflags for a sticker set with exact bit positions matching `Data::StickersSetFlag`
/// (`data/stickers/data_stickers_set.h`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct StickerSetFlags(u16);

impl StickerSetFlags {
    /// Empty bitmask.
    pub const EMPTY: Self = Self(0);
    /// Installed by the user (`1 << 0`).
    pub const INSTALLED: Self = Self(1 << 0);
    /// Archived by user or server (`1 << 1`).
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
    /// Synthetic/special set such as Recent or Faved (`1 << 7`).
    pub const SPECIAL: Self = Self(1 << 7);
    /// Custom emoji pack (`1 << 9`).
    pub const EMOJI: Self = Self(1 << 9);
    /// Adaptive text-color emoji set (`1 << 10`).
    pub const TEXT_COLOR: Self = Self(1 << 10);
    /// Channel emoji status set (`1 << 11`).
    pub const CHANNEL_STATUS: Self = Self(1 << 11);
    /// Current user is creator (`1 << 12`).
    pub const AM_CREATOR: Self = Self(1 << 12);

    /// Creates flags from raw bits.
    #[must_use]
    pub const fn from_bits(bits: u16) -> Self {
        Self(bits)
    }

    /// Returns raw bits.
    #[must_use]
    pub const fn bits(self) -> u16 {
        self.0
    }

    /// Returns `true` if all bits in `other` are set.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    /// Returns `true` if any bit in `other` is set.
    #[must_use]
    pub const fn intersects(self, other: Self) -> bool {
        (self.0 & other.0) != 0
    }
}

impl BitOr for StickerSetFlags {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl BitOrAssign for StickerSetFlags {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

impl BitAnd for StickerSetFlags {
    type Output = Self;
    fn bitand(self, rhs: Self) -> Self {
        Self(self.0 & rhs.0)
    }
}

impl BitAndAssign for StickerSetFlags {
    fn bitand_assign(&mut self, rhs: Self) {
        self.0 &= rhs.0;
    }
}

impl BitXor for StickerSetFlags {
    type Output = Self;
    fn bitxor(self, rhs: Self) -> Self {
        Self(self.0 ^ rhs.0)
    }
}

impl Not for StickerSetFlags {
    type Output = Self;
    fn not(self) -> Self {
        Self(!self.0)
    }
}

/// Parses TL `StickerSet` flags (`ParseStickersSetFlags`, `data/stickers/data_stickers_set.cpp:49-60`).
#[must_use]
pub fn parse_sticker_set_flags(s: &tl_types::StickerSet) -> StickerSetFlags {
    let mut flags = StickerSetFlags::EMPTY;
    if s.archived {
        flags |= StickerSetFlags::ARCHIVED;
    }
    if s.official {
        flags |= StickerSetFlags::OFFICIAL;
    }
    if s.masks {
        flags |= StickerSetFlags::MASKS;
    }
    if s.emojis {
        flags |= StickerSetFlags::EMOJI;
    }
    if s.installed_date.is_some() {
        flags |= StickerSetFlags::INSTALLED;
    }
    if s.text_color {
        flags |= StickerSetFlags::TEXT_COLOR;
    }
    if s.channel_emoji_status {
        flags |= StickerSetFlags::CHANNEL_STATUS;
    }
    if s.creator {
        flags |= StickerSetFlags::AM_CREATOR;
    }
    flags
}

/// Converts a TL `StickerSet.hash` (`int` / `i32`) into tdesktop's `uint64 StickersSet::hash`
/// by sign-extending `i32 -> i64 -> u64` (matching C++ `uint64(data.vhash().v)` in
/// `data/stickers/data_stickers.cpp:1565` & `data/stickers/data_stickers_set.cpp:66`).
#[inline]
#[must_use]
pub const fn tl_sticker_set_hash_to_u64(hash: i32) -> u64 {
    (hash as i64) as u64
}

/// Cached sticker / mask / custom-emoji set (`Data::StickersSet`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedStickerSet {
    /// Set ID.
    pub id: u64,
    /// Access hash.
    pub access_hash: u64,
    /// Set hash stored as sign-extended `u64` (matching `StickersSet::hash`).
    pub hash: u64,
    /// Display title.
    pub title: String,
    /// Short name / slug.
    pub short_name: String,
    /// Server-reported sticker count.
    pub count: i32,
    /// Set flags.
    pub flags: StickerSetFlags,
    /// Installation UNIX timestamp (`0` if not installed).
    pub install_date: i32,
    /// Optional thumbnail document ID.
    pub thumbnail_document_id: u64,
    /// Loaded sticker document IDs in pack order.
    pub stickers: Vec<u64>,
    /// Cover sticker document IDs (from `StickerSetCovered`).
    pub covers: Vec<u64>,
    /// Usage timestamps (for cloud recent sets).
    pub dates: Vec<i32>,
    /// Emoji -> sticker document IDs mapping.
    pub emoji: BTreeMap<String, Vec<u64>>,
}

impl CachedStickerSet {
    /// Returns the [`StickersType`] derived from flags (`StickersSet::type`,
    /// `data/stickers/data_stickers_set.cpp:106-112`).
    #[must_use]
    pub const fn kind(&self) -> StickersType {
        if self.flags.contains(StickerSetFlags::EMOJI) {
            StickersType::Emoji
        } else if self.flags.contains(StickerSetFlags::MASKS) {
            StickersType::Masks
        } else {
            StickersType::Stickers
        }
    }

    /// Builds `MTPInputStickerSet` matching `StickersSet::mtpInput`
    /// (`data/stickers/data_stickers_set.cpp:93-97`).
    #[must_use]
    pub fn to_input_sticker_set(&self) -> tl_enums::InputStickerSet {
        if self.id != 0 && self.access_hash != 0 {
            tl_enums::InputStickerSet::ID(tl_types::InputStickerSetID {
                id: self.id as i64,
                access_hash: self.access_hash as i64,
            })
        } else {
            tl_enums::InputStickerSet::ShortName(tl_types::InputStickerSetShortName {
                short_name: self.short_name.clone(),
            })
        }
    }
}

/// Typed selector covering every TL `InputStickerSet` constructor for `messages.getStickerSet`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum StickerSetSpec {
    /// `inputStickerSetEmpty`.
    Empty,
    /// `inputStickerSetID`.
    Id {
        /// Sticker set ID.
        id: u64,
        /// Sticker set access hash.
        access_hash: u64,
    },
    /// `inputStickerSetShortName`.
    ShortName(String),
    /// `inputStickerSetAnimatedEmoji`.
    AnimatedEmoji,
    /// `inputStickerSetDice`.
    Dice(String),
    /// `inputStickerSetAnimatedEmojiAnimations`.
    AnimatedEmojiAnimations,
    /// `inputStickerSetPremiumGifts`.
    PremiumGifts,
    /// `inputStickerSetEmojiGenericAnimations`.
    EmojiGenericAnimations,
    /// `inputStickerSetEmojiDefaultStatuses`.
    EmojiDefaultStatuses,
    /// `inputStickerSetEmojiDefaultTopicIcons`.
    EmojiDefaultTopicIcons,
    /// `inputStickerSetEmojiChannelDefaultStatuses`.
    EmojiChannelDefaultStatuses,
    /// `inputStickerSetTonGifts`.
    TonGifts,
}

impl StickerSetSpec {
    /// Converts this selector to the exact TL `InputStickerSet` constructor.
    #[must_use]
    pub fn to_tl(&self) -> tl_enums::InputStickerSet {
        match self {
            Self::Empty => tl_enums::InputStickerSet::Empty,
            Self::Id { id, access_hash } => {
                tl_enums::InputStickerSet::ID(tl_types::InputStickerSetID {
                    id: *id as i64,
                    access_hash: *access_hash as i64,
                })
            }
            Self::ShortName(short_name) => {
                tl_enums::InputStickerSet::ShortName(tl_types::InputStickerSetShortName {
                    short_name: short_name.clone(),
                })
            }
            Self::AnimatedEmoji => tl_enums::InputStickerSet::AnimatedEmoji,
            Self::Dice(emoticon) => {
                tl_enums::InputStickerSet::Dice(tl_types::InputStickerSetDice {
                    emoticon: emoticon.clone(),
                })
            }
            Self::AnimatedEmojiAnimations => tl_enums::InputStickerSet::AnimatedEmojiAnimations,
            Self::PremiumGifts => tl_enums::InputStickerSet::PremiumGifts,
            Self::EmojiGenericAnimations => tl_enums::InputStickerSet::EmojiGenericAnimations,
            Self::EmojiDefaultStatuses => tl_enums::InputStickerSet::EmojiDefaultStatuses,
            Self::EmojiDefaultTopicIcons => tl_enums::InputStickerSet::EmojiDefaultTopicIcons,
            Self::EmojiChannelDefaultStatuses => {
                tl_enums::InputStickerSet::EmojiChannelDefaultStatuses
            }
            Self::TonGifts => tl_enums::InputStickerSet::TonGifts,
        }
    }
}

/// Outcome of `messages.installStickerSet`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallStickerSetOutcome {
    /// `messages.stickerSetInstallResultSuccess`.
    Success,
    /// `messages.stickerSetInstallResultArchive`: list of archived set IDs returned by server.
    Archived(Vec<u64>),
}

/// Protocol-level cache & synchronization manager for stickers, masks, custom emoji,
/// featured sets, recent/faved stickers, and saved GIFs.
#[derive(Debug, Clone, Default)]
pub struct StickersSyncManager {
    /// All known sticker/mask/emoji/special sets keyed by set ID.
    pub sets: HashMap<u64, CachedStickerSet>,
    /// Installed regular sticker set IDs in display order.
    pub sets_order: Vec<u64>,
    /// Installed mask set IDs in display order.
    pub mask_sets_order: Vec<u64>,
    /// Installed custom emoji set IDs in display order.
    pub emoji_sets_order: Vec<u64>,
    /// Featured sticker set IDs in display order.
    pub featured_sets_order: Vec<u64>,
    /// Featured custom emoji set IDs in display order.
    pub featured_emoji_sets_order: Vec<u64>,
    /// Archived sticker set IDs.
    pub archived_sets_order: Vec<u64>,
    /// Archived mask set IDs.
    pub archived_mask_sets_order: Vec<u64>,
    /// Count of unread featured sticker sets (excluding custom emoji).
    pub featured_sets_unread_count: usize,
    /// Saved GIF document IDs in display order.
    pub saved_gifs: Vec<u64>,
    /// Cached special sets keyed by [`StickerSetSpec`] (e.g. `AnimatedEmoji`, `Dice`,
    /// `EmojiGenericAnimations`, `ShortName`) so `stickerSetNotModified` preserves them.
    pub sets_by_spec: HashMap<StickerSetSpec, u64>,

    /// Last refresh timestamp (ms) for `messages.getAllStickers` (`0` = never / invalidated).
    pub last_stickers_update_ms: u64,
    /// Last refresh timestamp (ms) for `messages.getMaskStickers`.
    pub last_masks_update_ms: u64,
    /// Last refresh timestamp (ms) for `messages.getEmojiStickers`.
    pub last_emoji_update_ms: u64,
    /// Last refresh timestamp (ms) for `messages.getRecentStickers(attached=false)`.
    pub last_recent_update_ms: u64,
    /// Last refresh timestamp (ms) for `messages.getRecentStickers(attached=true)`.
    pub last_recent_attached_update_ms: u64,
    /// Last refresh timestamp (ms) for `messages.getFavedStickers`.
    pub last_faved_update_ms: u64,
    /// Last refresh timestamp (ms) for `messages.getFeaturedStickers`.
    pub last_featured_update_ms: u64,
    /// Last refresh timestamp (ms) for `messages.getFeaturedEmojiStickers`.
    pub last_featured_emoji_update_ms: u64,
    /// Last refresh timestamp (ms) for `messages.getSavedGifs`.
    pub last_saved_gifs_update_ms: u64,
}

impl StickersSyncManager {
    /// Creates an empty [`StickersSyncManager`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns `true` if a refresh is due (`Data::Stickers::updateNeeded`,
    /// `data/stickers/data_stickers.h:271-274`).
    #[must_use]
    pub const fn update_needed(last_ms: u64, now_ms: u64) -> bool {
        (last_ms == 0) || (now_ms >= last_ms.saturating_add(STICKERS_UPDATE_TIMEOUT_MS))
    }

    // -------------------------------------------------------------------------
    // Hash computations (`api/api_hash.cpp`)
    // -------------------------------------------------------------------------

    /// Computes `CountStickersOrderHash` (`api/api_hash.cpp:38-61`).
    #[must_use]
    pub fn count_stickers_order_hash(&self, order: &[u64], check_outdated_info: bool) -> u64 {
        let mut result = hash_init();
        let mut found_outdated = false;
        for &set_id in order {
            if let Some(set) = self.sets.get(&set_id) {
                if set.id == DEFAULT_SET_ID {
                    found_outdated = true;
                } else if !set.flags.contains(StickerSetFlags::SPECIAL)
                    && !set.flags.contains(StickerSetFlags::ARCHIVED)
                {
                    hash_update_u64(&mut result, set.hash);
                }
            }
        }
        if !check_outdated_info || !found_outdated {
            hash_finalize(result)
        } else {
            0
        }
    }

    /// Computes the hash for `messages.getAllStickers` (`Api::CountStickersHash`, `api/api_hash.cpp:82-89`).
    #[must_use]
    pub fn count_stickers_hash(&self, check_outdated_info: bool) -> u64 {
        self.count_stickers_order_hash(&self.sets_order, check_outdated_info)
    }

    /// Computes the hash for `messages.getMaskStickers` (`Api::CountMasksHash`, `api/api_hash.cpp:91-98`).
    #[must_use]
    pub fn count_masks_hash(&self, check_outdated_info: bool) -> u64 {
        self.count_stickers_order_hash(&self.mask_sets_order, check_outdated_info)
    }

    /// Computes the hash for `messages.getEmojiStickers` (`Api::CountCustomEmojiHash`, `api/api_hash.cpp:100-107`).
    #[must_use]
    pub fn count_custom_emoji_hash(&self, check_outdated_info: bool) -> u64 {
        self.count_stickers_order_hash(&self.emoji_sets_order, check_outdated_info)
    }

    /// Computes the hash for `messages.getRecentStickers` (`Api::CountRecentStickersHash`, `api/api_hash.cpp:109-117`).
    #[must_use]
    pub fn count_recent_stickers_hash(&self, attached: bool) -> u64 {
        let set_id = if attached {
            CLOUD_RECENT_ATTACHED_SET_ID
        } else {
            CLOUD_RECENT_SET_ID
        };
        self.sets
            .get(&set_id)
            .map_or(0, |s| count_document_vector_hash(&s.stickers))
    }

    /// Computes the hash for `messages.getFavedStickers` (`Api::CountFavedStickersHash`, `api/api_hash.cpp:119-121`).
    #[must_use]
    pub fn count_faved_stickers_hash(&self) -> u64 {
        self.sets
            .get(&FAVED_SET_ID)
            .map_or(0, |s| count_document_vector_hash(&s.stickers))
    }

    fn count_featured_hash(&self, order: &[u64]) -> u64 {
        let mut result = hash_init();
        for &set_id in order {
            hash_update_u64(&mut result, set_id);
            if self
                .sets
                .get(&set_id)
                .is_some_and(|s| s.flags.contains(StickerSetFlags::UNREAD))
            {
                hash_update_u64(&mut result, 1);
            }
        }
        hash_finalize(result)
    }

    /// Computes the hash for `messages.getFeaturedStickers` (`Api::CountFeaturedStickersHash`, `api/api_hash.cpp:123-127`).
    #[must_use]
    pub fn count_featured_stickers_hash(&self) -> u64 {
        self.count_featured_hash(&self.featured_sets_order)
    }

    /// Computes the hash for `messages.getFeaturedEmojiStickers` (`Api::CountFeaturedEmojiHash`, `api/api_hash.cpp:129-133`).
    #[must_use]
    pub fn count_featured_emoji_hash(&self) -> u64 {
        self.count_featured_hash(&self.featured_emoji_sets_order)
    }

    /// Computes the hash for `messages.getSavedGifs` (`Api::CountSavedGifsHash`, `api/api_hash.cpp:135-137`).
    #[must_use]
    pub fn count_saved_gifs_hash(&self) -> u64 {
        count_document_vector_hash(&self.saved_gifs)
    }

    // -------------------------------------------------------------------------
    // Local ingestion (`data/stickers/data_stickers.cpp`)
    // -------------------------------------------------------------------------

    /// Ingests a `StickerSet` header (`Data::Stickers::feedSet`, `data_stickers.cpp:1537-1616`).
    pub fn feed_set(&mut self, info: &tl_enums::StickerSet, now_secs: i32) -> u64 {
        let tl_enums::StickerSet::StickerSet(data) = info;
        let set_id = data.id as u64;
        let access_hash = data.access_hash as u64;
        let set_hash = tl_sticker_set_hash_to_u64(data.hash);
        let flags = parse_sticker_set_flags(data);
        let install_date = match data.installed_date {
            Some(0) => now_secs,
            Some(ts) => ts,
            None => 0,
        };
        let thumb_doc_id = data.thumb_document_id.unwrap_or(0) as u64;

        let old_flags = if let Some(existing) = self.sets.get_mut(&set_id) {
            let old = existing.flags;
            existing.access_hash = access_hash;
            existing.title.clone_from(&data.title);
            existing.short_name.clone_from(&data.short_name);
            let client_flags = existing.flags
                & (StickerSetFlags::FEATURED
                    | StickerSetFlags::UNREAD
                    | StickerSetFlags::NOT_LOADED
                    | StickerSetFlags::SPECIAL
                    | StickerSetFlags::INSTALLED);
            existing.flags = flags | client_flags;
            existing.install_date = install_date;
            existing.thumbnail_document_id = thumb_doc_id;
            if existing.count != data.count
                || existing.hash != set_hash
                || existing.emoji.is_empty()
            {
                existing.count = data.count;
                existing.hash = set_hash;
                existing.flags |= StickerSetFlags::NOT_LOADED;
            }
            old
        } else {
            self.sets.insert(
                set_id,
                CachedStickerSet {
                    id: set_id,
                    access_hash,
                    hash: set_hash,
                    title: data.title.clone(),
                    short_name: data.short_name.clone(),
                    count: data.count,
                    flags: flags | StickerSetFlags::NOT_LOADED,
                    install_date,
                    thumbnail_document_id: thumb_doc_id,
                    stickers: Vec::new(),
                    covers: Vec::new(),
                    dates: Vec::new(),
                    emoji: BTreeMap::new(),
                },
            );
            StickerSetFlags::EMPTY
        };

        if let Some(set) = self.sets.get(&set_id) {
            let changed = old_flags ^ set.flags;
            if changed.contains(StickerSetFlags::ARCHIVED) {
                let is_masks = set.kind() == StickersType::Masks;
                let archived_flag = set.flags.contains(StickerSetFlags::ARCHIVED);
                let archived_order = if is_masks {
                    &mut self.archived_mask_sets_order
                } else {
                    &mut self.archived_sets_order
                };
                if archived_flag {
                    if !archived_order.contains(&set_id) {
                        archived_order.insert(0, set_id);
                    }
                } else {
                    archived_order.retain(|&id| id != set_id);
                }
            }
        }

        set_id
    }

    /// Ingests a `StickerSetCovered` (`Data::Stickers::feedSet(const MTPStickerSetCovered&)`,
    /// `data_stickers.cpp:1625-1643`).
    pub fn feed_set_covered(
        &mut self,
        covered: &tl_enums::StickerSetCovered,
        now_secs: i32,
    ) -> u64 {
        let raw_set = match covered {
            tl_enums::StickerSetCovered::StickerSetCovered(c) => &c.set,
            tl_enums::StickerSetCovered::StickerSetMultiCovered(c) => &c.set,
            tl_enums::StickerSetCovered::StickerSetFullCovered(c) => &c.set,
            tl_enums::StickerSetCovered::StickerSetNoCovered(c) => &c.set,
        };
        let set_id = self.feed_set(raw_set, now_secs);
        match covered {
            tl_enums::StickerSetCovered::StickerSetCovered(c) => {
                let cover = doc_id_from_tl(&c.cover);
                if let Some(set) = self.sets.get_mut(&set_id) {
                    set.covers.clear();
                    if let Some(id) = cover {
                        set.covers.push(id);
                    }
                }
            }
            tl_enums::StickerSetCovered::StickerSetMultiCovered(c) => {
                let covers: Vec<u64> = c.covers.iter().filter_map(doc_id_from_tl).collect();
                if let Some(set) = self.sets.get_mut(&set_id) {
                    set.covers = covers;
                }
            }
            tl_enums::StickerSetCovered::StickerSetFullCovered(c) => {
                self.feed_set_stickers(set_id, &c.documents, &c.packs);
            }
            tl_enums::StickerSetCovered::StickerSetNoCovered(_) => {}
        }
        set_id
    }

    /// Ingests a full `messages.stickerSet` (`Data::Stickers::feedSetFull`, `data_stickers.cpp:1618-1623`).
    pub fn feed_set_full(&mut self, data: &tl_types::messages::StickerSet, now_secs: i32) -> u64 {
        let set_id = self.feed_set(&data.set, now_secs);
        self.feed_set_stickers(set_id, &data.documents, &data.packs);
        set_id
    }

    /// Populates documents & emoji packs for a set (`Data::Stickers::feedSetStickers`,
    /// `data_stickers.cpp:1645-1752`).
    pub fn feed_set_stickers(
        &mut self,
        set_id: u64,
        documents: &[tl_enums::Document],
        packs: &[tl_enums::StickerPack],
    ) {
        let stickers: Vec<u64> = documents.iter().filter_map(doc_id_from_tl).collect();
        let emoji = packs_map_from_tl(packs);
        if let Some(set) = self.sets.get_mut(&set_id) {
            set.flags &= !StickerSetFlags::NOT_LOADED;
            set.stickers = stickers;
            set.emoji = emoji;
        }
    }

    /// Applies `messages.allStickers` / `getMaskStickers` / `getEmojiStickers` response
    /// (`Data::Stickers::somethingReceived`, `data_stickers.cpp:790-885`), returning
    /// `(set_id, access_hash)` pairs that need `messages.getStickerSet`.
    pub fn apply_all_stickers(
        &mut self,
        kind: StickersType,
        list: &[tl_enums::StickerSet],
        server_hash: i64,
        now_secs: i32,
    ) -> Vec<(u64, u64)> {
        let order_ref = match kind {
            StickersType::Emoji => &mut self.emoji_sets_order,
            StickersType::Masks => &mut self.mask_sets_order,
            StickersType::Stickers => &mut self.sets_order,
        };
        order_ref.clear();

        for set in self.sets.values_mut() {
            if !set.flags.contains(StickerSetFlags::ARCHIVED) && set.kind() == kind {
                set.flags &= !StickerSetFlags::INSTALLED;
                set.install_date = 0;
            }
        }

        let mut sets_to_request = BTreeMap::new();
        for raw in list {
            let set_id = self.feed_set(raw, now_secs);
            if let Some(set) = self.sets.get(&set_id)
                && (!set.flags.contains(StickerSetFlags::ARCHIVED)
                    || set.flags.contains(StickerSetFlags::OFFICIAL))
            {
                let order_ref = match kind {
                    StickersType::Emoji => &mut self.emoji_sets_order,
                    StickersType::Masks => &mut self.mask_sets_order,
                    StickersType::Stickers => &mut self.sets_order,
                };
                order_ref.push(set_id);
                if set.stickers.is_empty() || set.flags.contains(StickerSetFlags::NOT_LOADED) {
                    sets_to_request.insert(set_id, set.access_hash);
                }
            }
        }

        self.sets.retain(|_, set| {
            set.flags.intersects(
                StickerSetFlags::INSTALLED
                    | StickerSetFlags::FEATURED
                    | StickerSetFlags::SPECIAL
                    | StickerSetFlags::ARCHIVED
                    | StickerSetFlags::EMOJI,
            )
        });

        let counted = match kind {
            StickersType::Emoji => self.count_custom_emoji_hash(false),
            StickersType::Masks => self.count_masks_hash(false),
            StickersType::Stickers => self.count_stickers_hash(false),
        };
        if counted != server_hash as u64 {
            warn!(
                ?kind,
                server_hash = server_hash as u64,
                counted,
                "received sticker sets hash differs from counted hash"
            );
        }

        sets_to_request.into_iter().collect()
    }

    /// Applies a special set (`CloudRecentSetId`, `CloudRecentAttachedSetId`, or `FavedSetId`,
    /// `Data::Stickers::specialSetReceived`, `data_stickers.cpp:934-1060`).
    pub fn apply_special_set(
        &mut self,
        set_id: u64,
        title: &str,
        items: &[tl_enums::Document],
        server_hash: i64,
        packs: &[tl_enums::StickerPack],
        usage_dates: &[i32],
    ) {
        if items.is_empty() {
            self.sets.remove(&set_id);
            return;
        }
        let stickers: Vec<u64> = items.iter().filter_map(doc_id_from_tl).collect();
        if stickers.is_empty() {
            self.sets.remove(&set_id);
            return;
        }
        let dates = if items.len() == usage_dates.len()
            && (set_id == CLOUD_RECENT_SET_ID || set_id == CLOUD_RECENT_ATTACHED_SET_ID)
        {
            usage_dates.to_vec()
        } else {
            Vec::new()
        };
        let emoji = packs_map_from_tl(packs);
        let count = stickers.len() as i32;
        self.sets.insert(
            set_id,
            CachedStickerSet {
                id: set_id,
                access_hash: 0,
                hash: server_hash as u64,
                title: title.to_owned(),
                short_name: String::new(),
                count,
                flags: StickerSetFlags::SPECIAL,
                install_date: 0,
                thumbnail_document_id: 0,
                stickers,
                covers: Vec::new(),
                dates,
                emoji,
            },
        );
    }

    /// Applies `messages.featuredStickers` (`Data::Stickers::featuredReceived`,
    /// `data_stickers.cpp:1080-1218`), returning `(set_id, access_hash)` pairs that need loading.
    pub fn apply_featured_stickers(
        &mut self,
        kind: StickersType,
        data: &tl_types::messages::FeaturedStickers,
        now_secs: i32,
    ) -> Vec<(u64, u64)> {
        let unread_set: BTreeSet<u64> = data.unread.iter().map(|&id| id as u64).collect();
        let is_emoji = kind == StickersType::Emoji;
        if is_emoji {
            self.featured_emoji_sets_order.clear();
        } else {
            self.featured_sets_order.clear();
        }

        for set in self.sets.values_mut() {
            if set.kind() == kind {
                set.flags &= !StickerSetFlags::FEATURED;
            }
        }

        let mut sets_to_request = BTreeMap::new();
        for entry in &data.sets {
            let set_id = self.feed_set_covered(entry, now_secs);
            if let Some(set) = self.sets.get_mut(&set_id) {
                set.flags |= StickerSetFlags::FEATURED;
                if unread_set.contains(&set_id) {
                    set.flags |= StickerSetFlags::UNREAD;
                } else {
                    set.flags &= !StickerSetFlags::UNREAD;
                }
                if is_emoji {
                    set.flags |= StickerSetFlags::EMOJI;
                }
                if set.stickers.is_empty() || set.flags.contains(StickerSetFlags::NOT_LOADED) {
                    sets_to_request.insert(set_id, set.access_hash);
                }
            }
            if is_emoji {
                self.featured_emoji_sets_order.push(set_id);
            } else {
                self.featured_sets_order.push(set_id);
            }
        }

        let mut unread_count = 0usize;
        self.sets.retain(|_, set| {
            let keep = set.flags.intersects(
                StickerSetFlags::INSTALLED
                    | StickerSetFlags::FEATURED
                    | StickerSetFlags::SPECIAL
                    | StickerSetFlags::ARCHIVED
                    | StickerSetFlags::EMOJI,
            );
            if keep
                && set.flags.contains(StickerSetFlags::FEATURED)
                && set.flags.contains(StickerSetFlags::UNREAD)
                && !set.flags.contains(StickerSetFlags::EMOJI)
            {
                unread_count += 1;
            }
            keep
        });
        self.featured_sets_unread_count = unread_count;

        sets_to_request.into_iter().collect()
    }

    /// Applies `messages.savedGifs` (`Data::Stickers::gifsReceived`, `data_stickers.cpp:1220-1246`).
    pub fn apply_saved_gifs(&mut self, gifs: &[tl_enums::Document]) {
        self.saved_gifs = gifs.iter().filter_map(doc_id_from_tl).collect();
    }

    /// Applies `updateNewStickerSet` locally (`Data::Stickers::newSetReceived`,
    /// `data_stickers.cpp:1766-1791`). Returns `Some(set_id)` if accepted, `None` if rejected.
    pub fn apply_update_new_sticker_set(
        &mut self,
        stickerset: &tl_enums::messages::StickerSet,
        now_secs: i32,
    ) -> Option<u64> {
        let tl_enums::messages::StickerSet::StickerSet(full) = stickerset else {
            warn!("API Error: Unexpected messages.stickerSetNotModified in updateNewStickerSet");
            return None;
        };
        let tl_enums::StickerSet::StickerSet(s) = &full.set;
        if s.installed_date.is_none() || s.archived {
            return None;
        }
        let set_id = s.id as u64;
        let order = if s.emojis {
            &mut self.emoji_sets_order
        } else if s.masks {
            &mut self.mask_sets_order
        } else {
            &mut self.sets_order
        };
        if order.first().copied() != Some(set_id) {
            order.retain(|&id| id != set_id);
            order.insert(0, set_id);
        }
        self.feed_set_full(full, now_secs);
        Some(set_id)
    }

    /// Applies `messages.stickerSetInstallResultArchive` (`Data::Stickers::applyArchivedResult`,
    /// `data_stickers.cpp:451-506`), returning `(archived_ids, sets_to_request)`.
    pub fn apply_archived_result(
        &mut self,
        sets: &[tl_enums::StickerSetCovered],
        now_secs: i32,
    ) -> (Vec<u64>, Vec<(u64, u64)>) {
        let mut archived_ids = Vec::with_capacity(sets.len());
        let mut sets_to_request = BTreeMap::new();
        for covered in sets {
            let set_id = self.feed_set_covered(covered, now_secs);
            if let Some(set) = self.sets.get_mut(&set_id) {
                set.flags |= StickerSetFlags::ARCHIVED;
                if set.flags.contains(StickerSetFlags::NOT_LOADED) {
                    sets_to_request.insert(set_id, set.access_hash);
                }
                let kind = set.kind();
                if kind == StickersType::Emoji {
                    continue;
                }
                let is_masks = kind == StickersType::Masks;
                if is_masks {
                    self.mask_sets_order.retain(|&id| id != set_id);
                    if !self.archived_mask_sets_order.contains(&set_id) {
                        self.archived_mask_sets_order.insert(0, set_id);
                    }
                } else {
                    self.sets_order.retain(|&id| id != set_id);
                    if !self.archived_sets_order.contains(&set_id) {
                        self.archived_sets_order.insert(0, set_id);
                    }
                }
                archived_ids.push(set_id);
            }
        }
        (archived_ids, sets_to_request.into_iter().collect())
    }

    /// Marks a known sticker set installed locally (`Data::Stickers::installLocally`,
    /// `data_stickers.cpp:508-571`).
    pub fn install_locally(&mut self, set_id: u64, now_secs: i32) -> bool {
        let Some(set) = self.sets.get_mut(&set_id) else {
            return false;
        };
        let old_flags = set.flags;
        set.flags &= !(StickerSetFlags::ARCHIVED | StickerSetFlags::UNREAD);
        set.flags |= StickerSetFlags::INSTALLED;
        set.install_date = now_secs;
        let changed = old_flags ^ set.flags;
        let kind = set.kind();

        let order = match kind {
            StickersType::Emoji => &mut self.emoji_sets_order,
            StickersType::Masks => &mut self.mask_sets_order,
            StickersType::Stickers => &mut self.sets_order,
        };
        if order.first().copied() != Some(set_id) {
            order.retain(|&id| id != set_id);
            order.insert(0, set_id);
        }
        if kind != StickersType::Emoji && changed.contains(StickerSetFlags::ARCHIVED) {
            let archived_order = if kind == StickersType::Masks {
                &mut self.archived_mask_sets_order
            } else {
                &mut self.archived_sets_order
            };
            archived_order.retain(|&id| id != set_id);
        }
        true
    }

    // -------------------------------------------------------------------------
    // Async RPC methods (`apiwrap.cpp`)
    // -------------------------------------------------------------------------

    /// Fetches `messages.getAllStickers` if due (`ApiWrap::requestStickers`, `apiwrap.cpp:3235-3258`).
    /// Returns `Ok(true)` if an RPC was issued, `Ok(false)` if throttled within 1 hour.
    pub async fn request_stickers<C: RpcClient>(
        &mut self,
        rpc: &C,
        now_ms: u64,
    ) -> Result<bool, ApiError> {
        if !Self::update_needed(self.last_stickers_update_ms, now_ms) {
            return Ok(false);
        }
        let hash = self.count_stickers_hash(true) as i64;
        let resp = rpc
            .invoke(&tl_fns::messages::GetAllStickers { hash })
            .await
            .map_err(Into::into);
        self.last_stickers_update_ms = now_ms;
        let to_load = match resp? {
            tl_enums::messages::AllStickers::NotModified => Vec::new(),
            tl_enums::messages::AllStickers::AllStickers(data) => self.apply_all_stickers(
                StickersType::Stickers,
                &data.sets,
                data.hash,
                (now_ms / 1000) as i32,
            ),
        };
        for (id, access_hash) in to_load {
            let _ = self
                .get_sticker_set(
                    rpc,
                    &StickerSetSpec::Id { id, access_hash },
                    Some(0),
                    now_ms,
                )
                .await;
        }
        Ok(true)
    }

    /// Fetches `messages.getMaskStickers` if due (`ApiWrap::requestMasks`, `apiwrap.cpp:3260-3282`).
    pub async fn request_masks<C: RpcClient>(
        &mut self,
        rpc: &C,
        now_ms: u64,
    ) -> Result<bool, ApiError> {
        if !Self::update_needed(self.last_masks_update_ms, now_ms) {
            return Ok(false);
        }
        let hash = self.count_masks_hash(true) as i64;
        let resp = rpc
            .invoke(&tl_fns::messages::GetMaskStickers { hash })
            .await
            .map_err(Into::into);
        self.last_masks_update_ms = now_ms;
        let to_load = match resp? {
            tl_enums::messages::AllStickers::NotModified => Vec::new(),
            tl_enums::messages::AllStickers::AllStickers(data) => self.apply_all_stickers(
                StickersType::Masks,
                &data.sets,
                data.hash,
                (now_ms / 1000) as i32,
            ),
        };
        for (id, access_hash) in to_load {
            let _ = self
                .get_sticker_set(
                    rpc,
                    &StickerSetSpec::Id { id, access_hash },
                    Some(0),
                    now_ms,
                )
                .await;
        }
        Ok(true)
    }

    /// Fetches `messages.getEmojiStickers` if due (`ApiWrap::requestCustomEmoji`, `apiwrap.cpp:3284-3306`).
    pub async fn request_custom_emoji<C: RpcClient>(
        &mut self,
        rpc: &C,
        now_ms: u64,
    ) -> Result<bool, ApiError> {
        if !Self::update_needed(self.last_emoji_update_ms, now_ms) {
            return Ok(false);
        }
        let hash = self.count_custom_emoji_hash(true) as i64;
        let resp = rpc
            .invoke(&tl_fns::messages::GetEmojiStickers { hash })
            .await
            .map_err(Into::into);
        self.last_emoji_update_ms = now_ms;
        let to_load = match resp? {
            tl_enums::messages::AllStickers::NotModified => Vec::new(),
            tl_enums::messages::AllStickers::AllStickers(data) => self.apply_all_stickers(
                StickersType::Emoji,
                &data.sets,
                data.hash,
                (now_ms / 1000) as i32,
            ),
        };
        for (id, access_hash) in to_load {
            let _ = self
                .get_sticker_set(
                    rpc,
                    &StickerSetSpec::Id { id, access_hash },
                    Some(0),
                    now_ms,
                )
                .await;
        }
        Ok(true)
    }

    /// Fetches `messages.getRecentStickers` (`ApiWrap::requestRecentStickers`, `apiwrap.cpp:3308-3366`).
    /// Passing `now_ms = None` forces a refresh with `hash = 0` (`apiwrap.cpp:3341`).
    pub async fn request_recent_stickers<C: RpcClient>(
        &mut self,
        rpc: &C,
        now_ms: Option<u64>,
        attached: bool,
        wall_now_ms: u64,
    ) -> Result<bool, ApiError> {
        if let Some(now) = now_ms {
            let last = if attached {
                self.last_recent_attached_update_ms
            } else {
                self.last_recent_update_ms
            };
            if !Self::update_needed(last, now) {
                return Ok(false);
            }
        }
        let hash = if now_ms.is_some() {
            self.count_recent_stickers_hash(attached) as i64
        } else {
            0
        };
        let resp = rpc
            .invoke(&tl_fns::messages::GetRecentStickers { attached, hash })
            .await
            .map_err(Into::into);
        if attached {
            self.last_recent_attached_update_ms = wall_now_ms;
        } else {
            self.last_recent_update_ms = wall_now_ms;
        }
        match resp? {
            tl_enums::messages::RecentStickers::NotModified => {}
            tl_enums::messages::RecentStickers::RecentStickers(d) => {
                let target_id = if attached {
                    CLOUD_RECENT_ATTACHED_SET_ID
                } else {
                    CLOUD_RECENT_SET_ID
                };
                self.apply_special_set(
                    target_id,
                    "Recent",
                    &d.stickers,
                    d.hash,
                    &d.packs,
                    &d.dates,
                );
            }
        }
        Ok(true)
    }

    /// Fetches `messages.getFavedStickers` (`ApiWrap::requestFavedStickers`, `apiwrap.cpp:3368-3400`).
    /// Passing `now_ms = None` forces a refresh with `hash = 0` (`apiwrap.cpp:3376`).
    pub async fn request_faved_stickers<C: RpcClient>(
        &mut self,
        rpc: &C,
        now_ms: Option<u64>,
        wall_now_ms: u64,
    ) -> Result<bool, ApiError> {
        if let Some(now) = now_ms
            && !Self::update_needed(self.last_faved_update_ms, now)
        {
            return Ok(false);
        }
        let hash = if now_ms.is_some() {
            self.count_faved_stickers_hash() as i64
        } else {
            0
        };
        let resp = rpc
            .invoke(&tl_fns::messages::GetFavedStickers { hash })
            .await
            .map_err(Into::into);
        self.last_faved_update_ms = wall_now_ms;
        match resp? {
            tl_enums::messages::FavedStickers::NotModified => {}
            tl_enums::messages::FavedStickers::FavedStickers(d) => {
                self.apply_special_set(
                    FAVED_SET_ID,
                    "Favorites",
                    &d.stickers,
                    d.hash,
                    &d.packs,
                    &[],
                );
            }
        }
        Ok(true)
    }

    /// Fetches `messages.getFeaturedStickers` if due (`ApiWrap::requestFeaturedStickers`,
    /// `apiwrap.cpp:3402-3417`).
    pub async fn request_featured_stickers<C: RpcClient>(
        &mut self,
        rpc: &C,
        now_ms: u64,
    ) -> Result<bool, ApiError> {
        if !Self::update_needed(self.last_featured_update_ms, now_ms) {
            return Ok(false);
        }
        let hash = self.count_featured_stickers_hash() as i64;
        let resp = rpc
            .invoke(&tl_fns::messages::GetFeaturedStickers { hash })
            .await
            .map_err(Into::into);
        self.last_featured_update_ms = now_ms;
        let to_load = match resp? {
            tl_enums::messages::FeaturedStickers::NotModified(_) => Vec::new(),
            tl_enums::messages::FeaturedStickers::FeaturedStickers(d) => {
                self.apply_featured_stickers(StickersType::Stickers, &d, (now_ms / 1000) as i32)
            }
        };
        for (id, access_hash) in to_load {
            let _ = self
                .get_sticker_set(
                    rpc,
                    &StickerSetSpec::Id { id, access_hash },
                    Some(0),
                    now_ms,
                )
                .await;
        }
        Ok(true)
    }

    /// Fetches `messages.getFeaturedEmojiStickers` if due (`ApiWrap::requestFeaturedEmoji`,
    /// `apiwrap.cpp:3419-3435`, using `Api::CountFeaturedEmojiHash` matching
    /// `data/stickers/data_stickers.cpp:1195` & `api/api_hash.cpp:129-133`).
    pub async fn request_featured_emoji<C: RpcClient>(
        &mut self,
        rpc: &C,
        now_ms: u64,
    ) -> Result<bool, ApiError> {
        if !Self::update_needed(self.last_featured_emoji_update_ms, now_ms) {
            return Ok(false);
        }
        let hash = self.count_featured_emoji_hash() as i64;
        let resp = rpc
            .invoke(&tl_fns::messages::GetFeaturedEmojiStickers { hash })
            .await
            .map_err(Into::into);
        self.last_featured_emoji_update_ms = now_ms;
        let to_load = match resp? {
            tl_enums::messages::FeaturedStickers::NotModified(_) => Vec::new(),
            tl_enums::messages::FeaturedStickers::FeaturedStickers(d) => {
                self.apply_featured_stickers(StickersType::Emoji, &d, (now_ms / 1000) as i32)
            }
        };
        for (id, access_hash) in to_load {
            let _ = self
                .get_sticker_set(
                    rpc,
                    &StickerSetSpec::Id { id, access_hash },
                    Some(0),
                    now_ms,
                )
                .await;
        }
        Ok(true)
    }

    /// Fetches `messages.getSavedGifs` if due (`ApiWrap::requestSavedGifs`, `apiwrap.cpp:3437-3464`).
    pub async fn request_saved_gifs<C: RpcClient>(
        &mut self,
        rpc: &C,
        now_ms: u64,
    ) -> Result<bool, ApiError> {
        if !Self::update_needed(self.last_saved_gifs_update_ms, now_ms) {
            return Ok(false);
        }
        let hash = self.count_saved_gifs_hash() as i64;
        let resp = rpc
            .invoke(&tl_fns::messages::GetSavedGifs { hash })
            .await
            .map_err(Into::into);
        self.last_saved_gifs_update_ms = now_ms;
        match resp? {
            tl_enums::messages::SavedGifs::NotModified => {}
            tl_enums::messages::SavedGifs::SavedGifs(d) => {
                self.apply_saved_gifs(&d.gifs);
            }
        }
        Ok(true)
    }

    /// Composite sticker refresh (`ApiWrap::updateStickers`, `apiwrap.cpp:3122-3128`).
    pub async fn update_stickers<C: RpcClient>(
        &mut self,
        rpc: &C,
        now_ms: u64,
    ) -> Result<(), ApiError> {
        let _ = self.request_stickers(rpc, now_ms).await?;
        let _ = self
            .request_recent_stickers(rpc, Some(now_ms), false, now_ms)
            .await?;
        let _ = self
            .request_faved_stickers(rpc, Some(now_ms), now_ms)
            .await?;
        let _ = self.request_featured_stickers(rpc, now_ms).await?;
        Ok(())
    }

    /// Composite masks refresh (`ApiWrap::updateMasks`, `apiwrap.cpp:3135-3139`).
    pub async fn update_masks<C: RpcClient>(
        &mut self,
        rpc: &C,
        now_ms: u64,
    ) -> Result<(), ApiError> {
        let _ = self.request_masks(rpc, now_ms).await?;
        let _ = self
            .request_recent_stickers(rpc, Some(now_ms), true, now_ms)
            .await?;
        Ok(())
    }

    /// Composite custom emoji refresh (`ApiWrap::updateCustomEmoji`, `apiwrap.cpp:3141-3145`).
    pub async fn update_custom_emoji<C: RpcClient>(
        &mut self,
        rpc: &C,
        now_ms: u64,
    ) -> Result<(), ApiError> {
        let _ = self.request_custom_emoji(rpc, now_ms).await?;
        let _ = self.request_featured_emoji(rpc, now_ms).await?;
        Ok(())
    }

    /// Saved GIFs refresh (`ApiWrap::updateSavedGifs`, `apiwrap.cpp:3130-3133`).
    pub async fn update_saved_gifs<C: RpcClient>(
        &mut self,
        rpc: &C,
        now_ms: u64,
    ) -> Result<(), ApiError> {
        let _ = self.request_saved_gifs(rpc, now_ms).await?;
        Ok(())
    }

    /// Fetches a single sticker set via `messages.getStickerSet` with any [`StickerSetSpec`].
    ///
    /// If `hash_override` is `None`, uses the cached set's `hash as i32` when already loaded,
    /// or `0` when not yet loaded. On `messages.stickerSetNotModified`, preserves the cached
    /// set untouched and returns `Ok(None)`.
    pub async fn get_sticker_set<C: RpcClient>(
        &mut self,
        rpc: &C,
        spec: &StickerSetSpec,
        hash_override: Option<i32>,
        now_ms: u64,
    ) -> Result<Option<u64>, ApiError> {
        let hash = hash_override.unwrap_or_else(|| {
            let cached_id = match spec {
                StickerSetSpec::Id { id, .. } => Some(*id),
                other => self.sets_by_spec.get(other).copied(),
            };
            cached_id
                .and_then(|id| self.sets.get(&id))
                .filter(|s| {
                    !s.stickers.is_empty() && !s.flags.contains(StickerSetFlags::NOT_LOADED)
                })
                .map_or(0, |s| s.hash as i32)
        });
        let resp = rpc
            .invoke(&tl_fns::messages::GetStickerSet {
                stickerset: spec.to_tl(),
                hash,
            })
            .await
            .map_err(Into::into)?;
        match resp {
            tl_enums::messages::StickerSet::NotModified => Ok(None),
            tl_enums::messages::StickerSet::StickerSet(full) => {
                let set_id = self.feed_set_full(&full, (now_ms / 1000) as i32);
                self.sets_by_spec.insert(spec.clone(), set_id);
                Ok(Some(set_id))
            }
        }
    }

    /// Installs a sticker set via `messages.installStickerSet` and handles both
    /// `stickerSetInstallResultSuccess` and `stickerSetInstallResultArchive`
    /// (`data/stickers/data_stickers.cpp:451-571`).
    pub async fn install_sticker_set<C: RpcClient>(
        &mut self,
        rpc: &C,
        spec: &StickerSetSpec,
        archived: bool,
        now_ms: u64,
    ) -> Result<InstallStickerSetOutcome, ApiError> {
        let now_secs = (now_ms / 1000) as i32;
        let resp = rpc
            .invoke(&tl_fns::messages::InstallStickerSet {
                stickerset: spec.to_tl(),
                archived,
            })
            .await
            .map_err(Into::into)?;

        if !archived {
            let maybe_id = match spec {
                StickerSetSpec::Id { id, .. } => Some(*id),
                other => self.sets_by_spec.get(other).copied(),
            };
            if let Some(id) = maybe_id {
                self.install_locally(id, now_secs);
            }
        }

        match resp {
            tl_enums::messages::StickerSetInstallResult::Success => {
                Ok(InstallStickerSetOutcome::Success)
            }
            tl_enums::messages::StickerSetInstallResult::Archive(arch) => {
                let (archived_ids, to_load) = self.apply_archived_result(&arch.sets, now_secs);
                for (id, access_hash) in to_load {
                    let _ = self
                        .get_sticker_set(
                            rpc,
                            &StickerSetSpec::Id { id, access_hash },
                            Some(0),
                            now_ms,
                        )
                        .await;
                }
                Ok(InstallStickerSetOutcome::Archived(archived_ids))
            }
        }
    }

    /// Uninstalls a sticker set via `messages.uninstallStickerSet`
    /// (`ApiWrap::saveStickerSets`, `apiwrap.cpp:1794-1824`).
    pub async fn uninstall_sticker_set<C: RpcClient>(
        &mut self,
        rpc: &C,
        set_id: u64,
        fallback_spec: Option<&StickerSetSpec>,
    ) -> Result<bool, ApiError> {
        let stickerset = if let Some(set) = self.sets.get(&set_id) {
            set.to_input_sticker_set()
        } else if let Some(spec) = fallback_spec {
            spec.to_tl()
        } else {
            return Err(ApiError::InvalidState(format!(
                "sticker set {set_id} is not cached and no fallback InputStickerSet was provided"
            )));
        };
        let ok = rpc
            .invoke(&tl_fns::messages::UninstallStickerSet { stickerset })
            .await
            .map_err(Into::into)?;
        if ok {
            self.sets_order.retain(|&id| id != set_id);
            self.mask_sets_order.retain(|&id| id != set_id);
            self.emoji_sets_order.retain(|&id| id != set_id);
            self.archived_sets_order.retain(|&id| id != set_id);
            self.archived_mask_sets_order.retain(|&id| id != set_id);
            if let Some(set) = self.sets.get_mut(&set_id) {
                let keep = set.flags.intersects(
                    StickerSetFlags::FEATURED | StickerSetFlags::SPECIAL | StickerSetFlags::EMOJI,
                );
                if keep {
                    set.flags &= !(StickerSetFlags::INSTALLED | StickerSetFlags::ARCHIVED);
                    set.install_date = 0;
                } else {
                    self.sets.remove(&set_id);
                }
            }
        }
        Ok(ok)
    }

    /// Reorders installed sticker/mask/custom-emoji sets via `messages.reorderStickerSets`
    /// (`ApiWrap::saveStickerSets`, `apiwrap.cpp:1677-1711`).
    ///
    /// On RPC failure, resets the corresponding `last_*_update_ms = 0` and triggers a refetch
    /// to resynchronize local order with the server.
    pub async fn reorder_sticker_sets<C: RpcClient>(
        &mut self,
        rpc: &C,
        kind: StickersType,
        order: &[u64],
        now_ms: u64,
    ) -> Result<bool, ApiError> {
        if order.len() < 2 {
            return Ok(false);
        }
        let req = tl_fns::messages::ReorderStickerSets {
            masks: kind == StickersType::Masks,
            emojis: kind == StickersType::Emoji,
            order: order.iter().map(|&id| id as i64).collect(),
        };
        match rpc.invoke(&req).await {
            Ok(ok) => {
                if ok {
                    let target = match kind {
                        StickersType::Emoji => &mut self.emoji_sets_order,
                        StickersType::Masks => &mut self.mask_sets_order,
                        StickersType::Stickers => &mut self.sets_order,
                    };
                    *target = order.to_vec();
                }
                Ok(ok)
            }
            Err(err) => {
                let api_err = ApiError::Invocation(err.into());
                match kind {
                    StickersType::Emoji => {
                        self.last_emoji_update_ms = 0;
                        let _ = self.update_custom_emoji(rpc, now_ms).await;
                    }
                    StickersType::Masks => {
                        self.last_masks_update_ms = 0;
                        let _ = self.update_masks(rpc, now_ms).await;
                    }
                    StickersType::Stickers => {
                        self.last_stickers_update_ms = 0;
                        let _ = self.update_stickers(rpc, now_ms).await;
                    }
                }
                Err(api_err)
            }
        }
    }

    // -------------------------------------------------------------------------
    // Pack owner actions (`boxes/sticker_set_box.cpp`, `api/api_stickers_creator.cpp`)
    // -------------------------------------------------------------------------

    /// Renames an owned sticker or custom-emoji pack via `stickers.renameStickerSet` and feeds
    /// the returned `messages.StickerSet` into the local store
    /// (`boxes/sticker_set_box.cpp:327-363`).
    ///
    /// Trims leading/trailing whitespace and rejects empty titles or titles longer than
    /// [`MAX_STICKER_SET_TITLE_LEN`] Unicode scalar values without issuing an RPC. On
    /// `messages.stickerSetNotModified`, falls back to `messages.getStickerSet(hash = 0)`.
    pub async fn rename_sticker_set<C: RpcClient>(
        &mut self,
        rpc: &C,
        spec: &StickerSetSpec,
        title: &str,
        now_ms: u64,
    ) -> Result<Option<u64>, ApiError> {
        let trimmed = title.trim();
        if trimmed.is_empty() {
            return Err(ApiError::InvalidState(
                "sticker set title must not be empty".to_owned(),
            ));
        }
        if trimmed.chars().count() > MAX_STICKER_SET_TITLE_LEN {
            return Err(ApiError::InvalidState(format!(
                "sticker set title exceeds maximum length of {MAX_STICKER_SET_TITLE_LEN} characters"
            )));
        }
        let resp = rpc
            .invoke(&tl_fns::stickers::RenameStickerSet {
                stickerset: spec.to_tl(),
                title: trimmed.to_owned(),
            })
            .await
            .map_err(Into::into)?;
        match resp {
            tl_enums::messages::StickerSet::StickerSet(full) => {
                let set_id = self.feed_set_full(&full, (now_ms / 1000) as i32);
                self.sets_by_spec.insert(spec.clone(), set_id);
                Ok(Some(set_id))
            }
            tl_enums::messages::StickerSet::NotModified => {
                self.get_sticker_set(rpc, spec, Some(0), now_ms).await
            }
        }
    }

    /// Reorders a sticker inside an owned pack via `stickers.changeStickerPosition` and feeds
    /// the updated `messages.StickerSet` into the local store
    /// (`StickerSetBox::Inner::requestReorder`, `boxes/sticker_set_box.cpp:1585-1613`).
    pub async fn change_sticker_position<C: RpcClient>(
        &mut self,
        rpc: &C,
        sticker: tl_enums::InputDocument,
        position: i32,
        now_ms: u64,
    ) -> Result<Option<u64>, ApiError> {
        if position < 0 {
            return Err(ApiError::InvalidState(format!(
                "sticker position {position} must be non-negative"
            )));
        }
        let resp = rpc
            .invoke(&tl_fns::stickers::ChangeStickerPosition { sticker, position })
            .await
            .map_err(Into::into)?;
        match resp {
            tl_enums::messages::StickerSet::StickerSet(full) => {
                let set_id = self.feed_set_full(&full, (now_ms / 1000) as i32);
                Ok(Some(set_id))
            }
            tl_enums::messages::StickerSet::NotModified => Ok(None),
        }
    }

    /// Removes a sticker from an owned pack via `stickers.removeStickerFromSet` and feeds
    /// the updated `messages.StickerSet` into the local store
    /// (`boxes/sticker_set_box.cpp:1939-1957`).
    pub async fn remove_sticker_from_set<C: RpcClient>(
        &mut self,
        rpc: &C,
        sticker: tl_enums::InputDocument,
        now_ms: u64,
    ) -> Result<Option<u64>, ApiError> {
        let resp = rpc
            .invoke(&tl_fns::stickers::RemoveStickerFromSet { sticker })
            .await
            .map_err(Into::into)?;
        match resp {
            tl_enums::messages::StickerSet::StickerSet(full) => {
                let set_id = self.feed_set_full(&full, (now_ms / 1000) as i32);
                Ok(Some(set_id))
            }
            tl_enums::messages::StickerSet::NotModified => Ok(None),
        }
    }

    /// Permanently deletes an owned sticker or custom-emoji pack via `stickers.deleteStickerSet`
    /// and evicts it from every local order list and cache map
    /// (`Api::DeleteStickerSet`, `api/api_stickers_creator.cpp:364-382` /
    /// `boxes/sticker_set_box.cpp:922`).
    pub async fn delete_sticker_set<C: RpcClient>(
        &mut self,
        rpc: &C,
        set_id: u64,
        fallback_spec: Option<&StickerSetSpec>,
    ) -> Result<bool, ApiError> {
        let stickerset = if let Some(set) = self.sets.get(&set_id) {
            set.to_input_sticker_set()
        } else if let Some(spec) = fallback_spec {
            spec.to_tl()
        } else {
            return Err(ApiError::InvalidState(format!(
                "sticker set {set_id} is not cached and no fallback InputStickerSet was provided"
            )));
        };
        let ok = rpc
            .invoke(&tl_fns::stickers::DeleteStickerSet { stickerset })
            .await
            .map_err(Into::into)?;
        if ok {
            self.sets_order.retain(|&id| id != set_id);
            self.mask_sets_order.retain(|&id| id != set_id);
            self.emoji_sets_order.retain(|&id| id != set_id);
            self.archived_sets_order.retain(|&id| id != set_id);
            self.archived_mask_sets_order.retain(|&id| id != set_id);
            self.featured_sets_order.retain(|&id| id != set_id);
            self.featured_emoji_sets_order.retain(|&id| id != set_id);
            self.sets.remove(&set_id);
            self.sets_by_spec.retain(|_, &mut id| id != set_id);
        }
        Ok(ok)
    }

    // -------------------------------------------------------------------------
    // Update handlers (`api/api_updates.cpp:2740-2887`)
    // -------------------------------------------------------------------------

    /// Handles `updateStickerSets` (`api/api_updates.cpp:2828-2840`).
    pub async fn handle_update_sticker_sets<C: RpcClient>(
        &mut self,
        rpc: &C,
        masks: bool,
        emojis: bool,
        now_ms: u64,
    ) -> Result<(), ApiError> {
        if emojis {
            self.last_emoji_update_ms = 0;
            self.update_custom_emoji(rpc, now_ms).await
        } else if masks {
            self.last_masks_update_ms = 0;
            self.update_masks(rpc, now_ms).await
        } else {
            self.last_stickers_update_ms = 0;
            self.update_stickers(rpc, now_ms).await
        }
    }

    /// Handles `updateStickerSetsOrder` (`api/api_updates.cpp:2749-2796`).
    ///
    /// Reorders locally without an RPC when every set ID is already known and the list length
    /// matches; otherwise invalidates the timestamp and refetches from the server.
    pub async fn handle_update_sticker_sets_order<C: RpcClient>(
        &mut self,
        rpc: &C,
        masks: bool,
        emojis: bool,
        order: &[u64],
        now_ms: u64,
    ) -> Result<bool, ApiError> {
        let mut result = Vec::with_capacity(order.len());
        for &set_id in order {
            if !self.sets.contains_key(&set_id) {
                break;
            }
            result.push(set_id);
        }
        let local_size = if emojis {
            self.emoji_sets_order.len()
        } else if masks {
            self.mask_sets_order.len()
        } else {
            self.sets_order.len()
        };
        if result.len() != local_size || result.len() != order.len() {
            if emojis {
                self.last_emoji_update_ms = 0;
                self.update_custom_emoji(rpc, now_ms).await?;
            } else if masks {
                self.last_masks_update_ms = 0;
                self.update_masks(rpc, now_ms).await?;
            } else {
                self.last_stickers_update_ms = 0;
                self.update_stickers(rpc, now_ms).await?;
            }
            Ok(false)
        } else {
            if emojis {
                self.emoji_sets_order = result;
            } else if masks {
                self.mask_sets_order = result;
            } else {
                self.sets_order = result;
            }
            Ok(true)
        }
    }

    /// Handles `updateMoveStickerSetToTop` (`api/api_updates.cpp:2798-2826`).
    ///
    /// Rotates `set_id` to index 0 locally if present; otherwise resets timestamp and refetches.
    pub async fn handle_update_move_sticker_set_to_top<C: RpcClient>(
        &mut self,
        rpc: &C,
        _masks: bool,
        emojis: bool,
        set_id: u64,
        now_ms: u64,
    ) -> Result<bool, ApiError> {
        let order = if emojis {
            &mut self.emoji_sets_order
        } else {
            &mut self.sets_order
        };
        if let Some(pos) = order.iter().position(|&id| id == set_id) {
            if pos > 0 {
                order[..=pos].rotate_right(1);
            }
            Ok(true)
        } else {
            if emojis {
                self.last_emoji_update_ms = 0;
                self.update_custom_emoji(rpc, now_ms).await?;
            } else {
                self.last_stickers_update_ms = 0;
                self.update_stickers(rpc, now_ms).await?;
            }
            Ok(false)
        }
    }

    /// Handles `updateRecentStickers` (`api/api_updates.cpp:2842-2845`).
    pub async fn handle_update_recent_stickers<C: RpcClient>(
        &mut self,
        rpc: &C,
        now_ms: u64,
    ) -> Result<(), ApiError> {
        self.last_recent_update_ms = 0;
        self.update_stickers(rpc, now_ms).await
    }

    /// Handles `updateFavedStickers` (`api/api_updates.cpp:2847-2850`).
    pub async fn handle_update_faved_stickers<C: RpcClient>(
        &mut self,
        rpc: &C,
        now_ms: u64,
    ) -> Result<(), ApiError> {
        self.last_faved_update_ms = 0;
        self.update_stickers(rpc, now_ms).await
    }

    /// Handles `updateReadFeaturedStickers` (`api/api_updates.cpp:2852-2858`).
    pub async fn handle_update_read_featured_stickers<C: RpcClient>(
        &mut self,
        rpc: &C,
        now_ms: u64,
    ) -> Result<(), ApiError> {
        self.last_featured_update_ms = 0;
        self.update_stickers(rpc, now_ms).await
    }

    /// Handles `updateSavedGifs` (`api/api_updates.cpp:2884-2887`).
    pub async fn handle_update_saved_gifs<C: RpcClient>(
        &mut self,
        rpc: &C,
        now_ms: u64,
    ) -> Result<(), ApiError> {
        self.last_saved_gifs_update_ms = 0;
        self.update_saved_gifs(rpc, now_ms).await
    }
}

fn doc_id_from_tl(doc: &tl_enums::Document) -> Option<u64> {
    match doc {
        tl_enums::Document::Document(d) => Some(d.id as u64),
        tl_enums::Document::Empty(_) => None,
    }
}

fn packs_map_from_tl(packs: &[tl_enums::StickerPack]) -> BTreeMap<String, Vec<u64>> {
    let mut out = BTreeMap::new();
    for p in packs {
        let tl_enums::StickerPack::StickerPack(sp) = p;
        let ids: Vec<u64> = sp.documents.iter().map(|&id| id as u64).collect();
        out.insert(sp.emoticon.clone(), ids);
    }
    out
}
