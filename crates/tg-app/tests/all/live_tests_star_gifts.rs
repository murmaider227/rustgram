//! Integration tests for WP-16: Star Gifts marketplace (unique gifts, collections,
//! per-peer filtering, wear/unwear, resale, auctions, crafting), giveaways, boost
//! replacement, and Stars affiliate programs (StarRef).
//!
//! All tests run exclusively against synthetic keys/peers and an in-process
//! `FakeRpcClient` or `MockSession`/`MockPaymentsBackend`. Never contacts real Telegram
//! servers or initiates real payments.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use egui::{RawInput, Rect, Vec2};
use tg_api::{DcId, InvocationError, RpcClient};
use tg_data::{ChannelId, CollectibleId, PeerId as DataPeerId, UserId as DataUserId};
use tg_payments::credits::CreditsAmount;
use tg_payments::giveaways::{GiveawayDraft, GiveawayPrizeKind};
use tg_payments::premium::GiveawayState;
use tg_payments::star_gifts::{PeerGiftsFilter, ResaleGiftsFilter, ResaleGiftsSort};
use tg_payments::starref::SuggestedStarRefSort;
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tokio::sync::broadcast;

use tg_app::live::LiveSession;
use tg_app::payments::marketplace_backend::{CraftExecutionOutcome, StarGiftsMarketplaceBackend};
use tg_app::payments::star_gifts::StarGiftsTab;
use tg_app::payments::{PaymentsScreen, PaymentsScreenHost};
use tg_app::stand_in::mock_session::MockSession;
use tg_app::stand_in::theme::Palette;

const TEST_TIMEOUT: Duration = Duration::from_secs(15);

type QueuedResponse = (u32, Result<Vec<u8>, InvocationError>);
type RecordedRequest = (u32, Vec<u8>);

#[derive(Clone)]
struct FakeRpcClient {
    main_dc: Arc<Mutex<DcId>>,
    recorded: Arc<Mutex<Vec<RecordedRequest>>>,
    responses: Arc<Mutex<VecDeque<QueuedResponse>>>,
    updates_tx: broadcast::Sender<tl_enums::Updates>,
}

impl FakeRpcClient {
    fn new(main_dc: DcId) -> Self {
        let (updates_tx, _) = broadcast::channel(64);
        Self {
            main_dc: Arc::new(Mutex::new(main_dc)),
            recorded: Arc::new(Mutex::new(Vec::new())),
            responses: Arc::new(Mutex::new(VecDeque::new())),
            updates_tx,
        }
    }

    fn enqueue_ok<T: Serializable>(&self, expected_ctor: u32, resp: &T) {
        self.responses
            .lock()
            .expect("lock responses")
            .push_back((expected_ctor, Ok(resp.to_bytes())));
    }

    fn recorded_ctors(&self) -> Vec<u32> {
        self.recorded
            .lock()
            .expect("lock recorded")
            .iter()
            .map(|(c, _)| *c)
            .collect()
    }

    fn recorded_requests_for(&self, ctor: u32) -> Vec<Vec<u8>> {
        self.recorded
            .lock()
            .expect("lock recorded")
            .iter()
            .filter(|(c, _)| *c == ctor)
            .map(|(_, b)| b.clone())
            .collect()
    }
}

impl RpcClient for FakeRpcClient {
    type Error = InvocationError;

    async fn invoke<R: RemoteCall + Send + Sync>(&self, req: &R) -> Result<R::Return, Self::Error> {
        let dc = self.main_dc_id();
        self.invoke_in_dc(dc, req).await
    }

    async fn invoke_in_dc<R: RemoteCall + Send + Sync>(
        &self,
        _dc_id: DcId,
        req: &R,
    ) -> Result<R::Return, Self::Error> {
        let bytes = req.to_bytes();
        let ctor = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        self.recorded
            .lock()
            .expect("lock recorded")
            .push((ctor, bytes));

        let next = {
            let mut q = self.responses.lock().expect("lock responses");
            if let Some(pos) = q.iter().position(|(c, _)| *c == ctor) {
                q.remove(pos).map(|(_, r)| r)
            } else if crate::common::is_background_list_request(ctor) {
                // Never the answer a test queued for another request (TGB-2941).
                None
            } else {
                q.pop_front().map(|(_, r)| r)
            }
        };
        let raw = next.unwrap_or(Err(InvocationError::Dropped))?;
        R::Return::from_bytes(&raw).map_err(|e| InvocationError::Deserialize(e.to_string()))
    }

    fn main_dc_id(&self) -> DcId {
        *self.main_dc.lock().expect("lock main_dc")
    }

    fn set_main_dc_id(&self, dc_id: DcId) {
        *self.main_dc.lock().expect("lock main_dc") = dc_id;
    }

    fn subscribe_updates(&self) -> Option<broadcast::Receiver<tl_enums::Updates>> {
        Some(self.updates_tx.subscribe())
    }
}

fn synthetic_user(
    id: i64,
    first_name: &str,
    username: &str,
    is_self: bool,
    is_bot: bool,
) -> tl_types::User {
    tl_types::User {
        self_: is_self,
        contact: !is_self && !is_bot,
        mutual_contact: false,
        deleted: false,
        bot: is_bot,
        bot_chat_history: false,
        bot_nochats: false,
        verified: false,
        restricted: false,
        min: false,
        bot_inline_geo: false,
        support: false,
        scam: false,
        apply_min_photo: false,
        fake: false,
        bot_attach_menu: false,
        premium: true,
        attach_menu_enabled: false,
        bot_can_edit: false,
        close_friend: false,
        stories_hidden: false,
        stories_unavailable: false,
        contact_require_premium: false,
        bot_business: false,
        bot_has_main_app: false,
        bot_forum_view: false,
        bot_forum_can_manage_topics: false,
        bot_can_manage_bots: false,
        bot_guestchat: false,
        bot_guard: false,
        bot_forum_sidebar: false,
        id,
        access_hash: Some(id * 1000 + 13),
        first_name: Some(first_name.to_owned()),
        last_name: None,
        username: Some(username.to_owned()),
        phone: None,
        photo: None,
        status: None,
        bot_info_version: None,
        restriction_reason: None,
        bot_inline_placeholder: None,
        lang_code: Some("en".to_owned()),
        emoji_status: None,
        usernames: None,
        stories_max_id: None,
        color: None,
        profile_color: None,
        bot_active_users: None,
        bot_verification_icon: None,
        send_paid_messages_stars: None,
        linked_community_id: None,
    }
}

fn synthetic_channel(id: i64, title: &str, username: &str) -> tl_types::Channel {
    tl_types::Channel {
        creator: true,
        left: false,
        broadcast: true,
        verified: false,
        megagroup: false,
        restricted: false,
        signatures: false,
        min: false,
        scam: false,
        has_link: false,
        has_geo: false,
        slowmode_enabled: false,
        call_active: false,
        call_not_empty: false,
        fake: false,
        gigagroup: false,
        noforwards: false,
        join_to_send: false,
        join_request: false,
        forum: false,
        stories_hidden: false,
        stories_hidden_min: false,
        stories_unavailable: false,
        signature_profiles: false,
        autotranslation: false,
        broadcast_messages_allowed: false,
        monoforum: false,
        forum_tabs: false,
        in_news: false,
        forum_sidebar: false,
        id,
        access_hash: Some(id * 37 + 9),
        title: title.to_owned(),
        username: Some(username.to_owned()),
        photo: tl_enums::ChatPhoto::Empty,
        date: 1_760_000_000,
        restriction_reason: None,
        admin_rights: None,
        banned_rights: None,
        default_banned_rights: None,
        participants_count: Some(500),
        usernames: None,
        stories_max_id: None,
        color: None,
        profile_color: None,
        emoji_status: None,
        level: Some(5),
        subscription_until_date: None,
        bot_verification_icon: None,
        send_paid_messages_stars: None,
        linked_monoforum_id: None,
        linked_community_id: None,
        pinned_msg_id: None,
    }
}

fn synthetic_unique_tl_gift(
    id: i64,
    gift_id: i64,
    slug: &str,
    num: i32,
    resell_stars: i64,
) -> tl_enums::StarGift {
    tl_enums::StarGift::Unique(tl_types::StarGiftUnique {
        require_premium: false,
        resale_ton_only: false,
        theme_available: true,
        burned: false,
        crafted: false,
        id,
        gift_id,
        title: "Aurora Gem".to_owned(),
        slug: slug.to_owned(),
        num,
        owner_id: Some(tl_enums::Peer::User(tl_types::PeerUser {
            user_id: 600_001,
        })),
        owner_name: Some("Alex Demo".to_owned()),
        owner_address: None,
        attributes: vec![
            tl_enums::StarGiftAttribute::Model(tl_types::StarGiftAttributeModel {
                crafted: false,
                name: "Prism".to_owned(),
                document: tl_enums::Document::Empty(tl_types::DocumentEmpty { id: 3001 }),
                rarity: tl_enums::StarGiftAttributeRarity::Epic,
            }),
            tl_enums::StarGiftAttribute::Backdrop(tl_types::StarGiftAttributeBackdrop {
                name: "Cobalt".to_owned(),
                backdrop_id: 7,
                center_color: 0x3B82F6,
                edge_color: 0x1E3A8A,
                pattern_color: 0x93C5FD,
                text_color: 0xFFFFFF,
                rarity: tl_enums::StarGiftAttributeRarity::StarGiftAttributeRarity(
                    tl_types::StarGiftAttributeRarity { permille: 20 },
                ),
            }),
            tl_enums::StarGiftAttribute::Pattern(tl_types::StarGiftAttributePattern {
                name: "Starburst".to_owned(),
                document: tl_enums::Document::Empty(tl_types::DocumentEmpty { id: 3002 }),
                rarity: tl_enums::StarGiftAttributeRarity::StarGiftAttributeRarity(
                    tl_types::StarGiftAttributeRarity { permille: 15 },
                ),
            }),
        ],
        availability_issued: 100,
        availability_total: 1000,
        gift_address: None,
        resell_amount: (resell_stars > 0).then(|| {
            vec![tl_enums::StarsAmount::StarsAmount(tl_types::StarsAmount {
                amount: resell_stars,
                nanos: 0,
            })]
        }),
        released_by: None,
        value_amount: Some(4000),
        value_currency: Some("USD".to_owned()),
        value_usd_amount: Some(4000),
        theme_peer: None,
        peer_color: None,
        host_id: None,
        offer_min_stars: Some(100),
        craft_chance_permille: Some(250),
    })
}

fn synthetic_saved_gift(
    msg_id: i32,
    gift: tl_enums::StarGift,
    can_upgrade: bool,
) -> tl_enums::SavedStarGift {
    tl_enums::SavedStarGift::SavedStarGift(tl_types::SavedStarGift {
        name_hidden: false,
        unsaved: false,
        refunded: false,
        can_upgrade,
        pinned_to_top: false,
        upgrade_separate: false,
        from_id: Some(tl_enums::Peer::User(tl_types::PeerUser {
            user_id: 600_002,
        })),
        date: 1_763_700_000,
        gift,
        message: Some(tl_enums::TextWithEntities::TextWithEntities(
            tl_types::TextWithEntities {
                text: "Enjoy!".to_owned(),
                entities: Vec::new(),
            },
        )),
        msg_id: Some(msg_id),
        saved_id: None,
        convert_stars: Some(42),
        upgrade_stars: Some(25),
        can_export_at: None,
        transfer_stars: Some(25),
        can_transfer_at: Some(1_763_600_000),
        can_resell_at: Some(1_763_600_000),
        collection_id: Some(vec![11]),
        prepaid_upgrade_hash: None,
        drop_original_details_stars: None,
        gift_num: Some(1),
        can_craft_at: Some(1_763_600_000),
    })
}

fn empty_updates() -> tl_enums::Updates {
    tl_enums::Updates::Updates(tl_types::Updates {
        updates: Vec::new(),
        users: Vec::new(),
        chats: Vec::new(),
        date: 1_763_760_000,
        seq: 1,
    })
}

fn start_harness() -> (
    FakeRpcClient,
    Arc<SessionRuntime<tg_app::live::ErasedRpcClient>>,
    LiveSession,
) {
    let self_user = synthetic_user(600_001, "Alex", "alex_self", true, false);
    let peer_user = synthetic_user(600_002, "Sam", "sam_friend", false, false);
    let demo_bot = synthetic_user(600_099, "Synth Bot", "synth_demo_bot", false, true);
    let demo_chan = synthetic_channel(2_000_500, "Northwind Channel", "northwind_chan");

    let fake = FakeRpcClient::new(2);
    fake.enqueue_ok(
        tl_fns::users::GetUsers::CONSTRUCTOR_ID,
        &vec![tl_enums::User::User(self_user)],
    );
    fake.enqueue_ok(
        tl_fns::updates::GetState::CONSTRUCTOR_ID,
        &tl_enums::updates::State::State(tl_types::updates::State {
            pts: 10,
            qts: 0,
            date: 1_763_700_000,
            seq: 1,
            unread_count: 0,
        }),
    );
    fake.enqueue_ok(
        tl_fns::updates::GetDifference::CONSTRUCTOR_ID,
        &tl_enums::updates::Difference::Empty(tl_types::updates::DifferenceEmpty {
            date: 1_763_700_001,
            seq: 1,
        }),
    );
    let make_dialog = |peer: tl_enums::Peer, top_msg: i32| {
        tl_enums::Dialog::Dialog(tl_types::Dialog {
            pinned: false,
            unread_mark: false,
            view_forum_as_messages: false,
            peer,
            top_message: top_msg,
            read_inbox_max_id: top_msg,
            read_outbox_max_id: top_msg,
            unread_count: 0,
            unread_mentions_count: 0,
            unread_reactions_count: 0,
            unread_poll_votes_count: 0,
            notify_settings: tl_enums::PeerNotifySettings::PeerNotifySettings(
                tl_types::PeerNotifySettings {
                    show_previews: None,
                    silent: None,
                    mute_until: None,
                    ios_sound: None,
                    android_sound: None,
                    other_sound: None,
                    stories_muted: None,
                    stories_hide_sender: None,
                    stories_ios_sound: None,
                    stories_android_sound: None,
                    stories_other_sound: None,
                },
            ),
            pts: None,
            draft: None,
            folder_id: None,
            ttl_period: None,
        })
    };
    fake.enqueue_ok(
        tl_fns::messages::GetDialogs::CONSTRUCTOR_ID,
        &tl_enums::messages::Dialogs::Dialogs(tl_types::messages::Dialogs {
            dialogs: vec![
                make_dialog(
                    tl_enums::Peer::Channel(tl_types::PeerChannel {
                        channel_id: 2_000_500,
                    }),
                    10,
                ),
                make_dialog(
                    tl_enums::Peer::User(tl_types::PeerUser { user_id: 600_002 }),
                    11,
                ),
                make_dialog(
                    tl_enums::Peer::User(tl_types::PeerUser { user_id: 600_099 }),
                    12,
                ),
            ],
            messages: Vec::new(),
            chats: vec![tl_enums::Chat::Channel(demo_chan)],
            users: vec![
                tl_enums::User::User(peer_user),
                tl_enums::User::User(demo_bot),
            ],
        }),
    );
    fake.enqueue_ok(
        tl_fns::messages::GetPinnedDialogs::CONSTRUCTOR_ID,
        &tl_enums::messages::PeerDialogs::PeerDialogs(tl_types::messages::PeerDialogs {
            dialogs: Vec::new(),
            messages: Vec::new(),
            chats: Vec::new(),
            users: Vec::new(),
            state: tl_enums::updates::State::State(tl_types::updates::State {
                pts: 10,
                qts: 0,
                date: 1_763_700_001,
                seq: 1,
                unread_count: 0,
            }),
        }),
    );

    let runtime = Arc::new(
        SessionRuntime::start(
            DataUserId::from_tl(600_001),
            tg_app::live::ErasedRpcClient::new(fake.clone()),
            SessionRuntimeConfig::default(),
        )
        .expect("start SessionRuntime"),
    );
    runtime
        .tokio_handle()
        .block_on(async { tokio::time::timeout(TEST_TIMEOUT, runtime.wait_ready()).await })
        .expect("wait_ready timeout")
        .expect("startup ready");

    let live = LiveSession::new(Arc::clone(&runtime), 1);
    (fake, runtime, live)
}

fn shutdown_harness(
    runtime: Arc<SessionRuntime<tg_app::live::ErasedRpcClient>>,
    live: LiveSession,
) {
    drop(live);
    if let Ok(rt) = Arc::try_unwrap(runtime) {
        rt.shutdown();
    }
}

#[test]
fn live_unique_gifts_collections_wear_transfer_and_filters() {
    let (fake, runtime, mut live) = start_harness();

    let u1 = synthetic_unique_tl_gift(88_001, 10_002, "AuroraGem-1", 1, 0);
    let regular = tl_enums::StarGift::StarGift(tl_types::StarGift {
        limited: true,
        sold_out: false,
        birthday: false,
        require_premium: false,
        limited_per_user: false,
        peer_color_available: false,
        auction: false,
        id: 88_002,
        sticker: tl_enums::Document::Empty(tl_types::DocumentEmpty { id: 3005 }),
        stars: 50,
        availability_remains: Some(10),
        availability_total: Some(100),
        availability_resale: None,
        convert_stars: 42,
        first_sale_date: None,
        last_sale_date: None,
        upgrade_stars: Some(25),
        resell_min_stars: None,
        title: Some("Rocket".to_owned()),
        released_by: None,
        per_user_total: None,
        per_user_remains: None,
        locked_until_date: None,
        auction_slug: None,
        gifts_per_round: None,
        auction_start_date: None,
        upgrade_variants: None,
        background: None,
    });

    // 1. load_peer_gifts -> getSavedStarGifts + getStarGiftCollections
    fake.enqueue_ok(
        tl_fns::payments::GetSavedStarGifts::CONSTRUCTOR_ID,
        &tl_enums::payments::SavedStarGifts::SavedStarGifts(tl_types::payments::SavedStarGifts {
            count: 2,
            chat_notifications_enabled: None,
            gifts: vec![
                synthetic_saved_gift(501, u1.clone(), false),
                synthetic_saved_gift(502, regular, true),
            ],
            next_offset: None,
            chats: Vec::new(),
            users: Vec::new(),
        }),
    );
    fake.enqueue_ok(
        tl_fns::payments::GetStarGiftCollections::CONSTRUCTOR_ID,
        &tl_enums::payments::StarGiftCollections::StarGiftCollections(
            tl_types::payments::StarGiftCollections {
                collections: vec![tl_enums::StarGiftCollection::StarGiftCollection(
                    tl_types::StarGiftCollection {
                        collection_id: 11,
                        title: "Vault".to_owned(),
                        icon: None,
                        gifts_count: 1,
                        hash: 77,
                    },
                )],
            },
        ),
    );

    let filter = PeerGiftsFilter {
        sort_by_value: true,
        skip_unsaved: true,
        ..PeerGiftsFilter::default()
    };
    let page = live
        .load_peer_gifts("", &filter, "")
        .expect("load_peer_gifts");
    assert_eq!(page.items.len(), 2);
    assert_eq!(page.collections.len(), 1);
    assert_eq!(page.collections[0].title, "Vault");

    // 2. getUniqueStarGift + getUniqueStarGiftValueInfo
    fake.enqueue_ok(
        tl_fns::payments::GetUniqueStarGift::CONSTRUCTOR_ID,
        &tl_enums::payments::UniqueStarGift::UniqueStarGift(tl_types::payments::UniqueStarGift {
            gift: u1,
            chats: Vec::new(),
            users: Vec::new(),
        }),
    );
    fake.enqueue_ok(
        tl_fns::payments::GetUniqueStarGiftValueInfo::CONSTRUCTOR_ID,
        &tl_enums::payments::UniqueStarGiftValueInfo::UniqueStarGiftValueInfo(
            tl_types::payments::UniqueStarGiftValueInfo {
                last_sale_on_fragment: false,
                value_is_average: true,
                currency: "USD".to_owned(),
                value: 4100,
                initial_sale_date: 1_760_000_000,
                initial_sale_stars: 50,
                initial_sale_price: 100,
                last_sale_date: None,
                last_sale_price: None,
                floor_price: Some(3800),
                average_price: Some(4100),
                listed_count: Some(9),
                fragment_listed_count: Some(2),
                fragment_listed_url: None,
            },
        ),
    );
    let detail = live
        .get_unique_star_gift("AuroraGem-1")
        .expect("get_unique_star_gift");
    assert_eq!(
        detail.value_info.as_ref().expect("value").minimum_price,
        3800
    );

    // 3. Wear & unwear (`account.updateEmojiStatus`)
    fake.enqueue_ok(tl_fns::account::UpdateEmojiStatus::CONSTRUCTOR_ID, &true);
    live.wear_unique_gift(Some(CollectibleId::from_tl(88_001)), None)
        .expect("wear");
    fake.enqueue_ok(tl_fns::account::UpdateEmojiStatus::CONSTRUCTOR_ID, &true);
    live.wear_unique_gift(None, None).expect("unwear");

    // 4. Pin, save/hide, upgrade, transfer (to real peer `@sam_friend`), convert
    fake.enqueue_ok(
        tl_fns::payments::ToggleStarGiftsPinnedToTop::CONSTRUCTOR_ID,
        &true,
    );
    live.toggle_star_gifts_pinned_to_top(&[88_001])
        .expect("pin");

    fake.enqueue_ok(tl_fns::payments::SaveStarGift::CONSTRUCTOR_ID, &true);
    live.save_star_gift(88_001, true).expect("hide");

    fake.enqueue_ok(
        tl_fns::payments::UpgradeStarGift::CONSTRUCTOR_ID,
        &empty_updates(),
    );
    live.upgrade_star_gift(88_002, true).expect("upgrade");

    fake.enqueue_ok(
        tl_fns::payments::TransferStarGift::CONSTRUCTOR_ID,
        &empty_updates(),
    );
    live.transfer_star_gift(88_001, "@sam_friend")
        .expect("transfer");

    // Verify transfer sent `InputPeer::User(600_002)` instead of `InputPeer::Self_`
    let transfer_bytes =
        fake.recorded_requests_for(tl_fns::payments::TransferStarGift::CONSTRUCTOR_ID);
    assert_eq!(transfer_bytes.len(), 1);
    let decoded_transfer = tl_fns::payments::TransferStarGift::from_bytes(&transfer_bytes[0])
        .expect("decode transfer");
    assert!(matches!(
        decoded_transfer.to_id,
        tl_enums::InputPeer::User(tl_types::InputPeerUser {
            user_id: 600_002,
            ..
        })
    ));

    fake.enqueue_ok(tl_fns::payments::ConvertStarGift::CONSTRUCTOR_ID, &true);
    live.convert_star_gift(88_002).expect("convert");

    // 5. Collections CRUD (`create`, `update`, `reorder`, `delete`)
    fake.enqueue_ok(
        tl_fns::payments::CreateStarGiftCollection::CONSTRUCTOR_ID,
        &tl_enums::StarGiftCollection::StarGiftCollection(tl_types::StarGiftCollection {
            collection_id: 12,
            title: "Gems".to_owned(),
            icon: None,
            gifts_count: 0,
            hash: 88,
        }),
    );
    let created = live
        .create_star_gift_collection("", "Gems", &[])
        .expect("create collection");
    assert_eq!(created.id, 12);

    fake.enqueue_ok(
        tl_fns::payments::UpdateStarGiftCollection::CONSTRUCTOR_ID,
        &tl_enums::StarGiftCollection::StarGiftCollection(tl_types::StarGiftCollection {
            collection_id: 12,
            title: "RareGems".to_owned(),
            icon: None,
            gifts_count: 0,
            hash: 89,
        }),
    );
    let updated = live
        .update_star_gift_collection("", 12, Some("RareGems"), &[], &[])
        .expect("update collection");
    assert_eq!(updated.title, "RareGems");

    fake.enqueue_ok(
        tl_fns::payments::ReorderStarGiftCollections::CONSTRUCTOR_ID,
        &true,
    );
    live.reorder_star_gift_collections("", &[12, 11])
        .expect("reorder collections");

    fake.enqueue_ok(
        tl_fns::payments::DeleteStarGiftCollection::CONSTRUCTOR_ID,
        &true,
    );
    live.delete_star_gift_collection("", 12)
        .expect("delete collection");

    let ctors = fake.recorded_ctors();
    assert!(ctors.contains(&tl_fns::payments::GetSavedStarGifts::CONSTRUCTOR_ID));
    assert!(ctors.contains(&tl_fns::payments::GetUniqueStarGift::CONSTRUCTOR_ID));
    assert!(ctors.contains(&tl_fns::account::UpdateEmojiStatus::CONSTRUCTOR_ID));
    assert!(ctors.contains(&tl_fns::payments::CreateStarGiftCollection::CONSTRUCTOR_ID));
    assert!(ctors.contains(&tl_fns::payments::UpdateStarGiftCollection::CONSTRUCTOR_ID));
    assert!(ctors.contains(&tl_fns::payments::ReorderStarGiftCollections::CONSTRUCTOR_ID));
    assert!(ctors.contains(&tl_fns::payments::DeleteStarGiftCollection::CONSTRUCTOR_ID));

    shutdown_harness(runtime, live);
}

#[test]
fn live_resale_auctions_and_crafting_rpcs() {
    let (fake, runtime, mut live) = start_harness();

    let u1 = synthetic_unique_tl_gift(88_010, 10_002, "AuroraGem-10", 10, 450);

    // 1. Resale marketplace browse + updateStarGiftPrice + buy resale
    fake.enqueue_ok(
        tl_fns::payments::GetResaleStarGifts::CONSTRUCTOR_ID,
        &tl_enums::payments::ResaleStarGifts::ResaleStarGifts(
            tl_types::payments::ResaleStarGifts {
                count: 1,
                gifts: vec![u1.clone()],
                next_offset: None,
                attributes: Some(Vec::new()),
                attributes_hash: Some(555),
                chats: Vec::new(),
                counters: None,
                users: Vec::new(),
            },
        ),
    );
    let resale = live
        .load_resale_star_gifts(
            10_002,
            &ResaleGiftsFilter {
                sort: ResaleGiftsSort::Price,
                stars_only: true,
                ..ResaleGiftsFilter::default()
            },
            "",
        )
        .expect("load_resale_star_gifts");
    assert_eq!(resale.list.len(), 1);
    assert_eq!(resale.attributes_hash, 555);

    // Load into saved cache via craft query so updateStarGiftPrice & craftStarGift have manage_id
    fake.enqueue_ok(
        tl_fns::payments::GetCraftStarGifts::CONSTRUCTOR_ID,
        &tl_enums::payments::SavedStarGifts::SavedStarGifts(tl_types::payments::SavedStarGifts {
            count: 1,
            chat_notifications_enabled: None,
            gifts: vec![synthetic_saved_gift(601, u1.clone(), false)],
            next_offset: None,
            chats: Vec::new(),
            users: Vec::new(),
        }),
    );
    let craftable = live
        .load_craft_star_gifts(10_002, "")
        .expect("load_craft_star_gifts");
    assert_eq!(craftable.len(), 1);

    fake.enqueue_ok(
        tl_fns::payments::UpdateStarGiftPrice::CONSTRUCTOR_ID,
        &empty_updates(),
    );
    live.update_star_gift_price(88_010, CreditsAmount::stars(600))
        .expect("update_star_gift_price");

    fake.enqueue_ok(
        tl_fns::payments::GetPaymentForm::CONSTRUCTOR_ID,
        &tl_enums::payments::PaymentForm::StarGift(tl_types::payments::PaymentFormStarGift {
            form_id: 7001,
            invoice: tl_enums::Invoice::Invoice(tl_types::Invoice {
                test: true,
                name_requested: false,
                phone_requested: false,
                email_requested: false,
                shipping_address_requested: false,
                flexible: false,
                phone_to_provider: false,
                email_to_provider: false,
                recurring: false,
                currency: "XTR".to_owned(),
                prices: vec![tl_enums::LabeledPrice::LabeledPrice(
                    tl_types::LabeledPrice {
                        label: "Resale".to_owned(),
                        amount: 450,
                    },
                )],
                max_tip_amount: None,
                suggested_tip_amounts: None,
                terms_url: None,
                subscription_period: None,
            }),
        }),
    );
    fake.enqueue_ok(
        tl_fns::payments::SendStarsForm::CONSTRUCTOR_ID,
        &tl_enums::payments::PaymentResult::PaymentResult(tl_types::payments::PaymentResult {
            updates: empty_updates(),
        }),
    );
    live.buy_resale_star_gift("AuroraGem-10", "", false, "Nice!")
        .expect("buy_resale_star_gift");

    // 2. Auctions: active auctions, single state, acquired gifts, place bid
    let auction_gift = tl_enums::StarGift::StarGift(tl_types::StarGift {
        limited: true,
        sold_out: false,
        birthday: false,
        require_premium: false,
        limited_per_user: false,
        peer_color_available: false,
        auction: true,
        id: 10_099,
        sticker: tl_enums::Document::Empty(tl_types::DocumentEmpty { id: 3009 }),
        stars: 100,
        availability_remains: Some(20),
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
        upgrade_variants: None,
        background: None,
    });
    let auction_state_inner =
        tl_enums::StarGiftAuctionState::StarGiftAuctionState(tl_types::StarGiftAuctionState {
            version: 2,
            start_date: 1_763_700_000,
            end_date: 1_763_720_000,
            min_bid_amount: 500,
            bid_levels: vec![tl_enums::AuctionBidLevel::AuctionBidLevel(
                tl_types::AuctionBidLevel {
                    pos: 1,
                    amount: 1_200,
                    date: 1_763_700_100,
                },
            )],
            top_bidders: vec![600_001],
            next_round_at: 1_763_703_600,
            last_gift_num: 10,
            gifts_left: 20,
            current_round: 2,
            total_rounds: 5,
            rounds: Vec::new(),
        });
    let user_state_inner = tl_enums::StarGiftAuctionUserState::StarGiftAuctionUserState(
        tl_types::StarGiftAuctionUserState {
            returned: false,
            bid_amount: Some(1_200),
            bid_date: Some(1_763_700_100),
            min_bid_amount: Some(1_300),
            bid_peer: Some(tl_enums::Peer::User(tl_types::PeerUser {
                user_id: 600_001,
            })),
            acquired_count: 1,
        },
    );

    fake.enqueue_ok(
        tl_fns::payments::GetStarGiftActiveAuctions::CONSTRUCTOR_ID,
        &tl_enums::payments::StarGiftActiveAuctions::StarGiftActiveAuctions(
            tl_types::payments::StarGiftActiveAuctions {
                auctions: vec![
                    tl_enums::StarGiftActiveAuctionState::StarGiftActiveAuctionState(
                        tl_types::StarGiftActiveAuctionState {
                            gift: auction_gift.clone(),
                            state: auction_state_inner.clone(),
                            user_state: user_state_inner.clone(),
                        },
                    ),
                ],
                users: Vec::new(),
                chats: Vec::new(),
            },
        ),
    );
    let active = live.load_active_auctions().expect("load_active_auctions");
    assert_eq!(active.auctions.len(), 1);
    assert!(active.auctions[0].is_my_bid_winning());

    fake.enqueue_ok(
        tl_fns::payments::GetStarGiftAuctionState::CONSTRUCTOR_ID,
        &tl_enums::payments::StarGiftAuctionState::StarGiftAuctionState(
            tl_types::payments::StarGiftAuctionState {
                gift: auction_gift,
                state: auction_state_inner,
                user_state: user_state_inner,
                timeout: 15,
                users: Vec::new(),
                chats: Vec::new(),
            },
        ),
    );
    let single = live
        .load_auction_state(10_099, 0)
        .expect("load_auction_state");
    assert_eq!(single.current_round, 2);

    fake.enqueue_ok(
        tl_fns::payments::GetStarGiftAuctionAcquiredGifts::CONSTRUCTOR_ID,
        &tl_enums::payments::StarGiftAuctionAcquiredGifts::StarGiftAuctionAcquiredGifts(
            tl_types::payments::StarGiftAuctionAcquiredGifts {
                gifts: vec![
                    tl_enums::StarGiftAuctionAcquiredGift::StarGiftAuctionAcquiredGift(
                        tl_types::StarGiftAuctionAcquiredGift {
                            name_hidden: false,
                            peer: tl_enums::Peer::User(tl_types::PeerUser { user_id: 600_001 }),
                            date: 1_763_700_200,
                            bid_amount: 1_100,
                            round: 1,
                            pos: 1,
                            message: None,
                            gift_num: Some(4),
                        },
                    ),
                ],
                users: Vec::new(),
                chats: Vec::new(),
            },
        ),
    );
    let acq = live
        .load_auction_acquired_gifts(10_099)
        .expect("load_auction_acquired_gifts");
    assert_eq!(acq.len(), 1);

    fake.enqueue_ok(
        tl_fns::payments::GetPaymentForm::CONSTRUCTOR_ID,
        &tl_enums::payments::PaymentForm::StarGift(tl_types::payments::PaymentFormStarGift {
            form_id: 7002,
            invoice: tl_enums::Invoice::Invoice(tl_types::Invoice {
                test: true,
                name_requested: false,
                phone_requested: false,
                email_requested: false,
                shipping_address_requested: false,
                flexible: false,
                phone_to_provider: false,
                email_to_provider: false,
                recurring: false,
                currency: "XTR".to_owned(),
                prices: vec![tl_enums::LabeledPrice::LabeledPrice(
                    tl_types::LabeledPrice {
                        label: "Bid".to_owned(),
                        amount: 1_300,
                    },
                )],
                max_tip_amount: None,
                suggested_tip_amounts: None,
                terms_url: None,
                subscription_period: None,
            }),
        }),
    );
    fake.enqueue_ok(
        tl_fns::payments::SendStarsForm::CONSTRUCTOR_ID,
        &tl_enums::payments::PaymentResult::PaymentResult(tl_types::payments::PaymentResult {
            updates: empty_updates(),
        }),
    );
    live.place_auction_bid(10_099, 1_300, true, false, "", "")
        .expect("place_auction_bid");

    // 3. Crafting (`payments.craftStarGift`) — test craft failure update handling
    fake.enqueue_ok(
        tl_fns::payments::CraftStarGift::CONSTRUCTOR_ID,
        &tl_enums::Updates::Updates(tl_types::Updates {
            updates: vec![tl_enums::Update::StarGiftCraftFail],
            users: Vec::new(),
            chats: Vec::new(),
            date: 1_763_760_000,
            seq: 2,
        }),
    );
    let craft_res = live.craft_star_gift(&[88_010]).expect("craft_star_gift");
    assert!(matches!(craft_res, CraftExecutionOutcome::Failed { .. }));

    shutdown_harness(runtime, live);
}

#[test]
fn live_giveaways_replace_boost_and_starref_affiliate_rpcs() {
    let (fake, runtime, mut live) = start_harness();

    let chan_pid = DataPeerId::from_channel(ChannelId::from_tl(2_000_500));

    // 1. load_giveaway_options -> getPremiumGiftCodeOptions + getStarsGiveawayOptions + getBoostsStatus
    fake.enqueue_ok(
        tl_fns::payments::GetPremiumGiftCodeOptions::CONSTRUCTOR_ID,
        &vec![tl_enums::PremiumGiftCodeOption::PremiumGiftCodeOption(
            tl_types::PremiumGiftCodeOption {
                users: 10,
                months: 3,
                store_product: Some("prem_3m_10".to_owned()),
                store_quantity: Some(10),
                currency: "USD".to_owned(),
                amount: 11_990,
            },
        )],
    );
    fake.enqueue_ok(
        tl_fns::payments::GetStarsGiveawayOptions::CONSTRUCTOR_ID,
        &vec![tl_enums::StarsGiveawayOption::StarsGiveawayOption(
            tl_types::StarsGiveawayOption {
                extended: false,
                default: true,
                stars: 1_000,
                yearly_boosts: 4,
                store_product: Some("stars_1000".to_owned()),
                currency: "USD".to_owned(),
                amount: 1_999,
                winners: vec![
                    tl_enums::StarsGiveawayWinnersOption::StarsGiveawayWinnersOption(
                        tl_types::StarsGiveawayWinnersOption {
                            default: true,
                            users: 5,
                            per_user_stars: 200,
                        },
                    ),
                ],
            },
        )],
    );
    fake.enqueue_ok(
        tl_fns::premium::GetBoostsStatus::CONSTRUCTOR_ID,
        &tl_enums::premium::BoostsStatus::BoostsStatus(tl_types::premium::BoostsStatus {
            my_boost: true,
            level: 5,
            current_level_boosts: 20,
            boosts: 24,
            gift_boosts: Some(4),
            next_level_boosts: Some(30),
            premium_audience: None,
            boost_url: "https://t.me/boost/northwind_chan".to_owned(),
            prepaid_giveaways: Some(vec![tl_enums::PrepaidGiveaway::PrepaidGiveaway(
                tl_types::PrepaidGiveaway {
                    id: 501,
                    months: 3,
                    quantity: 10,
                    date: 1_763_600_000,
                },
            )]),
            my_boost_slots: Some(vec![1]),
        }),
    );

    let gw_opts = live
        .load_giveaway_options("@northwind_chan")
        .expect("load_giveaway_options");
    assert_eq!(gw_opts.prepaid_giveaways.len(), 1);
    assert_eq!(gw_opts.stars_options.len(), 1);

    // 2. Launch prepaid giveaway (`payments.launchPrepaidGiveaway`)
    let future_date = tg_app::live::convert::now_unix_i32() + 86_400 * 2;
    let prepaid_draft = GiveawayDraft {
        boost_peer: chan_pid,
        prize: GiveawayPrizeKind::Premium { months: 3 },
        quantity: 10,
        additional_peers: Vec::new(),
        countries_iso2: vec!["US".to_owned()],
        prize_description: "Bonus pack".to_owned(),
        until_date: future_date,
        only_new_subscribers: true,
        winners_are_visible: true,
        prepaid_id: Some(501),
    };
    fake.enqueue_ok(
        tl_fns::payments::LaunchPrepaidGiveaway::CONSTRUCTOR_ID,
        &empty_updates(),
    );
    let launch_msg = live
        .create_or_launch_giveaway(&prepaid_draft)
        .expect("launch prepaid");
    assert!(launch_msg.contains("501"));

    // 3. New giveaway invoice (`payments.getPaymentForm`)
    let mut invoice_draft = prepaid_draft;
    invoice_draft.prepaid_id = None;
    fake.enqueue_ok(
        tl_fns::payments::GetPaymentForm::CONSTRUCTOR_ID,
        &tl_enums::payments::PaymentForm::PaymentForm(tl_types::payments::PaymentForm {
            can_save_credentials: false,
            password_missing: false,
            form_id: 9001,
            bot_id: 600_099,
            title: "Giveaway".to_owned(),
            description: "10x Premium".to_owned(),
            photo: None,
            invoice: tl_enums::Invoice::Invoice(tl_types::Invoice {
                test: true,
                name_requested: false,
                phone_requested: false,
                email_requested: false,
                shipping_address_requested: false,
                flexible: false,
                phone_to_provider: false,
                email_to_provider: false,
                recurring: false,
                currency: "USD".to_owned(),
                prices: vec![tl_enums::LabeledPrice::LabeledPrice(
                    tl_types::LabeledPrice {
                        label: "Giveaway".to_owned(),
                        amount: 11_990,
                    },
                )],
                max_tip_amount: None,
                suggested_tip_amounts: None,
                terms_url: None,
                subscription_period: None,
            }),
            provider_id: 600_099,
            url: "https://example.test/giveaway".to_owned(),
            native_provider: None,
            native_params: None,
            additional_methods: None,
            saved_info: None,
            saved_credentials: None,
            users: Vec::new(),
        }),
    );
    let form_msg = live
        .create_or_launch_giveaway(&invoice_draft)
        .expect("create giveaway invoice");
    assert!(form_msg.contains("9001"));

    // 4. Giveaway info & results (`payments.getGiveawayInfo`)
    fake.enqueue_ok(
        tl_fns::payments::GetGiveawayInfo::CONSTRUCTOR_ID,
        &tl_enums::payments::GiveawayInfo::Results(tl_types::payments::GiveawayInfoResults {
            winner: true,
            refunded: false,
            start_date: 1_763_000_000,
            gift_code_slug: Some("WIN_SLUG_1".to_owned()),
            stars_prize: None,
            finish_date: 1_763_600_000,
            winners_count: 10,
            activated_count: Some(8),
        }),
    );
    let gw_info = live
        .load_giveaway_info("@northwind_chan", 44)
        .expect("load_giveaway_info");
    assert_eq!(gw_info.state, GiveawayState::Finished);
    assert_eq!(gw_info.gift_code, "WIN_SLUG_1");

    // 5. Replace boost (`premium.getMyBoosts` + `premium.applyBoost`)
    let my_boosts_resp = tl_enums::premium::MyBoosts::MyBoosts(tl_types::premium::MyBoosts {
        my_boosts: vec![
            tl_enums::MyBoost::MyBoost(tl_types::MyBoost {
                slot: 1,
                peer: Some(tl_enums::Peer::Channel(tl_types::PeerChannel {
                    channel_id: 2_000_500,
                })),
                date: 1_763_000_000,
                expires: 1_778_000_000,
                cooldown_until_date: None,
            }),
            tl_enums::MyBoost::MyBoost(tl_types::MyBoost {
                slot: 2,
                peer: None,
                date: 0,
                expires: 0,
                cooldown_until_date: None,
            }),
        ],
        chats: Vec::new(),
        users: Vec::new(),
    });
    fake.enqueue_ok(
        tl_fns::premium::GetMyBoosts::CONSTRUCTOR_ID,
        &my_boosts_resp,
    );
    let slots_bundle = live
        .load_boost_slots_for_channel("@northwind_chan")
        .expect("load_boost_slots");
    assert_eq!(slots_bundle.slots.already, vec![1]);
    assert_eq!(slots_bundle.slots.free, vec![2]);

    fake.enqueue_ok(tl_fns::premium::ApplyBoost::CONSTRUCTOR_ID, &my_boosts_resp);
    let applied = live
        .apply_boost_slots("@northwind_chan", &[2])
        .expect("apply_boost_slots");
    assert_eq!(applied.len(), 2);

    // 6. Stars Affiliate Programs (`getConnectedStarRefBots`, `getSuggestedStarRefBots`,
    //    `updateStarRefProgram`, `connectStarRefBot`, `editConnectedStarRefBot`)
    let conn_resp = tl_enums::payments::ConnectedStarRefBots::ConnectedStarRefBots(
        tl_types::payments::ConnectedStarRefBots {
            count: 1,
            connected_bots: vec![tl_enums::ConnectedBotStarRef::ConnectedBotStarRef(
                tl_types::ConnectedBotStarRef {
                    revoked: false,
                    url: "https://t.me/synth_demo_bot?start=_tgr_123".to_owned(),
                    date: 1_763_500_000,
                    bot_id: 600_099,
                    commission_permille: 200,
                    duration_months: Some(12),
                    participants: 9,
                    revenue: 180,
                },
            )],
            users: Vec::new(),
        },
    );
    fake.enqueue_ok(
        tl_fns::payments::GetConnectedStarRefBots::CONSTRUCTOR_ID,
        &conn_resp,
    );
    fake.enqueue_ok(
        tl_fns::payments::GetSuggestedStarRefBots::CONSTRUCTOR_ID,
        &tl_enums::payments::SuggestedStarRefBots::SuggestedStarRefBots(
            tl_types::payments::SuggestedStarRefBots {
                count: 1,
                suggested_bots: vec![tl_enums::StarRefProgram::StarRefProgram(
                    tl_types::StarRefProgram {
                        bot_id: 600_099,
                        commission_permille: 200,
                        duration_months: Some(12),
                        end_date: None,
                        daily_revenue_per_user: None,
                    },
                )],
                users: Vec::new(),
                next_offset: None,
            },
        ),
    );
    let overview = live
        .load_starref_overview("", SuggestedStarRefSort::Revenue)
        .expect("load_starref_overview");
    assert_eq!(overview.connected.len(), 1);
    assert_eq!(overview.suggested.len(), 1);

    fake.enqueue_ok(
        tl_fns::bots::UpdateStarRefProgram::CONSTRUCTOR_ID,
        &tl_enums::StarRefProgram::StarRefProgram(tl_types::StarRefProgram {
            bot_id: 600_099,
            commission_permille: 250,
            duration_months: Some(24),
            end_date: None,
            daily_revenue_per_user: None,
        }),
    );
    let updated_prog = live
        .update_starref_program("@synth_demo_bot", 250, 24)
        .expect("update_starref_program");
    assert_eq!(updated_prog.commission_permille, 250);

    fake.enqueue_ok(
        tl_fns::payments::ConnectStarRefBot::CONSTRUCTOR_ID,
        &conn_resp,
    );
    let joined = live
        .connect_starref_bot("", "@synth_demo_bot")
        .expect("connect_starref_bot");
    assert!(joined.url.contains("_tgr_123"));

    let revoked_resp = tl_enums::payments::ConnectedStarRefBots::ConnectedStarRefBots(
        tl_types::payments::ConnectedStarRefBots {
            count: 1,
            connected_bots: vec![tl_enums::ConnectedBotStarRef::ConnectedBotStarRef(
                tl_types::ConnectedBotStarRef {
                    revoked: true,
                    url: "https://t.me/synth_demo_bot?start=_tgr_123".to_owned(),
                    date: 1_763_500_000,
                    bot_id: 600_099,
                    commission_permille: 200,
                    duration_months: Some(12),
                    participants: 9,
                    revenue: 180,
                },
            )],
            users: Vec::new(),
        },
    );
    fake.enqueue_ok(
        tl_fns::payments::EditConnectedStarRefBot::CONSTRUCTOR_ID,
        &revoked_resp,
    );
    let edited = live
        .edit_connected_starref_bot("", "https://t.me/synth_demo_bot?start=_tgr_123", true)
        .expect("edit_connected_starref_bot");
    assert!(edited.revoked);

    shutdown_harness(runtime, live);
}

#[test]
fn mock_session_and_ui_screens_exercise_wp16_end_to_end() {
    let mut mock_session = MockSession::new(true);
    let page = mock_session
        .load_peer_gifts("", &PeerGiftsFilter::default(), "")
        .expect("mock_session peer gifts");
    assert!(!page.items.is_empty());
    assert!(!page.collections.is_empty());

    let created_col = mock_session
        .create_star_gift_collection("", "Showcase", &[88_001])
        .expect("create collection");
    assert_eq!(created_col.title, "Showcase");

    let resale = mock_session
        .load_resale_star_gifts(10_002, &ResaleGiftsFilter::default(), "")
        .expect("resale");
    assert!(!resale.list.is_empty());

    let auctions = mock_session.load_active_auctions().expect("auctions");
    assert!(!auctions.auctions.is_empty());

    let craft_res = mock_session
        .craft_star_gift(&[88_001, 88_005])
        .expect("craft");
    assert!(matches!(craft_res, CraftExecutionOutcome::Succeeded { .. }));

    let starref = mock_session
        .load_starref_overview("", SuggestedStarRefSort::Profitability)
        .expect("starref");
    assert!(!starref.connected.is_empty());
    assert!(!starref.suggested.is_empty());

    // Render every new UI screen/tab through `PaymentsScreenHost::with_mock()`
    let ctx = egui::Context::default();
    let palette = Palette::day();
    let mut host = PaymentsScreenHost::with_mock();
    // The tabbed screens below are the developer view (`TGFORK_PAYMENTS_DEV_OVERLAY=1`).
    host.set_dev_tabs(true);

    let screens_and_tabs: Vec<(PaymentsScreen, Option<StarGiftsTab>)> = vec![
        (PaymentsScreen::StarGifts, Some(StarGiftsTab::Received)),
        (PaymentsScreen::StarGifts, Some(StarGiftsTab::Resale)),
        (PaymentsScreen::StarGifts, Some(StarGiftsTab::Auctions)),
        (PaymentsScreen::StarGifts, Some(StarGiftsTab::Craft)),
        (PaymentsScreen::BoostsGiveaways, None),
        (PaymentsScreen::Affiliate, None),
    ];

    for (screen, gift_tab) in screens_and_tabs {
        host.select_screen(screen);
        if let Some(tab) = gift_tab
            && let Some(gv) = host.gifts_view_mut()
        {
            gv.active_tab = tab;
        }
        let input = RawInput {
            screen_rect: Some(Rect::from_min_size(
                egui::Pos2::ZERO,
                Vec2::new(620.0, 800.0),
            )),
            ..RawInput::default()
        };
        let mut out = ctx.run_ui(input, |ui| {
            let _ = host.show_at(ui, &palette, true, 1_000);
        });
        out.textures_delta.clear();
    }
}
