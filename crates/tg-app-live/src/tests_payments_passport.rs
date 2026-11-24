//! Live end-to-end tests for WP-55 Payments, Stars, Star Gifts & Telegram Passport RPCs
//! wired through `LiveSession` (`PaymentsBackend`, `StarGiftsMarketplaceBackend`, `PassportBackend`)
//! against an in-process `FakeRpcClient` (zero network, synthetic fixtures only).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use tg_api::convert::apply_tl_users_and_chats;
use tg_api::{DcId, InvocationError, RpcClient};
use tg_crypto::dh::{GOOD_PRIME, mod_exp};
use tg_crypto::hash::{sha256, sha512};
use tg_crypto::pbkdf2_hmac_sha512_64;
use tg_crypto::rsa::PRODUCTION_PUBLIC_KEYS;
use tg_crypto::srp::{
    SecureSecretAlgo, check_secret_bytes, compute_secure_secret_hash, count_secure_secret_id,
    encrypt_secure_secret,
};
use tg_data::{PeerId as DataPeerId, UserId as DataUserId};
use tg_passport::encryption::encrypt_data_with_secret;
use tg_passport::types::{FormRequest, ValueMap, ValueType};
use tg_payments::star_gifts::{CanSendGiftResult, PeerGiftsFilter, compute_gift_collections_hash};
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_tl::{
    BOOL_FALSE_ID, BOOL_TRUE_ID, Deserializable, Identifiable, RemoteCall, Serializable,
    enums as tl_enums, functions as tl_fns, types as tl_types,
};
use tokio::sync::broadcast;

use crate::LiveSession;
use tg_app_core::payments::backend::{PassportBackend, PaymentsBackend};
use tg_app_core::payments::marketplace_backend::StarGiftsMarketplaceBackend;

type QueuedResponse = (u32, Result<Vec<u8>, InvocationError>);
type RecordedCalls = Arc<Mutex<Vec<(u32, Vec<u8>)>>>;

#[derive(Clone)]
struct FakeRpcClient {
    main_dc: Arc<Mutex<DcId>>,
    recorded: RecordedCalls,
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

    fn enqueue_bool(&self, expected_ctor: u32, val: bool) {
        let id = if val { BOOL_TRUE_ID } else { BOOL_FALSE_ID };
        self.responses
            .lock()
            .expect("lock responses")
            .push_back((expected_ctor, Ok(id.to_le_bytes().to_vec())));
    }

    fn enqueue_rpc_err(&self, expected_ctor: u32, code: i32, name: &str) {
        self.responses.lock().expect("lock responses").push_back((
            expected_ctor,
            Err(InvocationError::Rpc(tg_api::RpcError::new(code, name))),
        ));
    }

    fn recorded_of(&self, ctor: u32) -> Vec<Vec<u8>> {
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
        self.invoke_in_dc(self.main_dc_id(), req).await
    }

    async fn invoke_in_dc<R: RemoteCall + Send + Sync>(
        &self,
        _dc: DcId,
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

fn synthetic_user(id: i64, first: &str, username: &str, is_self: bool) -> tl_types::User {
    tl_types::User {
        self_: is_self,
        contact: !is_self,
        mutual_contact: false,
        deleted: false,
        bot: false,
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
        premium: false,
        attach_menu_enabled: false,
        bot_can_edit: false,
        close_friend: false,
        stories_hidden: false,
        stories_unavailable: true,
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
        access_hash: Some(id * 17 + 3),
        first_name: Some(first.to_owned()),
        last_name: None,
        username: (!username.is_empty()).then(|| username.to_owned()),
        phone: None,
        photo: None,
        status: None,
        bot_info_version: None,
        restriction_reason: None,
        bot_inline_placeholder: None,
        lang_code: None,
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
        stories_unavailable: true,
        signature_profiles: false,
        autotranslation: false,
        broadcast_messages_allowed: false,
        monoforum: false,
        forum_tabs: false,
        in_news: false,
        forum_sidebar: false,
        id,
        access_hash: Some(id * 31 + 9),
        title: title.to_owned(),
        username: Some(username.to_owned()),
        photo: tl_enums::ChatPhoto::Empty,
        date: 1_700_000_000,
        restriction_reason: None,
        admin_rights: None,
        banned_rights: None,
        default_banned_rights: None,
        participants_count: Some(12),
        usernames: None,
        stories_max_id: None,
        color: None,
        profile_color: None,
        emoji_status: None,
        level: None,
        subscription_until_date: None,
        bot_verification_icon: None,
        send_paid_messages_stars: None,
        linked_monoforum_id: None,
        linked_community_id: None,
        pinned_msg_id: None,
    }
}

fn make_live_session() -> (
    Arc<SessionRuntime<crate::ErasedRpcClient>>,
    FakeRpcClient,
    LiveSession,
) {
    let rpc = FakeRpcClient::new(2);
    let self_user = synthetic_user(100_001, "Self User", "selfuser", true);
    let friend_user = synthetic_user(100_002, "Friend", "friend_user", false);
    let bot_user = synthetic_user(100_003, "PassportBot", "passport_bot", false);
    let channel = synthetic_channel(200_001, "Gift News", "gift_news");

    rpc.enqueue_ok(
        tl_fns::users::GetUsers::CONSTRUCTOR_ID,
        &vec![tl_enums::User::User(self_user.clone())],
    );
    rpc.enqueue_ok(
        tl_fns::updates::GetState::CONSTRUCTOR_ID,
        &tl_enums::updates::State::State(tl_types::updates::State {
            pts: 10,
            qts: 0,
            date: 1_750_000_000,
            seq: 1,
            unread_count: 0,
        }),
    );
    rpc.enqueue_ok(
        tl_fns::updates::GetDifference::CONSTRUCTOR_ID,
        &tl_enums::updates::Difference::Empty(tl_types::updates::DifferenceEmpty {
            date: 1_750_000_001,
            seq: 1,
        }),
    );
    let make_dialog = |peer: tl_enums::Peer| {
        tl_enums::Dialog::Dialog(tl_types::Dialog {
            pinned: false,
            unread_mark: false,
            view_forum_as_messages: false,
            peer,
            top_message: 1,
            read_inbox_max_id: 1,
            read_outbox_max_id: 1,
            unread_count: 0,
            unread_mentions_count: 0,
            unread_reactions_count: 0,
            unread_poll_votes_count: 0,
            notify_settings: tl_enums::PeerNotifySettings::PeerNotifySettings(
                tl_types::PeerNotifySettings {
                    show_previews: Some(true),
                    silent: Some(false),
                    mute_until: Some(0),
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
    rpc.enqueue_ok(
        tl_fns::messages::GetDialogs::CONSTRUCTOR_ID,
        &tl_enums::messages::Dialogs::Dialogs(tl_types::messages::Dialogs {
            dialogs: vec![
                make_dialog(tl_enums::Peer::User(tl_types::PeerUser {
                    user_id: 100_002,
                })),
                make_dialog(tl_enums::Peer::Channel(tl_types::PeerChannel {
                    channel_id: 200_001,
                })),
            ],
            messages: Vec::new(),
            chats: vec![tl_enums::Chat::Channel(channel.clone())],
            users: vec![
                tl_enums::User::User(friend_user.clone()),
                tl_enums::User::User(bot_user.clone()),
            ],
        }),
    );
    rpc.enqueue_ok(
        tl_fns::messages::GetPinnedDialogs::CONSTRUCTOR_ID,
        &tl_enums::messages::PeerDialogs::PeerDialogs(tl_types::messages::PeerDialogs {
            dialogs: Vec::new(),
            messages: Vec::new(),
            chats: Vec::new(),
            users: Vec::new(),
            state: tl_enums::updates::State::State(tl_types::updates::State {
                pts: 10,
                qts: 0,
                date: 1_750_000_001,
                seq: 1,
                unread_count: 0,
            }),
        }),
    );

    let runtime = Arc::new(
        SessionRuntime::start(
            DataUserId::from_tl(100_001),
            crate::ErasedRpcClient::new(rpc.clone()),
            SessionRuntimeConfig::default(),
        )
        .expect("start SessionRuntime"),
    );
    runtime.tokio_handle().block_on(async {
        tokio::time::timeout(std::time::Duration::from_secs(15), runtime.wait_ready())
            .await
            .expect("wait_ready timeout")
            .expect("startup ready");
        let mut s = runtime.session().write().await;
        apply_tl_users_and_chats(
            &mut s,
            &[
                tl_enums::User::User(self_user),
                tl_enums::User::User(friend_user),
                tl_enums::User::User(bot_user),
            ],
            &[tl_enums::Chat::Channel(channel)],
        );
    });
    let live = LiveSession::new(Arc::clone(&runtime), 1);
    (runtime, rpc, live)
}

fn sample_saved_gift_tl(id: i64, msg_id: i32, slug: &str) -> tl_enums::SavedStarGift {
    tl_enums::SavedStarGift::SavedStarGift(tl_types::SavedStarGift {
        name_hidden: false,
        unsaved: false,
        refunded: false,
        can_upgrade: false,
        pinned_to_top: false,
        upgrade_separate: false,
        from_id: Some(tl_enums::Peer::User(tl_types::PeerUser {
            user_id: 100_002,
        })),
        date: 1_750_000_000,
        gift: tl_enums::StarGift::Unique(tl_types::StarGiftUnique {
            require_premium: false,
            resale_ton_only: false,
            theme_available: true,
            burned: false,
            crafted: false,
            id,
            gift_id: 700,
            title: "Starlight".to_owned(),
            slug: slug.to_owned(),
            num: 12,
            owner_id: Some(tl_enums::Peer::User(tl_types::PeerUser {
                user_id: 100_001,
            })),
            owner_name: Some("Self User".to_owned()),
            owner_address: None,
            attributes: Vec::new(),
            availability_issued: 12,
            availability_total: 100,
            gift_address: None,
            resell_amount: None,
            released_by: None,
            value_amount: None,
            value_currency: None,
            value_usd_amount: None,
            theme_peer: None,
            peer_color: None,
            host_id: None,
            offer_min_stars: None,
            craft_chance_permille: None,
        }),
        message: None,
        msg_id: Some(msg_id),
        saved_id: None,
        convert_stars: None,
        upgrade_stars: None,
        can_export_at: Some(1_740_000_000),
        transfer_stars: Some(25),
        can_transfer_at: Some(1_740_000_000),
        can_resell_at: Some(1_740_000_000),
        collection_id: None,
        prepaid_upgrade_hash: None,
        drop_original_details_stars: None,
        gift_num: Some(12),
        can_craft_at: Some(1_740_000_000),
    })
}

fn sample_password_tl(
    password: &[u8],
    srp_id: i64,
    has_secure_values: bool,
) -> tl_enums::account::Password {
    let salt1 = b"live-s1-12345678".to_vec();
    let salt2 = b"live-s2-87654321".to_vec();
    let h1 = sha256(&[&salt1, password, &salt1]);
    let h2 = sha256(&[&salt2, &h1, &salt2]);
    let h3 = pbkdf2_hmac_sha512_64(&h2, &salt1, 100_000);
    let x = sha256(&[&salt2, &h3, &salt2]);
    let g_x = mod_exp(&[3], &x, &GOOD_PRIME).expect("g^x");
    let mut srp_b = [0u8; 256];
    let len = g_x.len().min(256);
    srp_b[256 - len..].copy_from_slice(&g_x[g_x.len() - len..]);

    tl_enums::account::Password::Password(tl_types::account::Password {
        has_recovery: true,
        has_secure_values,
        has_password: true,
        current_algo: Some(
            tl_enums::PasswordKdfAlgo::SHA256SHA256PBKDF2HMACSHA512iter100000SHA256ModPow(
                tl_types::PasswordKdfAlgoSHA256SHA256PBKDF2HMACSHA512iter100000SHA256ModPow {
                    salt1: salt1.clone(),
                    salt2: salt2.clone(),
                    g: 3,
                    p: GOOD_PRIME.to_vec(),
                },
            ),
        ),
        srp_b: Some(srp_b.to_vec()),
        srp_id: Some(srp_id),
        hint: Some("hint".to_owned()),
        email_unconfirmed_pattern: None,
        new_algo: tl_enums::PasswordKdfAlgo::SHA256SHA256PBKDF2HMACSHA512iter100000SHA256ModPow(
            tl_types::PasswordKdfAlgoSHA256SHA256PBKDF2HMACSHA512iter100000SHA256ModPow {
                salt1,
                salt2,
                g: 3,
                p: GOOD_PRIME.to_vec(),
            },
        ),
        new_secure_algo: tl_enums::SecurePasswordKdfAlgo::PBKDF2HMACSHA512iter100000(
            tl_types::SecurePasswordKdfAlgoPBKDF2HMACSHA512iter100000 {
                salt: b"live-sec-salt".to_vec(),
            },
        ),
        secure_random: vec![0x55; 32],
        pending_reset_date: None,
        login_email_pattern: None,
    })
}

fn now_secs() -> i32 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i32
}

#[test]
fn live_wp55_star_gifts_rpcs_and_collections_hash_and_gift_stars_options() {
    let (_rt, rpc, mut live) = make_live_session();

    // 1. check_can_send_gift (Ok & Fail)
    rpc.enqueue_ok(
        tl_fns::payments::CheckCanSendGift::CONSTRUCTOR_ID,
        &tl_enums::payments::CheckCanSendGiftResult::Ok,
    );
    assert_eq!(
        StarGiftsMarketplaceBackend::check_can_send_gift(&live, 700).expect("check ok"),
        CanSendGiftResult::Ok
    );

    rpc.enqueue_ok(
        tl_fns::payments::CheckCanSendGift::CONSTRUCTOR_ID,
        &tl_enums::payments::CheckCanSendGiftResult::Fail(
            tl_types::payments::CheckCanSendGiftResultFail {
                reason: tl_enums::TextWithEntities::TextWithEntities(tl_types::TextWithEntities {
                    text: "Limited to Premium accounts.".to_owned(),
                    entities: Vec::new(),
                }),
            },
        ),
    );
    assert_eq!(
        PaymentsBackend::check_can_send_gift(&live, 701).expect("check fail"),
        CanSendGiftResult::Fail {
            reason: "Limited to Premium accounts.".to_owned(),
        }
    );

    // 2. load_stars_gift_options + topup_stars sends InputStorePaymentStarsGift (bug fix)
    rpc.enqueue_ok(
        tl_fns::payments::GetStarsGiftOptions::CONSTRUCTOR_ID,
        &vec![tl_enums::StarsGiftOption::StarsGiftOption(
            tl_types::StarsGiftOption {
                extended: false,
                stars: 300,
                store_product: Some("org.telegram.stars.300".to_owned()),
                currency: "USD".to_owned(),
                amount: 599,
            },
        )],
    );
    let opts = PaymentsBackend::load_stars_gift_options(&live, "@friend_user")
        .expect("load_stars_gift_options");
    assert_eq!(opts.len(), 1);
    assert_eq!(opts[0].credits, 300);
    assert_eq!(
        opts[0].gift_bare_peer_id,
        DataPeerId::from_user(DataUserId::from_tl(100_002)).raw()
    );

    rpc.enqueue_ok(
        tl_fns::payments::GetPaymentForm::CONSTRUCTOR_ID,
        &tl_enums::payments::PaymentForm::Stars(tl_types::payments::PaymentFormStars {
            form_id: 1234,
            bot_id: 100_002,
            title: "Gift Stars".to_owned(),
            description: "300 Stars".to_owned(),
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
                currency: "XTR".to_owned(),
                prices: vec![tl_enums::LabeledPrice::LabeledPrice(
                    tl_types::LabeledPrice {
                        label: "Stars".to_owned(),
                        amount: 300,
                    },
                )],
                max_tip_amount: None,
                suggested_tip_amounts: None,
                terms_url: None,
                subscription_period: None,
            }),
            users: Vec::new(),
        }),
    );
    let _ = live.topup_stars(&opts[0]).expect("topup_stars gift");
    let form_req = tl_fns::payments::GetPaymentForm::from_bytes(
        &rpc.recorded_of(tl_fns::payments::GetPaymentForm::CONSTRUCTOR_ID)[0],
    )
    .expect("decode getPaymentForm");
    assert!(matches!(
        form_req.invoice,
        tl_enums::InputInvoice::Stars(tl_types::InputInvoiceStars {
            purpose: tl_enums::InputStorePaymentPurpose::InputStorePaymentStarsGift(ref g)
        }) if g.stars == 300 && matches!(g.user_id, tl_enums::InputUser::InputUser(ref u) if u.user_id == 100_002)
    ));

    // 3. load_peer_gifts populates cache + collections hash caching
    rpc.enqueue_ok(
        tl_fns::payments::GetSavedStarGifts::CONSTRUCTOR_ID,
        &tl_enums::payments::SavedStarGifts::SavedStarGifts(tl_types::payments::SavedStarGifts {
            count: 2,
            chat_notifications_enabled: Some(true),
            gifts: vec![
                sample_saved_gift_tl(8001, 501, "Starlight-12"),
                sample_saved_gift_tl(8002, 502, "Starlight-13"),
            ],
            next_offset: None,
            chats: Vec::new(),
            users: Vec::new(),
        }),
    );
    rpc.enqueue_ok(
        tl_fns::payments::GetStarGiftCollections::CONSTRUCTOR_ID,
        &tl_enums::payments::StarGiftCollections::StarGiftCollections(
            tl_types::payments::StarGiftCollections {
                collections: vec![tl_enums::StarGiftCollection::StarGiftCollection(
                    tl_types::StarGiftCollection {
                        collection_id: 1,
                        title: "Best".to_owned(),
                        icon: None,
                        gifts_count: 2,
                        hash: 42_4242,
                    },
                )],
            },
        ),
    );
    let page = live
        .load_peer_gifts("", &PeerGiftsFilter::default(), "")
        .expect("load_peer_gifts");
    assert_eq!(page.items.len(), 2);
    assert_eq!(page.collections.len(), 1);

    // Follow-up load_star_gift_collections must send computed CountHash and accept NotModified.
    rpc.enqueue_ok(
        tl_fns::payments::GetStarGiftCollections::CONSTRUCTOR_ID,
        &tl_enums::payments::StarGiftCollections::NotModified,
    );
    let cached_cols = live
        .load_star_gift_collections("")
        .expect("load_star_gift_collections cached");
    assert_eq!(cached_cols.len(), 1);
    let col_calls = rpc.recorded_of(tl_fns::payments::GetStarGiftCollections::CONSTRUCTOR_ID);
    let second_col_req =
        tl_fns::payments::GetStarGiftCollections::from_bytes(&col_calls[1]).expect("decode");
    assert_eq!(
        second_col_req.hash,
        compute_gift_collections_hash(&page.collections)
    );

    // 4. get_saved_star_gift (single gift lookup)
    rpc.enqueue_ok(
        tl_fns::payments::GetSavedStarGift::CONSTRUCTOR_ID,
        &tl_enums::payments::SavedStarGifts::SavedStarGifts(tl_types::payments::SavedStarGifts {
            count: 1,
            chat_notifications_enabled: None,
            gifts: vec![sample_saved_gift_tl(8001, 501, "Starlight-12")],
            next_offset: None,
            chats: Vec::new(),
            users: Vec::new(),
        }),
    );
    let single = live.get_saved_star_gift(8001).expect("get_saved_star_gift");
    assert_eq!(single.saved.info.id, 8001);

    // 5. get_star_gift_withdrawal_url (SRP -> account.getTmpPassword(600) -> URL)
    rpc.enqueue_ok(
        tl_fns::account::GetPassword::CONSTRUCTOR_ID,
        &sample_password_tl(b"secret-2fa", 77, false),
    );
    rpc.enqueue_ok(
        tl_fns::account::GetTmpPassword::CONSTRUCTOR_ID,
        &tl_enums::account::TmpPassword::TmpPassword(tl_types::account::TmpPassword {
            tmp_password: vec![0xDE, 0xAD, 0xBE, 0xEF],
            valid_until: now_secs() + 600,
        }),
    );
    rpc.enqueue_ok(
        tl_fns::payments::GetStarGiftWithdrawalUrl::CONSTRUCTOR_ID,
        &tl_enums::payments::StarGiftWithdrawalUrl::StarGiftWithdrawalUrl(
            tl_types::payments::StarGiftWithdrawalUrl {
                url: "https://fragment.com/gift/Starlight-12".to_owned(),
            },
        ),
    );
    let url = live
        .get_star_gift_withdrawal_url(8001, "secret-2fa")
        .expect("get_star_gift_withdrawal_url");
    assert_eq!(url, "https://fragment.com/gift/Starlight-12");

    // 6. withdraw_star_gift: first cached tmp_password refused -> drops & retries with fresh SRP!
    rpc.enqueue_rpc_err(
        tl_fns::payments::WithdrawStarGift::CONSTRUCTOR_ID,
        400,
        "TMP_PASSWORD_INVALID",
    );
    rpc.enqueue_ok(
        tl_fns::account::GetPassword::CONSTRUCTOR_ID,
        &sample_password_tl(b"secret-2fa", 78, false),
    );
    rpc.enqueue_ok(
        tl_fns::account::GetTmpPassword::CONSTRUCTOR_ID,
        &tl_enums::account::TmpPassword::TmpPassword(tl_types::account::TmpPassword {
            tmp_password: vec![0xCA, 0xFE, 0xBA, 0xBE],
            valid_until: now_secs() + 600,
        }),
    );
    rpc.enqueue_bool(tl_fns::payments::WithdrawStarGift::CONSTRUCTOR_ID, true);
    let withdrawn = live
        .withdraw_star_gift(8001, "secret-2fa", true)
        .expect("withdraw_star_gift after retry");
    assert!(withdrawn.contains("8001"));
    assert_eq!(
        rpc.recorded_of(tl_fns::payments::WithdrawStarGift::CONSTRUCTOR_ID)
            .len(),
        2
    );

    // 7. delete_star_gift
    rpc.enqueue_bool(tl_fns::payments::DeleteStarGift::CONSTRUCTOR_ID, true);
    let deleted = live.delete_star_gift(8002).expect("delete_star_gift");
    assert!(deleted.contains("8002"));

    // 8. toggle_chat_star_gift_notifications
    rpc.enqueue_bool(
        tl_fns::payments::ToggleChatStarGiftNotifications::CONSTRUCTOR_ID,
        true,
    );
    assert!(
        live.toggle_chat_star_gift_notifications("@gift_news", true)
            .expect("toggle notifications")
    );
    let toggle_req = tl_fns::payments::ToggleChatStarGiftNotifications::from_bytes(
        &rpc.recorded_of(tl_fns::payments::ToggleChatStarGiftNotifications::CONSTRUCTOR_ID)[0],
    )
    .expect("decode toggle");
    assert!(toggle_req.enabled);
}

#[test]
fn live_wp55_passport_config_and_full_srp_save_accept_roundtrip() {
    let (_rt, rpc, mut live) = make_live_session();
    let password = b"passport-live-pass";

    // Build deterministic master secret & pre-encrypted PersonalDetails (residence = US)
    let mut master_secret = [0x31u8; 32];
    let sum: u64 = master_secret.iter().map(|&b| u64::from(b)).sum();
    master_secret[0] = ((u64::from(master_secret[0]) + 255 + 239 - (sum % 255)) % 255) as u8;
    assert!(check_secret_bytes(&master_secret));
    let master_id = count_secure_secret_id(&master_secret);
    let sec_algo = SecureSecretAlgo::Pbkdf2HmacSha512Iter100000 {
        salt: b"live-sec-salt".to_vec(),
    };
    let sec_hash = compute_secure_secret_hash(&sec_algo, password);
    let enc_master = encrypt_secure_secret(&master_secret, &sec_hash).expect("enc master");

    let mut val_secret = [0x41u8; 32];
    let vsum: u64 = val_secret.iter().map(|&b| u64::from(b)).sum();
    val_secret[0] = ((u64::from(val_secret[0]) + 255 + 239 - (vsum % 255)) % 255) as u8;
    let initial_json = br#"{"first_name":"Alex","last_name":"River","birth_date":"01.01.1995","gender":"male","country_code":"US","residence_country_code":"US"}"#;
    let enc_val = encrypt_data_with_secret(initial_json, &val_secret).expect("enc val");
    let kv = sha512(&[&master_secret, &enc_val.hash]);
    let enc_val_sec = tg_crypto::aes::aes_cbc_encrypt(
        &val_secret,
        &kv[0..32].try_into().unwrap(),
        &kv[32..48].try_into().unwrap(),
    )
    .expect("enc val secret");

    // Configure FormRequest using valid RSA PEM key from tg-crypto test fixtures
    let bot_pem = PRODUCTION_PUBLIC_KEYS[0];
    live.configure_form_request(FormRequest::new(
        DataUserId::from_tl(100_003),
        r#"{"data":["personal_details"],"v":1}"#,
        "https://bot.example.invalid/cb",
        bot_pem,
        "live-nonce-1",
    ));

    // Enqueue: help.getPassportConfig, account.getAuthorizationForm, account.getPassword
    rpc.enqueue_ok(
        tl_fns::help::GetPassportConfig::CONSTRUCTOR_ID,
        &tl_enums::help::PassportConfig::PassportConfig(tl_types::help::PassportConfig {
            hash: 0x4242,
            countries_langs: tl_enums::DataJSON::DataJSON(tl_types::DataJSON {
                data: r#"{"US":"en","DE":"de"}"#.to_owned(),
            }),
        }),
    );
    rpc.enqueue_ok(
        tl_fns::account::GetAuthorizationForm::CONSTRUCTOR_ID,
        &tl_enums::account::AuthorizationForm::AuthorizationForm(
            tl_types::account::AuthorizationForm {
                required_types: vec![tl_enums::SecureRequiredType::SecureRequiredType(
                    tl_types::SecureRequiredType {
                        native_names: true,
                        selfie_required: false,
                        translation_required: false,
                        r#type: tl_enums::SecureValueType::PersonalDetails,
                    },
                )],
                values: vec![tl_enums::SecureValue::SecureValue(tl_types::SecureValue {
                    r#type: tl_enums::SecureValueType::PersonalDetails,
                    data: Some(tl_enums::SecureData::SecureData(tl_types::SecureData {
                        data: enc_val.bytes.clone(),
                        data_hash: enc_val.hash.to_vec(),
                        secret: enc_val_sec.clone(),
                    })),
                    front_side: None,
                    reverse_side: None,
                    selfie: None,
                    translation: None,
                    files: None,
                    plain_data: None,
                    hash: vec![0x77; 32],
                })],
                errors: Vec::new(),
                users: Vec::new(),
                privacy_policy_url: None,
            },
        ),
    );
    rpc.enqueue_ok(
        tl_fns::account::GetPassword::CONSTRUCTOR_ID,
        &sample_password_tl(password, 901, true),
    );

    let bundle = live
        .load_authorization_form("passport_bot")
        .expect("load_authorization_form");
    assert!(bundle.has_cloud_password);
    assert_eq!(live.passport_config().hash, 0x4242);
    assert_eq!(live.passport_config().preferred_language("US"), "en");

    // Calling load_passport_config now sends hash = 0x4242 and handles NotModified!
    rpc.enqueue_ok(
        tl_fns::help::GetPassportConfig::CONSTRUCTOR_ID,
        &tl_enums::help::PassportConfig::NotModified,
    );
    let refreshed_cfg = live.load_passport_config().expect("load_passport_config");
    assert_eq!(refreshed_cfg.hash, 0x4242);
    assert_eq!(refreshed_cfg.preferred_language("DE"), "de");
    let cfg_calls = rpc.recorded_of(tl_fns::help::GetPassportConfig::CONSTRUCTOR_ID);
    assert_eq!(cfg_calls.len(), 2);
    assert_eq!(
        tl_fns::help::GetPassportConfig::from_bytes(&cfg_calls[1])
            .unwrap()
            .hash,
        0x4242
    );

    // Unlock via SRP -> account.getPasswordSettings -> decrypts PersonalDetails AND triggers
    // fill_native_from_fallback() using the loaded `US -> en` PassportConfig!
    rpc.enqueue_ok(
        tl_fns::account::GetPasswordSettings::CONSTRUCTOR_ID,
        &tl_enums::account::PasswordSettings::PasswordSettings(
            tl_types::account::PasswordSettings {
                email: Some("alex@example.invalid".to_owned()),
                secure_settings: Some(tl_enums::SecureSecretSettings::SecureSecretSettings(
                    tl_types::SecureSecretSettings {
                        secure_algo: tl_enums::SecurePasswordKdfAlgo::PBKDF2HMACSHA512iter100000(
                            tl_types::SecurePasswordKdfAlgoPBKDF2HMACSHA512iter100000 {
                                salt: b"live-sec-salt".to_vec(),
                            },
                        ),
                        secure_secret: enc_master.to_vec(),
                        secure_secret_id: master_id as i64,
                    },
                )),
            },
        ),
    );
    let unlocked = live
        .verify_cloud_password("passport-live-pass")
        .expect("verify_cloud_password");
    let pd = &unlocked.form.values[&ValueType::PersonalDetails]
        .data
        .parsed;
    assert_eq!(pd.text("first_name"), "Alex");
    assert_eq!(
        pd.text("first_name_native"),
        "Alex",
        "PassportConfig US->en must populate native fallback on unlock"
    );
    assert_eq!(pd.text("last_name_native"), "River");

    // Save edited value (account.saveSecureValue)
    rpc.enqueue_ok(
        tl_fns::account::SaveSecureValue::CONSTRUCTOR_ID,
        &tl_enums::SecureValue::SecureValue(tl_types::SecureValue {
            r#type: tl_enums::SecureValueType::PersonalDetails,
            data: Some(tl_enums::SecureData::SecureData(tl_types::SecureData {
                data: enc_val.bytes,
                data_hash: enc_val.hash.to_vec(),
                secret: enc_val_sec,
            })),
            front_side: None,
            reverse_side: None,
            selfie: None,
            translation: None,
            files: None,
            plain_data: None,
            hash: vec![0x88; 32],
        }),
    );
    let _saved_form = live
        .save_passport_value(
            ValueType::PersonalDetails,
            ValueMap::from_texts([("first_name_native", "Alex"), ("last_name_native", "River")]),
            Vec::new(),
        )
        .expect("save_passport_value");
    assert_eq!(
        rpc.recorded_of(tl_fns::account::SaveSecureValue::CONSTRUCTOR_ID)
            .len(),
        1
    );

    // Accept authorization (account.acceptAuthorization)
    // Note: since server returned the pre-encrypted value without native names, re-run
    // load_passport_config so fill_native_from_fallback populates native names before accept.
    rpc.enqueue_ok(
        tl_fns::help::GetPassportConfig::CONSTRUCTOR_ID,
        &tl_enums::help::PassportConfig::NotModified,
    );
    let _ = live.load_passport_config().expect("refresh config");

    rpc.enqueue_bool(tl_fns::account::AcceptAuthorization::CONSTRUCTOR_ID, true);
    let callback = live.accept_authorization().expect("accept_authorization");
    assert_eq!(
        callback,
        "https://bot.example.invalid/cb?tg_passport=success"
    );
    assert_eq!(
        rpc.recorded_of(tl_fns::account::AcceptAuthorization::CONSTRUCTOR_ID)
            .len(),
        1
    );
}
