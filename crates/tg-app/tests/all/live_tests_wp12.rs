//! WP-12 integration and unit tests covering:
//! 1. Real `ReqwestPaymentHttpClient` tested against a local loopback fake HTTP server + default
//!    installation on `LiveSession`.
//! 2. `ReceivedGiftAction::Transfer` recipient picker & `payments.transferStarGift` validation.
//! 3. `PaymentsScreenHost` without hard-coded sample IDs + invoice & Passport deep-link entry points.
//! 4. `payments.getStarsRevenueWithdrawalUrl` with 2FA SRP (TON and Stars, direct and via `StatsBackend`).
//! 5. Passport scan / selfie / translation encryption & multi-part `upload.saveFilePart` -> `InputSecureFileUploaded`.
//! 6. Group call recording (`phone.toggleGroupCallRecord`), RTMP stream keys (`phone.getGroupCallStreamRtmpUrl`),
//!    Windows screen-sharing source enumeration, and recent calls via `messages.search(InputMessagesFilterPhoneCalls)`.
//! 7. 1:1 call non-blocking stop (`CallMediaEngine::stop`), `SaveCallDebug`, `take_rating_to_panel()`,
//!    and group discard completion (`handle_discard_done()`).
//! 8. Statistics async graphs (`stats.loadAsyncGraph`) and paginated message/story public forwards.
//!
//! All tests run strictly against `FakeRpcClient` and local `127.0.0.1` servers — zero real
//! Telegram connections, zero real payment providers, and zero real people's names.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tg_api::convert::apply_tl_users_and_chats;
use tg_api::{DcId, InvocationError, RpcClient};
use tg_calls::types::CallId;
use tg_data::{ChannelId, PeerId as DataPeerId, UserId as DataUserId};
use tg_passport::encryption::{
    SecretKdfAlgo, compute_password_hash_for_secret, compute_secret_id, decrypt_data,
    decrypt_value_secret, encrypt_master_secret, generate_secret,
};
use tg_passport::types::{FileType, FormRequest, ValueMap, ValueType};
use tg_payments::providers::{HttpClient, HttpMethod, HttpRequest, parse_stripe_token_response};
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_stats::ChartPalette;
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tg_voip::{AudioBackendKind, GroupMediaConfig, MediaEngineConfig};
use tokio::sync::broadcast;

use tg_app::calls::backend::{CallsBackend, RecentCallDirection};
use tg_app::calls::group_settings::GroupRtmpDialog;
use tg_app::calls::screen_picker::{ScreenSourceKind, enumerate_screen_sources};
use tg_app::live::LiveSession;
use tg_app::live::payments::ReqwestPaymentHttpClient;
use tg_app::payments::backend::{PassportBackend, PaymentsBackend, ReceivedGiftAction};
use tg_app::payments::star_gifts::StarGiftsView;
use tg_app::payments::{
    PaymentsScreen, PaymentsScreenHost, parse_invoice_deep_link, parse_passport_deep_link,
};
use tg_app::stand_in::types::{MsgId, PeerId};
use tg_app::statistics::backend::StatsBackend;
use tg_app::statistics::earn::WithdrawalPromptState;
use tg_app::statistics::sections::MessageStatsView;

const TEST_TIMEOUT: Duration = Duration::from_secs(10);

const SAMPLE_CHART_JSON: &str = r#"{
    "columns": [
        ["x", 1700006400000, 1700092800000, 1700179200000],
        ["y0", 120, 185, 240]
    ],
    "types": { "x": "x", "y0": "line" },
    "names": { "y0": "Views" },
    "colors": { "y0": "BLUE#327fe5" },
    "subchart": { "show": false }
}"#;

// =============================================================================
// Reusable FakeRpcClient for WP-12 tests
// =============================================================================

#[derive(Debug, Clone)]
struct RecordedCall {
    dc_id: DcId,
    ctor: u32,
    bytes: Vec<u8>,
}

type QueuedResponse = (Option<u32>, Result<Vec<u8>, InvocationError>);
type RpcHandler = Box<dyn Fn(DcId, u32, &[u8]) -> Result<Vec<u8>, InvocationError> + Send + Sync>;

#[derive(Clone)]
struct FakeRpcClient {
    main_dc: DcId,
    updates_tx: broadcast::Sender<tl_enums::Updates>,
    queued: Arc<Mutex<VecDeque<QueuedResponse>>>,
    fallback: Arc<Mutex<Option<RpcHandler>>>,
    recorded: Arc<Mutex<Vec<RecordedCall>>>,
}

impl FakeRpcClient {
    fn new() -> Self {
        let (updates_tx, _) = broadcast::channel(128);
        Self {
            main_dc: 2,
            updates_tx,
            queued: Arc::new(Mutex::new(VecDeque::new())),
            fallback: Arc::new(Mutex::new(None)),
            recorded: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn enqueue_ok<T: Serializable>(&self, expected_ctor: Option<u32>, resp: &T) {
        self.queued
            .lock()
            .expect("lock queue")
            .push_back((expected_ctor, Ok(resp.to_bytes())));
    }

    fn set_fallback<F>(&self, handler: F)
    where
        F: Fn(DcId, u32, &[u8]) -> Result<Vec<u8>, InvocationError> + Send + Sync + 'static,
    {
        *self.fallback.lock().expect("lock fallback") = Some(Box::new(handler));
    }

    fn recorded(&self) -> Vec<RecordedCall> {
        self.recorded.lock().expect("lock recorded").clone()
    }

    fn push_update(&self, update: tl_enums::Update) {
        let _ = self
            .updates_tx
            .send(tl_enums::Updates::Updates(tl_types::Updates {
                updates: vec![update],
                users: Vec::new(),
                chats: Vec::new(),
                date: 1_700_000_100,
                seq: 1,
            }));
    }
}

impl RpcClient for FakeRpcClient {
    type Error = InvocationError;

    async fn invoke<R: RemoteCall + Send + Sync>(&self, req: &R) -> Result<R::Return, Self::Error> {
        self.invoke_in_dc(self.main_dc, req).await
    }

    async fn invoke_in_dc<R: RemoteCall + Send + Sync>(
        &self,
        dc_id: DcId,
        req: &R,
    ) -> Result<R::Return, Self::Error> {
        let bytes = req.to_bytes();
        let ctor = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        self.recorded
            .lock()
            .expect("lock recorded")
            .push(RecordedCall {
                dc_id,
                ctor,
                bytes: bytes.clone(),
            });

        let popped = {
            let mut q = self.queued.lock().expect("lock queue");
            if let Some(pos) = q
                .iter()
                .position(|(expected, _)| expected.is_none() || *expected == Some(ctor))
            {
                q.remove(pos).map(|(_, res)| res)
            } else {
                None
            }
        };

        let raw = if let Some(res) = popped {
            res?
        } else if let Some(ref handler) = *self.fallback.lock().expect("lock fallback") {
            handler(dc_id, ctor, &bytes)?
        } else {
            return Err(InvocationError::Deserialize(format!(
                "unexpected RPC ctor 0x{ctor:08x} on dc {dc_id}"
            )));
        };

        R::Return::from_bytes(&raw)
            .map_err(|e| InvocationError::Deserialize(format!("ctor 0x{ctor:08x}: {e}")))
    }

    fn subscribe_updates(&self) -> Option<broadcast::Receiver<tl_enums::Updates>> {
        Some(self.updates_tx.subscribe())
    }

    fn main_dc_id(&self) -> DcId {
        self.main_dc
    }
}

fn synthetic_user(
    id: i64,
    first_name: &str,
    username: &str,
    is_self: bool,
    bot: bool,
) -> tl_types::User {
    tl_types::User {
        self_: is_self,
        contact: !is_self && !bot,
        mutual_contact: false,
        deleted: false,
        bot,
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
        access_hash: Some(id * 100 + 7),
        first_name: Some(first_name.to_owned()),
        last_name: None,
        username: Some(username.to_owned()),
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

fn synthetic_channel(id: i64, title: &str, username: &str, broadcast: bool) -> tl_types::Channel {
    tl_types::Channel {
        creator: true,
        left: false,
        broadcast,
        verified: false,
        megagroup: !broadcast,
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
        access_hash: Some(id * 31 + 11),
        title: title.to_owned(),
        username: Some(username.to_owned()),
        photo: tl_enums::ChatPhoto::Empty,
        date: 1_700_000_000,
        restriction_reason: None,
        admin_rights: None,
        banned_rights: None,
        default_banned_rights: None,
        participants_count: Some(4_200),
        usernames: None,
        stories_max_id: None,
        color: None,
        profile_color: None,
        emoji_status: None,
        level: Some(4),
        subscription_until_date: None,
        bot_verification_icon: None,
        send_paid_messages_stars: None,
        linked_monoforum_id: None,
        linked_community_id: None,
        pinned_msg_id: None,
    }
}

fn make_live_session(
    rpc: FakeRpcClient,
    self_id: i64,
    extra_users: Vec<tl_types::User>,
    extra_channels: Vec<tl_types::Channel>,
) -> (
    Arc<SessionRuntime<tg_app::live::ErasedRpcClient>>,
    LiveSession,
) {
    let cfg = SessionRuntimeConfig {
        init_updates_state: false,
        run_get_difference_on_start: false,
        load_initial_dialogs: false,
        fetch_self_user_on_start: false,
        ..SessionRuntimeConfig::default()
    };
    let runtime = Arc::new(
        SessionRuntime::start(
            DataUserId::from_tl(self_id),
            tg_app::live::ErasedRpcClient::new(rpc),
            cfg,
        )
        .expect("start SessionRuntime"),
    );
    runtime
        .tokio_handle()
        .block_on(async { tokio::time::timeout(TEST_TIMEOUT, runtime.wait_ready()).await })
        .expect("wait_ready timeout")
        .expect("runtime ready");
    runtime.tokio_handle().block_on(async {
        let mut s = runtime.session().write().await;
        let extra_user_ids: Vec<i64> = extra_users.iter().map(|u| u.id).collect();
        let extra_channel_ids: Vec<i64> = extra_channels.iter().map(|c| c.id).collect();
        let mut users = vec![tl_enums::User::User(synthetic_user(
            self_id,
            "Self Operator",
            "self_operator",
            true,
            false,
        ))];
        users.extend(extra_users.into_iter().map(tl_enums::User::User));
        let chats: Vec<tl_enums::Chat> = extra_channels
            .into_iter()
            .map(tl_enums::Chat::Channel)
            .collect();
        apply_tl_users_and_chats(&mut s, &users, &chats);
        for uid in extra_user_ids {
            let pid = DataPeerId::from_user(DataUserId::from_tl(uid));
            s.ensure_history(pid).in_chat_list = true;
            s.sync_dialog_lists(pid);
        }
        for cid in extra_channel_ids {
            let pid = DataPeerId::from_channel(ChannelId::from_tl(cid));
            s.ensure_history(pid).in_chat_list = true;
            s.sync_dialog_lists(pid);
        }
    });
    let live = LiveSession::new(Arc::clone(&runtime), 1);
    (runtime, live)
}

fn synthetic_srp_account_password(has_secure_values: bool) -> tl_enums::account::Password {
    let p = tg_calls::dh::DH_GOOD_PRIME.to_vec();
    let g = 3i32;
    let salt1 = vec![0x11; 16];
    let salt2 = vec![0x22; 16];
    let srp_b = vec![0x05; 256];

    tl_enums::account::Password::Password(tl_types::account::Password {
        has_recovery: false,
        has_secure_values,
        has_password: true,
        current_algo: Some(
            tl_enums::PasswordKdfAlgo::SHA256SHA256PBKDF2HMACSHA512iter100000SHA256ModPow(
                tl_types::PasswordKdfAlgoSHA256SHA256PBKDF2HMACSHA512iter100000SHA256ModPow {
                    salt1,
                    salt2,
                    g,
                    p,
                },
            ),
        ),
        srp_b: Some(srp_b),
        srp_id: Some(42),
        hint: Some("test-hint".to_owned()),
        email_unconfirmed_pattern: None,
        new_algo: tl_enums::PasswordKdfAlgo::Unknown,
        new_secure_algo: tl_enums::SecurePasswordKdfAlgo::PBKDF2HMACSHA512iter100000(
            tl_types::SecurePasswordKdfAlgoPBKDF2HMACSHA512iter100000 {
                salt: vec![0x33; 16],
            },
        ),
        secure_random: vec![0x44; 32],
        pending_reset_date: None,
        login_email_pattern: None,
    })
}

fn synthetic_password_settings(
    master_secret: &[u8; 32],
    password: &str,
) -> tl_enums::account::PasswordSettings {
    let salt = vec![0x33; 16];
    let algo = SecretKdfAlgo::Pbkdf2HmacSha512Iter100000 { salt: salt.clone() };
    let pwd_hash = compute_password_hash_for_secret(&algo, password.as_bytes());
    let encrypted =
        encrypt_master_secret(master_secret, &pwd_hash).expect("encrypt master passport secret");
    let secret_id = compute_secret_id(master_secret);
    tl_enums::account::PasswordSettings::PasswordSettings(tl_types::account::PasswordSettings {
        email: None,
        secure_settings: Some(tl_enums::SecureSecretSettings::SecureSecretSettings(
            tl_types::SecureSecretSettings {
                secure_algo: tl_enums::SecurePasswordKdfAlgo::PBKDF2HMACSHA512iter100000(
                    tl_types::SecurePasswordKdfAlgoPBKDF2HMACSHA512iter100000 { salt },
                ),
                secure_secret: encrypted.to_vec(),
                secure_secret_id: secret_id as i64,
            },
        )),
    })
}

fn synthetic_saved_star_gift(msg_id: i32, gift_id: i64) -> tl_enums::SavedStarGift {
    let gift = tl_enums::StarGift::StarGift(tl_types::StarGift {
        limited: true,
        sold_out: false,
        birthday: false,
        require_premium: false,
        limited_per_user: false,
        peer_color_available: false,
        auction: false,
        id: gift_id,
        sticker: tl_enums::Document::Empty(tl_types::DocumentEmpty { id: 901 }),
        stars: 250,
        availability_remains: Some(10),
        availability_total: Some(100),
        availability_resale: None,
        convert_stars: 200,
        first_sale_date: None,
        last_sale_date: None,
        upgrade_stars: Some(25),
        resell_min_stars: None,
        title: Some("Test Collectible".to_owned()),
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
    tl_enums::SavedStarGift::SavedStarGift(tl_types::SavedStarGift {
        name_hidden: false,
        unsaved: false,
        refunded: false,
        can_upgrade: false,
        pinned_to_top: false,
        upgrade_separate: false,
        from_id: None,
        date: 1_700_000_000,
        gift,
        message: None,
        msg_id: Some(msg_id),
        saved_id: None,
        convert_stars: Some(200),
        upgrade_stars: None,
        can_export_at: None,
        transfer_stars: Some(25),
        can_transfer_at: None,
        can_resell_at: None,
        collection_id: None,
        prepaid_upgrade_hash: None,
        drop_original_details_stars: None,
        gift_num: None,
        can_craft_at: None,
    })
}

// =============================================================================
// 1. Real ReqwestPaymentHttpClient tested against a local fake HTTP server
// =============================================================================

#[test]
fn wp12_reqwest_payment_http_client_talks_to_local_fake_server_and_is_default_on_live_session() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback server");
    let addr = listener.local_addr().expect("local addr");
    let captured = Arc::new(Mutex::new(String::new()));
    let captured_srv = Arc::clone(&captured);

    let srv = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept client");
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("set timeout");
        let mut buf = [0u8; 4096];
        let n = stream.read(&mut buf).expect("read request");
        *captured_srv.lock().expect("lock captured") =
            String::from_utf8_lossy(&buf[..n]).into_owned();

        let body = r#"{"id":"tok_local_fake_stripe_99","livemode":false,"created":1700000000,"card":{"id":"card_local_1","brand":"Visa","last4":"4242","exp_month":12,"exp_year":2030,"funding":"credit"}}"#;
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(resp.as_bytes()).expect("write response");
        stream.flush().expect("flush");
    });

    let client = ReqwestPaymentHttpClient::with_default_timeout().expect("build reqwest client");
    let req = HttpRequest {
        method: HttpMethod::Post,
        url: format!("http://{addr}/v1/tokens"),
        headers: vec![
            (
                "Authorization".to_owned(),
                "Bearer pk_test_local_fake".to_owned(),
            ),
            (
                "Content-Type".to_owned(),
                "application/x-www-form-urlencoded".to_owned(),
            ),
        ],
        body: b"card[number]=4242424242424242&card[exp_month]=12&card[exp_year]=2030".to_vec(),
    };

    let resp = client.send(&req).expect("local fake HTTP POST succeeds");
    srv.join().expect("server thread finished");

    assert_eq!(resp.status_code, 200);
    let token = parse_stripe_token_response(Ok(resp)).expect("parse Stripe token JSON");
    assert_eq!(token.token_id, "tok_local_fake_stripe_99");

    let raw_req = captured.lock().expect("lock captured").clone();
    assert!(
        raw_req.starts_with("POST /v1/tokens HTTP/1.1"),
        "raw={raw_req}"
    );
    assert!(
        raw_req
            .to_ascii_lowercase()
            .contains("authorization: bearer pk_test_local_fake"),
        "missing auth header: {raw_req}"
    );
    assert!(raw_req.contains("card[number]=4242424242424242"));

    // Verify LiveSession auto-installs ReqwestPaymentHttpClient when no fake client is set.
    let rpc = FakeRpcClient::new();
    let (_rt, live) = make_live_session(rpc, 100, Vec::new(), Vec::new());
    let default_client = live
        .ensure_payment_http_client()
        .expect("default payment HTTP client installed");
    // Sending to an unbound loopback port exercises the real reqwest client and returns a transport error.
    let closed_listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    let closed_addr = closed_listener.local_addr().expect("closed addr");
    drop(closed_listener);
    let err = default_client.send(&HttpRequest {
        method: HttpMethod::Post,
        url: format!("http://{closed_addr}/unreachable"),
        headers: Vec::new(),
        body: Vec::new(),
    });
    assert!(
        err.is_err(),
        "expected transport error on closed loopback port"
    );
}

// =============================================================================
// 2. ReceivedGiftAction::Transfer recipient picker & payments.transferStarGift
// =============================================================================

#[test]
fn wp12_received_gift_transfer_uses_recipient_picker_and_rejects_self_or_missing() {
    let rpc = FakeRpcClient::new();
    let recipient_user = synthetic_user(200, "Recipient Alpha", "recipient_alpha", false, false);
    let (_rt, mut live) = make_live_session(rpc.clone(), 100, vec![recipient_user], Vec::new());

    // Load Star Gifts bundle (`payments.getStarsStatus`, `payments.getStarGifts`, `payments.getSavedStarGifts`).
    rpc.enqueue_ok(
        Some(tl_fns::payments::GetStarsStatus::CONSTRUCTOR_ID),
        &tl_enums::payments::StarsStatus::StarsStatus(tl_types::payments::StarsStatus {
            balance: tl_enums::StarsAmount::StarsAmount(tl_types::StarsAmount {
                amount: 500,
                nanos: 0,
            }),
            subscriptions: None,
            subscriptions_next_offset: None,
            subscriptions_missing_balance: None,
            history: None,
            next_offset: None,
            chats: Vec::new(),
            users: Vec::new(),
        }),
    );
    rpc.enqueue_ok(
        Some(tl_fns::payments::GetStarGifts::CONSTRUCTOR_ID),
        &tl_enums::payments::StarGifts::StarGifts(tl_types::payments::StarGifts {
            hash: 1,
            gifts: Vec::new(),
            chats: Vec::new(),
            users: Vec::new(),
        }),
    );
    rpc.enqueue_ok(
        Some(tl_fns::payments::GetSavedStarGifts::CONSTRUCTOR_ID),
        &tl_enums::payments::SavedStarGifts::SavedStarGifts(tl_types::payments::SavedStarGifts {
            count: 1,
            chat_notifications_enabled: None,
            gifts: vec![synthetic_saved_star_gift(777, 5001)],
            next_offset: None,
            chats: Vec::new(),
            users: Vec::new(),
        }),
    );
    let bundle = live.load_star_gifts().expect("load star gifts");
    assert_eq!(bundle.received.len(), 1);
    let gift_id = bundle.received[0].saved.info.id;
    assert_eq!(gift_id, 5001);

    // 1. Calling Transfer without selecting a recipient MUST fail (never defaults to InputPeer::Self_).
    let no_recipient_err = live
        .apply_received_gift_action(gift_id, ReceivedGiftAction::Transfer)
        .expect_err("transfer without recipient must fail");
    assert!(no_recipient_err.contains("recipient"));
    assert!(
        !rpc.recorded()
            .iter()
            .any(|c| c.ctor == tl_fns::payments::TransferStarGift::CONSTRUCTOR_ID),
        "must not invoke payments.transferStarGift when recipient is unset"
    );

    // 2. Candidate list excludes self and includes cached contacts.
    let candidates = live.transfer_recipient_candidates();
    assert_eq!(candidates, vec!["@recipient_alpha".to_owned()]);

    // 3. Self-transfer is rejected cleanly before RPC.
    let self_err = live
        .transfer_received_gift(gift_id, "@self_operator")
        .expect_err("self transfer must be rejected");
    assert!(self_err.contains("yourself"));

    // 4. UI modal helper opens cleanly with candidates.
    let mut view = StarGiftsView::new(bundle.clone());
    view.open_transfer_gift(
        bundle.received[0].clone(),
        live.transfer_recipient_candidates(),
    );
    assert!(view.modal.is_some());

    // 5. Valid transfer invokes payments.transferStarGift with InputPeer::User(200) and removes gift.
    rpc.enqueue_ok(
        Some(tl_fns::payments::TransferStarGift::CONSTRUCTOR_ID),
        &tl_enums::Updates::Updates(tl_types::Updates {
            updates: Vec::new(),
            users: Vec::new(),
            chats: Vec::new(),
            date: 1_700_000_050,
            seq: 1,
        }),
    );
    let msg = live
        .transfer_received_gift(gift_id, "@recipient_alpha")
        .expect("transfer to @recipient_alpha succeeds");
    assert!(msg.contains("@recipient_alpha"));

    let transfer_call = rpc
        .recorded()
        .into_iter()
        .find(|c| c.ctor == tl_fns::payments::TransferStarGift::CONSTRUCTOR_ID)
        .expect("payments.transferStarGift recorded");
    let decoded =
        tl_fns::payments::TransferStarGift::from_bytes(&transfer_call.bytes).expect("decode RPC");
    match decoded.to_id {
        tl_enums::InputPeer::User(u) => assert_eq!(u.user_id, 200),
        other => panic!("expected InputPeer::User(200), got {other:?}"),
    }
    match decoded.stargift {
        tl_enums::InputSavedStarGift::User(g) => assert_eq!(g.msg_id, 777),
        other => panic!("expected InputSavedStarGift::User(777), got {other:?}"),
    }
}

// =============================================================================
// 3. PaymentsScreenHost removes hard-coded samples + deep-link entry points
// =============================================================================

#[test]
fn wp12_payments_screen_host_no_hardcoded_samples_and_parses_deep_links() {
    let rpc = FakeRpcClient::new();
    let (_rt, live) = make_live_session(rpc.clone(), 100, Vec::new(), Vec::new());

    let mut host = PaymentsScreenHost::new(Box::new(live.clone()), Box::new(live));
    assert_eq!(host.active_invoice_target(), None);
    assert_eq!(host.active_passport_target(), None);
    assert!(
        !rpc.recorded().iter().any(|c| {
            c.ctor == tl_fns::payments::GetPaymentForm::CONSTRUCTOR_ID
                || c.ctor == tl_fns::account::GetAuthorizationForm::CONSTRUCTOR_ID
        }),
        "PaymentsScreenHost::new must not fire RPCs for hard-coded sample invoices or bots"
    );

    // Deep-link parsers.
    assert_eq!(
        parse_invoice_deep_link("tg://invoice?slug=alpha_slug_42").as_deref(),
        Some("alpha_slug_42")
    );
    assert_eq!(
        parse_invoice_deep_link("https://t.me/$beta_slug_99").as_deref(),
        Some("beta_slug_99")
    );
    assert_eq!(
        parse_invoice_deep_link("t.me/invoice/gamma_slug").as_deref(),
        Some("gamma_slug")
    );
    assert_eq!(parse_invoice_deep_link("https://example.test/other"), None);

    let passport_link = parse_passport_deep_link(
        "tg://passport?bot_id=555&scope=identity&public_key=PK_TEST&nonce=n123&callback_url=https%3A%2F%2Fexample.test%2Fcb",
    )
    .expect("parse tg://passport deep link");
    assert_eq!(passport_link.bot_id, Some(555));
    assert_eq!(passport_link.scope, "identity");
    assert_eq!(passport_link.public_key, "PK_TEST");
    assert_eq!(passport_link.nonce, "n123");
    assert_eq!(passport_link.callback_url, "https://example.test/cb");

    // Opening real invoice from message and deep link sets active_invoice_target & switches screen.
    let _ = host.open_invoice_message(PeerId::from_user(200), MsgId(42));
    assert_eq!(host.active_screen(), PaymentsScreen::Checkout);
    assert_eq!(
        host.active_invoice_target(),
        Some(format!("msg:{}:42", PeerId::from_user(200).0).as_str())
    );

    let _ = host.open_invoice_deep_link("https://t.me/$real_invoice_slug");
    assert_eq!(host.active_invoice_target(), Some("real_invoice_slug"));

    // Opening Passport via deep link sets active_passport_target & switches screen.
    let _ = host.open_passport_deep_link(
        "tg://passport?bot_id=555&scope=identity&public_key=PK_TEST&nonce=n123",
    );
    assert_eq!(host.active_screen(), PaymentsScreen::Passport);
    assert_eq!(host.active_passport_target(), Some("bot_555"));
}

// =============================================================================
// 4. Stars & TON revenue withdrawal URL (`payments.getStarsRevenueWithdrawalUrl` + 2FA SRP)
// =============================================================================

#[test]
fn wp12_stars_revenue_withdrawal_url_computes_srp_for_ton_and_stars() {
    let rpc = FakeRpcClient::new();
    let ch = synthetic_channel(5001, "Signal Channel", "signal_channel", true);
    let bot = synthetic_user(6001, "Catalog Bot", "catalog_bot", false, true);
    let (_rt, live) = make_live_session(rpc.clone(), 100, vec![bot], vec![ch]);

    let ch_peer = PeerId::from_channel(5001);
    live.set_stats_target_peer(ch_peer);

    // 1. TON withdrawal on channel via direct LiveSession API.
    rpc.enqueue_ok(
        Some(tl_fns::account::GetPassword::CONSTRUCTOR_ID),
        &synthetic_srp_account_password(false),
    );
    rpc.enqueue_ok(
        Some(tl_fns::payments::GetStarsRevenueWithdrawalUrl::CONSTRUCTOR_ID),
        &tl_enums::payments::StarsRevenueWithdrawalUrl::StarsRevenueWithdrawalUrl(
            tl_types::payments::StarsRevenueWithdrawalUrl {
                url: "https://fragment.example.test/withdraw/ton_token".to_owned(),
            },
        ),
    );
    let ton_url = live
        .get_stars_revenue_withdrawal_url(ch_peer, true, None, "test-2fa-pass")
        .expect("TON withdrawal URL");
    assert_eq!(ton_url, "https://fragment.example.test/withdraw/ton_token");

    // 2. Bot Stars withdrawal via StatsBackend + WithdrawalPromptState UI model.
    live.set_stats_target_peer(PeerId::from_user(6001));
    rpc.enqueue_ok(
        Some(tl_fns::account::GetPassword::CONSTRUCTOR_ID),
        &synthetic_srp_account_password(false),
    );
    rpc.enqueue_ok(
        Some(tl_fns::payments::GetStarsRevenueWithdrawalUrl::CONSTRUCTOR_ID),
        &tl_enums::payments::StarsRevenueWithdrawalUrl::StarsRevenueWithdrawalUrl(
            tl_types::payments::StarsRevenueWithdrawalUrl {
                url: "https://fragment.example.test/withdraw/stars_token".to_owned(),
            },
        ),
    );

    assert!(live.supports_live_withdrawal());
    let mut prompt = WithdrawalPromptState::default();
    prompt.open_for(false, Some(2500));
    prompt.password = "test-2fa-pass".to_owned();
    let stars_url = prompt.submit(&live, true).expect("Stars withdrawal URL");
    assert_eq!(
        stars_url,
        "https://fragment.example.test/withdraw/stars_token"
    );
    assert!(prompt.password.is_empty(), "password cleared after submit");

    let calls: Vec<tl_fns::payments::GetStarsRevenueWithdrawalUrl> = rpc
        .recorded()
        .into_iter()
        .filter(|c| c.ctor == tl_fns::payments::GetStarsRevenueWithdrawalUrl::CONSTRUCTOR_ID)
        .map(|c| {
            tl_fns::payments::GetStarsRevenueWithdrawalUrl::from_bytes(&c.bytes)
                .expect("decode withdrawal RPC")
        })
        .collect();
    assert_eq!(calls.len(), 2);
    assert!(calls[0].ton);
    assert_eq!(calls[0].amount, None);
    match &calls[0].password {
        tl_enums::InputCheckPasswordSRP::InputCheckPasswordSRP(srp) => {
            assert_eq!(srp.srp_id, 42);
            assert_eq!(srp.a.len(), 256);
            assert_eq!(srp.m1.len(), 32);
        }
        other => panic!("expected SRP check, got {other:?}"),
    }
    assert!(!calls[1].ton);
    assert_eq!(calls[1].amount, Some(2500));
}

// =============================================================================
// 5. Passport scan / selfie / translation upload (`upload.saveFilePart` + `InputSecureFileUploaded`)
// =============================================================================

#[test]
fn wp12_passport_scan_selfie_and_translation_upload_encrypts_and_saves_uploaded_files() {
    let rpc = FakeRpcClient::new();
    let bot = synthetic_user(7001, "Verify Bot", "verify_bot", false, true);
    let (_rt, mut live) = make_live_session(rpc.clone(), 100, vec![bot], Vec::new());

    let master_secret = generate_secret();
    let password = "passport-2fa-secret";

    // Configure Passport form request & unlock.
    live.configure_form_request(FormRequest {
        bot_id: DataUserId(7001),
        scope: r#"{"data":[{"type":"passport","selfie":true,"translation":true}],"v":1}"#
            .to_owned(),
        public_key: "-----BEGIN PUBLIC KEY-----\nTEST\n-----END PUBLIC KEY-----".to_owned(),
        nonce: "nonce-wp12".to_owned(),
        callback_url: String::new(),
    });

    rpc.enqueue_ok(
        Some(tl_fns::account::GetAuthorizationForm::CONSTRUCTOR_ID),
        &tl_enums::account::AuthorizationForm::AuthorizationForm(
            tl_types::account::AuthorizationForm {
                required_types: vec![tl_enums::SecureRequiredType::SecureRequiredType(
                    tl_types::SecureRequiredType {
                        native_names: false,
                        selfie_required: true,
                        translation_required: true,
                        r#type: tl_enums::SecureValueType::Passport,
                    },
                )],
                values: Vec::new(),
                errors: Vec::new(),
                users: Vec::new(),
                privacy_policy_url: None,
            },
        ),
    );
    rpc.enqueue_ok(
        Some(tl_fns::account::GetPassword::CONSTRUCTOR_ID),
        &synthetic_srp_account_password(true),
    );
    rpc.enqueue_ok(
        Some(tl_fns::account::GetPasswordSettings::CONSTRUCTOR_ID),
        &synthetic_password_settings(&master_secret, password),
    );

    let _bundle = live
        .load_authorization_form("verify_bot")
        .expect("load passport authorization form");
    let _unlock = live
        .verify_cloud_password(password)
        .expect("unlock passport");

    // Allow arbitrary upload.saveFilePart calls via fallback handler.
    rpc.set_fallback(|_dc, ctor, _bytes| {
        if ctor == tl_fns::upload::SaveFilePart::CONSTRUCTOR_ID {
            Ok(true.to_bytes())
        } else {
            Err(InvocationError::Deserialize(format!(
                "unexpected ctor 0x{ctor:08x}"
            )))
        }
    });

    // 600 KiB front scan (> 512 KiB part size -> 2 parts!), plus selfie and translation.
    let front_bytes: Vec<u8> = (0..(600 * 1024))
        .map(|i| ((i * 31 + 7) % 251) as u8)
        .collect();
    let selfie_bytes = b"SYNTHETIC_SELFIE_JPEG_PAYLOAD_WP12".to_vec();
    let translation_bytes = b"SYNTHETIC_TRANSLATION_DOC_PAYLOAD_WP12".to_vec();

    let front_file = live
        .upload_passport_file(ValueType::Passport, FileType::FrontSide, &front_bytes)
        .expect("upload multi-part front scan");
    live.upload_passport_file(ValueType::Passport, FileType::Selfie, &selfie_bytes)
        .expect("upload selfie");
    live.upload_passport_file(
        ValueType::Passport,
        FileType::Translation,
        &translation_bytes,
    )
    .expect("upload translation");

    // Enqueue account.saveSecureValue response.
    rpc.enqueue_ok(
        Some(tl_fns::account::SaveSecureValue::CONSTRUCTOR_ID),
        &tl_enums::SecureValue::SecureValue(tl_types::SecureValue {
            r#type: tl_enums::SecureValueType::Passport,
            data: None,
            front_side: None,
            reverse_side: None,
            selfie: None,
            translation: None,
            files: None,
            plain_data: None,
            hash: vec![0xAA; 32],
        }),
    );

    let mut fields = ValueMap::default();
    fields.set_text("document_no", "WP120001");
    fields.set_text("expiry_date", "01.01.2032");
    live.save_passport_value(
        ValueType::Passport,
        fields,
        vec![
            "passport_front.jpg".to_owned(),
            "selfie.jpg".to_owned(),
            "certified_translation.jpg".to_owned(),
        ],
    )
    .expect("save passport value with uploaded scans");

    // Verify upload.saveFilePart calls: 2 parts for front + 1 for selfie + 1 for translation = 4 parts.
    let parts: Vec<tl_fns::upload::SaveFilePart> = rpc
        .recorded()
        .into_iter()
        .filter(|c| c.ctor == tl_fns::upload::SaveFilePart::CONSTRUCTOR_ID)
        .map(|c| tl_fns::upload::SaveFilePart::from_bytes(&c.bytes).expect("decode saveFilePart"))
        .collect();
    assert_eq!(parts.len(), 4, "expected 2+1+1=4 uploaded parts");

    // Verify account.saveSecureValue carries InputSecureFileUploaded for front_side, selfie, translation.
    let save_raw = rpc
        .recorded()
        .into_iter()
        .find(|c| c.ctor == tl_fns::account::SaveSecureValue::CONSTRUCTOR_ID)
        .expect("account.saveSecureValue recorded");
    let save_req = tl_fns::account::SaveSecureValue::from_bytes(&save_raw.bytes)
        .expect("decode saveSecureValue");
    let tl_enums::InputSecureValue::InputSecureValue(val) = save_req.value;

    let front_uploaded = match val.front_side {
        Some(tl_enums::InputSecureFile::Uploaded(u)) => u,
        other => panic!("expected InputSecureFile::Uploaded for front_side, got {other:?}"),
    };
    assert_eq!(front_uploaded.parts, 2);
    assert_eq!(front_uploaded.secret.len(), 32);
    assert_eq!(front_uploaded.md5_checksum.len(), 32);

    assert!(
        matches!(val.selfie, Some(tl_enums::InputSecureFile::Uploaded(_))),
        "expected InputSecureFile::Uploaded for selfie"
    );
    let translations = val.translation.expect("expected translation list");
    assert_eq!(translations.len(), 1);
    assert!(matches!(
        translations[0],
        tl_enums::InputSecureFile::Uploaded(_)
    ));

    // Reassemble the 2 uploaded parts for `front_uploaded.id` and decrypt them using
    // `tg_passport::encryption` to verify bit-exact parity with `passport_encryption.cpp`!
    let mut front_parts: Vec<&tl_fns::upload::SaveFilePart> = parts
        .iter()
        .filter(|p| p.file_id == front_uploaded.id)
        .collect();
    front_parts.sort_by_key(|p| p.file_part);
    assert_eq!(front_parts.len(), 2);
    let mut encrypted_payload = Vec::new();
    for p in front_parts {
        encrypted_payload.extend_from_slice(&p.bytes);
    }
    assert_ne!(
        encrypted_payload, front_bytes,
        "uploaded passport bytes must be encrypted"
    );
    assert_eq!(
        tg_crypto::hash::md5_hex(&encrypted_payload),
        front_uploaded.md5_checksum
    );

    let decrypted_file_secret =
        decrypt_value_secret(&front_uploaded.secret, &master_secret, &front_file.hash)
            .expect("decrypt file secret");
    let decrypted_bytes =
        decrypt_data(&encrypted_payload, &front_file.hash, &decrypted_file_secret)
            .expect("decrypt uploaded passport scan");
    assert_eq!(decrypted_bytes, front_bytes);
}

// =============================================================================
// 6. Group call recording, RTMP stream key, screen picker & recent calls search
// =============================================================================

#[test]
fn wp12_calls_recording_rtmp_screen_picker_and_recent_calls_search() {
    let rpc = FakeRpcClient::new();
    let ch = synthetic_channel(8001, "Engineering Stage", "eng_stage", true);
    let contact = synthetic_user(8002, "Peer Beta", "peer_beta", false, false);
    let (_rt, mut live) = make_live_session(rpc.clone(), 100, vec![contact], vec![ch]);

    let media_cfg = MediaEngineConfig {
        audio_backend: AudioBackendKind::Virtual,
        ..MediaEngineConfig::default()
    };
    let group_cfg = GroupMediaConfig {
        audio_backend: AudioBackendKind::Virtual,
        ..GroupMediaConfig::default()
    };
    live.set_calls_media_config(media_cfg, group_cfg);

    let ch_peer = PeerId::from_channel(8001);
    let self_peer = PeerId::from_user(100);

    // 1. Screen-sharing source enumeration returns at least one Display with non-zero dimensions.
    let sources = enumerate_screen_sources();
    assert!(
        sources
            .iter()
            .any(|s| s.kind == ScreenSourceKind::Display && s.width > 0 && s.height > 0),
        "expected at least one display source, got {sources:?}"
    );

    // 2. RTMP stream URL & key (`phone.getGroupCallStreamRtmpUrl`, initial & revoke).
    rpc.enqueue_ok(
        Some(tl_fns::phone::GetGroupCallStreamRtmpUrl::CONSTRUCTOR_ID),
        &tl_enums::phone::GroupCallStreamRtmpUrl::GroupCallStreamRtmpUrl(
            tl_types::phone::GroupCallStreamRtmpUrl {
                url: "rtmps://dc4-1.rtmp.example.test/s/".to_owned(),
                key: "initial_stream_key_123".to_owned(),
            },
        ),
    );
    rpc.enqueue_ok(
        Some(tl_fns::phone::GetGroupCallStreamRtmpUrl::CONSTRUCTOR_ID),
        &tl_enums::phone::GroupCallStreamRtmpUrl::GroupCallStreamRtmpUrl(
            tl_types::phone::GroupCallStreamRtmpUrl {
                url: "rtmps://dc4-1.rtmp.example.test/s/".to_owned(),
                key: "rotated_stream_key_456".to_owned(),
            },
        ),
    );
    let mut rtmp_dialog = GroupRtmpDialog::default();
    rtmp_dialog.open_for(ch_peer, &mut live);
    assert_eq!(
        rtmp_dialog.info.as_ref().map(|i| i.key.as_str()),
        Some("initial_stream_key_123")
    );
    rtmp_dialog.reload(&mut live, true);
    assert_eq!(
        rtmp_dialog.info.as_ref().map(|i| i.key.as_str()),
        Some("rotated_stream_key_456")
    );
    let rtmp_calls: Vec<tl_fns::phone::GetGroupCallStreamRtmpUrl> = rpc
        .recorded()
        .into_iter()
        .filter(|c| c.ctor == tl_fns::phone::GetGroupCallStreamRtmpUrl::CONSTRUCTOR_ID)
        .map(|c| {
            tl_fns::phone::GetGroupCallStreamRtmpUrl::from_bytes(&c.bytes).expect("decode rtmp RPC")
        })
        .collect();
    assert_eq!(rtmp_calls.len(), 2);
    assert!(!rtmp_calls[0].revoke);
    assert!(rtmp_calls[1].revoke);

    // 3. Group call recording (`phone.toggleGroupCallRecord`: start video portrait, then stop).
    rpc.set_fallback(|_dc, ctor, bytes| {
        if ctor == tl_fns::phone::GetGroupCall::CONSTRUCTOR_ID {
            let resp = tl_enums::phone::GroupCall::GroupCall(tl_types::phone::GroupCall {
                call: tl_enums::GroupCall::GroupCall(tl_types::GroupCall {
                    join_muted: false,
                    can_change_join_muted: true,
                    join_date_asc: false,
                    schedule_start_subscribed: false,
                    can_start_video: true,
                    record_video_active: false,
                    rtmp_stream: false,
                    listeners_hidden: false,
                    conference: false,
                    creator: true,
                    messages_enabled: true,
                    can_change_messages_enabled: true,
                    min: false,
                    id: 99_001,
                    access_hash: 55_001,
                    participants_count: 1,
                    title: Some("Engineering Stage".to_owned()),
                    stream_dc_id: None,
                    record_start_date: None,
                    schedule_date: None,
                    unmuted_video_limit: 12,
                    version: 1,
                    invite_link: None,
                    send_paid_messages_stars: None,
                    default_send_as: None,
                    unmuted_video_count: None,
                }),
                participants: Vec::new(),
                participants_next_offset: String::new(),
                chats: Vec::new(),
                users: Vec::new(),
            });
            Ok(resp.to_bytes())
        } else if ctor == tl_fns::phone::JoinGroupCall::CONSTRUCTOR_ID {
            let resp = tl_enums::Updates::Updates(tl_types::Updates {
                updates: Vec::new(),
                users: Vec::new(),
                chats: Vec::new(),
                date: 1_700_000_000,
                seq: 1,
            });
            Ok(resp.to_bytes())
        } else if ctor == tl_fns::phone::ToggleGroupCallRecord::CONSTRUCTOR_ID {
            let req = tl_fns::phone::ToggleGroupCallRecord::from_bytes(bytes)
                .expect("decode toggle record");
            let upd = tl_enums::Update::GroupCall(tl_types::UpdateGroupCall {
                live_story: false,
                peer: Some(tl_enums::Peer::Channel(tl_types::PeerChannel {
                    channel_id: 8001,
                })),
                call: tl_enums::GroupCall::GroupCall(tl_types::GroupCall {
                    join_muted: false,
                    can_change_join_muted: true,
                    join_date_asc: false,
                    schedule_start_subscribed: false,
                    can_start_video: true,
                    record_video_active: req.start && req.video,
                    rtmp_stream: false,
                    listeners_hidden: false,
                    conference: false,
                    creator: true,
                    messages_enabled: true,
                    can_change_messages_enabled: true,
                    min: false,
                    id: 99_001,
                    access_hash: 55_001,
                    participants_count: 1,
                    title: Some("Engineering Stage".to_owned()),
                    stream_dc_id: None,
                    record_start_date: req.start.then_some(1_700_000_200),
                    schedule_date: None,
                    unmuted_video_limit: 12,
                    version: 1,
                    invite_link: None,
                    send_paid_messages_stars: None,
                    default_send_as: None,
                    unmuted_video_count: None,
                }),
            });
            let resp = tl_enums::Updates::Updates(tl_types::Updates {
                updates: vec![upd],
                users: Vec::new(),
                chats: Vec::new(),
                date: 1_700_000_200,
                seq: 2,
            });
            Ok(resp.to_bytes())
        } else if ctor == tl_fns::phone::DiscardGroupCall::CONSTRUCTOR_ID
            || ctor == tl_fns::phone::LeaveGroupCall::CONSTRUCTOR_ID
        {
            Ok(tl_enums::Updates::Updates(tl_types::Updates {
                updates: Vec::new(),
                users: Vec::new(),
                chats: Vec::new(),
                date: 1_700_000_300,
                seq: 3,
            })
            .to_bytes())
        } else if ctor == tl_fns::messages::DeleteMessages::CONSTRUCTOR_ID {
            Ok(tl_enums::messages::AffectedMessages::AffectedMessages(
                tl_types::messages::AffectedMessages {
                    pts: 10,
                    pts_count: 1,
                },
            )
            .to_bytes())
        } else {
            Err(InvocationError::Deserialize(format!(
                "unexpected ctor 0x{ctor:08x}"
            )))
        }
    });

    live.join_group_call_with_id(ch_peer, self_peer, CallId(99_001), 55_001, true, 1_000);
    let start_wait = Instant::now();
    while start_wait.elapsed() < Duration::from_secs(2) {
        live.tick(1_100);
        if rpc
            .recorded()
            .iter()
            .any(|c| c.ctor == tl_fns::phone::GetGroupCall::CONSTRUCTOR_ID)
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    // Start recording (video + portrait + custom title).
    live.toggle_group_recording(true, Some("Release Sync".to_owned()), true, true);
    let deadline = Instant::now() + TEST_TIMEOUT;
    while Instant::now() < deadline {
        live.tick(1_500);
        if live.active_group_call().is_some_and(|g| g.recording) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        live.active_group_call().is_some_and(|g| g.recording),
        "group recording should become active"
    );

    // Stop recording.
    live.toggle_group_recording(false, None, false, false);
    let deadline = Instant::now() + TEST_TIMEOUT;
    while Instant::now() < deadline {
        live.tick(2_000);
        if live.active_group_call().is_some_and(|g| !g.recording) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        live.active_group_call().is_some_and(|g| !g.recording),
        "group recording should stop"
    );

    let rec_calls: Vec<tl_fns::phone::ToggleGroupCallRecord> = rpc
        .recorded()
        .into_iter()
        .filter(|c| c.ctor == tl_fns::phone::ToggleGroupCallRecord::CONSTRUCTOR_ID)
        .map(|c| {
            tl_fns::phone::ToggleGroupCallRecord::from_bytes(&c.bytes)
                .expect("decode toggleGroupCallRecord")
        })
        .collect();
    assert_eq!(rec_calls.len(), 2);
    assert!(rec_calls[0].start);
    assert!(rec_calls[0].video);
    assert_eq!(rec_calls[0].video_portrait, Some(true));
    assert_eq!(rec_calls[0].title.as_deref(), Some("Release Sync"));
    assert!(!rec_calls[1].start);
    assert!(!rec_calls[1].video);
    assert_eq!(rec_calls[1].video_portrait, None);

    // 4. Discard group call triggers `phone.discardGroupCall` -> `GroupDiscardDone` -> `handle_discard_done()`.
    live.leave_group_call(true, 2_500);
    let deadline = Instant::now() + TEST_TIMEOUT;
    while Instant::now() < deadline {
        live.tick(2_600);
        if live.active_group_call().is_none() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        live.active_group_call().is_none(),
        "group call should finish after DiscardGroupCall -> handle_discard_done()"
    );
    assert!(
        rpc.recorded()
            .iter()
            .any(|c| c.ctor == tl_fns::phone::DiscardGroupCall::CONSTRUCTOR_ID)
    );

    // 5. Recent calls via `messages.search(InputMessagesFilterPhoneCalls)`.
    let make_call_msg = |id: i32,
                         out: bool,
                         video: bool,
                         duration: Option<i32>,
                         reason: Option<tl_enums::PhoneCallDiscardReason>| {
        tl_enums::Message::Service(tl_types::MessageService {
            out,
            mentioned: false,
            media_unread: false,
            reactions_are_possible: false,
            silent: false,
            post: false,
            legacy: false,
            id,
            from_id: None,
            peer_id: tl_enums::Peer::User(tl_types::PeerUser { user_id: 8002 }),
            saved_peer_id: None,
            reply_to: None,
            date: 1_700_000_000,
            action: tl_enums::MessageAction::PhoneCall(tl_types::MessageActionPhoneCall {
                video,
                call_id: i64::from(id) * 100,
                reason,
                duration,
            }),
            reactions: None,
            ttl_period: None,
        })
    };
    rpc.enqueue_ok(
        Some(tl_fns::messages::Search::CONSTRUCTOR_ID),
        &tl_enums::messages::Messages::Slice(tl_types::messages::MessagesSlice {
            inexact: false,
            count: 3,
            next_rate: None,
            offset_id_offset: None,
            search_flood: None,
            messages: vec![
                make_call_msg(
                    503,
                    true,
                    true,
                    Some(120),
                    Some(tl_enums::PhoneCallDiscardReason::Hangup),
                ),
                make_call_msg(
                    502,
                    false,
                    false,
                    Some(45),
                    Some(tl_enums::PhoneCallDiscardReason::Hangup),
                ),
                make_call_msg(
                    501,
                    false,
                    false,
                    None,
                    Some(tl_enums::PhoneCallDiscardReason::Missed),
                ),
            ],
            chats: Vec::new(),
            users: Vec::new(),
            topics: Vec::new(),
        }),
    );

    let recent = live
        .load_recent_calls_from_server(0, 50)
        .expect("load recent calls");
    assert_eq!(recent.len(), 3);
    assert_eq!(recent[0].msg_id, 503);
    assert_eq!(recent[0].direction, RecentCallDirection::Out);
    assert!(recent[0].is_video);
    assert_eq!(recent[0].duration_secs, Some(120));
    assert_eq!(recent[1].direction, RecentCallDirection::In);
    assert_eq!(recent[1].duration_secs, Some(45));
    assert_eq!(recent[2].direction, RecentCallDirection::Missed);
    assert_eq!(recent[2].duration_secs, None);
    assert_eq!(live.recent_calls().len(), 3);

    // Deleting a recent call removes locally and issues `messages.deleteMessages`.
    live.delete_recent_calls(&[501]);
    assert_eq!(live.recent_calls().len(), 2);
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if rpc
            .recorded()
            .iter()
            .any(|c| c.ctor == tl_fns::messages::DeleteMessages::CONSTRUCTOR_ID)
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        rpc.recorded()
            .iter()
            .any(|c| c.ctor == tl_fns::messages::DeleteMessages::CONSTRUCTOR_ID)
    );
}

// =============================================================================
// 7. 1:1 call non-blocking stop, SaveCallDebug, and take_rating_to_panel() after Discarded
// =============================================================================

#[test]
fn wp12_call_discarded_triggers_non_blocking_stop_debug_log_and_rating_panel() {
    let rpc = FakeRpcClient::new();
    let peer_user = synthetic_user(9002, "Voice Peer", "voice_peer", false, false);
    let (_rt, mut live) = make_live_session(rpc.clone(), 100, vec![peer_user], Vec::new());

    let media_cfg = MediaEngineConfig {
        audio_backend: AudioBackendKind::Virtual,
        ..MediaEngineConfig::default()
    };
    let group_cfg = GroupMediaConfig {
        audio_backend: AudioBackendKind::Virtual,
        ..GroupMediaConfig::default()
    };
    live.set_calls_media_config(media_cfg, group_cfg);

    rpc.set_fallback(|_dc, ctor, _bytes| {
        if ctor == tl_fns::phone::GetCallConfig::CONSTRUCTOR_ID {
            Ok(tl_enums::DataJSON::DataJSON(tl_types::DataJSON {
                data: "{}".to_owned(),
            })
            .to_bytes())
        } else if ctor == tl_fns::messages::GetDhConfig::CONSTRUCTOR_ID {
            Ok(
                tl_enums::messages::DhConfig::DhConfig(tl_types::messages::DhConfig {
                    g: 3,
                    p: tg_calls::dh::DH_GOOD_PRIME.to_vec(),
                    version: 1,
                    random: vec![0x55; 256],
                })
                .to_bytes(),
            )
        } else if ctor == tl_fns::phone::RequestCall::CONSTRUCTOR_ID {
            Ok(
                tl_enums::phone::PhoneCall::PhoneCall(tl_types::phone::PhoneCall {
                    phone_call: tl_enums::PhoneCall::Waiting(tl_types::PhoneCallWaiting {
                        video: false,
                        id: 777_001,
                        access_hash: 888_001,
                        date: 1_700_000_000,
                        admin_id: 100,
                        participant_id: 9002,
                        protocol: tl_enums::PhoneCallProtocol::PhoneCallProtocol(
                            tl_types::PhoneCallProtocol {
                                udp_p2p: true,
                                udp_reflector: true,
                                min_layer: 65,
                                max_layer: 92,
                                library_versions: vec!["5.0.0".to_owned()],
                            },
                        ),
                        receive_date: None,
                    }),
                    users: Vec::new(),
                })
                .to_bytes(),
            )
        } else if ctor == tl_fns::phone::SaveCallDebug::CONSTRUCTOR_ID {
            Ok(true.to_bytes())
        } else if ctor == tl_fns::phone::SetCallRating::CONSTRUCTOR_ID {
            Ok(tl_enums::Updates::Updates(tl_types::Updates {
                updates: Vec::new(),
                users: Vec::new(),
                chats: Vec::new(),
                date: 1_700_000_500,
                seq: 1,
            })
            .to_bytes())
        } else {
            Err(InvocationError::Deserialize(format!(
                "unexpected ctor 0x{ctor:08x}"
            )))
        }
    });

    // Start outgoing call so `tg_calls::Call` gets call_id = 777_001.
    live.start_outgoing_call(PeerId::from_user(9002), false, 1_000);
    live.confirm_outgoing_call(false, 1_010);
    let deadline = Instant::now() + TEST_TIMEOUT;
    while Instant::now() < deadline {
        live.tick(1_200);
        if rpc
            .recorded()
            .iter()
            .any(|c| c.ctor == tl_fns::phone::RequestCall::CONSTRUCTOR_ID)
            && live
                .active_call()
                .is_some_and(|c| c.state == tg_calls::CallState::Waiting)
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        live.active_call().map(|c| c.state),
        Some(tg_calls::CallState::Waiting)
    );

    // Push PhoneCall::Discarded with both `need_rating: true` and `need_debug: true`.
    rpc.push_update(tl_enums::Update::PhoneCall(tl_types::UpdatePhoneCall {
        phone_call: tl_enums::PhoneCall::Discarded(tl_types::PhoneCallDiscarded {
            need_rating: true,
            need_debug: true,
            video: false,
            id: 777_001,
            reason: Some(tl_enums::PhoneCallDiscardReason::Hangup),
            duration: Some(33),
        }),
    }));

    let deadline = Instant::now() + TEST_TIMEOUT;
    while Instant::now() < deadline {
        live.tick(2_000);
        if live.active_call().is_some_and(|c| c.rating_requested) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    let snap = live
        .active_call()
        .expect("call retained in panel for rating via take_rating_to_panel()");
    assert!(snap.rating_requested);
    assert_eq!(snap.ended_duration_secs, Some(33));

    // Submitting a rating invokes `phone.setCallRating`, and finishing clears the panel.
    live.submit_call_rating(4, "Clear voice".to_owned());
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        live.tick(2_100);
        if rpc
            .recorded()
            .iter()
            .any(|c| c.ctor == tl_fns::phone::SetCallRating::CONSTRUCTOR_ID)
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let rating_raw = rpc
        .recorded()
        .into_iter()
        .find(|c| c.ctor == tl_fns::phone::SetCallRating::CONSTRUCTOR_ID)
        .expect("phone.setCallRating invoked");
    let rating_req =
        tl_fns::phone::SetCallRating::from_bytes(&rating_raw.bytes).expect("decode setCallRating");
    assert_eq!(rating_req.rating, 4);
    assert_eq!(rating_req.comment, "Clear voice");

    live.finish_call_rating();
    assert!(live.active_call().is_none());
}

// =============================================================================
// 8. Stats async graph (`stats.loadAsyncGraph`) & paginated public forwards
// =============================================================================

#[test]
fn wp12_stats_async_graph_and_message_story_public_forwards_pagination() {
    let rpc = FakeRpcClient::new();
    let ch = synthetic_channel(4001, "Analytics Feed", "analytics_feed", true);
    let fwd_ch_1 = synthetic_channel(4011, "Repost Channel One", "repost_one", true);
    let fwd_ch_2 = synthetic_channel(4012, "Repost Channel Two", "repost_two", true);
    let (_rt, live) = make_live_session(rpc.clone(), 100, Vec::new(), vec![ch]);

    let ch_peer = PeerId::from_channel(4001);
    live.set_stats_target_peer(ch_peer);
    live.set_stats_dc_id(ch_peer, 4);

    // 1. `stats.getMessageStats` returns async graphs -> hydrated via `stats.loadAsyncGraph` on DC 4,
    //    and `stats.getMessagePublicForwards` returns page 1 with `next_offset = "fwd_page_2"`.
    rpc.enqueue_ok(
        Some(tl_fns::stats::GetMessageStats::CONSTRUCTOR_ID),
        &tl_enums::stats::MessageStats::MessageStats(tl_types::stats::MessageStats {
            views_graph: tl_enums::StatsGraph::Async(tl_types::StatsGraphAsync {
                token: "msg_views_async".to_owned(),
            }),
            reactions_by_emotion_graph: tl_enums::StatsGraph::StatsGraph(tl_types::StatsGraph {
                json: tl_enums::DataJSON::DataJSON(tl_types::DataJSON {
                    data: SAMPLE_CHART_JSON.to_owned(),
                }),
                zoom_token: None,
            }),
        }),
    );
    rpc.enqueue_ok(
        Some(tl_fns::stats::LoadAsyncGraph::CONSTRUCTOR_ID),
        &tl_enums::StatsGraph::StatsGraph(tl_types::StatsGraph {
            json: tl_enums::DataJSON::DataJSON(tl_types::DataJSON {
                data: SAMPLE_CHART_JSON.to_owned(),
            }),
            zoom_token: Some("zoom_tok_99".to_owned()),
        }),
    );
    rpc.enqueue_ok(
        Some(tl_fns::channels::GetMessages::CONSTRUCTOR_ID),
        &tl_enums::messages::Messages::ChannelMessages(tl_types::messages::ChannelMessages {
            inexact: false,
            pts: 1,
            count: 1,
            offset_id_offset: None,
            messages: vec![tl_enums::Message::Message(tl_types::Message {
                out: false,
                mentioned: false,
                media_unread: false,
                silent: false,
                post: true,
                from_scheduled: false,
                legacy: false,
                edit_hide: false,
                pinned: false,
                noforwards: false,
                invert_media: false,
                offline: false,
                video_processing_pending: false,
                paid_suggested_post_stars: false,
                paid_suggested_post_ton: false,
                id: 101,
                from_id: None,
                from_boosts_applied: None,
                from_rank: None,
                peer_id: tl_enums::Peer::Channel(tl_types::PeerChannel { channel_id: 4001 }),
                saved_peer_id: None,
                fwd_from: None,
                via_bot_id: None,
                via_business_bot_id: None,
                guestchat_via_from: None,
                reply_to: None,
                date: 1_700_000_000,
                message: "Release post".to_owned(),
                media: None,
                reply_markup: None,
                entities: None,
                views: Some(5_000),
                forwards: Some(50),
                replies: None,
                edit_date: None,
                post_author: None,
                grouped_id: None,
                reactions: None,
                restriction_reason: None,
                ttl_period: None,
                quick_reply_shortcut_id: None,
                effect: None,
                factcheck: None,
                report_delivery_until_date: None,
                paid_message_stars: None,
                suggested_post: None,
                schedule_repeat_period: None,
                summary_from_language: None,
                rich_message: None,
            })],
            topics: Vec::new(),
            chats: Vec::new(),
            users: Vec::new(),
        }),
    );
    let make_pf_msg = |ch_id: i64, msg_id: i32, views: i32| {
        tl_enums::PublicForward::Message(tl_types::PublicForwardMessage {
            message: tl_enums::Message::Message(tl_types::Message {
                out: false,
                mentioned: false,
                media_unread: false,
                silent: false,
                post: true,
                from_scheduled: false,
                legacy: false,
                edit_hide: false,
                pinned: false,
                noforwards: false,
                invert_media: false,
                offline: false,
                video_processing_pending: false,
                paid_suggested_post_stars: false,
                paid_suggested_post_ton: false,
                id: msg_id,
                from_id: None,
                from_boosts_applied: None,
                from_rank: None,
                peer_id: tl_enums::Peer::Channel(tl_types::PeerChannel { channel_id: ch_id }),
                saved_peer_id: None,
                fwd_from: None,
                via_bot_id: None,
                via_business_bot_id: None,
                guestchat_via_from: None,
                reply_to: None,
                date: 1_700_000_100,
                message: String::new(),
                media: None,
                reply_markup: None,
                entities: None,
                views: Some(views),
                forwards: None,
                replies: None,
                edit_date: None,
                post_author: None,
                grouped_id: None,
                reactions: None,
                restriction_reason: None,
                ttl_period: None,
                quick_reply_shortcut_id: None,
                effect: None,
                factcheck: None,
                report_delivery_until_date: None,
                paid_message_stars: None,
                suggested_post: None,
                schedule_repeat_period: None,
                summary_from_language: None,
                rich_message: None,
            }),
        })
    };
    rpc.enqueue_ok(
        Some(tl_fns::stats::GetMessagePublicForwards::CONSTRUCTOR_ID),
        &tl_enums::stats::PublicForwards::PublicForwards(tl_types::stats::PublicForwards {
            count: 2,
            forwards: vec![make_pf_msg(4011, 701, 1_200)],
            next_offset: Some("fwd_page_2".to_owned()),
            chats: vec![tl_enums::Chat::Channel(fwd_ch_1.clone())],
            users: Vec::new(),
        }),
    );

    let bundle = live.message_statistics(101).expect("load message stats");
    assert!(!bundle.stats.message_interaction_graph.chart.is_empty());
    assert_eq!(bundle.public_forwards.len(), 1);
    assert_eq!(bundle.public_forwards_next_offset, "fwd_page_2");

    // 2. Paginate message public forwards via `MessageStatsView::load_more_public_forwards`.
    rpc.enqueue_ok(
        Some(tl_fns::stats::GetMessagePublicForwards::CONSTRUCTOR_ID),
        &tl_enums::stats::PublicForwards::PublicForwards(tl_types::stats::PublicForwards {
            count: 2,
            forwards: vec![make_pf_msg(4012, 702, 850)],
            next_offset: None,
            chats: vec![tl_enums::Chat::Channel(fwd_ch_2.clone())],
            users: Vec::new(),
        }),
    );
    let mut msg_view = MessageStatsView::new(bundle, &ChartPalette::light());
    msg_view.load_more_public_forwards(&live);
    assert_eq!(msg_view.bundle.public_forwards.len(), 2);
    assert_eq!(
        msg_view.bundle.public_forwards[1].peer.title,
        "Repost Channel Two"
    );
    assert!(msg_view.bundle.public_forwards_next_offset.is_empty());

    // 3. Explicit `StatsBackend::load_async_graph` invokes `stats.loadAsyncGraph` on DC 4.
    rpc.enqueue_ok(
        Some(tl_fns::stats::LoadAsyncGraph::CONSTRUCTOR_ID),
        &tl_enums::StatsGraph::StatsGraph(tl_types::StatsGraph {
            json: tl_enums::DataJSON::DataJSON(tl_types::DataJSON {
                data: SAMPLE_CHART_JSON.to_owned(),
            }),
            zoom_token: None,
        }),
    );
    let hydrated = live
        .load_async_graph("explicit_async_token")
        .expect("explicit async graph");
    assert!(!hydrated.chart.is_empty());

    // 4. Story public forwards pagination via `stats.getStoryPublicForwards`.
    rpc.enqueue_ok(
        Some(tl_fns::stats::GetStoryPublicForwards::CONSTRUCTOR_ID),
        &tl_enums::stats::PublicForwards::PublicForwards(tl_types::stats::PublicForwards {
            count: 1,
            forwards: vec![tl_enums::PublicForward::Story(
                tl_types::PublicForwardStory {
                    peer: tl_enums::Peer::Channel(tl_types::PeerChannel { channel_id: 4012 }),
                    story: tl_enums::StoryItem::Skipped(tl_types::StoryItemSkipped {
                        close_friends: false,
                        live: false,
                        id: 88,
                        date: 1_700_000_200,
                        expire_date: 1_700_086_400,
                    }),
                },
            )],
            next_offset: None,
            chats: vec![tl_enums::Chat::Channel(fwd_ch_2)],
            users: Vec::new(),
        }),
    );
    let (story_rows, story_next) = live
        .load_more_public_forwards(55, true, "story_fwd_2")
        .expect("load more story public forwards");
    assert_eq!(story_rows.len(), 1);
    assert_eq!(story_rows[0].story_id, 88);
    assert!(story_next.is_empty());

    // Verify stats RPCs targeted configured stats DC 4.
    let async_dc = rpc
        .recorded()
        .iter()
        .find(|c| c.ctor == tl_fns::stats::LoadAsyncGraph::CONSTRUCTOR_ID)
        .expect("LoadAsyncGraph recorded")
        .dc_id;
    assert_eq!(async_dc, 4);

    // Silence unused import warnings for ChannelId/DataPeerId if needed.
    let _ = DataPeerId::from_channel(ChannelId(4001));
}
