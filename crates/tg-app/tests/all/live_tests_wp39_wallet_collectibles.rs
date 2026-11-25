//! WP-39 tests: TON Wallet Collectibles (NFTs), Jetton Transfers (TEP-74), NFT Transfers (TEP-62),
//! Read-Only Fiat Onramp/Swap Handoff, and Profile `CollectibleInfoBox` (`fragment.getCollectibleInfo`).
//!
//! Hard rules enforced:
//! - Generates synthetic test keys inside tests only; never moves real funds.
//! - Uses strictly local in-process fake Toncenter v3 HTTP servers and `FakeRpcClient`;
//!   never contacts the real TON network, Toncenter, bridges, or onramp providers.
//! - Onramp is verified as a strictly read-only HTTPS handoff with zero payment execution.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use egui::{Context, Pos2, RawInput, Rect, Vec2};
use serde_json::{Value, json};
use tempfile::TempDir;
use tg_api::convert::apply_tl_users_and_chats;
use tg_api::{DcId, InvocationError, RpcClient};
use tg_data::{PeerId as DataPeerId, UserId as DataUserId};
use tg_platform::wallet_protection::FakeWalletKeyProtector;
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tg_wallet::address::TonAddress;
use tg_wallet::boc::deserialize_boc_base64;
use tg_wallet::contract::WalletVersion;
use tg_wallet::jettons::{
    DEFAULT_JETTON_GAS_NANO, DEFAULT_NFT_GAS_NANO, parse_jetton_transfer_body,
    parse_nft_transfer_body,
};
use tg_wallet::mnemonic::generate_mnemonic;
use tg_wallet::toncenter::NftKind;
use tg_wallet::vault::PasscodeKdf;
use tg_wallet::{WalletAppConfig, format_ton};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::broadcast;

use tg_app::boxes::collectible_info::CollectibleInfoBoxState;
use tg_app::boxes::{BoxHost, BoxKind};
use tg_app::info::actions::{ProfileAction, ProfileActionsView};
use tg_app::info::backend::{InfoBackend, InfoPeerKind, PeerProfile, PeerUsername};
use tg_app::info::values::{
    CollectibleEntityKind, DEFAULT_FRAGMENT_PHONE_PREFIXES, detect_collectible_type,
    format_collectible_crypto_amount, format_collectible_date, format_collectible_entity,
    format_collectible_fiat_amount, is_collectible_phone,
};
use tg_app::live::LiveSession;
use tg_app::live::wallet::{
    LiveWalletBackend, RpcOnrampProvider, request_onramp_session_url_async,
};
use tg_app::settings::theme_bridge::LiveThemeColors;
use tg_app::stand_in::theme::Palette;
use tg_app::stand_in::types::PeerId;
use tg_app::wallet::backend::WalletBackend;
use tg_app::wallet::mock::MockWalletBackend;
use tg_app::wallet::onramp::{build_readonly_swap_handoff_url, validate_handoff_url};
use tg_app::wallet::{WalletPanel, WalletScreen};

// ============================================================================
// Local In-Process Fake Toncenter v2 + v3 HTTP Server
// ============================================================================

struct FakeToncenterV3State {
    sent_bocs: Mutex<Vec<String>>,
    balance_nano: Mutex<u64>,
    seqno: Mutex<u64>,
    nft_requests: Mutex<usize>,
    jetton_wallet_requests: Mutex<usize>,
    jetton_master_requests: Mutex<usize>,
}

async fn spawn_local_fake_toncenter_v3(owner_raw: String) -> (String, Arc<FakeToncenterV3State>) {
    let state = Arc::new(FakeToncenterV3State {
        sent_bocs: Mutex::new(Vec::new()),
        balance_nano: Mutex::new(5_000_000_000),
        seqno: Mutex::new(4),
        nft_requests: Mutex::new(0),
        jetton_wallet_requests: Mutex::new(0),
        jetton_master_requests: Mutex::new(0),
    });

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake toncenter v3");
    let addr = listener.local_addr().expect("local addr");
    let state_clone = Arc::clone(&state);

    let nft_username_addr = TonAddress::new(0, [0xA1; 32]).to_raw();
    let nft_number_addr = TonAddress::new(0, [0xA2; 32]).to_raw();
    let nft_gift_addr = TonAddress::new(0, [0xA3; 32]).to_raw();
    let jw_usdt_addr = TonAddress::new(0, [0xB1; 32]).to_raw();
    let jm_usdt_addr = TonAddress::new(0, [0xD1; 32]).to_raw();
    let jw_usdt_friendly = TonAddress::new(0, [0xB1; 32]).to_friendly(true, false);
    let jm_usdt_friendly = TonAddress::new(0, [0xD1; 32]).to_friendly(true, false);

    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let st = Arc::clone(&state_clone);
            let owner_raw = owner_raw.clone();
            let nft_username_addr = nft_username_addr.clone();
            let nft_number_addr = nft_number_addr.clone();
            let nft_gift_addr = nft_gift_addr.clone();
            let jw_usdt_addr = jw_usdt_addr.clone();
            let jm_usdt_addr = jm_usdt_addr.clone();
            let jw_usdt_friendly = jw_usdt_friendly.clone();
            let jm_usdt_friendly = jm_usdt_friendly.clone();

            tokio::spawn(async move {
                let mut buf = vec![0_u8; 32768];
                let n = stream.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                let first_line = req.lines().next().unwrap_or("");
                let body_raw = req.split("\r\n\r\n").nth(1).unwrap_or("");

                let resp_body = if first_line.contains("/api/v2/getAddressInformation") {
                    let bal = *st.balance_nano.lock().expect("bal");
                    json!({
                        "ok": true,
                        "result": {
                            "balance": bal.to_string(),
                            "state": "active",
                            "last_transaction_id": { "lt": "51000001", "hash": "last_hash_v3" }
                        }
                    })
                    .to_string()
                } else if first_line.contains("/api/v2/getTransactions") {
                    json!({ "ok": true, "result": [] }).to_string()
                } else if first_line.contains("/api/v2/runGetMethod") {
                    let seq = *st.seqno.lock().expect("seq");
                    json!({
                        "ok": true,
                        "result": { "exit_code": 0, "stack": [["num", format!("0x{seq:x}")]] }
                    })
                    .to_string()
                } else if first_line.contains("/api/emulate/v1/emulateTrace") {
                    json!({
                        "mc_block_seqno": 120,
                        "trace": { "tx_hash": "root_tx" },
                        "transactions": {
                            "root_tx": {
                                "account": owner_raw,
                                "total_fees": "4800000",
                                "description": {
                                    "type": "ord",
                                    "aborted": false,
                                    "compute_ph": { "success": true, "exit_code": 0 },
                                    "action": { "success": true, "result_code": 0 }
                                }
                            }
                        }
                    })
                    .to_string()
                } else if first_line.contains("/api/v2/sendBoc") {
                    if let Ok(v) = serde_json::from_str::<Value>(body_raw)
                        && let Some(boc) = v.get("boc").and_then(Value::as_str)
                    {
                        st.sent_bocs.lock().expect("bocs").push(boc.to_owned());
                    }
                    json!({
                        "ok": true,
                        "result": { "@type": "ok", "hash": "fake_v3_tx_hash" }
                    })
                    .to_string()
                } else if first_line.contains("/api/v3/nft/items") {
                    *st.nft_requests.lock().expect("nft_req") += 1;
                    json!({
                        "nft_items": [
                            {
                                "address": nft_username_addr,
                                "index": "42",
                                "collection_address": "0:80D78A35F955A14B679FAA887FF4CD5BFC0F43B4A4EEA2A7E6927F3701B273C2",
                                "owner_address": owner_raw,
                                "real_owner": owner_raw,
                                "on_sale": false,
                                "content": {
                                    "domain": "aurora_test.t.me",
                                    "uri": "https://nft.fragment.com/username/aurora_test.json"
                                },
                                "collection": { "name": "Telegram Usernames" }
                            },
                            {
                                "address": nft_number_addr,
                                "index": "43",
                                "collection_address": "0:0E41DC1DC3C9067ED24248580E12B3359818D83DEE0304FABCF80845EAFAFDB2",
                                "owner_address": owner_raw,
                                "real_owner": owner_raw,
                                "on_sale": false,
                                "content": {
                                    "uri": "https://nft.fragment.com/number/88801234567.json"
                                },
                                "collection": { "name": "Anonymous Telegram Numbers" }
                            },
                            {
                                "address": nft_gift_addr,
                                "index": "45754",
                                "collection_address": "0:4C71F300665314AF55B75FC91D130DDF24C5006961F8F9772613947945F14863",
                                "owner_address": owner_raw,
                                "real_owner": owner_raw,
                                "on_sale": false,
                                "content": {
                                    "name": "Desk Calendar #45754",
                                    "uri": "https://nft.fragment.com/gift/deskcalendar-45754.json"
                                },
                                "collection": { "name": "Telegram Gifts" }
                            }
                        ]
                    })
                    .to_string()
                } else if first_line.contains("/api/v3/jetton/wallets") {
                    *st.jetton_wallet_requests.lock().expect("jw_req") += 1;
                    // Deliberately omit symbol/name in metadata to test `/api/v3/jetton/masters` enrichment!
                    json!({
                        "jetton_wallets": [
                            {
                                "address": jw_usdt_addr,
                                "balance": "125500000",
                                "owner": owner_raw,
                                "jetton": jm_usdt_addr,
                                "last_transaction_lt": "95000001"
                            }
                        ],
                        "address_book": {
                            jw_usdt_addr.to_ascii_uppercase(): { "user_friendly": jw_usdt_friendly },
                            jm_usdt_addr.to_ascii_uppercase(): { "user_friendly": jm_usdt_friendly }
                        },
                        "metadata": {}
                    })
                    .to_string()
                } else if first_line.contains("/api/v3/jetton/masters") {
                    *st.jetton_master_requests.lock().expect("jm_req") += 1;
                    json!({
                        "jetton_masters": [
                            {
                                "address": jm_usdt_addr,
                                "total_supply": "1429976002510000",
                                "mintable": true,
                                "jetton_content": {
                                    "decimals": "6",
                                    "name": "Tether USD (Test)",
                                    "symbol": "USDt",
                                    "uri": "https://tokens.example/usdt.json"
                                }
                            }
                        ],
                        "address_book": {
                            jm_usdt_addr.to_ascii_uppercase(): { "user_friendly": jm_usdt_friendly }
                        }
                    })
                    .to_string()
                } else {
                    json!({ "ok": false, "error": "unknown endpoint" }).to_string()
                };

                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    resp_body.len(),
                    resp_body
                );
                let _ = stream.write_all(resp.as_bytes()).await;
            });
        }
    });

    (format!("http://{addr}"), state)
}

// ============================================================================
// In-Process FakeRpcClient for `fragment.getCollectibleInfo` & `payments.*Onramp*`
// ============================================================================

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
        let (updates_tx, _) = broadcast::channel(32);
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
            .expect("responses")
            .push_back((expected_ctor, Ok(resp.to_bytes())));
    }

    fn recorded_calls(&self) -> Vec<(u32, Vec<u8>)> {
        self.recorded.lock().expect("recorded").clone()
    }
}

impl RpcClient for FakeRpcClient {
    type Error = InvocationError;

    async fn invoke<R: RemoteCall + Send + Sync>(&self, req: &R) -> Result<R::Return, Self::Error> {
        self.invoke_in_dc(self.main_dc_id(), req).await
    }

    async fn invoke_in_dc<R: RemoteCall + Send + Sync>(
        &self,
        _dc_id: DcId,
        req: &R,
    ) -> Result<R::Return, Self::Error> {
        let bytes = req.to_bytes();
        let ctor = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        self.recorded.lock().expect("recorded").push((ctor, bytes));

        let next = {
            let mut q = self.responses.lock().expect("responses");
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
        *self.main_dc.lock().expect("dc")
    }

    fn set_main_dc_id(&self, dc_id: DcId) {
        *self.main_dc.lock().expect("dc") = dc_id;
    }

    fn subscribe_updates(&self) -> Option<broadcast::Receiver<tl_enums::Updates>> {
        Some(self.updates_tx.subscribe())
    }
}

fn extract_first_v5r1_internal_msg_body(external_boc_b64: &str) -> tg_wallet::cell::Cell {
    // External-in message: ext_in_msg_info + init:Maybe + body:^Cell.
    let ext = deserialize_boc_base64(external_boc_b64).expect("deserialize external BOC");
    let wallet_body = ext.refs().last().expect("wallet body ref");
    // V5R1 wallet body: 0x7369676e (32) + wallet_id (32) + valid_until (32) + seqno (32)
    // + actions:Maybe ^OutList (first ref).
    let out_list = wallet_body.refs().first().expect("v5r1 OutList ref");
    // OutList node: prev:^OutList (ref 0) + action_send_msg + mode + msg:^InternalMsg (ref 1).
    let internal_msg = out_list.refs().get(1).expect("internal msg ref");
    // Internal message body is stored as Right (^Cell) when non-inline (last ref).
    if let Some(body_ref) = internal_msg.refs().last() {
        return (**body_ref).clone();
    }
    (**internal_msg).clone()
}

fn synthetic_tl_user(
    id: i64,
    first_name: &str,
    username: &str,
    phone: Option<&str>,
) -> tl_types::User {
    tl_types::User {
        self_: false,
        contact: true,
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
        access_hash: Some(id * 17 + 3),
        first_name: Some(first_name.to_owned()),
        last_name: None,
        username: (!username.is_empty()).then(|| username.to_owned()),
        phone: phone.map(ToOwned::to_owned),
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

// ============================================================================
// Test 1: Local Fake Toncenter v3 NFT/Jetton Fetch + TEP-74 & TEP-62 Transfer BOCs
// ============================================================================

#[test]
fn wp39_toncenter_v3_fetch_and_jetton_nft_transfer_bocs() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    let tmp = TempDir::new().expect("tempdir");
    // Generate a fresh synthetic test mnemonic inside the test (never real keys/funds).
    let words = generate_mnemonic("").expect("generate synthetic test mnemonic");

    // Create a temporary wallet first to get its deterministic address for the fake server.
    let mut temp_backend = LiveWalletBackend::with_dependencies(
        777,
        tmp.path().join("wallet_probe"),
        rt.handle().clone(),
        WalletAppConfig::default(),
        None,
        None,
        Arc::new(FakeWalletKeyProtector::new()),
        PasscodeKdf::fast_test_kdf(),
    );
    let wallet_addr = temp_backend
        .create_wallet(&words, WalletVersion::V5R1, "test-pass")
        .expect("create synthetic wallet");

    let (base_url, server_state) = rt.block_on(spawn_local_fake_toncenter_v3(wallet_addr.to_raw()));

    let cfg = WalletAppConfig {
        toncenter_base_url: base_url,
        ..WalletAppConfig::default()
    };
    let mut backend = LiveWalletBackend::with_dependencies(
        777,
        tmp.path().join("wallet_live"),
        rt.handle().clone(),
        cfg,
        None,
        None,
        Arc::new(FakeWalletKeyProtector::new()),
        PasscodeKdf::fast_test_kdf(),
    );
    let addr2 = backend
        .create_wallet(&words, WalletVersion::V5R1, "test-pass")
        .expect("create live test wallet");
    assert_eq!(addr2, wallet_addr);

    // 1. Refresh NFTs & Jettons against local fake Toncenter v3 server.
    backend.refresh_assets().expect("refresh_assets");
    assert_eq!(*server_state.nft_requests.lock().expect("nft_req"), 1);
    assert_eq!(
        *server_state.jetton_wallet_requests.lock().expect("jw_req"),
        1
    );
    assert_eq!(
        *server_state.jetton_master_requests.lock().expect("jm_req"),
        1
    );

    let nfts = backend.collectibles();
    assert_eq!(nfts.len(), 3);
    assert_eq!(nfts[0].kind, NftKind::TelegramUsername);
    assert_eq!(nfts[0].key, "aurora_test");
    assert_eq!(nfts[1].kind, NftKind::TelegramNumber);
    assert_eq!(nfts[1].key, "88801234567");
    assert_eq!(nfts[2].kind, NftKind::TelegramGift);
    assert_eq!(nfts[2].key, "deskcalendar-45754");

    let jettons = backend.jetton_balances();
    assert_eq!(jettons.len(), 1);
    assert_eq!(jettons[0].symbol, "USDt");
    assert_eq!(jettons[0].decimals, 6);
    assert_eq!(jettons[0].balance, 125_500_000);
    assert_eq!(jettons[0].formatted_balance(), "125.5");

    // 2. Prepare & confirm TEP-74 Jetton transfer and inspect submitted BOC payload.
    let recipient = TonAddress::new(0, [0x91; 32]);
    let recipient_friendly = recipient.to_friendly(false, false);
    let jquote = backend
        .prepare_jetton_transfer(
            &jettons[0].wallet_address,
            &recipient_friendly,
            25_250_000, // 25.25 USDt
            "Sprint split",
        )
        .expect("prepare_jetton_transfer");
    assert_eq!(jquote.attached_ton_nano, DEFAULT_JETTON_GAS_NANO);
    assert_eq!(jquote.fee_nano, 4_800_000);

    let jreceipt = backend
        .confirm_jetton_transfer(&jquote)
        .expect("confirm_jetton_transfer");
    assert!(jreceipt.signed_boc_base64.starts_with("te6cc"));
    assert_eq!(backend.jetton_balances()[0].balance, 100_250_000);

    // Decode the exact BOC received by the local fake server and assert TEP-74 fields.
    let submitted_bocs = server_state.sent_bocs.lock().expect("sent_bocs").clone();
    assert_eq!(submitted_bocs.len(), 1);
    let jetton_body_cell = extract_first_v5r1_internal_msg_body(&submitted_bocs[0]);
    let decoded_jetton = parse_jetton_transfer_body(&jetton_body_cell).expect("decode TEP-74");
    assert_eq!(decoded_jetton.jetton_amount, 25_250_000);
    assert_eq!(decoded_jetton.destination, recipient);
    assert_eq!(decoded_jetton.response_destination, Some(wallet_addr));
    assert_eq!(
        decoded_jetton.forward_comment.as_deref(),
        Some("Sprint split")
    );

    // 3. Prepare & confirm TEP-62 NFT transfer and inspect submitted BOC payload.
    let new_owner = TonAddress::new(0, [0x92; 32]);
    let new_owner_friendly = new_owner.to_friendly(false, false);
    let nquote = backend
        .prepare_nft_transfer(&nfts[2].address, &new_owner_friendly, "Gift transfer")
        .expect("prepare_nft_transfer");
    assert_eq!(nquote.attached_ton_nano, DEFAULT_NFT_GAS_NANO);
    assert_eq!(nquote.nft_name, "Desk Calendar #45754");

    let nreceipt = backend
        .confirm_nft_transfer(&nquote)
        .expect("confirm_nft_transfer");
    assert!(nreceipt.signed_boc_base64.starts_with("te6cc"));
    assert_eq!(backend.collectibles().len(), 2);

    let submitted_bocs = server_state.sent_bocs.lock().expect("sent_bocs").clone();
    assert_eq!(submitted_bocs.len(), 2);
    let nft_body_cell = extract_first_v5r1_internal_msg_body(&submitted_bocs[1]);
    let decoded_nft = parse_nft_transfer_body(&nft_body_cell).expect("decode TEP-62");
    assert_eq!(decoded_nft.new_owner, new_owner);
    assert_eq!(decoded_nft.response_destination, Some(wallet_addr));
    assert_eq!(
        decoded_nft.forward_comment.as_deref(),
        Some("Gift transfer")
    );
}

// ============================================================================
// Test 2: Read-Only Fiat Onramp & Swap Link Handoff (FakeRpcClient, Zero Payments)
// ============================================================================

#[test]
fn wp39_readonly_onramp_rpc_flow_and_url_validation() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    let rpc = FakeRpcClient::new(2);
    // Enqueue 3-step `wallet_onramp.cpp` responses:
    // 1. `payments.getOnrampProviders` (called once by refresh_onramp_providers and once by create_handoff)
    let providers_resp = vec![tl_enums::OnrampProviderInfo::OnrampProviderInfo(
        tl_types::OnrampProviderInfo {
            supports_base_currencies: true,
            supports_limits: true,
            supports_quote: true,
            id: "fake_provider".to_owned(),
            name: "Fake Read-Only Onramp".to_owned(),
            crypto_currencies: vec!["GRAM".to_owned(), "TON".to_owned()],
        },
    )];
    rpc.enqueue_ok(
        tl_fns::payments::GetOnrampProviders::CONSTRUCTOR_ID,
        &providers_resp,
    );
    rpc.enqueue_ok(
        tl_fns::payments::GetOnrampProviders::CONSTRUCTOR_ID,
        &providers_resp,
    );
    // 2. `payments.getOnrampBaseCurrencies`
    rpc.enqueue_ok(
        tl_fns::payments::GetOnrampBaseCurrencies::CONSTRUCTOR_ID,
        &vec!["usd".to_owned(), "eur".to_owned()],
    );
    // 3. `payments.createOnrampSession`
    rpc.enqueue_ok(
        tl_fns::payments::CreateOnrampSession::CONSTRUCTOR_ID,
        &tl_enums::OnrampSession::OnrampSession(tl_types::OnrampSession {
            provider: "fake_provider".to_owned(),
            session_id: "sess_ro_001".to_owned(),
            url: "https://onramp.example/session/sess_ro_001?readonly=1".to_owned(),
            expires_date: 1_800_000_000,
        }),
    );

    let tmp = TempDir::new().expect("tempdir");
    let words = generate_mnemonic("").expect("mnemonic");
    let mut backend = LiveWalletBackend::with_dependencies(
        778,
        tmp.path().join("wallet_onramp"),
        rt.handle().clone(),
        WalletAppConfig::default(),
        None,
        None,
        Arc::new(FakeWalletKeyProtector::new()),
        PasscodeKdf::fast_test_kdf(),
    );
    backend.set_onramp_provider(Arc::new(RpcOnrampProvider::new(rpc.clone())));
    let wallet_addr = backend
        .create_wallet(&words, WalletVersion::V5R1, "pass1234")
        .expect("create wallet");

    let providers = backend
        .refresh_onramp_providers()
        .expect("refresh_onramp_providers");
    assert_eq!(providers.len(), 1);
    assert_eq!(providers[0].id, "fake_provider");

    let session = backend
        .create_onramp_handoff("fake_provider", Some("EUR"))
        .expect("create_onramp_handoff");
    assert_eq!(session.provider_id, "fake_provider");
    assert_eq!(session.base_currency.as_deref(), Some("eur"));
    assert_eq!(
        session.wallet_address,
        wallet_addr.to_friendly(false, false)
    );
    assert_eq!(
        session.handoff_url,
        "https://onramp.example/session/sess_ro_001?readonly=1"
    );

    // Reject unsafe non-HTTPS URLs returned by a malicious/broken provider.
    let rpc_bad = FakeRpcClient::new(2);
    rpc_bad.enqueue_ok(
        tl_fns::payments::GetOnrampProviders::CONSTRUCTOR_ID,
        &providers_resp,
    );
    rpc_bad.enqueue_ok(
        tl_fns::payments::GetOnrampBaseCurrencies::CONSTRUCTOR_ID,
        &vec!["usd".to_owned()],
    );
    rpc_bad.enqueue_ok(
        tl_fns::payments::CreateOnrampSession::CONSTRUCTOR_ID,
        &tl_enums::OnrampSession::OnrampSession(tl_types::OnrampSession {
            provider: "fake_provider".to_owned(),
            session_id: "bad".to_owned(),
            url: "javascript:alert(1)".to_owned(),
            expires_date: 1_800_000_000,
        }),
    );
    let bad_res = rt.block_on(request_onramp_session_url_async(
        &rpc_bad,
        &wallet_addr.to_friendly(false, false),
        Some("USD"),
        Some("fake_provider"),
    ));
    assert!(bad_res.is_err());

    // URL validator & read-only swap link builder.
    assert!(validate_handoff_url("http://insecure.example").is_err());
    assert!(validate_handoff_url("javascript:void(0)").is_err());
    let swap_url = build_readonly_swap_handoff_url(
        "https://swap.example/portal",
        &wallet_addr.to_friendly(false, false),
        "TON",
        "USDt",
    )
    .expect("swap url");
    assert!(swap_url.starts_with("https://swap.example/portal?"));
    assert!(swap_url.contains("from=TON"));
    assert!(swap_url.contains("to=USDt"));
    assert!(swap_url.contains("readonly=1"));
}

// ============================================================================
// Test 3: `fragment.getCollectibleInfo` Fake-RPC + `CollectibleInfoBox` + Profile Reachability
// ============================================================================

#[test]
fn wp39_fragment_get_collectible_info_fake_rpc_and_modal() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    let rpc = FakeRpcClient::new(2);
    let cfg = SessionRuntimeConfig {
        init_updates_state: false,
        run_get_difference_on_start: false,
        load_initial_dialogs: false,
        fetch_self_user_on_start: false,
        ..SessionRuntimeConfig::default()
    };
    let runtime = Arc::new(
        SessionRuntime::start_on(
            DataUserId::from_tl(100),
            tg_app::live::ErasedRpcClient::new(rpc.clone()),
            rt.handle().clone(),
            cfg,
        )
        .expect("start SessionRuntime"),
    );
    let peer_tl = synthetic_tl_user(200, "Sora Lin", "aurora_nft", Some("+88801234567"));
    rt.block_on(async {
        let mut store = runtime.session().write().await;
        apply_tl_users_and_chats(&mut store, &[tl_enums::User::User(peer_tl)], &[]);
    });
    let live = LiveSession::new(runtime, 1);
    let peer_id = PeerId(DataPeerId::from_user(DataUserId::from_tl(200)).0);

    // 1. Collectible Username lookup (`InputCollectible::Username`).
    rpc.enqueue_ok(
        tl_fns::fragment::GetCollectibleInfo::CONSTRUCTOR_ID,
        &tl_enums::fragment::CollectibleInfo::CollectibleInfo(
            tl_types::fragment::CollectibleInfo {
                purchase_date: 1_700_000_000,
                currency: "USD".to_owned(),
                amount: 125_000, // $1,250.00
                crypto_currency: "TON".to_owned(),
                crypto_amount: 250_000_000_000, // 250 TON
                url: "https://fragment.com/username/aurora_nft".to_owned(),
            },
        ),
    );
    let username_info = InfoBackend::get_collectible_info(&live, peer_id, "aurora_nft")
        .expect("get_collectible_info username");
    assert_eq!(username_info.kind, CollectibleEntityKind::Username);
    assert_eq!(username_info.formatted_entity(), "@aurora_nft");
    assert_eq!(username_info.copy_text, "https://t.me/aurora_nft");
    assert_eq!(username_info.owner_name, "Sora Lin");
    assert_eq!(
        username_info.validated_fragment_url(),
        Some("https://fragment.com/username/aurora_nft")
    );
    assert!(username_info.summary_line().contains("250 TON"));
    assert!(username_info.summary_line().contains("$1250.00"));

    // 2. Anonymous Phone Number lookup (`InputCollectible::Phone`).
    rpc.enqueue_ok(
        tl_fns::fragment::GetCollectibleInfo::CONSTRUCTOR_ID,
        &tl_enums::fragment::CollectibleInfo::CollectibleInfo(
            tl_types::fragment::CollectibleInfo {
                purchase_date: 1_700_000_000,
                currency: "EUR".to_owned(),
                amount: 89_500, // €895.00
                crypto_currency: "TON".to_owned(),
                crypto_amount: 175_500_000_000, // 175.5 TON
                url: "https://fragment.com/number/88801234567".to_owned(),
            },
        ),
    );
    let phone_info = InfoBackend::get_collectible_info(&live, peer_id, "+88801234567")
        .expect("get_collectible_info phone");
    assert_eq!(phone_info.kind, CollectibleEntityKind::Phone);
    assert_eq!(phone_info.formatted_entity(), "+888 0123 4567");
    assert_eq!(phone_info.copy_button_label(), "Copy Phone Number");
    assert!(phone_info.headline().contains("anonymous number"));
    assert!(phone_info.summary_line().contains("175.5 TON"));
    assert!(phone_info.summary_line().contains("€895.00"));

    // Verify both RPC requests used `fragment.getCollectibleInfo` constructor ID.
    let calls = rpc.recorded_calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(
        calls[0].0,
        tl_fns::fragment::GetCollectibleInfo::CONSTRUCTOR_ID
    );
    assert_eq!(
        calls[1].0,
        tl_fns::fragment::GetCollectibleInfo::CONSTRUCTOR_ID
    );

    // Verify `info/values.rs` helpers.
    assert_eq!(
        detect_collectible_type("+88801234567"),
        CollectibleEntityKind::Phone
    );
    assert_eq!(
        detect_collectible_type("aurora_nft"),
        CollectibleEntityKind::Username
    );
    assert!(is_collectible_phone(
        "+888 0123 4567",
        DEFAULT_FRAGMENT_PHONE_PREFIXES
    ));
    assert!(!is_collectible_phone(
        "+999 661 0142",
        DEFAULT_FRAGMENT_PHONE_PREFIXES
    ));
    assert_eq!(
        format_collectible_entity(CollectibleEntityKind::Phone, "+88801234567"),
        "+888 0123 4567"
    );
    assert_eq!(
        format_collectible_crypto_amount("TON", 1_500_000_000),
        format!("{} TON", format_ton(1_500_000_000))
    );
    assert_eq!(format_collectible_fiat_amount("USD", 12_345), "$123.45");
    assert_eq!(format_collectible_date(1_700_000_000), "Nov 14, 2023");

    // 3. Render `CollectibleInfoBoxState` inside `BoxHost` and verify peer profile rendering.
    let ctx = Context::default();
    let colors = LiveThemeColors::default();
    let mut box_host = BoxHost::new();
    box_host.push(
        &ctx,
        BoxKind::CollectibleInfo(CollectibleInfoBoxState::new(username_info)),
        &colors,
    );
    assert!(!box_host.is_empty());

    let mut mock_info = tg_app::info::mock::MockBackend::new();
    let collectible_profile = PeerProfile {
        id: peer_id,
        kind: InfoPeerKind::User,
        title: "Sora Lin".to_owned(),
        status: "online".to_owned(),
        is_online: true,
        verified: false,
        premium: true,
        color_index: 2,
        phone: Some("+888 0123 4567".to_owned()),
        usernames: vec![
            PeerUsername {
                username: "sora_regular".to_owned(),
                is_editable: true,
                is_active: true,
            },
            PeerUsername {
                username: "aurora_nft".to_owned(),
                is_editable: false,
                is_active: true,
            },
        ],
        bio: "Collectible profile test".to_owned(),
        birthday: None,
        invite_link: None,
        muted: false,
        is_contact: true,
        is_blocked: false,
        can_edit: false,
        can_add_members: false,
        can_manage_admins: false,
        can_manage_permissions: false,
        is_public: false,
        no_forwards: false,
        is_forum: false,
        members_count: 0,
        online_count: 0,
        admins_count: 0,
        default_banned_rights: tg_data::rights::ChatBannedRights::empty(),
        slowmode_seconds: 0,
        auto_delete_ttl_seconds: 0,
        linked_discussion_peer: None,
        bot_commands_count: 0,
        bot_reads_all_messages: false,
        has_mini_app: false,
        media_counts: Default::default(),
        group_call: Default::default(),
    };

    let bounds = Rect::from_min_size(Pos2::ZERO, Vec2::new(900.0, 700.0));
    let input = RawInput {
        screen_rect: Some(bounds),
        ..RawInput::default()
    };
    let mut out = ctx.run_ui(input, |ui| {
        egui::CentralPanel::default().show(ui, |ui| {
            let act: Option<ProfileAction> =
                ProfileActionsView::show(ui, &collectible_profile, false, &colors);
            assert!(act.is_none());
        });
        let _ = box_host.show(ui.ctx(), bounds, &mut mock_info, &colors);
    });
    out.textures_delta.clear();
}

// ============================================================================
// Test 4: Wallet Panel Collectibles, Jettons & Onramp Screens Render Smoke Test
// ============================================================================

#[test]
fn wp39_wallet_panel_renders_collectibles_jettons_and_onramp_screens() {
    let ctx = Context::default();
    let palette = Palette::day();
    let mut panel = WalletPanel::new(Box::new(MockWalletBackend::with_demo_wallet()));

    for screen in [
        WalletScreen::Overview,
        WalletScreen::Collectibles,
        WalletScreen::Onramp,
    ] {
        panel.select_screen(screen);
        let input = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(900.0, 700.0))),
            ..RawInput::default()
        };
        let mut out = ctx.run_ui(input, |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                let _ = panel.show(ui, &palette, true);
            });
        });
        out.textures_delta.clear();
    }

    // Verify demo wallet mock backend collectibles, jettons, and read-only onramp handoff.
    assert_eq!(panel.backend_mut().collectibles().len(), 3);
    assert_eq!(panel.backend_mut().jetton_balances().len(), 2);
    let providers = panel.backend_mut().onramp_providers();
    assert_eq!(providers.len(), 1);
    let handoff = panel
        .backend_mut()
        .create_onramp_handoff(&providers[0].id, Some("USD"))
        .expect("demo onramp handoff");
    assert!(handoff.handoff_url.starts_with("https://"));
    assert!(handoff.handoff_url.contains("readonly=1"));
}
