//! Star gifts, unique collectible gifts, attributes, upgrade/transfer/resale lifecycle states,
//! and TL conversions.
//!
//! Port of `data/data_star_gift.h`, `data/data_star_gift.cpp`, and `api/api_premium.cpp`
//! (`FromTL` for `StarGift`, `SavedStarGift`, and `StarGiftAttribute`).

use std::collections::{BTreeMap, BTreeSet};

use tg_data::{ChannelId, ChatId, CollectibleId, DocumentId, MsgId, PeerId, TimeId, UserId};
use tg_tl::{enums as tl_enums, functions as tl_fn, types as tl_types};

use crate::credits::{CreditsAmount, CreditsType, ONE_STAR_IN_NANO};

/// Default page size for saved/resale/craft star gift queries (`kMyGiftsPerPage` etc.).
pub const GIFTS_PER_PAGE: i32 = 50;

/// Temporary cloud password validity period used for Star Gift blockchain export/withdrawal
/// (`kTmpPasswordPeriod = 10 * TimeId(60)` in `boxes/transfer_gift_box.cpp:80`).
pub const GIFT_WITHDRAW_TMP_PASSWORD_PERIOD_SECS: i32 = 10 * 60;

/// Returns `true` if the server RPC error indicates the cached `tmp_password` was refused
/// (`TmpPasswordRefused` in `boxes/transfer_gift_box.cpp:208-215`).
///
/// Accepts either a raw Telegram error code (e.g. `"TMP_PASSWORD_INVALID"`) or a formatted
/// `InvocationError` string containing the error code.
pub fn is_tmp_password_refused_error(error_type: &str) -> bool {
    error_type.contains("TMP_PASSWORD_INVALID")
        || error_type.contains("PASSWORD_HASH_INVALID")
        || error_type.contains("SRP_PASSWORD_CHANGED")
}

/// Result of `payments.checkCanSendGift` (`CheckMaybeGiftLocked` in `boxes/star_gift_box.cpp:1536-1547`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CanSendGiftResult {
    /// The gift can currently be sent (`payments.checkCanSendGiftResultOk`).
    Ok,
    /// The gift is locked or unavailable (`payments.checkCanSendGiftResultFail`).
    Fail {
        /// Human-readable server explanation text.
        reason: String,
    },
}

/// Packed 24-bit RGB color decoded from Telegram's `int` color fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct RgbColor {
    /// Red channel (`0..=255`).
    pub r: u8,
    /// Green channel (`0..=255`).
    pub g: u8,
    /// Blue channel (`0..=255`).
    pub b: u8,
}

impl RgbColor {
    /// Creates an [`RgbColor`] from `(r, g, b)`.
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// Decodes a 24-bit `0x00RRGGBB` integer (`Ui::ColorFromSerialized`).
    pub const fn from_serialized(value: i32) -> Self {
        let u = value as u32;
        Self {
            r: ((u >> 16) & 0xff) as u8,
            g: ((u >> 8) & 0xff) as u8,
            b: (u & 0xff) as u8,
        }
    }

    /// Encodes back into `0x00RRGGBB`.
    pub const fn to_serialized(self) -> i32 {
        (((self.r as u32) << 16) | ((self.g as u32) << 8) | (self.b as u32)) as i32
    }
}

/// Converts a TL [`tl_enums::Peer`] into a `tg-data` [`PeerId`].
pub const fn peer_id_from_tl(peer: &tl_enums::Peer) -> PeerId {
    match peer {
        tl_enums::Peer::User(u) => PeerId::from_user(UserId::from_tl(u.user_id)),
        tl_enums::Peer::Chat(c) => PeerId::from_chat(ChatId::from_tl(c.chat_id)),
        tl_enums::Peer::Channel(ch) => PeerId::from_channel(ChannelId::from_tl(ch.channel_id)),
    }
}

/// Extracts a [`DocumentId`] from a TL [`tl_enums::Document`].
pub const fn document_id_from_tl(doc: &tl_enums::Document) -> DocumentId {
    match doc {
        tl_enums::Document::Empty(d) => DocumentId::from_tl(d.id),
        tl_enums::Document::Document(d) => DocumentId::from_tl(d.id),
    }
}

/// Special rarity tier for collectible gift attributes (`Data::UniqueGiftRarity`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum UniqueGiftRarity {
    /// Numeric permille rarity (`rarity_value >= 0`).
    #[default]
    Default = 0,
    /// Uncommon (`-1`).
    Uncommon = -1,
    /// Rare (`-2`).
    Rare = -2,
    /// Epic (`-3`).
    Epic = -3,
    /// Legendary (`-4`).
    Legendary = -4,
}

impl UniqueGiftRarity {
    /// Badge RGB color matching `UniqueGiftRarityBadgeColors` in `data_star_gift.cpp`.
    pub const fn badge_color(self) -> Option<RgbColor> {
        match self {
            Self::Default => None,
            Self::Uncommon => Some(RgbColor::new(0x40, 0xA9, 0x20)),
            Self::Rare => Some(RgbColor::new(0x11, 0xAA, 0xBE)),
            Self::Epic => Some(RgbColor::new(0x95, 0x5C, 0xDB)),
            Self::Legendary => Some(RgbColor::new(0xBF, 0x76, 0x00)),
        }
    }

    /// English display label for special rarities (`UniqueGiftRarityText`).
    pub const fn label(self) -> &'static str {
        match self {
            Self::Default => "",
            Self::Uncommon => "Uncommon",
            Self::Rare => "Rare",
            Self::Epic => "Epic",
            Self::Legendary => "Legendary",
        }
    }
}

/// Parses a TL [`tl_enums::StarGiftAttributeRarity`] into the signed `rarity_value` convention
/// used by `data_star_gift.h` (`>= 0` permille, `< 0` special rarity enum).
pub fn rarity_value_from_tl(rarity: &tl_enums::StarGiftAttributeRarity) -> i32 {
    match rarity {
        tl_enums::StarGiftAttributeRarity::StarGiftAttributeRarity(r) => r.permille.max(0),
        tl_enums::StarGiftAttributeRarity::Uncommon => UniqueGiftRarity::Uncommon as i32,
        tl_enums::StarGiftAttributeRarity::Rare => UniqueGiftRarity::Rare as i32,
        tl_enums::StarGiftAttributeRarity::Epic => UniqueGiftRarity::Epic as i32,
        tl_enums::StarGiftAttributeRarity::Legendary => UniqueGiftRarity::Legendary as i32,
    }
}

/// Base attribute metadata (`Data::UniqueGiftAttribute`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UniqueGiftAttribute {
    /// Attribute display name.
    pub name: String,
    /// Non-negative permille or negative [`UniqueGiftRarity`] discriminant.
    pub rarity_value: i32,
}

impl UniqueGiftAttribute {
    /// Returns the rarity tier.
    pub const fn rarity_type(&self) -> UniqueGiftRarity {
        match self.rarity_value {
            -1 => UniqueGiftRarity::Uncommon,
            -2 => UniqueGiftRarity::Rare,
            -3 => UniqueGiftRarity::Epic,
            -4 => UniqueGiftRarity::Legendary,
            _ => UniqueGiftRarity::Default,
        }
    }

    /// Returns permille rarity (`0` if special tier).
    pub const fn rarity_permille(&self) -> i32 {
        if self.rarity_value >= 0 {
            self.rarity_value
        } else {
            0
        }
    }

    /// Returns `true` if this attribute has a special rarity tier.
    pub const fn has_special_rarity(&self) -> bool {
        !matches!(self.rarity_type(), UniqueGiftRarity::Default)
    }

    /// Formats the rarity label or percentage (`UniqueGiftAttributeText`).
    pub fn rarity_text(&self) -> String {
        let tier = self.rarity_type();
        if tier != UniqueGiftRarity::Default {
            return tier.label().to_string();
        }
        let permille = self.rarity_permille();
        if permille <= 0 {
            "< 0.1%".to_string()
        } else if permille % 10 == 0 {
            format!("{}%", permille / 10)
        } else {
            format!("{}.{}%", permille / 10, permille % 10)
        }
    }
}

/// Collectible gift 3D/sticker model attribute (`Data::UniqueGiftModel`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UniqueGiftModel {
    /// Name and rarity.
    pub attribute: UniqueGiftAttribute,
    /// Sticker document ID.
    pub document_id: DocumentId,
    /// Whether this model was obtained via crafting.
    pub crafted: bool,
}

/// Collectible gift pattern symbol attribute (`Data::UniqueGiftPattern`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UniqueGiftPattern {
    /// Name and rarity.
    pub attribute: UniqueGiftAttribute,
    /// Pattern sticker document ID.
    pub document_id: DocumentId,
}

/// Collectible gift gradient backdrop attribute (`Data::UniqueGiftBackdrop`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UniqueGiftBackdrop {
    /// Name and rarity.
    pub attribute: UniqueGiftAttribute,
    /// Backdrop identifier.
    pub id: i32,
    /// Radial gradient center color.
    pub center_color: RgbColor,
    /// Radial gradient edge color.
    pub edge_color: RgbColor,
    /// Overlay pattern tint color.
    pub pattern_color: RgbColor,
    /// Foreground text color.
    pub text_color: RgbColor,
}

/// Full set of possible attributes for a gift type (`Data::UniqueGiftAttributes`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UniqueGiftAttributes {
    /// Candidate models.
    pub models: Vec<UniqueGiftModel>,
    /// Candidate backdrops.
    pub backdrops: Vec<UniqueGiftBackdrop>,
    /// Candidate patterns.
    pub patterns: Vec<UniqueGiftPattern>,
}

/// Original gift inscription details (`Data::UniqueGiftOriginalDetails`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UniqueGiftOriginalDetails {
    /// Original sender peer ID (`PeerId::ZERO` if anonymous).
    pub sender_id: PeerId,
    /// Original recipient peer ID.
    pub recipient_id: PeerId,
    /// Original send Unix timestamp.
    pub date: TimeId,
    /// Original gift message text.
    pub message: String,
}

/// Market valuation summary (`Data::UniqueGiftValue`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UniqueGiftValue {
    /// Valuation currency code.
    pub currency: String,
    /// Estimated value in `currency` minor units.
    pub value_price: i64,
    /// Estimated value in USD cents.
    pub value_price_usd: i64,
    /// Initial sale price in Stars.
    pub initial_price_stars: CreditsAmount,
    /// Initial sale fiat price.
    pub initial_sale_price: i64,
    /// Initial sale timestamp.
    pub initial_sale_date: TimeId,
    /// Last sale price.
    pub last_sale_price: i64,
    /// Last sale timestamp.
    pub last_sale_date: TimeId,
    /// Average market price.
    pub average_price: i64,
    /// Minimum market listing price.
    pub minimum_price: i64,
    /// Listings count on Telegram.
    pub for_sale_on_telegram: i32,
    /// Listings count on Fragment.
    pub for_sale_on_fragment: i32,
    /// Fragment listing URL.
    pub fragment_url: String,
    /// Whether the last sale occurred on Fragment.
    pub last_sale_fragment: bool,
}

/// Collectible unique gift (`Data::UniqueGift`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UniqueGift {
    /// Collectible gift ID.
    pub id: CollectibleId,
    /// Base catalog gift ID (`gift_id`).
    pub initial_gift_id: u64,
    /// Unique URL slug (e.g. `"PlushPepe-1"`).
    pub slug: String,
    /// Gift collection title (e.g. `"Plush Pepe"`).
    pub title: String,
    /// On-chain TON NFT address, if exported.
    pub gift_address: String,
    /// On-chain TON owner wallet address, if exported.
    pub owner_address: String,
    /// Display owner name when `owner_id` is absent.
    pub owner_name: String,
    /// Owner peer ID (`PeerId::ZERO` if off-Telegram wallet).
    pub owner_id: PeerId,
    /// Host peer ID where the gift is displayed.
    pub host_id: PeerId,
    /// Channel/peer that released this gift.
    pub released_by_id: PeerId,
    /// Peer currently using this gift as chat theme.
    pub theme_peer_id: PeerId,
    /// Resale price in nanoton (`<= 0` when not listed in TON).
    pub nano_ton_for_resale: i64,
    /// Craft success chance in permille.
    pub craft_chance_permille: i32,
    /// Resale price in Stars (`<= 0` when not listed in Stars).
    pub stars_for_resale: i64,
    /// Transfer fee in Stars (`-1` if unknown, `0` if free).
    pub stars_for_transfer: i64,
    /// Minimum offer price in Stars (`-1` if offers disabled).
    pub stars_min_offer: i32,
    /// Collectible serial number (`#num`).
    pub number: i32,
    /// Only TON resale payment accepted.
    pub only_accept_ton: bool,
    /// Can be applied as a chat theme.
    pub can_be_theme: bool,
    /// Created via crafting.
    pub crafted: bool,
    /// Burned (destroyed in crafting).
    pub burned: bool,
    /// Earliest timestamp for on-chain NFT export.
    pub export_at: TimeId,
    /// Earliest timestamp for transfer.
    pub can_transfer_at: TimeId,
    /// Earliest timestamp for resale listing.
    pub can_resell_at: TimeId,
    /// Earliest timestamp for crafting.
    pub can_craft_at: TimeId,
    /// Model attribute.
    pub model: UniqueGiftModel,
    /// Pattern attribute.
    pub pattern: UniqueGiftPattern,
    /// Backdrop attribute.
    pub backdrop: UniqueGiftBackdrop,
    /// Original gift inscription details.
    pub original_details: UniqueGiftOriginalDetails,
    /// Optional valuation summary.
    pub value: Option<UniqueGiftValue>,
}

impl UniqueGift {
    /// Formatted collectible title `"<title> #<number>"` (`Data::UniqueGiftName`).
    pub fn display_name(&self) -> String {
        format!("{} #{}", self.title, self.number)
    }

    /// Resale price as Stars [`CreditsAmount`] (`UniqueGiftResaleStars`).
    pub fn resale_stars(&self) -> CreditsAmount {
        CreditsAmount::stars(self.stars_for_resale)
    }

    /// Resale price as TON [`CreditsAmount`] (`UniqueGiftResaleTon`).
    pub fn resale_ton(&self) -> CreditsAmount {
        CreditsAmount::new(
            self.nano_ton_for_resale / ONE_STAR_IN_NANO,
            self.nano_ton_for_resale % ONE_STAR_IN_NANO,
            CreditsType::Ton,
        )
    }

    /// Primary asked resale amount (TON when `only_accept_ton`, otherwise Stars)
    /// (`UniqueGiftResaleAsked`).
    pub fn resale_asked(&self) -> CreditsAmount {
        if self.only_accept_ton {
            self.resale_ton()
        } else {
            self.resale_stars()
        }
    }

    /// Returns `true` if currently listed for resale.
    pub fn is_for_resale(&self) -> bool {
        self.stars_for_resale > 0 || self.nano_ton_for_resale > 0
    }

    /// Returns `true` if transferable at timestamp `now`.
    pub fn can_transfer_now(&self, now: TimeId) -> bool {
        !self.burned && self.can_transfer_at <= now
    }

    /// Returns `true` if listable for resale at timestamp `now`.
    pub fn can_resell_now(&self, now: TimeId) -> bool {
        !self.burned && self.can_resell_at <= now
    }
}

/// Optional gift card background colors (`Data::StarGiftBackground`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StarGiftBackground {
    /// Center color.
    pub center: RgbColor,
    /// Edge color.
    pub edge: RgbColor,
    /// Text color.
    pub text: RgbColor,
}

/// Catalog or collectible Star Gift (`Data::StarGift`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StarGift {
    /// Gift ID (or collectible ID when `unique` is `Some`).
    pub id: u64,
    /// Collectible gift data if this is a unique upgraded gift.
    pub unique: Option<UniqueGift>,
    /// Optional custom card background.
    pub background: Option<StarGiftBackground>,
    /// Purchase price in Stars.
    pub stars: i64,
    /// Stars received when converting to Stars.
    pub stars_converted: i64,
    /// Stars required to upgrade to a collectible.
    pub stars_to_upgrade: i64,
    /// Minimum resale price in Stars on the marketplace.
    pub stars_resell_min: i64,
    /// Sticker document ID.
    pub sticker_document_id: DocumentId,
    /// Releasing peer ID, if any.
    pub released_by_id: PeerId,
    /// Collection title for resale marketplace.
    pub resell_title: String,
    /// Count of resale listings available.
    pub resell_count: i64,
    /// Auction slug (non-empty if sold via auction).
    pub auction_slug: String,
    /// Gifts awarded per auction round.
    pub auction_gifts_per_round: i32,
    /// Auction start timestamp.
    pub auction_start_date: TimeId,
    /// Remaining limited supply.
    pub limited_left: i32,
    /// Total limited supply (`0` for unlimited).
    pub limited_count: i32,
    /// Per-user purchase cap (`0` if unlimited).
    pub per_user_total: i32,
    /// Remaining per-user purchases.
    pub per_user_remains: i32,
    /// Number of unique upgrade variants.
    pub upgrade_variants: i32,
    /// First sale timestamp (for sold-out gifts).
    pub first_sale_date: TimeId,
    /// Last sale timestamp (for sold-out gifts).
    pub last_sale_date: TimeId,
    /// Locked until timestamp.
    pub locked_until_date: TimeId,
    /// Resale accepts TON only.
    pub resell_ton_only: bool,
    /// Requires Telegram Premium to purchase.
    pub require_premium: bool,
    /// Peer color collectible available.
    pub peer_color_available: bool,
    /// Can be upgraded to unique collectible.
    pub upgradable: bool,
    /// Birthday gift tag.
    pub birthday: bool,
    /// Limited supply exhausted.
    pub sold_out: bool,
}

impl StarGift {
    /// Returns `true` if this gift is distributed via auction (`StarGift::auction`).
    pub fn is_auction(&self) -> bool {
        !self.auction_slug.is_empty()
    }

    /// Returns `true` if this gift is a unique upgraded collectible.
    pub fn is_unique(&self) -> bool {
        self.unique.is_some()
    }
}

/// High-level lifecycle state of a saved Star Gift (upgrade / transfer / resale).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StarGiftLifecycleState {
    /// Standard non-upgradable gift (can be saved/hidden or converted to Stars).
    Regular,
    /// Standard gift that can be upgraded to a unique collectible.
    Upgradable,
    /// Upgraded unique collectible owned by the peer (not listed for resale).
    UniqueOwned,
    /// Unique collectible actively listed on the resale marketplace.
    ListedForResale,
    /// Unique collectible exported to an on-chain TON wallet.
    ExportedOnChain,
    /// Unique collectible burned (e.g., consumed in crafting).
    Burned,
    /// Gift was refunded.
    Refunded,
}

/// Identifier of a saved Star Gift (`Data::SavedStarGiftId`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SavedStarGiftId {
    /// Saved on a user's profile (addressed by message ID).
    User(MsgId),
    /// Saved on a channel/chat profile (addressed by `(peer_id, saved_id)`).
    Chat {
        /// Channel or chat peer ID.
        peer_id: PeerId,
        /// Server `saved_id`.
        saved_id: u64,
    },
    /// Addressed directly by unique gift slug.
    Slug,
}

/// Saved Star Gift on a user or channel profile (`Data::SavedStarGift`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SavedStarGift {
    /// Gift metadata.
    pub info: StarGift,
    /// Identifier for manage/upgrade/transfer/resale RPCs.
    pub manage_id: Option<SavedStarGiftId>,
    /// Collection IDs containing this gift.
    pub collection_ids: Vec<i32>,
    /// Optional gift caption/message text.
    pub message: String,
    /// Stars obtainable by converting this gift.
    pub stars_converted: i64,
    /// Pre-paid upgrade Stars paid by sender.
    pub stars_upgraded_by_sender: i64,
    /// Stars required to strip original inscription details.
    pub stars_for_details_remove: i64,
    /// Hash for prepaid upgrade link.
    pub gift_prepay_upgrade_hash: String,
    /// Sender peer ID (`PeerId::ZERO` if anonymous).
    pub from_id: PeerId,
    /// Received timestamp.
    pub date: TimeId,
    /// Gift edition number.
    pub gift_num: i32,
    /// Upgrade must be paid separately.
    pub upgrade_separate: bool,
    /// Can currently be upgraded.
    pub upgradable: bool,
    /// Sender name hidden.
    pub anonymous: bool,
    /// Pinned to top of profile.
    pub pinned: bool,
    /// Hidden from public profile (`unsaved`).
    pub hidden: bool,
    /// Owned by the current user.
    pub mine: bool,
    /// Refunded gift.
    pub refunded: bool,
}

impl SavedStarGift {
    /// Computes the current [`StarGiftLifecycleState`] for this saved gift.
    pub fn lifecycle_state(&self) -> StarGiftLifecycleState {
        if self.refunded {
            return StarGiftLifecycleState::Refunded;
        }
        if let Some(ref unique) = self.info.unique {
            if unique.burned {
                StarGiftLifecycleState::Burned
            } else if !unique.gift_address.is_empty() {
                StarGiftLifecycleState::ExportedOnChain
            } else if unique.is_for_resale() {
                StarGiftLifecycleState::ListedForResale
            } else {
                StarGiftLifecycleState::UniqueOwned
            }
        } else if self.upgradable || self.info.upgradable {
            StarGiftLifecycleState::Upgradable
        } else {
            StarGiftLifecycleState::Regular
        }
    }
}

/// Star gift collection folder (`Data::GiftCollection`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GiftCollection {
    /// Collection ID.
    pub id: i32,
    /// Number of gifts in collection.
    pub count: i32,
    /// Collection display title.
    pub title: String,
    /// Optional icon sticker document ID.
    pub icon_document_id: Option<DocumentId>,
    /// Server hash for caching.
    pub hash: i64,
}

/// Resale marketplace sort order (`Data::ResaleGiftsSort`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ResaleGiftsSort {
    /// Sort by listing date (newest first).
    #[default]
    Date,
    /// Sort by lowest price.
    Price,
    /// Sort by lowest serial number.
    Number,
}

/// Attribute filter identifier category (`Data::GiftAttributeIdType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GiftAttributeIdType {
    /// Model sticker document ID.
    Model,
    /// Pattern sticker document ID.
    Pattern,
    /// Backdrop integer ID.
    Backdrop,
}

/// Unique identifier for filtering by a collectible attribute (`Data::GiftAttributeId`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GiftAttributeId {
    /// Raw document ID or backdrop ID.
    pub value: u64,
    /// Attribute category.
    pub kind: GiftAttributeIdType,
}

impl GiftAttributeId {
    /// Identifier for a [`UniqueGiftBackdrop`].
    pub const fn for_backdrop(backdrop: &UniqueGiftBackdrop) -> Self {
        Self {
            value: (backdrop.id as u32) as u64,
            kind: GiftAttributeIdType::Backdrop,
        }
    }

    /// Identifier for a [`UniqueGiftModel`].
    pub const fn for_model(model: &UniqueGiftModel) -> Self {
        Self {
            value: model.document_id.raw(),
            kind: GiftAttributeIdType::Model,
        }
    }

    /// Identifier for a [`UniqueGiftPattern`].
    pub const fn for_pattern(pattern: &UniqueGiftPattern) -> Self {
        Self {
            value: pattern.document_id.raw(),
            kind: GiftAttributeIdType::Pattern,
        }
    }

    /// Converts to a TL [`tl_enums::StarGiftAttributeId`].
    pub const fn to_tl(self) -> tl_enums::StarGiftAttributeId {
        match self.kind {
            GiftAttributeIdType::Backdrop => {
                tl_enums::StarGiftAttributeId::Backdrop(tl_types::StarGiftAttributeIdBackdrop {
                    backdrop_id: (self.value as u32) as i32,
                })
            }
            GiftAttributeIdType::Model => {
                tl_enums::StarGiftAttributeId::Model(tl_types::StarGiftAttributeIdModel {
                    document_id: self.value as i64,
                })
            }
            GiftAttributeIdType::Pattern => {
                tl_enums::StarGiftAttributeId::Pattern(tl_types::StarGiftAttributeIdPattern {
                    document_id: self.value as i64,
                })
            }
        }
    }

    /// Converts from a TL [`tl_enums::StarGiftAttributeId`].
    pub const fn from_tl(tl: &tl_enums::StarGiftAttributeId) -> Self {
        match tl {
            tl_enums::StarGiftAttributeId::Backdrop(b) => Self {
                value: (b.backdrop_id as u32) as u64,
                kind: GiftAttributeIdType::Backdrop,
            },
            tl_enums::StarGiftAttributeId::Model(m) => Self {
                value: m.document_id as u64,
                kind: GiftAttributeIdType::Model,
            },
            tl_enums::StarGiftAttributeId::Pattern(p) => Self {
                value: p.document_id as u64,
                kind: GiftAttributeIdType::Pattern,
            },
        }
    }
}

/// Filter parameters for `payments.getResaleStarGifts` (`Data::ResaleGiftsFilter`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResaleGiftsFilter {
    /// Cached attributes hash.
    pub attributes_hash: i64,
    /// Selected attribute filters.
    pub attributes: BTreeSet<GiftAttributeId>,
    /// Sort mode.
    pub sort: ResaleGiftsSort,
    /// Filter gifts eligible for crafting.
    pub for_craft: bool,
    /// Exclude TON-only listings.
    pub stars_only: bool,
}

impl Default for ResaleGiftsFilter {
    fn default() -> Self {
        Self {
            attributes_hash: 0,
            attributes: BTreeSet::new(),
            sort: ResaleGiftsSort::Price,
            for_craft: false,
            stars_only: false,
        }
    }
}

/// Resale listing slice returned by `payments.getResaleStarGifts` (`Data::ResaleGiftsDescriptor`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ResaleGiftsDescriptor {
    /// Base catalog gift ID.
    pub gift_id: u64,
    /// Next pagination offset.
    pub offset: String,
    /// Listed unique gifts in this page.
    pub list: Vec<StarGift>,
    /// Available model attributes with listing counts.
    pub models: Vec<(UniqueGiftModel, i32)>,
    /// Available backdrop attributes with listing counts.
    pub backdrops: Vec<(UniqueGiftBackdrop, i32)>,
    /// Available pattern attributes with listing counts.
    pub patterns: Vec<(UniqueGiftPattern, i32)>,
    /// Returned attributes hash.
    pub attributes_hash: i64,
    /// Total matching listings count.
    pub count: i32,
}

/// Paginated list of saved Star Gifts (`Data::MyGiftsDescriptor`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SavedStarGiftsSlice {
    /// Gifts in this page.
    pub list: Vec<SavedStarGift>,
    /// Next pagination offset (`""` if last page).
    pub offset: String,
    /// Total count reported by server.
    pub count: i32,
    /// Whether channel Star Gift notifications are enabled (`chat_notifications_enabled`,
    /// `info/peer_gifts/info_peer_gifts_widget.cpp:728-730`).
    pub chat_notifications_enabled: Option<bool>,
}

/// Owned unique gift query mode (`Data::MyUniqueType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MyUniqueType {
    /// Include both owned and hosted unique gifts.
    OwnedAndHosted,
    /// Exclude hosted gifts (`exclude_hosted = true`).
    OnlyOwned,
}

// ============================================================================
// TL Conversions & Request Builders
// ============================================================================

fn find_stars_for_resale(list: Option<&[tl_enums::StarsAmount]>) -> i64 {
    list.unwrap_or(&[])
        .iter()
        .find_map(|a| match a {
            tl_enums::StarsAmount::StarsAmount(s) => Some(s.amount),
            _ => None,
        })
        .unwrap_or(0)
}

fn find_ton_for_resale(list: Option<&[tl_enums::StarsAmount]>) -> i64 {
    list.unwrap_or(&[])
        .iter()
        .find_map(|a| match a {
            tl_enums::StarsAmount::StarsTonAmount(t) => Some(t.amount),
            _ => None,
        })
        .unwrap_or(0)
}

/// Converts a TL `StarGiftAttributeModel` into [`UniqueGiftModel`].
pub fn unique_gift_model_from_tl(tl: &tl_types::StarGiftAttributeModel) -> UniqueGiftModel {
    UniqueGiftModel {
        attribute: UniqueGiftAttribute {
            name: tl.name.clone(),
            rarity_value: rarity_value_from_tl(&tl.rarity),
        },
        document_id: document_id_from_tl(&tl.document),
        crafted: tl.crafted,
    }
}

/// Converts a TL `StarGiftAttributePattern` into [`UniqueGiftPattern`].
pub fn unique_gift_pattern_from_tl(tl: &tl_types::StarGiftAttributePattern) -> UniqueGiftPattern {
    UniqueGiftPattern {
        attribute: UniqueGiftAttribute {
            name: tl.name.clone(),
            rarity_value: rarity_value_from_tl(&tl.rarity),
        },
        document_id: document_id_from_tl(&tl.document),
    }
}

/// Converts a TL `StarGiftAttributeBackdrop` into [`UniqueGiftBackdrop`].
pub fn unique_gift_backdrop_from_tl(
    tl: &tl_types::StarGiftAttributeBackdrop,
) -> UniqueGiftBackdrop {
    UniqueGiftBackdrop {
        attribute: UniqueGiftAttribute {
            name: tl.name.clone(),
            rarity_value: rarity_value_from_tl(&tl.rarity),
        },
        id: tl.backdrop_id,
        center_color: RgbColor::from_serialized(tl.center_color),
        edge_color: RgbColor::from_serialized(tl.edge_color),
        pattern_color: RgbColor::from_serialized(tl.pattern_color),
        text_color: RgbColor::from_serialized(tl.text_color),
    }
}

/// Converts a TL `StarGiftAttributeOriginalDetails` into [`UniqueGiftOriginalDetails`].
pub fn unique_gift_original_details_from_tl(
    tl: &tl_types::StarGiftAttributeOriginalDetails,
) -> UniqueGiftOriginalDetails {
    let message = tl
        .message
        .as_ref()
        .map(|tl_enums::TextWithEntities::TextWithEntities(t)| t.text.clone())
        .unwrap_or_default();
    UniqueGiftOriginalDetails {
        sender_id: tl
            .sender_id
            .as_ref()
            .map(peer_id_from_tl)
            .unwrap_or(PeerId::ZERO),
        recipient_id: peer_id_from_tl(&tl.recipient_id),
        date: tl.date,
        message,
    }
}

/// Converts a TL [`tl_enums::StarGift`] into [`StarGift`] (`Api::FromTL` in `api_premium.cpp`).
pub fn star_gift_from_tl(tl: &tl_enums::StarGift) -> StarGift {
    match tl {
        tl_enums::StarGift::StarGift(g) => {
            let background =
                g.background
                    .as_ref()
                    .map(|tl_enums::StarGiftBackground::StarGiftBackground(b)| {
                        StarGiftBackground {
                            center: RgbColor::from_serialized(b.center_color),
                            edge: RgbColor::from_serialized(b.edge_color),
                            text: RgbColor::from_serialized(b.text_color),
                        }
                    });
            StarGift {
                id: g.id as u64,
                unique: None,
                background,
                stars: g.stars,
                stars_converted: g.convert_stars,
                stars_to_upgrade: g.upgrade_stars.unwrap_or(0),
                stars_resell_min: g.resell_min_stars.unwrap_or(0),
                sticker_document_id: document_id_from_tl(&g.sticker),
                released_by_id: g
                    .released_by
                    .as_ref()
                    .map(peer_id_from_tl)
                    .unwrap_or(PeerId::ZERO),
                resell_title: g.title.clone().unwrap_or_default(),
                resell_count: g.availability_resale.unwrap_or(0),
                auction_slug: g.auction_slug.clone().unwrap_or_default(),
                auction_gifts_per_round: g.gifts_per_round.unwrap_or(0),
                auction_start_date: g.auction_start_date.unwrap_or(0),
                limited_left: g.availability_remains.unwrap_or(0),
                limited_count: g.availability_total.unwrap_or(0),
                per_user_total: g.per_user_total.unwrap_or(0),
                per_user_remains: g.per_user_remains.unwrap_or(0),
                upgrade_variants: g.upgrade_variants.unwrap_or(0),
                first_sale_date: g.first_sale_date.unwrap_or(0),
                last_sale_date: g.last_sale_date.unwrap_or(0),
                locked_until_date: g.locked_until_date.unwrap_or(0),
                resell_ton_only: false,
                require_premium: g.require_premium,
                peer_color_available: g.peer_color_available,
                upgradable: g.upgrade_stars.is_some(),
                birthday: g.birthday,
                sold_out: g.sold_out,
            }
        }
        tl_enums::StarGift::Unique(u) => {
            let mut model = UniqueGiftModel::default();
            let mut pattern = UniqueGiftPattern::default();
            let mut backdrop = UniqueGiftBackdrop::default();
            let mut original_details = UniqueGiftOriginalDetails::default();
            for attr in &u.attributes {
                match attr {
                    tl_enums::StarGiftAttribute::Model(m) => model = unique_gift_model_from_tl(m),
                    tl_enums::StarGiftAttribute::Pattern(p) => {
                        pattern = unique_gift_pattern_from_tl(p)
                    }
                    tl_enums::StarGiftAttribute::Backdrop(b) => {
                        backdrop = unique_gift_backdrop_from_tl(b)
                    }
                    tl_enums::StarGiftAttribute::OriginalDetails(o) => {
                        original_details = unique_gift_original_details_from_tl(o)
                    }
                }
            }
            let released_by_id = u
                .released_by
                .as_ref()
                .map(peer_id_from_tl)
                .unwrap_or(PeerId::ZERO);
            let value = u.value_amount.map(|amount| UniqueGiftValue {
                currency: u.value_currency.clone().unwrap_or_default(),
                value_price: amount,
                value_price_usd: u.value_usd_amount.unwrap_or(0),
                ..UniqueGiftValue::default()
            });
            let sticker_document_id = model.document_id;
            let unique = UniqueGift {
                id: CollectibleId::from_tl(u.id),
                initial_gift_id: u.gift_id as u64,
                slug: u.slug.clone(),
                title: u.title.clone(),
                gift_address: u.gift_address.clone().unwrap_or_default(),
                owner_address: u.owner_address.clone().unwrap_or_default(),
                owner_name: u.owner_name.clone().unwrap_or_default(),
                owner_id: u
                    .owner_id
                    .as_ref()
                    .map(peer_id_from_tl)
                    .unwrap_or(PeerId::ZERO),
                host_id: u
                    .host_id
                    .as_ref()
                    .map(peer_id_from_tl)
                    .unwrap_or(PeerId::ZERO),
                released_by_id,
                theme_peer_id: u
                    .theme_peer
                    .as_ref()
                    .map(peer_id_from_tl)
                    .unwrap_or(PeerId::ZERO),
                nano_ton_for_resale: find_ton_for_resale(u.resell_amount.as_deref()),
                craft_chance_permille: u.craft_chance_permille.unwrap_or(0),
                stars_for_resale: find_stars_for_resale(u.resell_amount.as_deref()),
                stars_for_transfer: -1,
                stars_min_offer: u.offer_min_stars.unwrap_or(-1),
                number: u.num,
                only_accept_ton: u.resale_ton_only,
                can_be_theme: u.theme_available,
                crafted: u.crafted,
                burned: u.burned,
                export_at: 0,
                can_transfer_at: 0,
                can_resell_at: 0,
                can_craft_at: 0,
                model,
                pattern,
                backdrop,
                original_details,
                value,
            };
            StarGift {
                id: u.id as u64,
                unique: Some(unique),
                sticker_document_id,
                released_by_id,
                limited_left: u.availability_total.saturating_sub(u.availability_issued),
                limited_count: u.availability_total,
                resell_ton_only: u.resale_ton_only,
                require_premium: u.require_premium,
                ..StarGift::default()
            }
        }
    }
}

/// Converts a TL [`tl_enums::SavedStarGift`] into [`SavedStarGift`] (`Api::FromTL`).
pub fn saved_star_gift_from_tl(
    tl: &tl_enums::SavedStarGift,
    owner_peer_id: PeerId,
    is_self: bool,
) -> SavedStarGift {
    let tl_enums::SavedStarGift::SavedStarGift(g) = tl;
    let mut info = star_gift_from_tl(&g.gift);
    if let Some(ref mut unique) = info.unique {
        unique.stars_for_transfer = g.transfer_stars.unwrap_or(-1);
        unique.export_at = g.can_export_at.unwrap_or(0);
        unique.can_transfer_at = g.can_transfer_at.unwrap_or(0);
        unique.can_resell_at = g.can_resell_at.unwrap_or(0);
        unique.can_craft_at = g.can_craft_at.unwrap_or(0);
    }
    let has_unique = info.unique.is_some();
    let manage_id = if owner_peer_id.is_user() {
        g.msg_id.map(|id| SavedStarGiftId::User(MsgId::from(id)))
    } else {
        g.saved_id.map(|saved_id| SavedStarGiftId::Chat {
            peer_id: owner_peer_id,
            saved_id: saved_id as u64,
        })
    };
    let message = g
        .message
        .as_ref()
        .map(|tl_enums::TextWithEntities::TextWithEntities(t)| t.text.clone())
        .unwrap_or_default();

    SavedStarGift {
        info,
        manage_id,
        collection_ids: g.collection_id.clone().unwrap_or_default(),
        message,
        stars_converted: g.convert_stars.unwrap_or(0),
        stars_upgraded_by_sender: g.upgrade_stars.unwrap_or(0),
        stars_for_details_remove: g.drop_original_details_stars.unwrap_or(0),
        gift_prepay_upgrade_hash: g.prepaid_upgrade_hash.clone().unwrap_or_default(),
        from_id: g
            .from_id
            .as_ref()
            .map(peer_id_from_tl)
            .unwrap_or(PeerId::ZERO),
        date: g.date,
        gift_num: g.gift_num.unwrap_or(0),
        upgrade_separate: g.upgrade_separate,
        upgradable: g.can_upgrade,
        anonymous: g.name_hidden,
        pinned: g.pinned_to_top && has_unique,
        hidden: g.unsaved,
        mine: is_self,
        refunded: g.refunded,
    }
}

/// Converts a TL `payments.StarGifts` response into `Option<(hash, Vec<StarGift>)>`.
///
/// Returns `None` for `payments.starGiftsNotModified`.
pub fn star_gifts_from_tl(tl: &tl_enums::payments::StarGifts) -> Option<(i32, Vec<StarGift>)> {
    match tl {
        tl_enums::payments::StarGifts::NotModified => None,
        tl_enums::payments::StarGifts::StarGifts(g) => {
            Some((g.hash, g.gifts.iter().map(star_gift_from_tl).collect()))
        }
    }
}

/// Converts a TL `payments.SavedStarGifts` response into [`SavedStarGiftsSlice`].
pub fn saved_star_gifts_from_tl(
    tl: &tl_enums::payments::SavedStarGifts,
    owner_peer_id: PeerId,
    is_self: bool,
) -> SavedStarGiftsSlice {
    let tl_enums::payments::SavedStarGifts::SavedStarGifts(s) = tl;
    SavedStarGiftsSlice {
        list: s
            .gifts
            .iter()
            .map(|g| saved_star_gift_from_tl(g, owner_peer_id, is_self))
            .collect(),
        offset: s.next_offset.clone().unwrap_or_default(),
        count: s.count,
        // Port of `info/peer_gifts/info_peer_gifts_widget.cpp:728-730`: preserve
        // `chat_notifications_enabled` so channel gift notification toggle reflects server state.
        chat_notifications_enabled: s.chat_notifications_enabled,
    }
}

/// Converts a TL `payments.ResaleStarGifts` response into [`ResaleGiftsDescriptor`].
pub fn resale_star_gifts_from_tl(
    gift_id: u64,
    tl: &tl_enums::payments::ResaleStarGifts,
) -> ResaleGiftsDescriptor {
    let tl_enums::payments::ResaleStarGifts::ResaleStarGifts(data) = tl;
    let mut counts: BTreeMap<GiftAttributeId, i32> = BTreeMap::new();
    for c in data.counters.as_deref().unwrap_or(&[]) {
        let tl_enums::StarGiftAttributeCounter::StarGiftAttributeCounter(counter) = c;
        counts.insert(GiftAttributeId::from_tl(&counter.attribute), counter.count);
    }
    let mut models = Vec::new();
    let mut patterns = Vec::new();
    let mut backdrops = Vec::new();
    for attr in data.attributes.as_deref().unwrap_or(&[]) {
        match attr {
            tl_enums::StarGiftAttribute::Model(m) => {
                let parsed = unique_gift_model_from_tl(m);
                let count = counts
                    .get(&GiftAttributeId::for_model(&parsed))
                    .copied()
                    .unwrap_or(0);
                models.push((parsed, count));
            }
            tl_enums::StarGiftAttribute::Pattern(p) => {
                let parsed = unique_gift_pattern_from_tl(p);
                let count = counts
                    .get(&GiftAttributeId::for_pattern(&parsed))
                    .copied()
                    .unwrap_or(0);
                patterns.push((parsed, count));
            }
            tl_enums::StarGiftAttribute::Backdrop(b) => {
                let parsed = unique_gift_backdrop_from_tl(b);
                let count = counts
                    .get(&GiftAttributeId::for_backdrop(&parsed))
                    .copied()
                    .unwrap_or(0);
                backdrops.push((parsed, count));
            }
            tl_enums::StarGiftAttribute::OriginalDetails(_) => {}
        }
    }
    ResaleGiftsDescriptor {
        gift_id,
        offset: data.next_offset.clone().unwrap_or_default(),
        list: data.gifts.iter().map(star_gift_from_tl).collect(),
        models,
        backdrops,
        patterns,
        attributes_hash: data.attributes_hash.unwrap_or(0),
        count: data.count,
    }
}

/// Builds `payments.getStarGifts`.
pub fn build_get_star_gifts(hash: i32) -> tl_fn::payments::GetStarGifts {
    tl_fn::payments::GetStarGifts { hash }
}

/// Builds `payments.getSavedStarGifts` for owned unique gifts (`PeerUniqueGiftsSlice`).
pub fn build_get_unique_saved_star_gifts(
    peer: tl_enums::InputPeer,
    mode: MyUniqueType,
    offset: impl Into<String>,
) -> tl_fn::payments::GetSavedStarGifts {
    tl_fn::payments::GetSavedStarGifts {
        exclude_unsaved: false,
        exclude_saved: false,
        exclude_unlimited: true,
        exclude_unique: false,
        sort_by_value: false,
        exclude_upgradable: true,
        exclude_unupgradable: true,
        peer_color_available: false,
        exclude_hosted: matches!(mode, MyUniqueType::OnlyOwned),
        peer,
        collection_id: None,
        offset: offset.into(),
        limit: GIFTS_PER_PAGE,
    }
}

/// Builds `payments.getResaleStarGifts` (`ResaleGiftsSlice`).
pub fn build_get_resale_star_gifts(
    gift_id: u64,
    filter: &ResaleGiftsFilter,
    offset: impl Into<String>,
) -> tl_fn::payments::GetResaleStarGifts {
    let attributes = if filter.attributes.is_empty() {
        None
    } else {
        Some(filter.attributes.iter().map(|id| id.to_tl()).collect())
    };
    tl_fn::payments::GetResaleStarGifts {
        sort_by_price: filter.sort == ResaleGiftsSort::Price,
        sort_by_num: filter.sort == ResaleGiftsSort::Number,
        for_craft: filter.for_craft,
        stars_only: filter.stars_only,
        attributes_hash: Some(filter.attributes_hash),
        gift_id: gift_id as i64,
        attributes,
        offset: offset.into(),
        limit: GIFTS_PER_PAGE,
    }
}

/// Builds `payments.upgradeStarGift`.
pub fn build_upgrade_star_gift(
    stargift: tl_enums::InputSavedStarGift,
    keep_original_details: bool,
) -> tl_fn::payments::UpgradeStarGift {
    tl_fn::payments::UpgradeStarGift {
        keep_original_details,
        stargift,
    }
}

/// Builds `payments.transferStarGift`.
pub fn build_transfer_star_gift(
    stargift: tl_enums::InputSavedStarGift,
    to_id: tl_enums::InputPeer,
) -> tl_fn::payments::TransferStarGift {
    tl_fn::payments::TransferStarGift { stargift, to_id }
}

/// Builds `payments.updateStarGiftPrice` (pass zero `StarsAmount` to unlist from resale).
pub fn build_update_star_gift_price(
    stargift: tl_enums::InputSavedStarGift,
    resell_amount: CreditsAmount,
) -> tl_fn::payments::UpdateStarGiftPrice {
    tl_fn::payments::UpdateStarGiftPrice {
        stargift,
        resell_amount: resell_amount.to_tl(),
    }
}

/// Builds `payments.saveStarGift`.
pub fn build_save_star_gift(
    stargift: tl_enums::InputSavedStarGift,
    unsave: bool,
) -> tl_fn::payments::SaveStarGift {
    tl_fn::payments::SaveStarGift { unsave, stargift }
}

/// Builds `payments.convertStarGift`.
pub fn build_convert_star_gift(
    stargift: tl_enums::InputSavedStarGift,
) -> tl_fn::payments::ConvertStarGift {
    tl_fn::payments::ConvertStarGift { stargift }
}

// ============================================================================
// Per-Peer Gifts Filter, Collections, Wear/Unwear, Unique Detail & Offers
// ============================================================================

/// Maximum character count for a Star Gift collection title (`kCollectionNameLimit` in
/// `info/peer_gifts/info_peer_gifts_collections.cpp`).
pub const COLLECTION_NAME_LIMIT: usize = 12;

/// Filter & sort parameters for a peer's saved Star Gifts list (`Info::PeerGifts::Filter`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PeerGiftsFilter {
    /// Sort gifts by Stars value instead of received date (`sort_by_value`).
    pub sort_by_value: bool,
    /// Exclude unlimited catalog gifts (`exclude_unlimited`).
    pub skip_unlimited: bool,
    /// Exclude limited non-unique gifts (`exclude_upgradable` + `exclude_unupgradable` when paired).
    pub skip_limited: bool,
    /// Exclude upgradable regular gifts (`exclude_upgradable`).
    pub skip_upgradable: bool,
    /// Exclude unique upgraded collectibles (`exclude_unique`).
    pub skip_unique: bool,
    /// Exclude gifts visible on profile (`exclude_saved`).
    pub skip_saved: bool,
    /// Exclude gifts hidden from profile (`exclude_unsaved`).
    pub skip_unsaved: bool,
    /// Filter to a specific collection ID (`None` = all gifts).
    pub collection_id: Option<i32>,
}

impl PeerGiftsFilter {
    /// Returns `true` if any exclusion filter is active (`Filter::skipsSomething`).
    pub const fn skips_something(&self) -> bool {
        self.skip_limited
            || self.skip_unlimited
            || self.skip_saved
            || self.skip_unsaved
            || self.skip_upgradable
            || self.skip_unique
    }

    /// Evaluates whether `gift` matches this filter locally (useful for demo/mock filtering).
    pub fn matches(&self, gift: &SavedStarGift) -> bool {
        if let Some(cid) = self.collection_id
            && cid != 0
            && !gift.collection_ids.contains(&cid)
        {
            return false;
        }
        if self.skip_saved && !gift.hidden {
            return false;
        }
        if self.skip_unsaved && gift.hidden {
            return false;
        }
        let is_unique = gift.info.unique.is_some();
        let is_limited = !is_unique && gift.info.limited_count > 0;
        let is_unlimited = !is_unique && gift.info.limited_count == 0;
        let is_upgradable = !is_unique && (gift.upgradable || gift.info.upgradable);
        if self.skip_unique && is_unique {
            return false;
        }
        if self.skip_limited && is_limited {
            return false;
        }
        if self.skip_unlimited && is_unlimited {
            return false;
        }
        if self.skip_upgradable && is_upgradable {
            return false;
        }
        true
    }
}

/// Scheduled upgrade price tier (`StarGiftUpgradePrice`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StarGiftUpgradePrice {
    /// Effective Unix timestamp.
    pub date: TimeId,
    /// Upgrade cost in Stars.
    pub upgrade_stars: i64,
}

/// Upgrade preview returned by `payments.getStarGiftUpgradePreview`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StarGiftUpgradePreview {
    /// Sample attributes that may be rolled on upgrade.
    pub sample_attributes: UniqueGiftAttributes,
    /// Current upgrade price schedule.
    pub prices: Vec<StarGiftUpgradePrice>,
    /// Upcoming upgrade price schedule.
    pub next_prices: Vec<StarGiftUpgradePrice>,
}

/// Validates a Star Gift collection title (`info_peer_gifts_collections.cpp:67-71`).
pub fn validate_collection_title(title: &str) -> Result<String, &'static str> {
    let trimmed = title.trim();
    if trimmed.is_empty() {
        return Err("Collection title cannot be empty.");
    }
    if trimmed.chars().count() > COLLECTION_NAME_LIMIT {
        return Err("Collection title exceeds 12 characters.");
    }
    Ok(trimmed.to_owned())
}

/// Converts a TL `StarGiftCollection` into [`GiftCollection`].
pub fn gift_collection_from_tl(tl: &tl_enums::StarGiftCollection) -> GiftCollection {
    let tl_enums::StarGiftCollection::StarGiftCollection(c) = tl;
    GiftCollection {
        id: c.collection_id,
        count: c.gifts_count,
        title: c.title.clone(),
        icon_document_id: c.icon.as_ref().map(document_id_from_tl),
        hash: c.hash,
    }
}

/// Converts a TL `payments.StarGiftCollections` into `Option<Vec<GiftCollection>>`.
///
/// Returns `None` for `payments.starGiftCollectionsNotModified`.
pub fn gift_collections_from_tl(
    tl: &tl_enums::payments::StarGiftCollections,
) -> Option<Vec<GiftCollection>> {
    match tl {
        tl_enums::payments::StarGiftCollections::NotModified => None,
        tl_enums::payments::StarGiftCollections::StarGiftCollections(d) => {
            Some(d.collections.iter().map(gift_collection_from_tl).collect())
        }
    }
}

/// Converts a TL `payments.UniqueStarGift` into [`StarGift`].
pub fn unique_star_gift_from_tl(tl: &tl_enums::payments::UniqueStarGift) -> StarGift {
    let tl_enums::payments::UniqueStarGift::UniqueStarGift(d) = tl;
    star_gift_from_tl(&d.gift)
}

/// Converts a TL `payments.UniqueStarGiftValueInfo` into [`UniqueGiftValue`].
pub fn unique_gift_value_from_tl(
    tl: &tl_enums::payments::UniqueStarGiftValueInfo,
) -> UniqueGiftValue {
    let tl_enums::payments::UniqueStarGiftValueInfo::UniqueStarGiftValueInfo(v) = tl;
    UniqueGiftValue {
        currency: v.currency.clone(),
        value_price: v.value,
        value_price_usd: 0,
        initial_price_stars: CreditsAmount::stars(v.initial_sale_stars),
        initial_sale_price: v.initial_sale_price,
        initial_sale_date: v.initial_sale_date,
        last_sale_price: v.last_sale_price.unwrap_or(0),
        last_sale_date: v.last_sale_date.unwrap_or(0),
        average_price: v.average_price.unwrap_or(0),
        minimum_price: v.floor_price.unwrap_or(0),
        for_sale_on_telegram: v.listed_count.unwrap_or(0),
        for_sale_on_fragment: v.fragment_listed_count.unwrap_or(0),
        fragment_url: v.fragment_listed_url.clone().unwrap_or_default(),
        last_sale_fragment: v.last_sale_on_fragment,
    }
}

/// Converts a TL `payments.StarGiftUpgradePreview` into [`StarGiftUpgradePreview`].
pub fn star_gift_upgrade_preview_from_tl(
    tl: &tl_enums::payments::StarGiftUpgradePreview,
) -> StarGiftUpgradePreview {
    let tl_enums::payments::StarGiftUpgradePreview::StarGiftUpgradePreview(d) = tl;
    let sample_attributes = unique_gift_attributes_from_slice(&d.sample_attributes);
    let prices = d
        .prices
        .iter()
        .map(
            |tl_enums::StarGiftUpgradePrice::StarGiftUpgradePrice(p)| StarGiftUpgradePrice {
                date: p.date,
                upgrade_stars: p.upgrade_stars,
            },
        )
        .collect();
    let next_prices = d
        .next_prices
        .iter()
        .map(
            |tl_enums::StarGiftUpgradePrice::StarGiftUpgradePrice(p)| StarGiftUpgradePrice {
                date: p.date,
                upgrade_stars: p.upgrade_stars,
            },
        )
        .collect();
    StarGiftUpgradePreview {
        sample_attributes,
        prices,
        next_prices,
    }
}

/// Converts a TL `payments.StarGiftUpgradeAttributes` into [`UniqueGiftAttributes`].
pub fn star_gift_upgrade_attributes_from_tl(
    tl: &tl_enums::payments::StarGiftUpgradeAttributes,
) -> UniqueGiftAttributes {
    let tl_enums::payments::StarGiftUpgradeAttributes::StarGiftUpgradeAttributes(d) = tl;
    unique_gift_attributes_from_slice(&d.attributes)
}

fn unique_gift_attributes_from_slice(
    attrs: &[tl_enums::StarGiftAttribute],
) -> UniqueGiftAttributes {
    let mut out = UniqueGiftAttributes::default();
    for attr in attrs {
        match attr {
            tl_enums::StarGiftAttribute::Model(m) => out.models.push(unique_gift_model_from_tl(m)),
            tl_enums::StarGiftAttribute::Pattern(p) => {
                out.patterns.push(unique_gift_pattern_from_tl(p))
            }
            tl_enums::StarGiftAttribute::Backdrop(b) => {
                out.backdrops.push(unique_gift_backdrop_from_tl(b))
            }
            tl_enums::StarGiftAttribute::OriginalDetails(_) => {}
        }
    }
    out
}

/// Builds `payments.getSavedStarGifts` using a [`PeerGiftsFilter`].
pub fn build_get_saved_star_gifts_filtered(
    peer: tl_enums::InputPeer,
    filter: &PeerGiftsFilter,
    offset: impl Into<String>,
    limit: i32,
) -> tl_fn::payments::GetSavedStarGifts {
    // Port of `info/peer_gifts/info_peer_gifts_widget.cpp:687-695`:
    // `skip_limited` maps ONLY to `f_exclude_unupgradable`, and `skip_upgradable` maps
    // independently to `f_exclude_upgradable`.
    tl_fn::payments::GetSavedStarGifts {
        exclude_unsaved: filter.skip_unsaved,
        exclude_saved: filter.skip_saved,
        exclude_unlimited: filter.skip_unlimited,
        exclude_unique: filter.skip_unique,
        sort_by_value: filter.sort_by_value,
        exclude_upgradable: filter.skip_upgradable,
        exclude_unupgradable: filter.skip_limited,
        peer_color_available: false,
        exclude_hosted: false,
        peer,
        collection_id: filter.collection_id.filter(|&id| id != 0),
        offset: offset.into(),
        limit: if limit > 0 { limit } else { GIFTS_PER_PAGE },
    }
}

/// Builds `payments.getUniqueStarGift`.
pub fn build_get_unique_star_gift(slug: impl Into<String>) -> tl_fn::payments::GetUniqueStarGift {
    tl_fn::payments::GetUniqueStarGift { slug: slug.into() }
}

/// Builds `payments.getUniqueStarGiftValueInfo`.
pub fn build_get_unique_star_gift_value_info(
    slug: impl Into<String>,
) -> tl_fn::payments::GetUniqueStarGiftValueInfo {
    tl_fn::payments::GetUniqueStarGiftValueInfo { slug: slug.into() }
}

/// Builds `payments.getStarGiftUpgradePreview`.
pub fn build_get_star_gift_upgrade_preview(
    gift_id: u64,
) -> tl_fn::payments::GetStarGiftUpgradePreview {
    tl_fn::payments::GetStarGiftUpgradePreview {
        gift_id: gift_id as i64,
    }
}

/// Builds `payments.getStarGiftUpgradeAttributes`.
pub fn build_get_star_gift_upgrade_attributes(
    gift_id: u64,
) -> tl_fn::payments::GetStarGiftUpgradeAttributes {
    tl_fn::payments::GetStarGiftUpgradeAttributes {
        gift_id: gift_id as i64,
    }
}

/// Builds `payments.toggleStarGiftsPinnedToTop`.
pub fn build_toggle_star_gifts_pinned_to_top(
    peer: tl_enums::InputPeer,
    stargifts: Vec<tl_enums::InputSavedStarGift>,
) -> tl_fn::payments::ToggleStarGiftsPinnedToTop {
    tl_fn::payments::ToggleStarGiftsPinnedToTop {
        peer,
        stargift: stargifts,
    }
}

/// Builds `account.updateEmojiStatus` to wear a unique collectible gift as the user's emoji status.
pub fn build_wear_unique_gift(
    collectible_id: CollectibleId,
    until: Option<TimeId>,
) -> tl_fn::account::UpdateEmojiStatus {
    tl_fn::account::UpdateEmojiStatus {
        emoji_status: tl_enums::EmojiStatus::InputEmojiStatusCollectible(
            tl_types::InputEmojiStatusCollectible {
                collectible_id: collectible_id.to_tl(),
                until: until.filter(|&u| u > 0),
            },
        ),
    }
}

/// Builds `account.updateEmojiStatus` with `EmojiStatusEmpty` to take off / unwear the current gift.
pub fn build_unwear_emoji_status() -> tl_fn::account::UpdateEmojiStatus {
    tl_fn::account::UpdateEmojiStatus {
        emoji_status: tl_enums::EmojiStatus::Empty,
    }
}

/// Builds `channels.updateEmojiStatus` to wear/unwear a collectible gift on a channel.
pub fn build_channel_wear_unique_gift(
    channel: tl_enums::InputChannel,
    collectible_id: Option<CollectibleId>,
    until: Option<TimeId>,
) -> tl_fn::channels::UpdateEmojiStatus {
    let emoji_status = match collectible_id {
        Some(id) => tl_enums::EmojiStatus::InputEmojiStatusCollectible(
            tl_types::InputEmojiStatusCollectible {
                collectible_id: id.to_tl(),
                until: until.filter(|&u| u > 0),
            },
        ),
        None => tl_enums::EmojiStatus::Empty,
    };
    tl_fn::channels::UpdateEmojiStatus {
        channel,
        emoji_status,
    }
}

/// Builds `payments.getStarGiftCollections`.
pub fn build_get_star_gift_collections(
    peer: tl_enums::InputPeer,
    hash: i64,
) -> tl_fn::payments::GetStarGiftCollections {
    tl_fn::payments::GetStarGiftCollections { peer, hash }
}

/// Builds `payments.createStarGiftCollection`.
pub fn build_create_star_gift_collection(
    peer: tl_enums::InputPeer,
    title: impl Into<String>,
    stargifts: Vec<tl_enums::InputSavedStarGift>,
) -> tl_fn::payments::CreateStarGiftCollection {
    tl_fn::payments::CreateStarGiftCollection {
        peer,
        title: title.into(),
        stargift: stargifts,
    }
}

/// Builds `payments.updateStarGiftCollection`.
pub fn build_update_star_gift_collection(
    peer: tl_enums::InputPeer,
    collection_id: i32,
    title: Option<String>,
    delete_stargift: Option<Vec<tl_enums::InputSavedStarGift>>,
    add_stargift: Option<Vec<tl_enums::InputSavedStarGift>>,
    order: Option<Vec<tl_enums::InputSavedStarGift>>,
) -> tl_fn::payments::UpdateStarGiftCollection {
    tl_fn::payments::UpdateStarGiftCollection {
        peer,
        collection_id,
        title,
        delete_stargift,
        add_stargift,
        order,
    }
}

/// Builds `payments.reorderStarGiftCollections`.
pub fn build_reorder_star_gift_collections(
    peer: tl_enums::InputPeer,
    order: Vec<i32>,
) -> tl_fn::payments::ReorderStarGiftCollections {
    tl_fn::payments::ReorderStarGiftCollections { peer, order }
}

/// Builds `payments.deleteStarGiftCollection`.
pub fn build_delete_star_gift_collection(
    peer: tl_enums::InputPeer,
    collection_id: i32,
) -> tl_fn::payments::DeleteStarGiftCollection {
    tl_fn::payments::DeleteStarGiftCollection {
        peer,
        collection_id,
    }
}

/// Builds `payments.sendStarGiftOffer`.
pub fn build_send_star_gift_offer(
    peer: tl_enums::InputPeer,
    slug: impl Into<String>,
    price: CreditsAmount,
    duration: i32,
    random_id: i64,
    allow_paid_stars: Option<i64>,
) -> tl_fn::payments::SendStarGiftOffer {
    tl_fn::payments::SendStarGiftOffer {
        peer,
        slug: slug.into(),
        price: price.to_tl(),
        duration,
        random_id,
        allow_paid_stars,
    }
}

/// Builds `payments.resolveStarGiftOffer`.
pub fn build_resolve_star_gift_offer(
    offer_msg_id: i32,
    decline: bool,
) -> tl_fn::payments::ResolveStarGiftOffer {
    tl_fn::payments::ResolveStarGiftOffer {
        decline,
        offer_msg_id,
    }
}

/// Builds `inputInvoiceStarGiftResale` for purchasing a resale listing.
pub fn build_resale_invoice(
    slug: impl Into<String>,
    to_id: tl_enums::InputPeer,
    ton: bool,
    show_name: bool,
    message: Option<tl_enums::TextWithEntities>,
) -> tl_enums::InputInvoice {
    tl_enums::InputInvoice::StarGiftResale(tl_types::InputInvoiceStarGiftResale {
        ton,
        show_name,
        slug: slug.into(),
        to_id,
        message,
    })
}

/// Builds `inputInvoiceStarGiftUpgrade` for paid upgrades via `payments.sendStarsForm`.
pub fn build_upgrade_invoice(
    stargift: tl_enums::InputSavedStarGift,
    keep_original_details: bool,
) -> tl_enums::InputInvoice {
    tl_enums::InputInvoice::StarGiftUpgrade(tl_types::InputInvoiceStarGiftUpgrade {
        keep_original_details,
        stargift,
    })
}

/// Builds `inputInvoiceStarGiftTransfer` for paid transfers via `payments.sendStarsForm`.
pub fn build_transfer_invoice(
    stargift: tl_enums::InputSavedStarGift,
    to_id: tl_enums::InputPeer,
) -> tl_enums::InputInvoice {
    tl_enums::InputInvoice::StarGiftTransfer(tl_types::InputInvoiceStarGiftTransfer {
        stargift,
        to_id,
    })
}

/// Computes the 64-bit Telegram list hash for `payments.getStarGiftCollections`
/// (`Api::CountHash(_collections | transform(&Data::GiftCollection::hash))` in
/// `info/peer_gifts/info_peer_gifts_widget.cpp:429-432` and `api/api_hash.h:36-70`).
pub fn compute_gift_collections_hash(collections: &[GiftCollection]) -> i64 {
    let mut hash: u64 = 0;
    for col in collections {
        hash ^= hash >> 21;
        hash ^= hash << 35;
        hash ^= hash >> 4;
        hash = hash.wrapping_add(col.hash as u64);
    }
    hash as i64
}

/// Builds `payments.checkCanSendGift` (`CheckMaybeGiftLocked` in `boxes/star_gift_box.cpp:1536`).
pub fn build_check_can_send_gift(gift_id: u64) -> tl_fn::payments::CheckCanSendGift {
    tl_fn::payments::CheckCanSendGift {
        gift_id: gift_id as i64,
    }
}

/// Parses `payments.CheckCanSendGiftResult` into [`CanSendGiftResult`]
/// (`boxes/star_gift_box.cpp:1539-1547`).
pub fn check_can_send_gift_result_from_tl(
    tl: &tl_enums::payments::CheckCanSendGiftResult,
) -> CanSendGiftResult {
    match tl {
        tl_enums::payments::CheckCanSendGiftResult::Ok => CanSendGiftResult::Ok,
        tl_enums::payments::CheckCanSendGiftResult::Fail(fail) => {
            let tl_enums::TextWithEntities::TextWithEntities(ref reason) = fail.reason;
            CanSendGiftResult::Fail {
                reason: reason.text.clone(),
            }
        }
    }
}

/// Builds `payments.withdrawStarGift` (`MoveToWalletBox` in `boxes/transfer_gift_box.cpp:700-706`).
///
/// `tmp_password` must be obtained from `account.getTmpPassword` with an SRP 2FA check.
pub fn build_withdraw_star_gift(
    stargift: tl_enums::InputSavedStarGift,
    tmp_password: Vec<u8>,
    keep_on_profile: bool,
) -> tl_fn::payments::WithdrawStarGift {
    tl_fn::payments::WithdrawStarGift {
        keep_on_profile,
        stargift,
        tmp_password,
    }
}

/// Builds `payments.getStarGiftWithdrawalUrl`
/// (`ExportOnBlockchain` in `boxes/transfer_gift_box.cpp:491-494`).
///
/// `tmp_password` must be obtained from `account.getTmpPassword` with an SRP 2FA check.
pub fn build_get_star_gift_withdrawal_url(
    stargift: tl_enums::InputSavedStarGift,
    tmp_password: Vec<u8>,
) -> tl_fn::payments::GetStarGiftWithdrawalUrl {
    tl_fn::payments::GetStarGiftWithdrawalUrl {
        stargift,
        tmp_password,
    }
}

/// Extracts the withdrawal URL from `payments.StarGiftWithdrawalUrl`
/// (`boxes/transfer_gift_box.cpp:495-497`).
pub fn star_gift_withdrawal_url_from_tl(tl: &tl_enums::payments::StarGiftWithdrawalUrl) -> String {
    let tl_enums::payments::StarGiftWithdrawalUrl::StarGiftWithdrawalUrl(data) = tl;
    data.url.clone()
}

/// Builds `payments.deleteStarGift`
/// (`DeleteStarGift` in `settings/settings_credits_graphics.cpp:311-313`).
pub fn build_delete_star_gift(
    stargift: tl_enums::InputSavedStarGift,
) -> tl_fn::payments::DeleteStarGift {
    tl_fn::payments::DeleteStarGift { stargift }
}

/// Builds `payments.getSavedStarGift`
/// (`RecentSharedMediaGifts::togglePinned` in `data/components/recent_shared_media_gifts.cpp:217-220`).
pub fn build_get_saved_star_gift(
    stargifts: Vec<tl_enums::InputSavedStarGift>,
) -> tl_fn::payments::GetSavedStarGift {
    tl_fn::payments::GetSavedStarGift {
        stargift: stargifts,
    }
}

/// Builds `payments.toggleChatStarGiftNotifications`
/// (`Widget::setupNotifyCheckbox` in `info/peer_gifts/info_peer_gifts_widget.cpp:2658-2662`).
pub fn build_toggle_chat_star_gift_notifications(
    peer: tl_enums::InputPeer,
    enabled: bool,
) -> tl_fn::payments::ToggleChatStarGiftNotifications {
    tl_fn::payments::ToggleChatStarGiftNotifications { enabled, peer }
}
