//! Deterministic offline fake implementation of [`StarGiftsMarketplaceBackend`] for
//! [`MockPaymentsBackend`] and [`tg_app_core::stand_in::mock_session::MockSession`].
//!
//! Safety guarantees:
//! - Zero network calls; never initiates any real payment.
//! - Uses only fictional placeholder names and deterministic demo data.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use tg_data::{ChannelId, CollectibleId, DocumentId, MsgId, PeerId, TimeId, UserId};
use tg_payments::credits::{
    CreditsAmount, CreditsGiveawayOption, CreditsGiveawayWinnerOption, CreditsType,
};
use tg_payments::giveaways::{
    GiveawayDraft, GiveawayLimits, GiveawayPrizeKind, PremiumGiftCodeCatalog,
    parse_for_channel_boost_slots,
};
use tg_payments::premium::{BoostPrepaidGiveaway, GiveawayInfo, GiveawayState, MyBoostSlot};
use tg_payments::star_gift_auctions::{
    ActiveAuctionsSnapshot, GiftAuctionAcquiredGift, GiftAuctionBidLevel, GiftAuctionRound,
    GiftAuctionState, StarGiftAuctionMyState,
};
use tg_payments::star_gift_craft::{CraftGiftSlot, validate_craft_slots};
use tg_payments::star_gifts::{
    GiftAttributeId, GiftCollection, PeerGiftsFilter, ResaleGiftsDescriptor, ResaleGiftsFilter,
    ResaleGiftsSort, RgbColor, SavedStarGift, SavedStarGiftId, StarGift, UniqueGift,
    UniqueGiftAttribute, UniqueGiftBackdrop, UniqueGiftModel, UniqueGiftOriginalDetails,
    UniqueGiftPattern, UniqueGiftRarity, UniqueGiftValue, validate_collection_title,
};
use tg_payments::starref::{
    ConnectedStarRefBot, STARREF_COMMISSION_MAX_FALLBACK, STARREF_COMMISSION_MIN_FALLBACK,
    StarRefProgram, SuggestedStarRefSort, validate_star_ref_update,
};
use tg_tl::{enums as tl_enums, types as tl_types};

use crate::payments::backend::ReceivedGiftItem;
use crate::payments::marketplace_backend::{
    ConnectedStarRefItem, CraftExecutionOutcome, GiveawayOptionsBundle, PeerGiftsPage,
    ReplaceBoostBundle, StarGiftsMarketplaceBackend, StarRefOverviewBundle, SuggestedStarRefItem,
    UniqueGiftDetailBundle,
};
use crate::payments::mock::MockPaymentsBackend;

const DEMO_NOW: TimeId = 1_763_760_000;

/// Stateful in-memory demo store for WP-16 Star Gifts marketplace, giveaways, boosts,
/// and Stars affiliate programs.
#[derive(Debug, Clone)]
pub struct MockMarketplaceState {
    /// Owned/received gifts shown in per-peer gifts list & collections.
    pub gifts: Vec<ReceivedGiftItem>,
    /// User's custom Star Gift collections.
    pub collections: Vec<GiftCollection>,
    /// Next collection ID.
    pub next_collection_id: i32,
    /// Collectible ID currently worn as the user's emoji status.
    pub worn_collectible_id: Option<CollectibleId>,
    /// Resale marketplace listings.
    pub resale_listings: Vec<StarGift>,
    /// Live & finished Star Gift auctions.
    pub auctions: Vec<GiftAuctionState>,
    /// Gifts acquired by the user in auctions.
    pub acquired: Vec<GiftAuctionAcquiredGift>,
    /// User's Premium boost slots.
    pub my_boost_slots: Vec<MyBoostSlot>,
    /// Prepaid giveaways available on the demo channel.
    pub prepaid_giveaways: Vec<BoostPrepaidGiveaway>,
    /// Managed bot's Stars affiliate program (Setup tab).
    pub managed_program: Option<StarRefProgram>,
    /// Connected Stars affiliate bots (Join tab).
    pub connected_bots: Vec<ConnectedStarRefItem>,
    /// Suggested Stars affiliate programs (Join tab).
    pub suggested_bots: Vec<SuggestedStarRefItem>,
    /// Next synthetic serial number for upgrades/crafts.
    pub next_serial: i32,
    /// Whether channel/peer Star Gift notifications are enabled (`chat_notifications_enabled`).
    pub chat_notifications_enabled: bool,
    /// Recorded fake TON withdrawal calls `(gift_id, keep_on_profile)`.
    pub withdrawn_gifts: Vec<(u64, bool)>,
    /// Recorded fake Fragment/TON withdrawal URLs `(gift_id, url)`.
    pub withdrawal_urls: Vec<(u64, String)>,
}

impl Default for MockMarketplaceState {
    fn default() -> Self {
        Self::new()
    }
}

impl MockMarketplaceState {
    /// Builds deterministic fictional demo data covering every WP-16 screen.
    #[must_use]
    pub fn new() -> Self {
        let u1 = make_demo_unique(
            88_001,
            10_002,
            "CrystalPrism-42",
            "Crystal Prism",
            42,
            "Prism Nova",
            UniqueGiftRarity::Epic as i32,
            "Midnight Sapphire",
            7,
            RgbColor::new(0x3B, 0x82, 0xF6),
            RgbColor::new(0x1E, 0x3A, 0x8A),
            "Starburst",
            2002,
            250,
            0,
            0,
        );
        let u2 = make_demo_unique(
            88_004,
            10_002,
            "CrystalPrism-77",
            "Crystal Prism",
            77,
            "Frost Facet",
            UniqueGiftRarity::Rare as i32,
            "Midnight Sapphire",
            7,
            RgbColor::new(0x3B, 0x82, 0xF6),
            RgbColor::new(0x1E, 0x3A, 0x8A),
            "Starburst",
            2002,
            300,
            450,
            0,
        );
        let u3 = make_demo_unique(
            88_005,
            10_002,
            "CrystalPrism-108",
            "Crystal Prism",
            108,
            "Amber Core",
            35,
            "Solar Crimson",
            11,
            RgbColor::new(0xE1, 0x4B, 0x62),
            RgbColor::new(0x7B, 0x1E, 0x33),
            "Hex Crest",
            2003,
            200,
            0,
            0,
        );

        let gifts = vec![
            ReceivedGiftItem {
                saved: SavedStarGift {
                    info: StarGift {
                        id: 88_001,
                        unique: Some(u1),
                        stars: 50,
                        stars_converted: 43,
                        ..StarGift::default()
                    },
                    manage_id: Some(SavedStarGiftId::User(MsgId::from(930))),
                    collection_ids: vec![1],
                    message: "Congrats on shipping the Rust port!".to_owned(),
                    stars_converted: 43,
                    from_id: PeerId::from_user(UserId::from_tl(900_002)),
                    date: DEMO_NOW - 86_400 * 4,
                    gift_num: 42,
                    pinned: true,
                    mine: true,
                    ..SavedStarGift::default()
                },
                title: "Crystal Prism #42".to_owned(),
                sender_name: "Sam Taylor".to_owned(),
                artwork_kind: "gem".to_owned(),
            },
            ReceivedGiftItem {
                saved: SavedStarGift {
                    info: StarGift {
                        id: 88_004,
                        unique: Some(u2),
                        stars: 50,
                        stars_converted: 43,
                        ..StarGift::default()
                    },
                    manage_id: Some(SavedStarGiftId::User(MsgId::from(931))),
                    collection_ids: vec![1, 2],
                    message: "Listed on resale marketplace.".to_owned(),
                    stars_converted: 43,
                    from_id: PeerId::from_user(UserId::from_tl(900_003)),
                    date: DEMO_NOW - 86_400 * 3,
                    gift_num: 77,
                    mine: true,
                    ..SavedStarGift::default()
                },
                title: "Crystal Prism #77".to_owned(),
                sender_name: "Casey Morgan".to_owned(),
                artwork_kind: "gem".to_owned(),
            },
            ReceivedGiftItem {
                saved: SavedStarGift {
                    info: StarGift {
                        id: 88_005,
                        unique: Some(u3),
                        stars: 50,
                        stars_converted: 43,
                        ..StarGift::default()
                    },
                    manage_id: Some(SavedStarGiftId::User(MsgId::from(932))),
                    collection_ids: vec![2],
                    message: "Ready for the crafting forge!".to_owned(),
                    stars_converted: 43,
                    from_id: PeerId::from_user(UserId::from_tl(900_004)),
                    date: DEMO_NOW - 86_400 * 2,
                    gift_num: 108,
                    mine: true,
                    ..SavedStarGift::default()
                },
                title: "Crystal Prism #108".to_owned(),
                sender_name: "Jordan Lee".to_owned(),
                artwork_kind: "gem".to_owned(),
            },
            ReceivedGiftItem {
                saved: SavedStarGift {
                    info: StarGift {
                        id: 88_002,
                        stars: 75,
                        stars_converted: 64,
                        stars_to_upgrade: 25,
                        limited_count: 10_000,
                        limited_left: 420,
                        upgradable: true,
                        ..StarGift::default()
                    },
                    manage_id: Some(SavedStarGiftId::User(MsgId::from(940))),
                    collection_ids: vec![],
                    message: "Happy launch week!".to_owned(),
                    stars_converted: 64,
                    from_id: PeerId::from_user(UserId::from_tl(900_003)),
                    date: DEMO_NOW - 86_400,
                    upgradable: true,
                    mine: true,
                    ..SavedStarGift::default()
                },
                title: "Orbital Rocket".to_owned(),
                sender_name: "Casey Morgan".to_owned(),
                artwork_kind: "rocket".to_owned(),
            },
            ReceivedGiftItem {
                saved: SavedStarGift {
                    info: StarGift {
                        id: 88_003,
                        stars: 15,
                        stars_converted: 13,
                        upgradable: false,
                        ..StarGift::default()
                    },
                    manage_id: Some(SavedStarGiftId::User(MsgId::from(945))),
                    collection_ids: vec![],
                    message: "Thanks for the code review!".to_owned(),
                    stars_converted: 13,
                    from_id: PeerId::ZERO,
                    date: DEMO_NOW - 3_600,
                    anonymous: true,
                    mine: true,
                    ..SavedStarGift::default()
                },
                title: "Velvet Rose".to_owned(),
                sender_name: "Anonymous".to_owned(),
                artwork_kind: "rose".to_owned(),
            },
        ];

        let collections = vec![
            GiftCollection {
                id: 1,
                count: 2,
                title: "Favorites".to_owned(),
                icon_document_id: Some(DocumentId::from_tl(2001)),
                hash: 1001,
            },
            GiftCollection {
                id: 2,
                count: 2,
                title: "Craftables".to_owned(),
                icon_document_id: Some(DocumentId::from_tl(2002)),
                hash: 1002,
            },
        ];

        let resale_listings = vec![
            StarGift {
                id: 99_001,
                unique: Some(make_demo_unique(
                    99_001,
                    10_002,
                    "CrystalPrism-19",
                    "Crystal Prism",
                    19,
                    "Prism Nova",
                    UniqueGiftRarity::Epic as i32,
                    "Midnight Sapphire",
                    7,
                    RgbColor::new(0x3B, 0x82, 0xF6),
                    RgbColor::new(0x1E, 0x3A, 0x8A),
                    "Starburst",
                    2002,
                    250,
                    320,
                    0,
                )),
                limited_count: 5000,
                limited_left: 0,
                ..StarGift::default()
            },
            StarGift {
                id: 99_002,
                unique: Some(make_demo_unique(
                    99_002,
                    10_002,
                    "CrystalPrism-5",
                    "Crystal Prism",
                    5,
                    "Solaris",
                    UniqueGiftRarity::Legendary as i32,
                    "Emerald Mist",
                    9,
                    RgbColor::new(0x10, 0xB9, 0x81),
                    RgbColor::new(0x06, 0x5F, 0x46),
                    "Orbit Ring",
                    2004,
                    350,
                    900,
                    4_500_000_000,
                )),
                limited_count: 5000,
                limited_left: 0,
                resell_ton_only: false,
                ..StarGift::default()
            },
            StarGift {
                id: 99_003,
                unique: Some(make_demo_unique(
                    99_003,
                    10_002,
                    "CrystalPrism-204",
                    "Crystal Prism",
                    204,
                    "Amber Core",
                    35,
                    "Solar Crimson",
                    11,
                    RgbColor::new(0xE1, 0x4B, 0x62),
                    RgbColor::new(0x7B, 0x1E, 0x33),
                    "Hex Crest",
                    2003,
                    200,
                    0,
                    2_200_000_000,
                )),
                limited_count: 5000,
                limited_left: 0,
                resell_ton_only: true,
                ..StarGift::default()
            },
        ];

        let auction_gift = StarGift {
            id: 10_099,
            stars: 250,
            stars_converted: 210,
            stars_to_upgrade: 50,
            resell_title: "Comet Crown".to_owned(),
            auction_slug: "comet_crown".to_owned(),
            auction_gifts_per_round: 3,
            auction_start_date: DEMO_NOW - 1_800,
            limited_count: 100,
            limited_left: 42,
            upgradable: true,
            ..StarGift::default()
        };

        let auctions = vec![GiftAuctionState {
            gift: Some(auction_gift),
            my: StarGiftAuctionMyState {
                to_peer_id: PeerId::from_user(UserId::from_tl(900_001)),
                min_bid_amount: 950,
                bid: 900,
                date: DEMO_NOW - 600,
                got_count: 1,
                returned: false,
            },
            bid_levels: vec![
                GiftAuctionBidLevel {
                    amount: 1_800,
                    position: 1,
                    date: DEMO_NOW - 900,
                },
                GiftAuctionBidLevel {
                    amount: 1_250,
                    position: 2,
                    date: DEMO_NOW - 750,
                },
                GiftAuctionBidLevel {
                    amount: 900,
                    position: 3,
                    date: DEMO_NOW - 600,
                },
                GiftAuctionBidLevel {
                    amount: 650,
                    position: 4,
                    date: DEMO_NOW - 500,
                },
            ],
            top_bidders: vec![
                UserId::from_tl(900_002),
                UserId::from_tl(900_003),
                UserId::from_tl(900_001),
            ],
            round_parameters: vec![
                GiftAuctionRound {
                    number: 1,
                    duration: 3600,
                    extend_top: 3,
                    extend_duration: 300,
                },
                GiftAuctionRound {
                    number: 2,
                    duration: 3600,
                    extend_top: 3,
                    extend_duration: 300,
                },
            ],
            min_bid_amount: 500,
            average_price: 0,
            start_date: DEMO_NOW - 1_800,
            end_date: DEMO_NOW + 14_400,
            next_round_at: DEMO_NOW + 1_800,
            last_gift_num: 58,
            current_round: 2,
            total_rounds: 8,
            gifts_left: 42,
            version: 5,
            timeout: 15,
            ..GiftAuctionState::default()
        }];

        let acquired = vec![GiftAuctionAcquiredGift {
            to_peer_id: PeerId::from_user(UserId::from_tl(900_001)),
            message: "Won in Round 1!".to_owned(),
            date: DEMO_NOW - 3_600,
            bid_amount: 1_100,
            round: 1,
            number: 14,
            position: 2,
            name_hidden: false,
        }];

        let my_boost_slots = vec![
            MyBoostSlot {
                slot: 1,
                peer_id: Some(PeerId::from_channel(ChannelId::from_tl(2_000_101))),
                date: DEMO_NOW - 86_400 * 30,
                expires: DEMO_NOW + 86_400 * 150,
                cooldown_until_date: DEMO_NOW - 86_400 * 29,
            },
            MyBoostSlot {
                slot: 2,
                peer_id: Some(PeerId::from_channel(ChannelId::from_tl(2_000_202))),
                date: DEMO_NOW - 86_400 * 10,
                expires: DEMO_NOW + 86_400 * 170,
                cooldown_until_date: DEMO_NOW - 3_600,
            },
            MyBoostSlot {
                slot: 3,
                peer_id: Some(PeerId::from_channel(ChannelId::from_tl(2_000_303))),
                date: DEMO_NOW - 1_800,
                expires: DEMO_NOW + 86_400 * 180,
                cooldown_until_date: DEMO_NOW + 7_200,
            },
            MyBoostSlot {
                slot: 4,
                peer_id: None,
                date: 0,
                expires: 0,
                cooldown_until_date: 0,
            },
        ];

        let prepaid_giveaways = vec![
            BoostPrepaidGiveaway {
                date: DEMO_NOW - 86_400,
                id: 501,
                credits: 0,
                months: 3,
                quantity: 10,
                boosts: 40,
            },
            BoostPrepaidGiveaway {
                date: DEMO_NOW - 43_200,
                id: 502,
                credits: 2_500,
                months: 0,
                quantity: 5,
                boosts: 10,
            },
        ];

        let managed_program = Some(StarRefProgram {
            bot_id: UserId::from_tl(910_001),
            commission_permille: 200,
            duration_months: 12,
            end_date: 0,
            daily_revenue_per_user: CreditsAmount::new(2, 500_000_000, CreditsType::Stars),
        });

        let connected_bots = vec![ConnectedStarRefItem {
            bot_name: "Synth Studio Bot".to_owned(),
            bot_username: "synth_studio_bot".to_owned(),
            info: ConnectedStarRefBot {
                bot_id: UserId::from_tl(910_010),
                url: "https://t.me/synth_studio_bot?start=_tgr_demo01".to_owned(),
                date: DEMO_NOW - 86_400 * 5,
                commission_permille: 250,
                duration_months: 12,
                participants: 34,
                revenue: 680,
                revoked: false,
            },
        }];

        let suggested_bots = vec![
            SuggestedStarRefItem {
                bot_name: "Pixel Forge Bot".to_owned(),
                bot_username: "pixel_forge_bot".to_owned(),
                program: StarRefProgram {
                    bot_id: UserId::from_tl(910_020),
                    commission_permille: 300,
                    duration_months: 0,
                    end_date: 0,
                    daily_revenue_per_user: CreditsAmount::new(4, 200_000_000, CreditsType::Stars),
                },
            },
            SuggestedStarRefItem {
                bot_name: "Cloud Notes Mini App".to_owned(),
                bot_username: "cloud_notes_bot".to_owned(),
                program: StarRefProgram {
                    bot_id: UserId::from_tl(910_021),
                    commission_permille: 150,
                    duration_months: 6,
                    end_date: 0,
                    daily_revenue_per_user: CreditsAmount::new(6, 0, CreditsType::Stars),
                },
            },
            SuggestedStarRefItem {
                bot_name: "Vector Icons Market".to_owned(),
                bot_username: "vector_icons_bot".to_owned(),
                program: StarRefProgram {
                    bot_id: UserId::from_tl(910_022),
                    commission_permille: 200,
                    duration_months: 24,
                    end_date: 0,
                    daily_revenue_per_user: CreditsAmount::new(1, 800_000_000, CreditsType::Stars),
                },
            },
        ];

        Self {
            gifts,
            collections,
            next_collection_id: 3,
            worn_collectible_id: Some(CollectibleId::from_tl(88_001)),
            resale_listings,
            auctions,
            acquired,
            my_boost_slots,
            prepaid_giveaways,
            managed_program,
            connected_bots,
            suggested_bots,
            next_serial: 250,
            chat_notifications_enabled: true,
            withdrawn_gifts: Vec::new(),
            withdrawal_urls: Vec::new(),
        }
    }

    /// Recomputes each collection's gift count from `self.gifts`.
    fn refresh_collection_counts(&mut self) {
        for col in &mut self.collections {
            col.count = self
                .gifts
                .iter()
                .filter(|g| g.saved.collection_ids.contains(&col.id))
                .count() as i32;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn make_demo_unique(
    id: i64,
    initial_gift_id: u64,
    slug: &str,
    title: &str,
    number: i32,
    model_name: &str,
    model_rarity: i32,
    backdrop_name: &str,
    backdrop_id: i32,
    center: RgbColor,
    edge: RgbColor,
    pattern_name: &str,
    pattern_doc: i64,
    craft_permille: i32,
    stars_for_resale: i64,
    nano_ton_for_resale: i64,
) -> UniqueGift {
    let only_accept_ton = stars_for_resale <= 0 && nano_ton_for_resale > 0;
    UniqueGift {
        id: CollectibleId::from_tl(id),
        initial_gift_id,
        slug: slug.to_owned(),
        title: title.to_owned(),
        owner_name: "Alex River".to_owned(),
        owner_id: PeerId::from_user(UserId::from_tl(900_001)),
        host_id: PeerId::from_user(UserId::from_tl(900_001)),
        stars_for_resale,
        nano_ton_for_resale,
        stars_for_transfer: 25,
        stars_min_offer: 100,
        craft_chance_permille: craft_permille,
        number,
        only_accept_ton,
        can_be_theme: true,
        can_transfer_at: DEMO_NOW - 100,
        can_resell_at: DEMO_NOW - 100,
        can_craft_at: DEMO_NOW - 100,
        model: UniqueGiftModel {
            attribute: UniqueGiftAttribute {
                name: model_name.to_owned(),
                rarity_value: model_rarity,
            },
            document_id: DocumentId::from_tl(2000 + (id % 100)),
            crafted: false,
        },
        backdrop: UniqueGiftBackdrop {
            attribute: UniqueGiftAttribute {
                name: backdrop_name.to_owned(),
                rarity_value: 20,
            },
            id: backdrop_id,
            center_color: center,
            edge_color: edge,
            pattern_color: RgbColor::new(0xFF, 0xFF, 0xFF),
            text_color: RgbColor::new(0xFF, 0xFF, 0xFF),
        },
        pattern: UniqueGiftPattern {
            attribute: UniqueGiftAttribute {
                name: pattern_name.to_owned(),
                rarity_value: 15,
            },
            document_id: DocumentId::from_tl(pattern_doc),
        },
        original_details: UniqueGiftOriginalDetails {
            sender_id: PeerId::from_user(UserId::from_tl(900_002)),
            recipient_id: PeerId::from_user(UserId::from_tl(900_001)),
            date: DEMO_NOW - 86_400 * 4,
            message: "Demo collectible inscription".to_owned(),
        },
        value: Some(UniqueGiftValue {
            currency: "USD".to_owned(),
            value_price: 2_850,
            value_price_usd: 2_850,
            initial_price_stars: CreditsAmount::stars(50),
            initial_sale_price: 100,
            initial_sale_date: DEMO_NOW - 86_400 * 14,
            last_sale_price: 2_600,
            last_sale_date: DEMO_NOW - 86_400,
            average_price: 2_750,
            minimum_price: 2_200,
            for_sale_on_telegram: 18,
            for_sale_on_fragment: 5,
            fragment_url: format!("https://fragment.com/gift/{slug}"),
            last_sale_fragment: false,
        }),
        ..UniqueGift::default()
    }
}

fn demo_gift_code_catalog() -> PremiumGiftCodeCatalog {
    PremiumGiftCodeCatalog::from_tl(&[
        tl_enums::PremiumGiftCodeOption::PremiumGiftCodeOption(tl_types::PremiumGiftCodeOption {
            users: 1,
            months: 3,
            store_product: Some("prem_3m_1".to_owned()),
            store_quantity: Some(1),
            currency: "USD".to_owned(),
            amount: 1_199,
        }),
        tl_enums::PremiumGiftCodeOption::PremiumGiftCodeOption(tl_types::PremiumGiftCodeOption {
            users: 1,
            months: 6,
            store_product: Some("prem_6m_1".to_owned()),
            store_quantity: Some(1),
            currency: "USD".to_owned(),
            amount: 1_599,
        }),
        tl_enums::PremiumGiftCodeOption::PremiumGiftCodeOption(tl_types::PremiumGiftCodeOption {
            users: 1,
            months: 12,
            store_product: Some("prem_12m_1".to_owned()),
            store_quantity: Some(1),
            currency: "USD".to_owned(),
            amount: 2_899,
        }),
        tl_enums::PremiumGiftCodeOption::PremiumGiftCodeOption(tl_types::PremiumGiftCodeOption {
            users: 5,
            months: 3,
            store_product: Some("prem_3m_5".to_owned()),
            store_quantity: Some(5),
            currency: "USD".to_owned(),
            amount: 5_995,
        }),
        tl_enums::PremiumGiftCodeOption::PremiumGiftCodeOption(tl_types::PremiumGiftCodeOption {
            users: 10,
            months: 3,
            store_product: Some("prem_3m_10".to_owned()),
            store_quantity: Some(10),
            currency: "USD".to_owned(),
            amount: 11_990,
        }),
        tl_enums::PremiumGiftCodeOption::PremiumGiftCodeOption(tl_types::PremiumGiftCodeOption {
            users: 10,
            months: 6,
            store_product: Some("prem_6m_10".to_owned()),
            store_quantity: Some(10),
            currency: "USD".to_owned(),
            amount: 15_990,
        }),
        tl_enums::PremiumGiftCodeOption::PremiumGiftCodeOption(tl_types::PremiumGiftCodeOption {
            users: 10,
            months: 12,
            store_product: Some("prem_12m_10".to_owned()),
            store_quantity: Some(10),
            currency: "USD".to_owned(),
            amount: 28_990,
        }),
    ])
}

fn demo_stars_giveaway_options() -> Vec<CreditsGiveawayOption> {
    vec![
        CreditsGiveawayOption {
            credits: 1_000,
            yearly_boosts: 4,
            store_product: "stars_gw_1000".to_owned(),
            currency: "USD".to_owned(),
            amount: 1_999,
            winners: vec![
                CreditsGiveawayWinnerOption {
                    users: 5,
                    per_user_stars: 200,
                    is_default: true,
                },
                CreditsGiveawayWinnerOption {
                    users: 10,
                    per_user_stars: 100,
                    is_default: false,
                },
            ],
            is_extended: false,
            is_default: true,
        },
        CreditsGiveawayOption {
            credits: 2_500,
            yearly_boosts: 10,
            store_product: "stars_gw_2500".to_owned(),
            currency: "USD".to_owned(),
            amount: 4_999,
            winners: vec![
                CreditsGiveawayWinnerOption {
                    users: 5,
                    per_user_stars: 500,
                    is_default: false,
                },
                CreditsGiveawayWinnerOption {
                    users: 10,
                    per_user_stars: 250,
                    is_default: true,
                },
                CreditsGiveawayWinnerOption {
                    users: 25,
                    per_user_stars: 100,
                    is_default: false,
                },
            ],
            is_extended: false,
            is_default: false,
        },
    ]
}

impl StarGiftsMarketplaceBackend for MockMarketplaceState {
    fn load_peer_gifts(
        &self,
        peer_name: &str,
        filter: &PeerGiftsFilter,
        _offset: &str,
    ) -> Result<PeerGiftsPage, String> {
        let mut items: Vec<ReceivedGiftItem> = self
            .gifts
            .iter()
            .filter(|g| filter.matches(&g.saved))
            .cloned()
            .collect();
        if filter.sort_by_value {
            items.sort_by(|a, b| {
                let va = a
                    .saved
                    .info
                    .unique
                    .as_ref()
                    .and_then(|u| u.value.as_ref())
                    .map_or(a.saved.info.stars, |v| v.value_price);
                let vb = b
                    .saved
                    .info
                    .unique
                    .as_ref()
                    .and_then(|u| u.value.as_ref())
                    .map_or(b.saved.info.stars, |v| v.value_price);
                vb.cmp(&va)
            });
        } else {
            items.sort_by(|a, b| {
                b.saved
                    .pinned
                    .cmp(&a.saved.pinned)
                    .then_with(|| b.saved.date.cmp(&a.saved.date))
            });
        }
        let total_count = items.len() as i32;
        Ok(PeerGiftsPage {
            peer_name: if peer_name.is_empty() {
                "Alex River".to_owned()
            } else {
                peer_name.to_owned()
            },
            items,
            next_offset: String::new(),
            total_count,
            collections: self.collections.clone(),
            worn_collectible_id: self.worn_collectible_id,
        })
    }

    fn get_unique_star_gift(&self, slug: &str) -> Result<UniqueGiftDetailBundle, String> {
        let trimmed = slug.trim();
        let found = self
            .gifts
            .iter()
            .map(|g| &g.saved.info)
            .chain(self.resale_listings.iter())
            .find(|g| {
                g.unique
                    .as_ref()
                    .is_some_and(|u| u.slug.eq_ignore_ascii_case(trimmed))
            })
            .cloned()
            .ok_or_else(|| format!("Unique Star Gift '{trimmed}' not found in demo catalog."))?;
        let (owner_name, value_info, is_worn) = match &found.unique {
            Some(u) => (
                if u.owner_name.is_empty() {
                    "Alex River".to_owned()
                } else {
                    u.owner_name.clone()
                },
                u.value.clone(),
                self.worn_collectible_id == Some(u.id),
            ),
            None => (String::new(), None, false),
        };
        Ok(UniqueGiftDetailBundle {
            gift: found,
            owner_name,
            value_info,
            is_worn,
        })
    }

    fn upgrade_star_gift(
        &mut self,
        gift_id: u64,
        keep_original_details: bool,
    ) -> Result<String, String> {
        let serial = self.next_serial;
        let item = self
            .gifts
            .iter_mut()
            .find(|g| g.saved.info.id == gift_id)
            .ok_or_else(|| format!("Gift #{gift_id} not found."))?;
        if item.saved.info.unique.is_some() {
            return Err("Gift is already a unique collectible.".to_owned());
        }
        if !(item.saved.upgradable || item.saved.info.upgradable) {
            return Err("This gift cannot be upgraded.".to_owned());
        }
        let slug = format!("{}-{serial}", item.title.replace(' ', ""));
        let mut unique = make_demo_unique(
            gift_id as i64,
            gift_id,
            &slug,
            &item.title,
            serial,
            "Ascendant",
            UniqueGiftRarity::Rare as i32,
            "Emerald Mist",
            9,
            RgbColor::new(0x10, 0xB9, 0x81),
            RgbColor::new(0x06, 0x5F, 0x46),
            "Orbit Ring",
            2004,
            250,
            0,
            0,
        );
        if !keep_original_details {
            unique.original_details = UniqueGiftOriginalDetails::default();
        }
        item.title = unique.display_name();
        item.saved.info.unique = Some(unique);
        item.saved.upgradable = false;
        item.saved.info.upgradable = false;
        self.next_serial += 1;
        Ok(format!("Upgraded to \"{}\" (offline demo).", item.title))
    }

    fn transfer_star_gift(&mut self, gift_id: u64, recipient: &str) -> Result<String, String> {
        let target = recipient.trim();
        if target.is_empty() {
            return Err("Please enter a recipient username or name.".to_owned());
        }
        let pos = self
            .gifts
            .iter()
            .position(|g| g.saved.info.id == gift_id)
            .ok_or_else(|| format!("Gift #{gift_id} not found."))?;
        if self.gifts[pos].saved.info.unique.is_none() {
            return Err("Only upgraded unique collectibles can be transferred.".to_owned());
        }
        let removed = self.gifts.remove(pos);
        if let Some(ref u) = removed.saved.info.unique
            && self.worn_collectible_id == Some(u.id)
        {
            self.worn_collectible_id = None;
        }
        self.refresh_collection_counts();
        Ok(format!(
            "Transferred \"{}\" to {target} (offline demo).",
            removed.title
        ))
    }

    fn convert_star_gift(&mut self, gift_id: u64) -> Result<String, String> {
        let pos = self
            .gifts
            .iter()
            .position(|g| g.saved.info.id == gift_id)
            .ok_or_else(|| format!("Gift #{gift_id} not found."))?;
        if self.gifts[pos].saved.info.unique.is_some() {
            return Err("Unique collectibles cannot be converted back to Stars.".to_owned());
        }
        let removed = self.gifts.remove(pos);
        self.refresh_collection_counts();
        Ok(format!(
            "Converted \"{}\" for {} Stars (offline demo).",
            removed.title, removed.saved.stars_converted
        ))
    }

    fn save_star_gift(&mut self, gift_id: u64, unsave: bool) -> Result<String, String> {
        let item = self
            .gifts
            .iter_mut()
            .find(|g| g.saved.info.id == gift_id)
            .ok_or_else(|| format!("Gift #{gift_id} not found."))?;
        item.saved.hidden = unsave;
        Ok(if unsave {
            format!("Hid \"{}\" from profile (offline demo).", item.title)
        } else {
            format!("Displayed \"{}\" on profile (offline demo).", item.title)
        })
    }

    fn toggle_star_gifts_pinned_to_top(&mut self, gift_ids: &[u64]) -> Result<String, String> {
        if gift_ids.is_empty() {
            return Err("No gift specified to pin.".to_owned());
        }
        let mut toggled = 0usize;
        for &gid in gift_ids {
            if let Some(item) = self.gifts.iter_mut().find(|g| g.saved.info.id == gid) {
                item.saved.pinned = !item.saved.pinned;
                toggled += 1;
            }
        }
        if toggled == 0 {
            return Err("Requested gift was not found.".to_owned());
        }
        Ok(format!(
            "Updated pin state for {toggled} gift(s) (offline demo)."
        ))
    }

    fn wear_unique_gift(
        &mut self,
        collectible_id: Option<CollectibleId>,
        _until: Option<TimeId>,
    ) -> Result<String, String> {
        self.worn_collectible_id = collectible_id;
        match collectible_id {
            Some(id) => Ok(format!(
                "Now wearing collectible #{} as your profile emoji status (offline demo).",
                id.raw()
            )),
            None => Ok("Removed collectible emoji status (offline demo).".to_owned()),
        }
    }

    fn load_star_gift_collections(&self, _peer_name: &str) -> Result<Vec<GiftCollection>, String> {
        Ok(self.collections.clone())
    }

    fn create_star_gift_collection(
        &mut self,
        _peer_name: &str,
        title: &str,
        gift_ids: &[u64],
    ) -> Result<GiftCollection, String> {
        let valid_title = validate_collection_title(title).map_err(str::to_owned)?;
        let id = self.next_collection_id;
        self.next_collection_id += 1;
        for &gid in gift_ids {
            if let Some(item) = self.gifts.iter_mut().find(|g| g.saved.info.id == gid)
                && !item.saved.collection_ids.contains(&id)
            {
                item.saved.collection_ids.push(id);
            }
        }
        let col = GiftCollection {
            id,
            count: gift_ids.len() as i32,
            title: valid_title,
            icon_document_id: None,
            hash: i64::from(id) * 1000,
        };
        self.collections.push(col.clone());
        self.refresh_collection_counts();
        Ok(col)
    }

    fn update_star_gift_collection(
        &mut self,
        _peer_name: &str,
        collection_id: i32,
        title: Option<&str>,
        add_gift_ids: &[u64],
        delete_gift_ids: &[u64],
    ) -> Result<GiftCollection, String> {
        let new_title = match title {
            Some(t) => Some(validate_collection_title(t).map_err(str::to_owned)?),
            None => None,
        };
        if !self.collections.iter().any(|c| c.id == collection_id) {
            return Err(format!("Collection #{collection_id} not found."));
        }
        for &gid in add_gift_ids {
            if let Some(item) = self.gifts.iter_mut().find(|g| g.saved.info.id == gid)
                && !item.saved.collection_ids.contains(&collection_id)
            {
                item.saved.collection_ids.push(collection_id);
            }
        }
        for &gid in delete_gift_ids {
            if let Some(item) = self.gifts.iter_mut().find(|g| g.saved.info.id == gid) {
                item.saved.collection_ids.retain(|&id| id != collection_id);
            }
        }
        if let Some(ref nt) = new_title
            && let Some(col) = self.collections.iter_mut().find(|c| c.id == collection_id)
        {
            col.title = nt.clone();
        }
        self.refresh_collection_counts();
        self.collections
            .iter()
            .find(|c| c.id == collection_id)
            .cloned()
            .ok_or_else(|| format!("Collection #{collection_id} not found."))
    }

    fn reorder_star_gift_collections(
        &mut self,
        _peer_name: &str,
        order: &[i32],
    ) -> Result<String, String> {
        let mut reordered = Vec::with_capacity(self.collections.len());
        for &cid in order {
            if let Some(c) = self.collections.iter().find(|c| c.id == cid).cloned() {
                reordered.push(c);
            }
        }
        for c in &self.collections {
            if !reordered.iter().any(|r| r.id == c.id) {
                reordered.push(c.clone());
            }
        }
        self.collections = reordered;
        Ok("Reordered Star Gift collections (offline demo).".to_owned())
    }

    fn delete_star_gift_collection(
        &mut self,
        _peer_name: &str,
        collection_id: i32,
    ) -> Result<String, String> {
        let before = self.collections.len();
        self.collections.retain(|c| c.id != collection_id);
        if self.collections.len() == before {
            return Err(format!("Collection #{collection_id} not found."));
        }
        for item in &mut self.gifts {
            item.saved.collection_ids.retain(|&id| id != collection_id);
        }
        Ok(format!(
            "Deleted collection #{collection_id} (offline demo)."
        ))
    }

    fn load_resale_star_gifts(
        &self,
        gift_id: u64,
        filter: &ResaleGiftsFilter,
        _offset: &str,
    ) -> Result<ResaleGiftsDescriptor, String> {
        let mut models = Vec::new();
        let mut backdrops = Vec::new();
        let mut patterns = Vec::new();
        for g in &self.resale_listings {
            if let Some(ref u) = g.unique {
                if let Some((_, c)) = models
                    .iter_mut()
                    .find(|(m, _): &&mut (UniqueGiftModel, i32)| *m == u.model)
                {
                    *c += 1;
                } else {
                    models.push((u.model.clone(), 1));
                }
                if let Some((_, c)) = backdrops
                    .iter_mut()
                    .find(|(b, _): &&mut (UniqueGiftBackdrop, i32)| *b == u.backdrop)
                {
                    *c += 1;
                } else {
                    backdrops.push((u.backdrop.clone(), 1));
                }
                if let Some((_, c)) = patterns
                    .iter_mut()
                    .find(|(p, _): &&mut (UniqueGiftPattern, i32)| *p == u.pattern)
                {
                    *c += 1;
                } else {
                    patterns.push((u.pattern.clone(), 1));
                }
            }
        }

        let mut list: Vec<StarGift> = self
            .resale_listings
            .iter()
            .filter(|g| {
                let Some(ref u) = g.unique else {
                    return false;
                };
                if filter.stars_only && u.only_accept_ton {
                    return false;
                }
                if !filter.attributes.is_empty() {
                    let ids = [
                        GiftAttributeId::for_model(&u.model),
                        GiftAttributeId::for_backdrop(&u.backdrop),
                        GiftAttributeId::for_pattern(&u.pattern),
                    ];
                    if !ids.iter().any(|id| filter.attributes.contains(id)) {
                        return false;
                    }
                }
                true
            })
            .cloned()
            .collect();

        match filter.sort {
            ResaleGiftsSort::Price => {
                list.sort_by_key(|g| {
                    g.unique.as_ref().map_or(i64::MAX, |u| {
                        if u.stars_for_resale > 0 {
                            u.stars_for_resale
                        } else {
                            u.nano_ton_for_resale / 1_000_000
                        }
                    })
                });
            }
            ResaleGiftsSort::Number => {
                list.sort_by_key(|g| g.unique.as_ref().map_or(i32::MAX, |u| u.number));
            }
            ResaleGiftsSort::Date => {}
        }

        let count = list.len() as i32;
        Ok(ResaleGiftsDescriptor {
            gift_id,
            offset: String::new(),
            list,
            models,
            backdrops,
            patterns,
            attributes_hash: 777,
            count,
        })
    }

    fn update_star_gift_price(
        &mut self,
        gift_id: u64,
        resell_amount: CreditsAmount,
    ) -> Result<String, String> {
        let item = self
            .gifts
            .iter_mut()
            .find(|g| g.saved.info.id == gift_id)
            .ok_or_else(|| format!("Gift #{gift_id} not found."))?;
        let unique = item
            .saved
            .info
            .unique
            .as_mut()
            .ok_or_else(|| "Only unique collectibles can be listed for resale.".to_owned())?;
        if resell_amount.is_empty() {
            unique.stars_for_resale = 0;
            unique.nano_ton_for_resale = 0;
            unique.only_accept_ton = false;
            return Ok(format!(
                "Unlisted \"{}\" from the resale marketplace (offline demo).",
                item.title
            ));
        }
        match resell_amount.credits_type() {
            CreditsType::Stars => {
                unique.stars_for_resale = resell_amount.whole();
                unique.nano_ton_for_resale = 0;
                unique.only_accept_ton = false;
                Ok(format!(
                    "Listed \"{}\" for {} Stars (offline demo).",
                    item.title,
                    resell_amount.whole()
                ))
            }
            CreditsType::Ton => {
                unique.stars_for_resale = 0;
                unique.nano_ton_for_resale =
                    resell_amount.whole() * 1_000_000_000 + resell_amount.nano();
                unique.only_accept_ton = true;
                Ok(format!(
                    "Listed \"{}\" for {} TON (offline demo).",
                    item.title,
                    resell_amount.whole()
                ))
            }
        }
    }

    fn buy_resale_star_gift(
        &mut self,
        slug: &str,
        recipient: &str,
        ton: bool,
        _message: &str,
    ) -> Result<String, String> {
        let target = if recipient.trim().is_empty() {
            "Alex River"
        } else {
            recipient.trim()
        };
        let currency = if ton { "TON" } else { "Stars" };
        Ok(format!(
            "Placeholder: Resale purchase of \"{slug}\" for {target} ({currency}) is simulated in offline demo."
        ))
    }

    fn load_active_auctions(&self) -> Result<ActiveAuctionsSnapshot, String> {
        Ok(ActiveAuctionsSnapshot {
            auctions: self.auctions.clone(),
        })
    }

    fn load_auction_state(&self, gift_id: u64, _version: i32) -> Result<GiftAuctionState, String> {
        self.auctions
            .iter()
            .find(|a| a.gift.as_ref().is_some_and(|g| g.id == gift_id))
            .cloned()
            .or_else(|| self.auctions.first().cloned())
            .ok_or_else(|| format!("Auction #{gift_id} not found."))
    }

    fn load_auction_acquired_gifts(
        &self,
        _gift_id: u64,
    ) -> Result<Vec<GiftAuctionAcquiredGift>, String> {
        Ok(self.acquired.clone())
    }

    fn place_auction_bid(
        &mut self,
        gift_id: u64,
        bid_amount: i64,
        _update_bid: bool,
        _hide_name: bool,
        _recipient: &str,
        _message: &str,
    ) -> Result<String, String> {
        let auction = self
            .auctions
            .iter_mut()
            .find(|a| a.gift.as_ref().is_some_and(|g| g.id == gift_id))
            .ok_or_else(|| format!("Auction #{gift_id} not found."))?;
        let min_req = auction.effective_min_bid();
        if bid_amount < min_req {
            return Err(format!("Bid must be at least {min_req} Stars."));
        }
        auction.my.bid = bid_amount;
        auction.my.date = DEMO_NOW;
        auction.my.min_bid_amount = bid_amount + 50;
        auction.version += 1;
        Ok(format!(
            "Bid of {bid_amount} Stars placed on auction #{gift_id} (offline demo)."
        ))
    }

    fn load_craft_star_gifts(
        &self,
        gift_id: u64,
        _offset: &str,
    ) -> Result<Vec<ReceivedGiftItem>, String> {
        Ok(self
            .gifts
            .iter()
            .filter(|g| {
                g.saved
                    .info
                    .unique
                    .as_ref()
                    .is_some_and(|u| !u.burned && (gift_id == 0 || u.initial_gift_id == gift_id))
            })
            .cloned()
            .collect())
    }

    fn craft_star_gift(&mut self, gift_ids: &[u64]) -> Result<CraftExecutionOutcome, String> {
        let mut slots = Vec::with_capacity(gift_ids.len());
        for &gid in gift_ids {
            let item = self
                .gifts
                .iter()
                .find(|g| g.saved.info.id == gid)
                .ok_or_else(|| format!("Craft input gift #{gid} not found."))?;
            let unique = item
                .saved
                .info
                .unique
                .clone()
                .ok_or_else(|| "Only unique gifts can be crafted.".to_owned())?;
            let manage_id = item.saved.manage_id.unwrap_or(SavedStarGiftId::Slug);
            slots.push(CraftGiftSlot { unique, manage_id });
        }
        validate_craft_slots(&slots, DEMO_NOW).map_err(|e| format!("Cannot craft: {e:?}"))?;
        let serial = self.next_serial;
        self.next_serial += 1;
        let base_id = slots[0].unique.initial_gift_id;
        let slug = format!("CrystalPrism-Craft-{serial}");
        let mut crafted = make_demo_unique(
            95_000 + i64::from(serial),
            base_id,
            &slug,
            "Crystal Prism",
            serial,
            "Forged Singularity",
            UniqueGiftRarity::Legendary as i32,
            &slots[0].unique.backdrop.attribute.name,
            slots[0].unique.backdrop.id,
            slots[0].unique.backdrop.center_color,
            slots[0].unique.backdrop.edge_color,
            &slots[0].unique.pattern.attribute.name,
            slots[0].unique.pattern.document_id.raw() as i64,
            0,
            0,
            0,
        );
        crafted.crafted = true;
        crafted.model.crafted = true;
        let title = crafted.display_name();

        self.gifts.retain(|g| !gift_ids.contains(&g.saved.info.id));
        self.gifts.insert(
            0,
            ReceivedGiftItem {
                saved: SavedStarGift {
                    info: StarGift {
                        id: crafted.id.raw(),
                        unique: Some(crafted),
                        stars: 50,
                        ..StarGift::default()
                    },
                    manage_id: Some(SavedStarGiftId::Slug),
                    message: "Forged in the Star Gift Craft Box!".to_owned(),
                    date: DEMO_NOW,
                    gift_num: serial,
                    mine: true,
                    ..SavedStarGift::default()
                },
                title: title.clone(),
                sender_name: "Crafting Forge".to_owned(),
                artwork_kind: "gem".to_owned(),
            },
        );
        self.refresh_collection_counts();
        Ok(CraftExecutionOutcome::Succeeded {
            result_title: title.clone(),
            result_slug: slug,
            notice: format!("Craft succeeded! Forged \"{title}\" (offline demo)."),
        })
    }

    fn load_giveaway_options(&self, channel_name: &str) -> Result<GiveawayOptionsBundle, String> {
        Ok(GiveawayOptionsBundle {
            channel_peer_id: PeerId::from_channel(ChannelId::from_tl(2_000_101)),
            channel_title: if channel_name.is_empty() {
                "Northwind Sound Lab".to_owned()
            } else {
                channel_name.to_owned()
            },
            premium_catalog: demo_gift_code_catalog(),
            stars_options: demo_stars_giveaway_options(),
            prepaid_giveaways: self.prepaid_giveaways.clone(),
            limits: GiveawayLimits::default(),
        })
    }

    fn create_or_launch_giveaway(&mut self, draft: &GiveawayDraft) -> Result<String, String> {
        draft
            .validate(DEMO_NOW, &GiveawayLimits::default())
            .map_err(|e| format!("Invalid giveaway draft: {e:?}"))?;
        if let Some(prepaid_id) = draft.prepaid_id {
            self.prepaid_giveaways.retain(|p| p.id != prepaid_id);
            return Ok(format!(
                "Launched prepaid giveaway #{prepaid_id} for {} winner(s) (offline demo).",
                draft.quantity
            ));
        }
        let desc = match draft.prize {
            GiveawayPrizeKind::Premium { months } => {
                format!("{} x {months}-month Telegram Premium", draft.quantity)
            }
            GiveawayPrizeKind::Stars { stars } => {
                format!(
                    "{stars} Telegram Stars split among {} winners",
                    draft.quantity
                )
            }
        };
        Ok(format!(
            "Giveaway invoice prepared ({desc}; offline demo, no real payment)."
        ))
    }

    fn load_giveaway_info(&self, _peer_name: &str, msg_id: i32) -> Result<GiveawayInfo, String> {
        Ok(GiveawayInfo {
            state: GiveawayState::Running,
            start_date: DEMO_NOW - 86_400,
            finish_date: DEMO_NOW + 86_400 * 2,
            winners_count: 10,
            participating: msg_id % 2 == 0,
            ..GiveawayInfo::default()
        })
    }

    fn load_boost_slots_for_channel(
        &self,
        channel_name: &str,
    ) -> Result<ReplaceBoostBundle, String> {
        let target = PeerId::from_channel(ChannelId::from_tl(2_000_101));
        let slots = parse_for_channel_boost_slots(target, &self.my_boost_slots, DEMO_NOW);
        Ok(ReplaceBoostBundle {
            channel_peer_id: target,
            channel_title: if channel_name.is_empty() {
                "Northwind Sound Lab".to_owned()
            } else {
                channel_name.to_owned()
            },
            slots,
            peer_names: vec![
                (
                    PeerId::from_channel(ChannelId::from_tl(2_000_101)),
                    "Northwind Sound Lab".to_owned(),
                ),
                (
                    PeerId::from_channel(ChannelId::from_tl(2_000_202)),
                    "Design Notes Channel".to_owned(),
                ),
                (
                    PeerId::from_channel(ChannelId::from_tl(2_000_303)),
                    "Rust Audio Builders".to_owned(),
                ),
            ],
            now: DEMO_NOW,
        })
    }

    fn apply_boost_slots(
        &mut self,
        _channel_name: &str,
        slots: &[i32],
    ) -> Result<Vec<MyBoostSlot>, String> {
        if slots.is_empty() {
            return Err("Select at least one boost slot to apply.".to_owned());
        }
        let target = PeerId::from_channel(ChannelId::from_tl(2_000_101));
        for &slot_id in slots {
            let entry = self
                .my_boost_slots
                .iter_mut()
                .find(|s| s.slot == slot_id)
                .ok_or_else(|| format!("Boost slot #{slot_id} not found."))?;
            if entry.peer_id.is_some() && entry.cooldown_until_date > DEMO_NOW {
                return Err(format!("Boost slot #{slot_id} is still on cooldown."));
            }
            entry.peer_id = Some(target);
            entry.date = DEMO_NOW;
            entry.cooldown_until_date = DEMO_NOW + 86_400;
        }
        Ok(self.my_boost_slots.clone())
    }

    fn load_starref_overview(
        &self,
        peer_name: &str,
        sort: SuggestedStarRefSort,
    ) -> Result<StarRefOverviewBundle, String> {
        let mut suggested = self.suggested_bots.clone();
        match sort {
            SuggestedStarRefSort::Profitability => {
                suggested.sort_by_key(|item| {
                    std::cmp::Reverse(
                        i64::from(item.program.commission_permille)
                            * item.program.daily_revenue_per_user.whole().max(1),
                    )
                });
            }
            SuggestedStarRefSort::Revenue => {
                suggested.sort_by_key(|item| {
                    std::cmp::Reverse(item.program.daily_revenue_per_user.whole())
                });
            }
            SuggestedStarRefSort::Date => {
                suggested.sort_by_key(|item| std::cmp::Reverse(item.program.bot_id.raw()));
            }
        }
        Ok(StarRefOverviewBundle {
            peer_name: if peer_name.is_empty() {
                "Alex River".to_owned()
            } else {
                peer_name.to_owned()
            },
            managed_bot_username: "northwind_audio_bot".to_owned(),
            managed_bot_name: "Northwind Audio Bot".to_owned(),
            managed_bot_id: UserId::from_tl(910_001),
            managed_program: self.managed_program.clone(),
            connected: self.connected_bots.clone(),
            suggested,
            sort,
        })
    }

    fn update_starref_program(
        &mut self,
        _bot_username: &str,
        commission_permille: i32,
        duration_months: i32,
    ) -> Result<StarRefProgram, String> {
        validate_star_ref_update(
            self.managed_program.as_ref(),
            commission_permille,
            duration_months,
            STARREF_COMMISSION_MIN_FALLBACK,
            STARREF_COMMISSION_MAX_FALLBACK,
        )
        .map_err(|e| format!("Invalid affiliate program terms: {e:?}"))?;
        let program = StarRefProgram {
            bot_id: UserId::from_tl(910_001),
            commission_permille,
            duration_months: if commission_permille > 0 {
                duration_months
            } else {
                0
            },
            end_date: if commission_permille == 0 {
                DEMO_NOW + 86_400 * 30
            } else {
                0
            },
            daily_revenue_per_user: CreditsAmount::new(2, 500_000_000, CreditsType::Stars),
        };
        self.managed_program = Some(program.clone());
        Ok(program)
    }

    fn connect_starref_bot(
        &mut self,
        _peer_name: &str,
        bot_username: &str,
    ) -> Result<ConnectedStarRefBot, String> {
        let cleaned = bot_username.trim().trim_start_matches('@');
        if let Some(existing) = self
            .connected_bots
            .iter()
            .find(|c| c.bot_username.eq_ignore_ascii_case(cleaned) && !c.info.revoked)
        {
            return Ok(existing.info.clone());
        }
        let sugg = self
            .suggested_bots
            .iter()
            .find(|s| s.bot_username.eq_ignore_ascii_case(cleaned))
            .cloned()
            .ok_or_else(|| format!("Affiliate program for @{cleaned} not found."))?;
        let info = ConnectedStarRefBot {
            bot_id: sugg.program.bot_id,
            url: format!(
                "https://t.me/{cleaned}?start=_tgr_demo_{}",
                sugg.program.bot_id.raw()
            ),
            date: DEMO_NOW,
            commission_permille: sugg.program.commission_permille,
            duration_months: sugg.program.duration_months,
            participants: 0,
            revenue: 0,
            revoked: false,
        };
        self.connected_bots.insert(
            0,
            ConnectedStarRefItem {
                bot_name: sugg.bot_name,
                bot_username: sugg.bot_username,
                info: info.clone(),
            },
        );
        Ok(info)
    }

    fn edit_connected_starref_bot(
        &mut self,
        _peer_name: &str,
        link: &str,
        revoked: bool,
    ) -> Result<ConnectedStarRefBot, String> {
        let entry = self
            .connected_bots
            .iter_mut()
            .find(|c| c.info.url == link)
            .ok_or_else(|| format!("Connected affiliate link '{link}' not found."))?;
        entry.info.revoked = revoked;
        Ok(entry.info.clone())
    }

    fn withdraw_star_gift(
        &mut self,
        gift_id: u64,
        password: &str,
        keep_on_profile: bool,
    ) -> Result<String, String> {
        let trimmed = password.trim();
        if trimmed.is_empty() {
            return Err("Enter your Two-Step Verification password to confirm.".to_owned());
        }
        if trimmed != crate::payments::mock::DEMO_CLOUD_PASSWORD {
            return Err(format!(
                "Incorrect 2FA cloud password (hint: use '{}' in offline demo mode).",
                crate::payments::mock::DEMO_CLOUD_PASSWORD
            ));
        }
        let Some(pos) = self.gifts.iter().position(|g| g.saved.info.id == gift_id) else {
            return Err(format!("Gift #{gift_id} not found."));
        };
        if self.gifts[pos].saved.info.unique.is_none() {
            return Err(
                "Only upgraded unique collectible gifts can be withdrawn to TON.".to_owned(),
            );
        }
        let title = self.gifts[pos].title.clone();
        self.withdrawn_gifts.push((gift_id, keep_on_profile));
        if keep_on_profile {
            if let Some(ref mut unique) = self.gifts[pos].saved.info.unique {
                unique.owner_address = format!("EQDemoTonWallet{gift_id:08x}");
            }
        } else {
            self.gifts.remove(pos);
            self.refresh_collection_counts();
        }
        Ok(format!(
            "Simulated TON withdrawal queued for \"{title}\" ({}).",
            if keep_on_profile {
                "kept visible on profile"
            } else {
                "removed from profile"
            }
        ))
    }

    fn get_star_gift_withdrawal_url(
        &mut self,
        gift_id: u64,
        password: &str,
    ) -> Result<String, String> {
        let trimmed = password.trim();
        if trimmed.is_empty() {
            return Err("Enter your Two-Step Verification password to confirm.".to_owned());
        }
        if trimmed != crate::payments::mock::DEMO_CLOUD_PASSWORD {
            return Err(format!(
                "Incorrect 2FA cloud password (hint: use '{}' in offline demo mode).",
                crate::payments::mock::DEMO_CLOUD_PASSWORD
            ));
        }
        let Some(item) = self.gifts.iter().find(|g| g.saved.info.id == gift_id) else {
            return Err(format!("Gift #{gift_id} not found."));
        };
        let Some(ref unique) = item.saved.info.unique else {
            return Err(
                "Only upgraded unique collectible gifts have a blockchain withdrawal link."
                    .to_owned(),
            );
        };
        let url = format!(
            "https://fragment.com/nft/withdraw/{}?gift={gift_id}&demo=1",
            unique.slug
        );
        self.withdrawal_urls.push((gift_id, url.clone()));
        Ok(url)
    }

    fn delete_star_gift(&mut self, gift_id: u64) -> Result<String, String> {
        let Some(pos) = self.gifts.iter().position(|g| g.saved.info.id == gift_id) else {
            return Err(format!("Gift #{gift_id} not found."));
        };
        let removed = self.gifts.remove(pos);
        if let Some(ref unique) = removed.saved.info.unique
            && self.worn_collectible_id == Some(unique.id)
        {
            self.worn_collectible_id = None;
        }
        self.refresh_collection_counts();
        Ok(format!("Deleted \"{}\" permanently.", removed.title))
    }

    fn get_saved_star_gift(&self, gift_id: u64) -> Result<ReceivedGiftItem, String> {
        self.gifts
            .iter()
            .find(|g| g.saved.info.id == gift_id)
            .cloned()
            .ok_or_else(|| format!("Gift #{gift_id} not found."))
    }

    fn chat_star_gift_notifications_enabled(&self, _peer_name: &str) -> bool {
        self.chat_notifications_enabled
    }

    fn toggle_chat_star_gift_notifications(
        &mut self,
        _peer_name: &str,
        enabled: bool,
    ) -> Result<bool, String> {
        self.chat_notifications_enabled = enabled;
        Ok(enabled)
    }
}

// Per-instance registry so `MockPaymentsBackend` and `MockSession` gain full WP-16
// demo data without requiring edits to `stand_in/mock_session.rs` or `payments/mock.rs` structs.
fn mock_marketplace_registry() -> &'static Mutex<HashMap<usize, MockMarketplaceState>> {
    static REGISTRY: OnceLock<Mutex<HashMap<usize, MockMarketplaceState>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

#[doc(hidden)]
pub fn with_mock_marketplace<R>(key: usize, f: impl FnOnce(&mut MockMarketplaceState) -> R) -> R {
    let mut map = mock_marketplace_registry()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let st = map.entry(key).or_default();
    f(st)
}

#[doc(hidden)]
pub mod __macro_reexports {
    pub use tg_data::{CollectibleId, TimeId};
    pub use tg_payments::credits::{
        CreditsAmount, CreditsGiveawayOption, CreditsGiveawayWinnerOption, CreditsType,
    };
    pub use tg_payments::giveaways::{
        GiveawayDraft, GiveawayLimits, GiveawayPrizeKind, PremiumGiftCodeCatalog,
    };
    pub use tg_payments::premium::{BoostPrepaidGiveaway, GiveawayInfo, MyBoostSlot};
    pub use tg_payments::star_gift_auctions::{
        ActiveAuctionsSnapshot, GiftAuctionAcquiredGift, GiftAuctionState, StarGiftAuctionMyState,
    };
    pub use tg_payments::star_gift_craft::CraftGiftSlot;
    pub use tg_payments::star_gifts::{
        GiftCollection, PeerGiftsFilter, ResaleGiftsDescriptor, ResaleGiftsFilter, ResaleGiftsSort,
        SavedStarGift, SavedStarGiftId, StarGift,
    };
    pub use tg_payments::starref::{ConnectedStarRefBot, StarRefProgram, SuggestedStarRefSort};

    pub use super::with_mock_marketplace;
    pub use crate::payments::backend::ReceivedGiftItem;
    pub use crate::payments::marketplace_backend::{
        ConnectedStarRefItem, CraftExecutionOutcome, GiveawayOptionsBundle, PeerGiftsPage,
        ReplaceBoostBundle, StarGiftsMarketplaceBackend, StarRefOverviewBundle,
        SuggestedStarRefItem, UniqueGiftDetailBundle,
    };
}

/// Delegates [`StarGiftsMarketplaceBackend`] for `$ty` via the per-instance mock registry.
#[macro_export]
macro_rules! delegate_marketplace_via_registry {
    ($ty:ty) => {
        impl $crate::payments::mock_marketplace::__macro_reexports::StarGiftsMarketplaceBackend for $ty {
            fn load_peer_gifts(
                &self,
                peer_name: &str,
                filter: &$crate::payments::mock_marketplace::__macro_reexports::PeerGiftsFilter,
                offset: &str,
            ) -> Result<$crate::payments::mock_marketplace::__macro_reexports::PeerGiftsPage, String> {
                let key = (self as *const Self) as usize;
                $crate::payments::mock_marketplace::__macro_reexports::with_mock_marketplace(key, |st| st.load_peer_gifts(peer_name, filter, offset))
            }

            fn get_unique_star_gift(&self, slug: &str) -> Result<$crate::payments::mock_marketplace::__macro_reexports::UniqueGiftDetailBundle, String> {
                let key = (self as *const Self) as usize;
                $crate::payments::mock_marketplace::__macro_reexports::with_mock_marketplace(key, |st| st.get_unique_star_gift(slug))
            }

            fn upgrade_star_gift(
                &mut self,
                gift_id: u64,
                keep_original_details: bool,
            ) -> Result<String, String> {
                let key = (self as *const Self) as usize;
                $crate::payments::mock_marketplace::__macro_reexports::with_mock_marketplace(key, |st| {
                    st.upgrade_star_gift(gift_id, keep_original_details)
                })
            }

            fn transfer_star_gift(
                &mut self,
                gift_id: u64,
                recipient: &str,
            ) -> Result<String, String> {
                let key = (self as *const Self) as usize;
                $crate::payments::mock_marketplace::__macro_reexports::with_mock_marketplace(key, |st| st.transfer_star_gift(gift_id, recipient))
            }

            fn convert_star_gift(&mut self, gift_id: u64) -> Result<String, String> {
                let key = (self as *const Self) as usize;
                $crate::payments::mock_marketplace::__macro_reexports::with_mock_marketplace(key, |st| st.convert_star_gift(gift_id))
            }

            fn save_star_gift(&mut self, gift_id: u64, unsave: bool) -> Result<String, String> {
                let key = (self as *const Self) as usize;
                $crate::payments::mock_marketplace::__macro_reexports::with_mock_marketplace(key, |st| st.save_star_gift(gift_id, unsave))
            }

            fn toggle_star_gifts_pinned_to_top(
                &mut self,
                gift_ids: &[u64],
            ) -> Result<String, String> {
                let key = (self as *const Self) as usize;
                $crate::payments::mock_marketplace::__macro_reexports::with_mock_marketplace(key, |st| st.toggle_star_gifts_pinned_to_top(gift_ids))
            }

            fn wear_unique_gift(
                &mut self,
                collectible_id: Option<$crate::payments::mock_marketplace::__macro_reexports::CollectibleId>,
                until: Option<$crate::payments::mock_marketplace::__macro_reexports::TimeId>,
            ) -> Result<String, String> {
                let key = (self as *const Self) as usize;
                $crate::payments::mock_marketplace::__macro_reexports::with_mock_marketplace(key, |st| st.wear_unique_gift(collectible_id, until))
            }

            fn load_star_gift_collections(
                &self,
                peer_name: &str,
            ) -> Result<Vec<$crate::payments::mock_marketplace::__macro_reexports::GiftCollection>, String> {
                let key = (self as *const Self) as usize;
                $crate::payments::mock_marketplace::__macro_reexports::with_mock_marketplace(key, |st| st.load_star_gift_collections(peer_name))
            }

            fn create_star_gift_collection(
                &mut self,
                peer_name: &str,
                title: &str,
                gift_ids: &[u64],
            ) -> Result<$crate::payments::mock_marketplace::__macro_reexports::GiftCollection, String> {
                let key = (self as *const Self) as usize;
                $crate::payments::mock_marketplace::__macro_reexports::with_mock_marketplace(key, |st| {
                    st.create_star_gift_collection(peer_name, title, gift_ids)
                })
            }

            fn update_star_gift_collection(
                &mut self,
                peer_name: &str,
                collection_id: i32,
                title: Option<&str>,
                add_gift_ids: &[u64],
                delete_gift_ids: &[u64],
            ) -> Result<$crate::payments::mock_marketplace::__macro_reexports::GiftCollection, String> {
                let key = (self as *const Self) as usize;
                $crate::payments::mock_marketplace::__macro_reexports::with_mock_marketplace(key, |st| {
                    st.update_star_gift_collection(
                        peer_name,
                        collection_id,
                        title,
                        add_gift_ids,
                        delete_gift_ids,
                    )
                })
            }

            fn reorder_star_gift_collections(
                &mut self,
                peer_name: &str,
                order: &[i32],
            ) -> Result<String, String> {
                let key = (self as *const Self) as usize;
                $crate::payments::mock_marketplace::__macro_reexports::with_mock_marketplace(key, |st| st.reorder_star_gift_collections(peer_name, order))
            }

            fn delete_star_gift_collection(
                &mut self,
                peer_name: &str,
                collection_id: i32,
            ) -> Result<String, String> {
                let key = (self as *const Self) as usize;
                $crate::payments::mock_marketplace::__macro_reexports::with_mock_marketplace(key, |st| {
                    st.delete_star_gift_collection(peer_name, collection_id)
                })
            }

            fn load_resale_star_gifts(
                &self,
                gift_id: u64,
                filter: &$crate::payments::mock_marketplace::__macro_reexports::ResaleGiftsFilter,
                offset: &str,
            ) -> Result<$crate::payments::mock_marketplace::__macro_reexports::ResaleGiftsDescriptor, String> {
                let key = (self as *const Self) as usize;
                $crate::payments::mock_marketplace::__macro_reexports::with_mock_marketplace(key, |st| st.load_resale_star_gifts(gift_id, filter, offset))
            }

            fn update_star_gift_price(
                &mut self,
                gift_id: u64,
                resell_amount: $crate::payments::mock_marketplace::__macro_reexports::CreditsAmount,
            ) -> Result<String, String> {
                let key = (self as *const Self) as usize;
                $crate::payments::mock_marketplace::__macro_reexports::with_mock_marketplace(key, |st| st.update_star_gift_price(gift_id, resell_amount))
            }

            fn buy_resale_star_gift(
                &mut self,
                slug: &str,
                recipient: &str,
                ton: bool,
                message: &str,
            ) -> Result<String, String> {
                let key = (self as *const Self) as usize;
                $crate::payments::mock_marketplace::__macro_reexports::with_mock_marketplace(key, |st| {
                    st.buy_resale_star_gift(slug, recipient, ton, message)
                })
            }

            fn load_active_auctions(&self) -> Result<$crate::payments::mock_marketplace::__macro_reexports::ActiveAuctionsSnapshot, String> {
                let key = (self as *const Self) as usize;
                $crate::payments::mock_marketplace::__macro_reexports::with_mock_marketplace(key, |st| st.load_active_auctions())
            }

            fn load_auction_state(
                &self,
                gift_id: u64,
                version: i32,
            ) -> Result<$crate::payments::mock_marketplace::__macro_reexports::GiftAuctionState, String> {
                let key = (self as *const Self) as usize;
                $crate::payments::mock_marketplace::__macro_reexports::with_mock_marketplace(key, |st| st.load_auction_state(gift_id, version))
            }

            fn load_auction_acquired_gifts(
                &self,
                gift_id: u64,
            ) -> Result<Vec<$crate::payments::mock_marketplace::__macro_reexports::GiftAuctionAcquiredGift>, String> {
                let key = (self as *const Self) as usize;
                $crate::payments::mock_marketplace::__macro_reexports::with_mock_marketplace(key, |st| st.load_auction_acquired_gifts(gift_id))
            }

            fn place_auction_bid(
                &mut self,
                gift_id: u64,
                bid_amount: i64,
                update_bid: bool,
                hide_name: bool,
                recipient: &str,
                message: &str,
            ) -> Result<String, String> {
                let key = (self as *const Self) as usize;
                $crate::payments::mock_marketplace::__macro_reexports::with_mock_marketplace(key, |st| {
                    st.place_auction_bid(
                        gift_id, bid_amount, update_bid, hide_name, recipient, message,
                    )
                })
            }

            fn load_craft_star_gifts(
                &self,
                gift_id: u64,
                offset: &str,
            ) -> Result<Vec<$crate::payments::mock_marketplace::__macro_reexports::ReceivedGiftItem>, String> {
                let key = (self as *const Self) as usize;
                $crate::payments::mock_marketplace::__macro_reexports::with_mock_marketplace(key, |st| st.load_craft_star_gifts(gift_id, offset))
            }

            fn craft_star_gift(
                &mut self,
                gift_ids: &[u64],
            ) -> Result<$crate::payments::mock_marketplace::__macro_reexports::CraftExecutionOutcome, String> {
                let key = (self as *const Self) as usize;
                $crate::payments::mock_marketplace::__macro_reexports::with_mock_marketplace(key, |st| st.craft_star_gift(gift_ids))
            }

            fn load_giveaway_options(
                &self,
                channel_name: &str,
            ) -> Result<$crate::payments::mock_marketplace::__macro_reexports::GiveawayOptionsBundle, String> {
                let key = (self as *const Self) as usize;
                $crate::payments::mock_marketplace::__macro_reexports::with_mock_marketplace(key, |st| st.load_giveaway_options(channel_name))
            }

            fn create_or_launch_giveaway(
                &mut self,
                draft: &$crate::payments::mock_marketplace::__macro_reexports::GiveawayDraft,
            ) -> Result<String, String> {
                let key = (self as *const Self) as usize;
                $crate::payments::mock_marketplace::__macro_reexports::with_mock_marketplace(key, |st| st.create_or_launch_giveaway(draft))
            }

            fn load_giveaway_info(
                &self,
                peer_name: &str,
                msg_id: i32,
            ) -> Result<$crate::payments::mock_marketplace::__macro_reexports::GiveawayInfo, String> {
                let key = (self as *const Self) as usize;
                $crate::payments::mock_marketplace::__macro_reexports::with_mock_marketplace(key, |st| st.load_giveaway_info(peer_name, msg_id))
            }

            fn load_boost_slots_for_channel(
                &self,
                channel_name: &str,
            ) -> Result<$crate::payments::mock_marketplace::__macro_reexports::ReplaceBoostBundle, String> {
                let key = (self as *const Self) as usize;
                $crate::payments::mock_marketplace::__macro_reexports::with_mock_marketplace(key, |st| st.load_boost_slots_for_channel(channel_name))
            }

            fn apply_boost_slots(
                &mut self,
                channel_name: &str,
                slots: &[i32],
            ) -> Result<Vec<$crate::payments::mock_marketplace::__macro_reexports::MyBoostSlot>, String> {
                let key = (self as *const Self) as usize;
                $crate::payments::mock_marketplace::__macro_reexports::with_mock_marketplace(key, |st| st.apply_boost_slots(channel_name, slots))
            }

            fn load_starref_overview(
                &self,
                peer_name: &str,
                sort: $crate::payments::mock_marketplace::__macro_reexports::SuggestedStarRefSort,
            ) -> Result<$crate::payments::mock_marketplace::__macro_reexports::StarRefOverviewBundle, String> {
                let key = (self as *const Self) as usize;
                $crate::payments::mock_marketplace::__macro_reexports::with_mock_marketplace(key, |st| st.load_starref_overview(peer_name, sort))
            }

            fn update_starref_program(
                &mut self,
                bot_username: &str,
                commission_permille: i32,
                duration_months: i32,
            ) -> Result<$crate::payments::mock_marketplace::__macro_reexports::StarRefProgram, String> {
                let key = (self as *const Self) as usize;
                $crate::payments::mock_marketplace::__macro_reexports::with_mock_marketplace(key, |st| {
                    st.update_starref_program(bot_username, commission_permille, duration_months)
                })
            }

            fn connect_starref_bot(
                &mut self,
                peer_name: &str,
                bot_username: &str,
            ) -> Result<$crate::payments::mock_marketplace::__macro_reexports::ConnectedStarRefBot, String> {
                let key = (self as *const Self) as usize;
                $crate::payments::mock_marketplace::__macro_reexports::with_mock_marketplace(key, |st| st.connect_starref_bot(peer_name, bot_username))
            }

            fn edit_connected_starref_bot(
                &mut self,
                peer_name: &str,
                link: &str,
                revoked: bool,
            ) -> Result<$crate::payments::mock_marketplace::__macro_reexports::ConnectedStarRefBot, String> {
                let key = (self as *const Self) as usize;
                $crate::payments::mock_marketplace::__macro_reexports::with_mock_marketplace(key, |st| {
                    st.edit_connected_starref_bot(peer_name, link, revoked)
                })
            }

            fn withdraw_star_gift(
                &mut self,
                gift_id: u64,
                password: &str,
                keep_on_profile: bool,
            ) -> Result<String, String> {
                let key = (self as *const Self) as usize;
                $crate::payments::mock_marketplace::__macro_reexports::with_mock_marketplace(key, |st| {
                    st.withdraw_star_gift(gift_id, password, keep_on_profile)
                })
            }

            fn get_star_gift_withdrawal_url(
                &mut self,
                gift_id: u64,
                password: &str,
            ) -> Result<String, String> {
                let key = (self as *const Self) as usize;
                $crate::payments::mock_marketplace::__macro_reexports::with_mock_marketplace(key, |st| {
                    st.get_star_gift_withdrawal_url(gift_id, password)
                })
            }

            fn delete_star_gift(&mut self, gift_id: u64) -> Result<String, String> {
                let key = (self as *const Self) as usize;
                $crate::payments::mock_marketplace::__macro_reexports::with_mock_marketplace(key, |st| {
                    st.delete_star_gift(gift_id)
                })
            }

            fn get_saved_star_gift(
                &self,
                gift_id: u64,
            ) -> Result<$crate::payments::mock_marketplace::__macro_reexports::ReceivedGiftItem, String> {
                let key = (self as *const Self) as usize;
                $crate::payments::mock_marketplace::__macro_reexports::with_mock_marketplace(key, |st| {
                    st.get_saved_star_gift(gift_id)
                })
            }

            fn chat_star_gift_notifications_enabled(&self, peer_name: &str) -> bool {
                let key = (self as *const Self) as usize;
                $crate::payments::mock_marketplace::__macro_reexports::with_mock_marketplace(key, |st| {
                    st.chat_star_gift_notifications_enabled(peer_name)
                })
            }

            fn toggle_chat_star_gift_notifications(
                &mut self,
                peer_name: &str,
                enabled: bool,
            ) -> Result<bool, String> {
                let key = (self as *const Self) as usize;
                $crate::payments::mock_marketplace::__macro_reexports::with_mock_marketplace(key, |st| {
                    st.toggle_chat_star_gift_notifications(peer_name, enabled)
                })
            }
        }
    };
}

delegate_marketplace_via_registry!(MockPaymentsBackend);
