//! Unit & known-answer tests for WP-16 models in `tg-payments`:
//! unique gifts, collections, peer gifts filtering, wear/unwear, resale, auctions,
//! crafting, giveaways, boost replacement, and Stars affiliate programs.

use tg_data::{ChannelId, CollectibleId, DocumentId, MsgId, PeerId, UserId};
use tg_payments::credits::{CreditsAmount, CreditsType};
use tg_payments::giveaways::{
    ForChannelBoostSlots, GIVEAWAY_ADDITIONAL_PRIZE_MAX_LEN, GiveawayDraft, GiveawayLimits,
    GiveawayPrizeKind, GiveawayValidationError, PremiumGiftCodeCatalog, TakenBoostSlot,
    build_apply_boost, build_get_premium_gift_code_options, build_launch_prepaid_giveaway,
    format_boost_cooldown, giveaway_results_from_tl, parse_for_channel_boost_slots,
    sort_taken_boost_slots,
};
use tg_payments::premium::MyBoostSlot;
use tg_payments::star_gift_auctions::{
    GiftAuctionBidLevel, GiftAuctionState, StarGiftAuctionMyState, acquired_gifts_from_tl,
    active_auctions_from_tl, apply_auction_state_tl, auction_state_from_tl,
    build_auction_bid_invoice, build_get_star_gift_active_auctions,
    build_get_star_gift_auction_acquired_gifts, build_get_star_gift_auction_state_by_id,
    build_get_star_gift_auction_state_by_slug, compute_active_auctions_hash,
    format_auction_countdown,
};
use tg_payments::star_gift_craft::{
    CraftEligibilityError, CraftGiftSlot, MAX_CRAFT_SLOTS, build_craft_star_gift,
    build_get_craft_star_gifts, compute_craft_plan, compute_craft_success_permille,
    format_craft_percent, lookup_craft_attribute_permille, parse_craft_too_early_error,
    validate_craft_slots,
};
use tg_payments::star_gifts::{
    COLLECTION_NAME_LIMIT, PeerGiftsFilter, RgbColor, SavedStarGift, SavedStarGiftId, StarGift,
    UniqueGift, UniqueGiftAttribute, UniqueGiftBackdrop, UniqueGiftModel, UniqueGiftPattern,
    build_channel_wear_unique_gift, build_create_star_gift_collection,
    build_delete_star_gift_collection, build_get_saved_star_gifts_filtered,
    build_get_star_gift_collections, build_get_star_gift_upgrade_attributes,
    build_get_star_gift_upgrade_preview, build_get_unique_star_gift,
    build_get_unique_star_gift_value_info, build_reorder_star_gift_collections,
    build_resale_invoice, build_resolve_star_gift_offer, build_send_star_gift_offer,
    build_toggle_star_gifts_pinned_to_top, build_transfer_invoice, build_unwear_emoji_status,
    build_update_star_gift_collection, build_upgrade_invoice, build_wear_unique_gift,
    gift_collection_from_tl, gift_collections_from_tl, star_gift_upgrade_attributes_from_tl,
    star_gift_upgrade_preview_from_tl, unique_gift_value_from_tl, unique_star_gift_from_tl,
    validate_collection_title,
};
use tg_payments::starref::{
    STARREF_COMMISSION_MAX_FALLBACK, STARREF_COMMISSION_MIN_FALLBACK, StarRefProgram,
    StarRefValidationError, SuggestedStarRefSort, build_connect_star_ref_bot,
    build_edit_connected_star_ref_bot, build_finish_star_ref_program,
    build_get_connected_star_ref_bot, build_get_connected_star_ref_bots,
    build_get_suggested_star_ref_bots, build_update_star_ref_program,
    connected_star_ref_bots_from_tl, format_commission, format_program_duration,
    star_ref_program_from_tl, suggested_star_ref_bots_from_tl, validate_star_ref_update,
};
use tg_tl::{Serializable, enums as tl_enums, types as tl_types};

#[allow(clippy::too_many_arguments)]
fn make_unique(
    id: i64,
    initial_gift_id: u64,
    slug: &str,
    backdrop_id: i32,
    backdrop_name: &str,
    pattern_doc: i64,
    pattern_name: &str,
    craft_permille: i32,
) -> UniqueGift {
    UniqueGift {
        id: CollectibleId::from_tl(id),
        initial_gift_id,
        slug: slug.to_owned(),
        title: "Aurora Gem".to_owned(),
        number: id as i32,
        craft_chance_permille: craft_permille,
        model: UniqueGiftModel {
            attribute: UniqueGiftAttribute {
                name: "Prism".to_owned(),
                rarity_value: 25,
            },
            document_id: DocumentId::from_tl(500 + id),
            crafted: false,
        },
        backdrop: UniqueGiftBackdrop {
            attribute: UniqueGiftAttribute {
                name: backdrop_name.to_owned(),
                rarity_value: 40,
            },
            id: backdrop_id,
            center_color: RgbColor::new(0x20, 0x60, 0xA0),
            edge_color: RgbColor::new(0x10, 0x30, 0x55),
            pattern_color: RgbColor::new(0x80, 0xC0, 0xFF),
            text_color: RgbColor::new(0xFF, 0xFF, 0xFF),
        },
        pattern: UniqueGiftPattern {
            attribute: UniqueGiftAttribute {
                name: pattern_name.to_owned(),
                rarity_value: 15,
            },
            document_id: DocumentId::from_tl(pattern_doc),
        },
        ..UniqueGift::default()
    }
}

#[test]
fn peer_gifts_filter_and_collections_builders_and_validation() {
    assert_eq!(COLLECTION_NAME_LIMIT, 12);
    assert_eq!(
        validate_collection_title("  Favorites  "),
        Ok("Favorites".to_owned())
    );
    assert!(validate_collection_title("   ").is_err());
    assert!(validate_collection_title("ThirteenChars").is_err());

    let unique_gift = SavedStarGift {
        info: StarGift {
            id: 1,
            unique: Some(make_unique(
                1,
                100,
                "AuroraGem-1",
                1,
                "Cobalt",
                7,
                "Star",
                250,
            )),
            ..StarGift::default()
        },
        collection_ids: vec![42],
        hidden: false,
        ..SavedStarGift::default()
    };
    let regular_limited = SavedStarGift {
        info: StarGift {
            id: 2,
            limited_count: 500,
            limited_left: 10,
            upgradable: true,
            ..StarGift::default()
        },
        upgradable: true,
        hidden: true,
        ..SavedStarGift::default()
    };

    let mut filter = PeerGiftsFilter {
        sort_by_value: true,
        skip_unsaved: true,
        collection_id: Some(42),
        ..PeerGiftsFilter::default()
    };
    assert!(filter.skips_something());
    assert!(filter.matches(&unique_gift));
    assert!(!filter.matches(&regular_limited));

    filter.skip_unique = true;
    assert!(!filter.matches(&unique_gift));

    let req = build_get_saved_star_gifts_filtered(tl_enums::InputPeer::Self_, &filter, "off1", 25);
    assert!(req.sort_by_value);
    assert!(req.exclude_unsaved);
    assert!(req.exclude_unique);
    assert_eq!(req.collection_id, Some(42));
    assert_eq!(req.limit, 25);

    // Collections TL parsing & builders
    let tl_col = tl_enums::StarGiftCollection::StarGiftCollection(tl_types::StarGiftCollection {
        collection_id: 42,
        title: "Favorites".to_owned(),
        icon: Some(tl_enums::Document::Empty(tl_types::DocumentEmpty {
            id: 909,
        })),
        gifts_count: 3,
        hash: 123456,
    });
    let parsed_col = gift_collection_from_tl(&tl_col);
    assert_eq!(parsed_col.id, 42);
    assert_eq!(parsed_col.count, 3);
    assert_eq!(parsed_col.icon_document_id, Some(DocumentId::from_tl(909)));

    assert!(
        gift_collections_from_tl(&tl_enums::payments::StarGiftCollections::NotModified).is_none()
    );
    let list = gift_collections_from_tl(
        &tl_enums::payments::StarGiftCollections::StarGiftCollections(
            tl_types::payments::StarGiftCollections {
                collections: vec![tl_col],
            },
        ),
    )
    .expect("collections list");
    assert_eq!(list.len(), 1);

    let sg_in = tl_enums::InputSavedStarGift::User(tl_types::InputSavedStarGiftUser { msg_id: 77 });
    let _ = build_get_star_gift_collections(tl_enums::InputPeer::Self_, 0).to_bytes();
    let _ =
        build_create_star_gift_collection(tl_enums::InputPeer::Self_, "Vault", vec![sg_in.clone()])
            .to_bytes();
    let _ = build_update_star_gift_collection(
        tl_enums::InputPeer::Self_,
        42,
        Some("Renamed".to_owned()),
        None,
        Some(vec![sg_in.clone()]),
        None,
    )
    .to_bytes();
    let _ = build_reorder_star_gift_collections(tl_enums::InputPeer::Self_, vec![42, 7]).to_bytes();
    let _ = build_delete_star_gift_collection(tl_enums::InputPeer::Self_, 42).to_bytes();
    let _ = build_toggle_star_gifts_pinned_to_top(tl_enums::InputPeer::Self_, vec![sg_in.clone()])
        .to_bytes();

    // Wear / unwear emoji status builders
    let wear = build_wear_unique_gift(CollectibleId::from_tl(555), Some(1_800_000_000));
    assert!(matches!(
        wear.emoji_status,
        tl_enums::EmojiStatus::InputEmojiStatusCollectible(tl_types::InputEmojiStatusCollectible {
            collectible_id: 555,
            until: Some(1_800_000_000),
        })
    ));
    let unwear = build_unwear_emoji_status();
    assert!(matches!(unwear.emoji_status, tl_enums::EmojiStatus::Empty));
    let ch_wear = build_channel_wear_unique_gift(
        tl_enums::InputChannel::Empty,
        Some(CollectibleId::from_tl(555)),
        None,
    );
    assert!(matches!(
        ch_wear.emoji_status,
        tl_enums::EmojiStatus::InputEmojiStatusCollectible(_)
    ));

    // Unique detail, value info, upgrade preview/attributes, offers & invoices
    let _ = build_get_unique_star_gift("AuroraGem-1").to_bytes();
    let _ = build_get_unique_star_gift_value_info("AuroraGem-1").to_bytes();
    let _ = build_get_star_gift_upgrade_preview(100).to_bytes();
    let _ = build_get_star_gift_upgrade_attributes(100).to_bytes();
    let _ = build_send_star_gift_offer(
        tl_enums::InputPeer::Self_,
        "AuroraGem-1",
        CreditsAmount::stars(500),
        86_400,
        999,
        None,
    )
    .to_bytes();
    let _ = build_resolve_star_gift_offer(123, false).to_bytes();
    let _ = build_resale_invoice("AuroraGem-1", tl_enums::InputPeer::Self_, false, true, None)
        .to_bytes();
    let _ = build_upgrade_invoice(sg_in.clone(), true).to_bytes();
    let _ = build_transfer_invoice(sg_in, tl_enums::InputPeer::Self_).to_bytes();

    let val_tl = tl_enums::payments::UniqueStarGiftValueInfo::UniqueStarGiftValueInfo(
        tl_types::payments::UniqueStarGiftValueInfo {
            last_sale_on_fragment: true,
            value_is_average: false,
            currency: "USD".to_owned(),
            value: 4500,
            initial_sale_date: 1_760_000_000,
            initial_sale_stars: 500,
            initial_sale_price: 1000,
            last_sale_date: Some(1_763_000_000),
            last_sale_price: Some(4200),
            floor_price: Some(3900),
            average_price: Some(4300),
            listed_count: Some(12),
            fragment_listed_count: Some(4),
            fragment_listed_url: Some("https://fragment.com/gift/AuroraGem-1".to_owned()),
        },
    );
    let val = unique_gift_value_from_tl(&val_tl);
    assert_eq!(val.value_price, 4500);
    assert_eq!(val.minimum_price, 3900);
    assert_eq!(val.for_sale_on_telegram, 12);
    assert!(val.last_sale_fragment);

    let prev_tl = tl_enums::payments::StarGiftUpgradePreview::StarGiftUpgradePreview(
        tl_types::payments::StarGiftUpgradePreview {
            sample_attributes: vec![tl_enums::StarGiftAttribute::Backdrop(
                tl_types::StarGiftAttributeBackdrop {
                    name: "Cobalt".to_owned(),
                    backdrop_id: 1,
                    center_color: 0x2060A0,
                    edge_color: 0x103055,
                    pattern_color: 0x80C0FF,
                    text_color: 0xFFFFFF,
                    rarity: tl_enums::StarGiftAttributeRarity::Rare,
                },
            )],
            prices: vec![tl_enums::StarGiftUpgradePrice::StarGiftUpgradePrice(
                tl_types::StarGiftUpgradePrice {
                    date: 1_760_000_000,
                    upgrade_stars: 25,
                },
            )],
            next_prices: Vec::new(),
        },
    );
    let preview = star_gift_upgrade_preview_from_tl(&prev_tl);
    assert_eq!(preview.sample_attributes.backdrops.len(), 1);
    assert_eq!(preview.prices[0].upgrade_stars, 25);

    let attrs_tl = tl_enums::payments::StarGiftUpgradeAttributes::StarGiftUpgradeAttributes(
        tl_types::payments::StarGiftUpgradeAttributes {
            attributes: vec![tl_enums::StarGiftAttribute::Model(
                tl_types::StarGiftAttributeModel {
                    crafted: true,
                    name: "Solar".to_owned(),
                    document: tl_enums::Document::Empty(tl_types::DocumentEmpty { id: 888 }),
                    rarity: tl_enums::StarGiftAttributeRarity::Legendary,
                },
            )],
        },
    );
    let attrs = star_gift_upgrade_attributes_from_tl(&attrs_tl);
    assert_eq!(attrs.models.len(), 1);
    assert!(attrs.models[0].crafted);

    let u_tl =
        tl_enums::payments::UniqueStarGift::UniqueStarGift(tl_types::payments::UniqueStarGift {
            gift: tl_enums::StarGift::StarGift(tl_types::StarGift {
                limited: false,
                sold_out: false,
                birthday: false,
                require_premium: false,
                limited_per_user: false,
                peer_color_available: false,
                auction: false,
                id: 100,
                sticker: tl_enums::Document::Empty(tl_types::DocumentEmpty { id: 10 }),
                stars: 50,
                availability_remains: None,
                availability_total: None,
                availability_resale: None,
                convert_stars: 42,
                first_sale_date: None,
                last_sale_date: None,
                upgrade_stars: None,
                resell_min_stars: None,
                title: Some("Gem".to_owned()),
                released_by: None,
                per_user_total: None,
                per_user_remains: None,
                locked_until_date: None,
                auction_slug: None,
                gifts_per_round: None,
                auction_start_date: None,
                upgrade_variants: None,
                background: None,
            }),
            chats: Vec::new(),
            users: Vec::new(),
        });
    assert_eq!(unique_star_gift_from_tl(&u_tl).id, 100);
}

#[test]
fn star_gift_auctions_state_position_hash_and_builders() {
    assert_eq!(format_auction_countdown(45), "0:45");
    assert_eq!(format_auction_countdown(125), "2:05");
    assert_eq!(format_auction_countdown(3_661), "1:01:01");

    let gift_tl = tl_enums::StarGift::StarGift(tl_types::StarGift {
        limited: true,
        sold_out: false,
        birthday: false,
        require_premium: false,
        limited_per_user: false,
        peer_color_available: false,
        auction: true,
        id: 777,
        sticker: tl_enums::Document::Empty(tl_types::DocumentEmpty { id: 11 }),
        stars: 100,
        availability_remains: Some(50),
        availability_total: Some(100),
        availability_resale: None,
        convert_stars: 85,
        first_sale_date: None,
        last_sale_date: None,
        upgrade_stars: Some(25),
        resell_min_stars: None,
        title: Some("Comet Crown".to_owned()),
        released_by: None,
        per_user_total: None,
        per_user_remains: None,
        locked_until_date: None,
        auction_slug: Some("comet_crown".to_owned()),
        gifts_per_round: Some(3),
        auction_start_date: Some(1_763_700_000),
        upgrade_variants: Some(20),
        background: None,
    });

    let state_tl = tl_enums::payments::StarGiftAuctionState::StarGiftAuctionState(
        tl_types::payments::StarGiftAuctionState {
            gift: gift_tl.clone(),
            state: tl_enums::StarGiftAuctionState::StarGiftAuctionState(
                tl_types::StarGiftAuctionState {
                    version: 4,
                    start_date: 1_763_700_000,
                    end_date: 1_763_710_800,
                    min_bid_amount: 500,
                    bid_levels: vec![
                        tl_enums::AuctionBidLevel::AuctionBidLevel(tl_types::AuctionBidLevel {
                            pos: 1,
                            amount: 2_000,
                            date: 1_763_700_100,
                        }),
                        tl_enums::AuctionBidLevel::AuctionBidLevel(tl_types::AuctionBidLevel {
                            pos: 2,
                            amount: 1_200,
                            date: 1_763_700_200,
                        }),
                        tl_enums::AuctionBidLevel::AuctionBidLevel(tl_types::AuctionBidLevel {
                            pos: 3,
                            amount: 800,
                            date: 1_763_700_300,
                        }),
                        tl_enums::AuctionBidLevel::AuctionBidLevel(tl_types::AuctionBidLevel {
                            pos: 4,
                            amount: 500,
                            date: 1_763_700_400,
                        }),
                    ],
                    top_bidders: vec![101, 102, 103],
                    next_round_at: 1_763_703_600,
                    last_gift_num: 12,
                    gifts_left: 48,
                    current_round: 2,
                    total_rounds: 10,
                    rounds: vec![tl_enums::StarGiftAuctionRound::Extendable(
                        tl_types::StarGiftAuctionRoundExtendable {
                            num: 2,
                            duration: 3600,
                            extend_top: 3,
                            extend_window: 300,
                        },
                    )],
                },
            ),
            user_state: tl_enums::StarGiftAuctionUserState::StarGiftAuctionUserState(
                tl_types::StarGiftAuctionUserState {
                    returned: false,
                    bid_amount: Some(1_200),
                    bid_date: Some(1_763_700_150),
                    min_bid_amount: Some(1_300),
                    bid_peer: Some(tl_enums::Peer::User(tl_types::PeerUser { user_id: 999 })),
                    acquired_count: 1,
                },
            ),
            timeout: 15,
            users: Vec::new(),
            chats: Vec::new(),
        },
    );

    let mut st = auction_state_from_tl(&state_tl);
    assert!(!st.is_finished());
    assert_eq!(st.version, 4);
    assert_eq!(st.my_position(), 2);
    assert!(st.is_my_bid_winning());
    assert_eq!(st.min_winning_bid(), 800);
    assert!(st.round_parameters[0].is_extendable());

    // Lower bid drops out of top 3
    st.my.bid = 600;
    assert_eq!(st.my_position(), 4);
    assert!(!st.is_my_bid_winning());

    // Stale version update is ignored
    apply_auction_state_tl(
        &mut st,
        &tl_enums::StarGiftAuctionState::StarGiftAuctionState(tl_types::StarGiftAuctionState {
            version: 3,
            start_date: 0,
            end_date: 0,
            min_bid_amount: 1,
            bid_levels: Vec::new(),
            top_bidders: Vec::new(),
            next_round_at: 0,
            last_gift_num: 0,
            gifts_left: 0,
            current_round: 1,
            total_rounds: 1,
            rounds: Vec::new(),
        }),
    );
    assert_eq!(st.version, 4);

    // Active auctions hash is order-independent (sorts by (date << 32) | version)
    let st_b = GiftAuctionState {
        gift: st.gift.clone(),
        version: 2,
        my: StarGiftAuctionMyState {
            bid: 500,
            date: 1_763_700_050,
            ..StarGiftAuctionMyState::default()
        },
        bid_levels: vec![GiftAuctionBidLevel {
            amount: 500,
            position: 1,
            date: 1_763_700_050,
        }],
        ..GiftAuctionState::default()
    };
    let h1 = compute_active_auctions_hash(&[st.clone(), st_b.clone()]);
    let h2 = compute_active_auctions_hash(&[st_b, st.clone()]);
    assert_eq!(h1, h2);
    assert_ne!(h1, 0);

    // Acquired gifts & active auctions TL parsing
    let acq_tl = tl_enums::payments::StarGiftAuctionAcquiredGifts::StarGiftAuctionAcquiredGifts(
        tl_types::payments::StarGiftAuctionAcquiredGifts {
            gifts: vec![
                tl_enums::StarGiftAuctionAcquiredGift::StarGiftAuctionAcquiredGift(
                    tl_types::StarGiftAuctionAcquiredGift {
                        name_hidden: false,
                        peer: tl_enums::Peer::User(tl_types::PeerUser { user_id: 999 }),
                        date: 1_763_700_500,
                        bid_amount: 1_500,
                        round: 1,
                        pos: 1,
                        message: None,
                        gift_num: Some(1),
                    },
                ),
            ],
            users: Vec::new(),
            chats: Vec::new(),
        },
    );
    let acq = acquired_gifts_from_tl(&acq_tl);
    assert_eq!(acq.len(), 1);
    assert_eq!(acq[0].number, 1);

    assert!(
        active_auctions_from_tl(&tl_enums::payments::StarGiftActiveAuctions::NotModified).is_none()
    );
    let active_tl = tl_enums::payments::StarGiftActiveAuctions::StarGiftActiveAuctions(
        tl_types::payments::StarGiftActiveAuctions {
            auctions: vec![
                tl_enums::StarGiftActiveAuctionState::StarGiftActiveAuctionState(
                    tl_types::StarGiftActiveAuctionState {
                        gift: gift_tl,
                        state: tl_enums::StarGiftAuctionState::Finished(
                            tl_types::StarGiftAuctionStateFinished {
                                start_date: 1_763_700_000,
                                end_date: 1_763_710_800,
                                average_price: 1_450,
                                listed_count: Some(5),
                                fragment_listed_count: Some(2),
                                fragment_listed_url: None,
                            },
                        ),
                        user_state: tl_enums::StarGiftAuctionUserState::StarGiftAuctionUserState(
                            tl_types::StarGiftAuctionUserState {
                                returned: false,
                                bid_amount: None,
                                bid_date: None,
                                min_bid_amount: None,
                                bid_peer: None,
                                acquired_count: 1,
                            },
                        ),
                    },
                ),
            ],
            users: Vec::new(),
            chats: Vec::new(),
        },
    );
    let snap = active_auctions_from_tl(&active_tl).expect("active snapshot");
    assert_eq!(snap.auctions.len(), 1);
    assert!(snap.auctions[0].is_finished());

    let _ = build_get_star_gift_auction_state_by_id(777, 4).to_bytes();
    let _ = build_get_star_gift_auction_state_by_slug("comet_crown", 0).to_bytes();
    let _ = build_get_star_gift_auction_acquired_gifts(777).to_bytes();
    let _ = build_get_star_gift_active_auctions(h1).to_bytes();
    let _ = build_auction_bid_invoice(
        777,
        1_500,
        true,
        false,
        Some(tl_enums::InputPeer::Self_),
        None,
    )
    .to_bytes();
}

#[test]
fn star_gift_crafting_math_and_eligibility() {
    assert_eq!(format_craft_percent(0), "0%");
    assert_eq!(format_craft_percent(90), "9%");
    assert_eq!(format_craft_percent(455), "46%");
    assert_eq!(lookup_craft_attribute_permille(4, 4, None), 1000);
    assert_eq!(lookup_craft_attribute_permille(4, 3, None), 450);
    assert_eq!(lookup_craft_attribute_permille(3, 2, None), 190);
    assert_eq!(lookup_craft_attribute_permille(2, 1, None), 80);
    assert_eq!(lookup_craft_attribute_permille(1, 1, None), 90);
    assert_eq!(
        parse_craft_too_early_error("STARGIFT_CRAFT_TOO_EARLY_3600"),
        Some(3600)
    );
    assert_eq!(parse_craft_too_early_error("OTHER_ERROR"), None);

    let g1 = CraftGiftSlot {
        unique: make_unique(1, 100, "AuroraGem-1", 1, "Cobalt", 7, "Star", 200),
        manage_id: SavedStarGiftId::User(MsgId::from(10)),
    };
    let g2 = CraftGiftSlot {
        unique: make_unique(2, 100, "AuroraGem-2", 1, "Cobalt", 8, "Crown", 250),
        manage_id: SavedStarGiftId::User(MsgId::from(11)),
    };
    let g3 = CraftGiftSlot {
        unique: make_unique(3, 100, "AuroraGem-3", 2, "Crimson", 7, "Star", 300),
        manage_id: SavedStarGiftId::User(MsgId::from(12)),
    };

    let slots = vec![g1.clone(), g2.clone(), g3.clone()];
    assert!(validate_craft_slots(&slots, 1_763_700_000).is_ok());
    assert_eq!(compute_craft_success_permille(&slots), 750);

    let plan = compute_craft_plan(&slots, None);
    assert_eq!(plan.success_permille, 750);
    assert_eq!(plan.success_percent_text, "75%");
    // 2 of 3 share Cobalt backdrop -> 190 permille; 1 of 3 has Crimson -> 70 permille
    assert_eq!(plan.backdrop_chances[0].name, "Cobalt");
    assert_eq!(plan.backdrop_chances[0].permille, 190);
    assert_eq!(plan.backdrop_chances[1].name, "Crimson");
    assert_eq!(plan.backdrop_chances[1].permille, 70);

    // Error cases
    assert_eq!(
        validate_craft_slots(&[], 100),
        Err(CraftEligibilityError::Empty)
    );
    let mut locked = g1.clone();
    locked.unique.can_craft_at = 2_000_000_000;
    assert!(matches!(
        validate_craft_slots(&[locked], 1_763_700_000),
        Err(CraftEligibilityError::TooEarly { .. })
    ));
    let mut on_chain = g1.clone();
    on_chain.unique.gift_address = "EQ...".to_owned();
    assert!(matches!(
        validate_craft_slots(&[on_chain], 1_763_700_000),
        Err(CraftEligibilityError::ExportedOnChain { .. })
    ));
    let mut other_base = g2.clone();
    other_base.unique.initial_gift_id = 999;
    assert!(matches!(
        validate_craft_slots(&[g1.clone(), other_base], 1_763_700_000),
        Err(CraftEligibilityError::MismatchedBaseGift { .. })
    ));
    assert!(matches!(
        validate_craft_slots(&[g1.clone(), g1.clone()], 1_763_700_000),
        Err(CraftEligibilityError::DuplicateGift { .. })
    ));
    let too_many: Vec<CraftGiftSlot> = (0..=MAX_CRAFT_SLOTS)
        .map(|i| CraftGiftSlot {
            unique: make_unique(
                10 + i as i64,
                100,
                &format!("AuroraGem-{}", 10 + i),
                1,
                "Cobalt",
                7,
                "Star",
                100,
            ),
            manage_id: SavedStarGiftId::User(MsgId::from(20 + i as i32)),
        })
        .collect();
    assert!(matches!(
        validate_craft_slots(&too_many, 1_763_700_000),
        Err(CraftEligibilityError::TooManySlots { .. })
    ));

    let _ = build_get_craft_star_gifts(100, "", 50).to_bytes();
    let _ = build_craft_star_gift(vec![tl_enums::InputSavedStarGift::User(
        tl_types::InputSavedStarGiftUser { msg_id: 10 },
    )])
    .to_bytes();
}

#[test]
fn giveaways_catalog_validation_and_boost_replacement() {
    let now = 1_763_700_000;
    let limits = GiveawayLimits::default();
    let until = GiveawayDraft::default_until_date(now);
    assert_eq!(until % 300, 0);
    assert!(until > now);

    let chan = PeerId::from_channel(ChannelId::from_tl(2_000_101));
    let mut draft = GiveawayDraft {
        boost_peer: chan,
        prize: GiveawayPrizeKind::Premium { months: 3 },
        quantity: 10,
        additional_peers: Vec::new(),
        countries_iso2: vec!["US".to_owned(), "DE".to_owned()],
        prize_description: "Bonus vinyl pack".to_owned(),
        until_date: until,
        only_new_subscribers: true,
        winners_are_visible: true,
        prepaid_id: Some(501),
    };
    assert!(draft.validate(now, &limits).is_ok());
    assert_eq!(draft.expected_boosts(&limits, 0), 40);

    // Too long prize description fails validation
    draft.prize_description = "x".repeat(GIVEAWAY_ADDITIONAL_PRIZE_MAX_LEN + 1);
    assert!(matches!(
        draft.validate(now, &limits),
        Err(GiveawayValidationError::PrizeDescriptionTooLong { .. })
    ));
    draft.prize_description = "Bonus vinyl pack".to_owned();

    // Gift code catalog with linear extrapolation fallback
    let catalog = PremiumGiftCodeCatalog::from_tl(&[
        tl_enums::PremiumGiftCodeOption::PremiumGiftCodeOption(tl_types::PremiumGiftCodeOption {
            users: 1,
            months: 3,
            store_product: Some("prem_3m".to_owned()),
            store_quantity: Some(1),
            currency: "USD".to_owned(),
            amount: 1199,
        }),
        tl_enums::PremiumGiftCodeOption::PremiumGiftCodeOption(tl_types::PremiumGiftCodeOption {
            users: 1,
            months: 3,
            store_product: None,
            store_quantity: None,
            currency: "XTR".to_owned(),
            amount: 1000,
        }),
        tl_enums::PremiumGiftCodeOption::PremiumGiftCodeOption(tl_types::PremiumGiftCodeOption {
            users: 10,
            months: 3,
            store_product: Some("prem_3m_10".to_owned()),
            store_quantity: Some(10),
            currency: "USD".to_owned(),
            amount: 11990,
        }),
    ]);
    assert_eq!(catalog.available_presets, vec![1, 10]);
    let opts_10 = catalog.options_for_giveaway(10);
    assert_eq!(opts_10.len(), 1);
    assert_eq!(opts_10[0].currency, "USD");
    let opts_5 = catalog.options_for_giveaway(5);
    assert_eq!(opts_5.len(), 1);
    assert_eq!(opts_5[0].raw_amount, 1199 * 5);

    let inv = draft.to_invoice(
        tl_enums::InputPeer::Self_,
        Vec::new(),
        "USD",
        11990,
        "prem_3m_10",
        10,
        4242,
    );
    assert!(inv.to_input_invoice().is_some());
    let launch_req = build_launch_prepaid_giveaway(tl_enums::InputPeer::Self_, 501, &inv)
        .expect("launch prepaid request");
    let _ = launch_req.to_bytes();
    let _ = build_get_premium_gift_code_options(Some(tl_enums::InputPeer::Self_)).to_bytes();

    let res_summary = giveaway_results_from_tl(&tl_types::MessageMediaGiveawayResults {
        only_new_subscribers: true,
        refunded: false,
        channel_id: 2_000_101,
        additional_peers_count: Some(2),
        launch_msg_id: 88,
        winners_count: 10,
        unclaimed_count: 1,
        winners: vec![101, 102],
        months: Some(3),
        stars: None,
        prize_description: Some("Bonus".to_owned()),
        until_date: until,
    });
    assert_eq!(res_summary.winners.len(), 2);
    assert_eq!(res_summary.unclaimed_count, 1);

    // Boost slot partitioning & sorting (`replace_boost_box.cpp`)
    let other_a = PeerId::from_channel(ChannelId::from_tl(2_000_202));
    let other_b = PeerId::from_channel(ChannelId::from_tl(2_000_303));
    let my_slots = vec![
        MyBoostSlot {
            slot: 1,
            peer_id: None,
            date: 0,
            expires: 0,
            cooldown_until_date: 0,
        },
        MyBoostSlot {
            slot: 2,
            peer_id: Some(chan),
            date: now - 10_000,
            expires: now + 100_000,
            cooldown_until_date: now - 1_000,
        },
        MyBoostSlot {
            slot: 3,
            peer_id: Some(other_a),
            date: now - 100,
            expires: now + 100_000,
            cooldown_until_date: now + 7_200, // waiting 2h
        },
        MyBoostSlot {
            slot: 4,
            peer_id: Some(other_b),
            date: now - 50_000,
            expires: now + 100_000,
            cooldown_until_date: now - 500, // ready
        },
    ];
    let partitioned: ForChannelBoostSlots = parse_for_channel_boost_slots(chan, &my_slots, now);
    assert_eq!(partitioned.free, vec![1]);
    assert_eq!(partitioned.already, vec![2]);
    assert_eq!(partitioned.other.len(), 2);
    // Ready slot (4) sorts before waiting slot (3)
    assert_eq!(partitioned.other[0].id, 4);
    assert!(!partitioned.other[0].is_waiting(now));
    assert_eq!(partitioned.other[1].id, 3);
    assert!(partitioned.other[1].is_waiting(now));

    let mut manual = vec![
        TakenBoostSlot {
            id: 10,
            expires: 0,
            peer_id: other_a,
            cooldown: now + 500,
        },
        TakenBoostSlot {
            id: 11,
            expires: 0,
            peer_id: other_b,
            cooldown: now + 100,
        },
    ];
    sort_taken_boost_slots(&mut manual, now);
    assert_eq!(manual[0].id, 11);
    assert_eq!(format_boost_cooldown(3_665), "1:01:05");
    assert_eq!(format_boost_cooldown(65), "1:05");

    let _ = build_apply_boost(tl_enums::InputPeer::Self_, vec![1, 4]).to_bytes();
}

#[test]
fn starref_affiliate_program_validation_formatting_and_tl() {
    assert_eq!(format_commission(200), "20%");
    assert_eq!(format_commission(125), "12.5%");
    assert_eq!(format_program_duration(0), "Forever");
    assert_eq!(format_program_duration(1), "1 month");
    assert_eq!(format_program_duration(6), "6 months");
    assert_eq!(format_program_duration(12), "1 year");
    assert_eq!(format_program_duration(24), "2 years");

    let active = StarRefProgram {
        bot_id: UserId::from_tl(910_001),
        commission_permille: 200,
        duration_months: 12,
        end_date: 0,
        daily_revenue_per_user: CreditsAmount::new(2, 500_000_000, CreditsType::Stars),
    };
    assert!(active.is_active());

    // Raising commission & extending duration is allowed
    assert!(
        validate_star_ref_update(
            Some(&active),
            250,
            0, // forever > 12 months
            STARREF_COMMISSION_MIN_FALLBACK,
            STARREF_COMMISSION_MAX_FALLBACK,
        )
        .is_ok()
    );

    // Lowering commission on active program fails
    assert!(matches!(
        validate_star_ref_update(
            Some(&active),
            150,
            12,
            STARREF_COMMISSION_MIN_FALLBACK,
            STARREF_COMMISSION_MAX_FALLBACK,
        ),
        Err(StarRefValidationError::CommissionDecreased { .. })
    ));

    // Shortening duration on active program fails
    assert!(matches!(
        validate_star_ref_update(
            Some(&active),
            200,
            6,
            STARREF_COMMISSION_MIN_FALLBACK,
            STARREF_COMMISSION_MAX_FALLBACK,
        ),
        Err(StarRefValidationError::DurationShortened { .. })
    ));

    // Ending program with commission = 0 is always allowed
    assert!(
        validate_star_ref_update(
            Some(&active),
            0,
            0,
            STARREF_COMMISSION_MIN_FALLBACK,
            STARREF_COMMISSION_MAX_FALLBACK,
        )
        .is_ok()
    );

    // TL parsing & request builders
    let tl_prog = tl_enums::StarRefProgram::StarRefProgram(tl_types::StarRefProgram {
        bot_id: 910_001,
        commission_permille: 200,
        duration_months: Some(12),
        end_date: None,
        daily_revenue_per_user: Some(tl_enums::StarsAmount::StarsAmount(tl_types::StarsAmount {
            amount: 3,
            nanos: 500_000_000,
        })),
    });
    let parsed = star_ref_program_from_tl(&tl_prog);
    assert_eq!(parsed.commission_permille, 200);
    assert_eq!(parsed.duration_months, 12);
    assert_eq!(parsed.daily_revenue_per_user.whole(), 3);

    let conn_tl = tl_enums::payments::ConnectedStarRefBots::ConnectedStarRefBots(
        tl_types::payments::ConnectedStarRefBots {
            count: 1,
            connected_bots: vec![tl_enums::ConnectedBotStarRef::ConnectedBotStarRef(
                tl_types::ConnectedBotStarRef {
                    revoked: false,
                    url: "https://t.me/demo_bot?start=_tgr_abc".to_owned(),
                    date: 1_763_600_000,
                    bot_id: 910_001,
                    commission_permille: 200,
                    duration_months: Some(12),
                    participants: 14,
                    revenue: 340,
                },
            )],
            users: Vec::new(),
        },
    );
    let conn = connected_star_ref_bots_from_tl(&conn_tl);
    assert_eq!(conn.count, 1);
    assert_eq!(conn.bots[0].participants, 14);
    assert_eq!(conn.bots[0].revenue, 340);

    let sugg_tl = tl_enums::payments::SuggestedStarRefBots::SuggestedStarRefBots(
        tl_types::payments::SuggestedStarRefBots {
            count: 1,
            suggested_bots: vec![tl_prog],
            users: Vec::new(),
            next_offset: Some("p2".to_owned()),
        },
    );
    let sugg = suggested_star_ref_bots_from_tl(&sugg_tl);
    assert_eq!(sugg.programs.len(), 1);
    assert_eq!(sugg.next_offset, "p2");

    let upd = build_update_star_ref_program(tl_enums::InputUser::Empty, 250, 0);
    assert_eq!(upd.duration_months, None);
    let _ = upd.to_bytes();
    let fin = build_finish_star_ref_program(tl_enums::InputUser::Empty);
    assert_eq!(fin.commission_permille, 0);
    let _ = fin.to_bytes();
    let _ = build_get_connected_star_ref_bots(tl_enums::InputPeer::Self_, None, 50).to_bytes();
    let _ =
        build_get_connected_star_ref_bot(tl_enums::InputPeer::Self_, tl_enums::InputUser::Empty)
            .to_bytes();
    let _ = build_get_suggested_star_ref_bots(
        tl_enums::InputPeer::Self_,
        SuggestedStarRefSort::Revenue,
        "",
        50,
    )
    .to_bytes();
    let _ = build_connect_star_ref_bot(tl_enums::InputPeer::Self_, tl_enums::InputUser::Empty)
        .to_bytes();
    let _ = build_edit_connected_star_ref_bot(
        tl_enums::InputPeer::Self_,
        "https://t.me/demo_bot?start=_tgr_abc",
        true,
    )
    .to_bytes();
}
