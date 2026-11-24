//! Wave 9 Wire1 integration tests and headless `egui_kittest` screenshot harness.
//!
//! All tests use strictly fake signers, fake keys generated in the test, fake `ToncenterApi`,
//! fake `TonConnectBridge`, fake `CableBleScanner`, fake `WidgetSandboxLauncher`, and fake
//! `AutostartProvider` — never real seeds, never the TON network, never real OS hardware/registry.

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use egui::{Frame, Margin, ScrollArea, Vec2};
use egui_kittest::HarnessBuilder;
use serde_json::Value;
use tg_api::types::ApiCredentials;
use tg_api::{DcId, InvocationError, RpcClient};
use tg_app::intro::IntroWidget;
use tg_app::live::convert::now_unix_i32;
use tg_app::live::passkeys::LivePasskeysHandle;
use tg_app::live::wallet::LiveWalletBackend;
use tg_app::live::widgets::{
    LiveWidgetHost, attach_widget_disk_storage, widget_meter_configure_live,
    widget_meter_record_frame_live, widget_meter_reset_live, widget_meter_verdict_live,
    widget_poll_public_data_live, widget_query_live, widget_storage_clear_chat_live,
    widget_storage_flush_live, widget_storage_height_live, widget_storage_prune_expired_live,
    widget_storage_read_live, widget_storage_set_height_live, widget_storage_write_live,
};
use tg_app::live::{ErasedRpcClient, LiveRuntimeBridge, LiveSession};
use tg_app::settings::advanced::{AdvancedState, render_advanced_section};
use tg_app::settings::backend::{PasskeyHybridQrStatus, SettingsBackend};
use tg_app::settings::codes::DebugOptions;
use tg_app::settings::mock::MockSettingsBackend;
use tg_app::settings::passkeys::{PasskeysModal, PasskeysState, render_passkeys_section};
use tg_app::settings::theme_bridge::LiveThemeColors;
use tg_app::stand_in::theme::Palette;
use tg_app::stand_in::types::{ConnectionStatus, MsgId, PeerId};
use tg_app::wallet::backend::WalletBackend;
use tg_app::wallet::backup::{BackupTab, BackupUiState};
use tg_app::wallet::mock::MockWalletBackend;
use tg_app::wallet::{WalletPanel, WalletScreen};
use tg_app_core::core_types::startup_types::UpdateSnapshot;
use tg_app_core::core_types::window_types::{ChatLayout, ColumnLayout, WindowLayout};
use tg_core::{CoreSettings, GlobalConfig};
use tg_data::{
    Message, MessageContent, MessageMedia, MsgId as DataMsgId, PeerId as DataPeerId,
    PeerUpdateFlags, User as DataUser, UserId as DataUserId, WidgetData,
};
use tg_platform::autostart::{AutostartEntry, FakeAutostartProvider, StartupTaskState};
use tg_platform::cable_scanner::{
    BleAdvertisement, CableBleScanner, CableServiceUuid, FakeCableBleScanner,
    build_cable_service_data_section,
};
use tg_platform::wallet_protection::FakeWalletKeyProtector;
use tg_platform::webauthn::{FakeWebAuthnProvider, cable_encrypt_advert};
use tg_platform::widget_sandbox::{
    FakeWidgetSandboxLauncher, WidgetHelperChannel, WidgetSandboxConfig,
};
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_tgw::{
    DiskStorageConfig, HeightRecord, MeterConfig, MeterVerdict, RuntimeState, StorageArea,
    StorageMeta, WidgetRequestFailure, pack_payload_zlib,
};
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tg_ui::fonts::register_tdesktop_fonts;
use tg_wallet::address::TonAddress;
use tg_wallet::boc::serialize_boc_base64;
use tg_wallet::contract::{TransferMessage, WalletVersion};
use tg_wallet::emulate::{
    AccountBalanceDelta, EmulateMessageRequest, EmulationResult, EmulationWarning, JettonDelta,
    SendEmulationAction,
};
use tg_wallet::mnemonic::{generate_mnemonic, mnemonic_to_keypair};
use tg_wallet::toncenter::{
    AccountState, AddressInfo, BoxFuture, EmulatedTrace, GetMethodResult, SendBocResult,
    TonTransaction, ToncenterApi,
};
use tg_wallet::tonconnect::{
    BridgeEvent, DappManifest, TonConnectBridge, TonConnectKeypair, TonConnectSession,
    verify_ton_connect_data,
};
use tg_wallet::vault::PasscodeKdf;
use tg_wallet::{WalletAppConfig, WalletError};
use tokio::sync::broadcast;

use crate::common::{TEST_STEP_DT, apply_deterministic_style, init_headless_env, step_frames};

static WIRE1_WGPU_LOCK: Mutex<()> = Mutex::new(());

fn snap_dir() -> PathBuf {
    let dir = std::env::var_os("RUSTGRAM_SNAPSHOT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("rustgram-snap").join("wire1"));
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn save_widget_png<S>(
    name: &str,
    size: Vec2,
    state: S,
    mut render: impl FnMut(&mut egui::Ui, &mut S) + 'static,
) {
    init_headless_env();
    tg_app::clock::init_test_clock();
    let mut harness = HarnessBuilder::default()
        .with_size(size)
        .with_step_dt(TEST_STEP_DT)
        .with_render_cursor(false)
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .wgpu()
        .with_context_setup(|ctx| {
            register_tdesktop_fonts(ctx);
            apply_deterministic_style(ctx);
        })
        .build_ui_state(
            move |ui, st: &mut S| {
                let pal = Palette::day();
                egui::CentralPanel::default()
                    .frame(
                        Frame::NONE
                            .fill(pal.window_bg)
                            .inner_margin(Margin::same(12)),
                    )
                    .show(ui, |ui| {
                        render(ui, st);
                    });
            },
            state,
        );
    step_frames(&mut harness, 4);
    let img = harness.render().expect("wgpu render");
    let path = snap_dir().join(format!("{name}.png"));
    img.save(&path).expect("save screenshot png");
}

// ============================================================================
// Local In-Process Fake RPC Client (strictly offline)
// ============================================================================

type QueuedResp = (u32, Result<Vec<u8>, InvocationError>);
type RecordedRpcCalls = Arc<Mutex<Vec<(u32, Vec<u8>)>>>;

#[derive(Clone)]
struct Wire1FakeRpc {
    main_dc: Arc<Mutex<DcId>>,
    recorded: RecordedRpcCalls,
    responses: Arc<Mutex<VecDeque<QueuedResp>>>,
    updates_tx: broadcast::Sender<tl_enums::Updates>,
}

impl Wire1FakeRpc {
    fn new() -> Self {
        let (updates_tx, _) = broadcast::channel(32);
        Self {
            main_dc: Arc::new(Mutex::new(2)),
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

    fn recorded(&self) -> Vec<(u32, Vec<u8>)> {
        self.recorded.lock().expect("lock recorded").clone()
    }

    fn clear_recorded(&self) {
        self.recorded.lock().expect("lock recorded").clear();
    }
}

impl RpcClient for Wire1FakeRpc {
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
            q.iter()
                .position(|(c, _)| *c == ctor)
                .and_then(|pos| q.remove(pos))
                .map(|(_, r)| r)
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

fn synthetic_self_user(id: i64) -> tl_types::User {
    tl_types::User {
        self_: true,
        contact: false,
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
        access_hash: Some(0x1111_2222),
        first_name: Some("Alex".to_owned()),
        last_name: None,
        username: Some("alex_tester".to_owned()),
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

fn enqueue_startup_responses(rpc: &Wire1FakeRpc, self_id: i64) {
    rpc.enqueue_ok(
        tl_fns::users::GetUsers::CONSTRUCTOR_ID,
        &vec![tl_enums::User::User(synthetic_self_user(self_id))],
    );
    rpc.enqueue_ok(
        tl_fns::updates::GetState::CONSTRUCTOR_ID,
        &tl_enums::updates::State::State(tl_types::updates::State {
            pts: 10,
            qts: 0,
            date: now_unix_i32(),
            seq: 1,
            unread_count: 0,
        }),
    );
    rpc.enqueue_ok(
        tl_fns::updates::GetDifference::CONSTRUCTOR_ID,
        &tl_enums::updates::Difference::Empty(tl_types::updates::DifferenceEmpty {
            date: now_unix_i32(),
            seq: 1,
        }),
    );
    rpc.enqueue_ok(
        tl_fns::messages::GetDialogs::CONSTRUCTOR_ID,
        &tl_enums::messages::Dialogs::Dialogs(tl_types::messages::Dialogs {
            dialogs: Vec::new(),
            messages: Vec::new(),
            chats: Vec::new(),
            users: Vec::new(),
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
                date: now_unix_i32(),
                seq: 1,
                unread_count: 0,
            }),
        }),
    );
}

// ============================================================================
// Fake ToncenterApi & Fake TonConnectBridge (strictly offline, synthetic only)
// ============================================================================

struct Wire1FakeToncenter {
    emulation: Mutex<Option<EmulationResult>>,
    emulate_calls: Mutex<Vec<EmulateMessageRequest>>,
    sent_bocs: Mutex<Vec<String>>,
}

impl Wire1FakeToncenter {
    fn new(emulation: Option<EmulationResult>) -> Self {
        Self {
            emulation: Mutex::new(emulation),
            emulate_calls: Mutex::new(Vec::new()),
            sent_bocs: Mutex::new(Vec::new()),
        }
    }
}

impl ToncenterApi for Wire1FakeToncenter {
    fn get_address_information<'a>(
        &'a self,
        _address: &'a str,
    ) -> BoxFuture<'a, Result<AddressInfo, WalletError>> {
        Box::pin(async {
            Ok(AddressInfo {
                balance_nano: 25_000_000_000,
                state: AccountState::Active,
                last_tx_lt: 100,
                last_tx_hash: "00".repeat(32),
                never_used: false,
            })
        })
    }

    fn get_transactions<'a>(
        &'a self,
        _address: &'a str,
        _limit: usize,
        _lt: Option<u64>,
        _hash: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Vec<TonTransaction>, WalletError>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn send_boc<'a>(
        &'a self,
        boc_base64: &'a str,
    ) -> BoxFuture<'a, Result<SendBocResult, WalletError>> {
        let boc = boc_base64.to_owned();
        Box::pin(async move {
            self.sent_bocs.lock().unwrap().push(boc);
            Ok(SendBocResult {
                ok: true,
                hash: Some("bb".repeat(32)),
            })
        })
    }

    fn run_get_method<'a>(
        &'a self,
        _address: &'a str,
        _method: &'a str,
        _stack: &'a [Value],
    ) -> BoxFuture<'a, Result<GetMethodResult, WalletError>> {
        Box::pin(async {
            Ok(GetMethodResult {
                exit_code: 0,
                stack: vec![serde_json::json!(["num", "0x7"])],
            })
        })
    }

    fn emulate_trace<'a>(
        &'a self,
        _boc_base64: &'a str,
    ) -> BoxFuture<'a, Result<EmulatedTrace, WalletError>> {
        Box::pin(async {
            Ok(EmulatedTrace {
                fee_nano: 4_800_000,
                account: format!("0:{}", "11".repeat(32)),
            })
        })
    }

    fn emulate_message<'a>(
        &'a self,
        request: &'a EmulateMessageRequest,
    ) -> BoxFuture<'a, Result<EmulationResult, WalletError>> {
        let req_clone = request.clone();
        Box::pin(async move {
            self.emulate_calls.lock().unwrap().push(req_clone);
            self.emulation
                .lock()
                .unwrap()
                .clone()
                .ok_or_else(|| WalletError::Toncenter("emulation fixture absent".to_owned()))
        })
    }
}

struct Wire1FakeBridge {
    manifest: DappManifest,
    inbox: Mutex<Vec<BridgeEvent>>,
    sent: Mutex<Vec<(String, String, String)>>,
}

impl Wire1FakeBridge {
    fn new(manifest: DappManifest) -> Self {
        Self {
            manifest,
            inbox: Mutex::new(Vec::new()),
            sent: Mutex::new(Vec::new()),
        }
    }

    fn push_event(&self, ev: BridgeEvent) {
        self.inbox.lock().unwrap().push(ev);
    }

    fn sent_messages(&self) -> Vec<(String, String, String)> {
        self.sent.lock().unwrap().clone()
    }
}

impl TonConnectBridge for Wire1FakeBridge {
    fn fetch_manifest<'a>(
        &'a self,
        _manifest_url: &'a str,
    ) -> BoxFuture<'a, Result<DappManifest, WalletError>> {
        let manifest = self.manifest.clone();
        Box::pin(async move { Ok(manifest) })
    }

    fn send_message<'a>(
        &'a self,
        from_client_id: &'a str,
        to_client_id: &'a str,
        _ttl_secs: u32,
        encrypted_base64: &'a str,
    ) -> BoxFuture<'a, Result<(), WalletError>> {
        let entry = (
            from_client_id.to_owned(),
            to_client_id.to_owned(),
            encrypted_base64.to_owned(),
        );
        Box::pin(async move {
            self.sent.lock().unwrap().push(entry);
            Ok(())
        })
    }

    fn poll_events<'a>(
        &'a self,
        _client_id: &'a str,
        _last_event_id: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Vec<BridgeEvent>, WalletError>> {
        Box::pin(async move { Ok(std::mem::take(&mut *self.inbox.lock().unwrap())) })
    }
}

struct ConnectedWalletFixture {
    _dir: tempfile::TempDir,
    _rt: tokio::runtime::Runtime,
    backend: LiveWalletBackend,
    toncenter: Arc<Wire1FakeToncenter>,
    bridge: Arc<Wire1FakeBridge>,
    dapp_session: TonConnectSession,
    pubkey: [u8; 32],
    addr: TonAddress,
}

fn setup_connected_live_wallet(emulation: Option<EmulationResult>) -> ConnectedWalletFixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio rt");
    let manifest = DappManifest {
        url: "https://demoswap.example".to_owned(),
        name: "Demo Swap".to_owned(),
        icon_url: "https://demoswap.example/icon.png".to_owned(),
    };
    let toncenter = Arc::new(Wire1FakeToncenter::new(emulation));
    let bridge = Arc::new(Wire1FakeBridge::new(manifest.clone()));
    let cfg = WalletAppConfig {
        testnet: true,
        ..WalletAppConfig::default()
    };

    let mut backend = LiveWalletBackend::with_dependencies(
        42,
        dir.path().to_path_buf(),
        rt.handle().clone(),
        cfg,
        Some(toncenter.clone()),
        Some(bridge.clone()),
        Arc::new(FakeWalletKeyProtector::new()),
        PasscodeKdf::fast_test_kdf(),
    );
    // Fresh synthetic key generated inside the test — never a real wallet seed.
    let words = generate_mnemonic("").expect("generate synthetic test phrase");
    let kp = mnemonic_to_keypair(&words, "").expect("keypair");
    let addr = backend
        .create_wallet(&words, WalletVersion::V5R1, "test-passcode")
        .expect("create synthetic wallet");

    let dapp_kp = TonConnectKeypair::generate();
    let link = format!(
        "tc://?v=2&id={}&r={{\"manifestUrl\":\"https://demoswap.example/manifest.json\",\"items\":[{{\"name\":\"ton_addr\"}}]}}",
        dapp_kp.client_id()
    );
    backend
        .handle_ton_connect_link(&link)
        .expect("queue connect link");
    let card = backend.approve_ton_connect().expect("approve connect");
    assert_eq!(card.dapp_client_id, dapp_kp.client_id());

    let sent = bridge.sent_messages();
    assert_eq!(sent.len(), 1);
    let wallet_cid = sent[0].0.clone();
    let dapp_session =
        TonConnectSession::new(dapp_kp, &wallet_cid, manifest).expect("dapp session");

    ConnectedWalletFixture {
        _dir: dir,
        _rt: rt,
        backend,
        toncenter,
        bridge,
        dapp_session,
        pubkey: kp.public_key_bytes(),
        addr,
    }
}

// ============================================================================
// 1. tg-wallet (WP-43): signData + emulate_message + Shamir phrase backup UI
// ============================================================================

#[test]
fn wallet_live_sign_data_requires_confirmation_and_verifies_signatures() {
    let ConnectedWalletFixture {
        mut backend,
        bridge,
        dapp_session,
        pubkey,
        addr,
        ..
    } = setup_connected_live_wallet(None);

    // 1a. Text signData: must queue for confirmation and NEVER auto-sign.
    let req_json = serde_json::json!({
        "id": "sd-text-1",
        "method": "signData",
        "params": [serde_json::json!({
            "type": "text",
            "text": "Confirm synthetic Wire1 test login"
        }).to_string()]
    })
    .to_string();
    let enc = dapp_session.encrypt_message(req_json.as_bytes()).unwrap();
    bridge.push_event(BridgeEvent {
        id: Some("ev-1".to_owned()),
        from: dapp_session.wallet_client_id(),
        message: enc,
    });
    let sent_before = bridge.sent_messages().len();
    assert_eq!(backend.poll_ton_connect_events().unwrap(), 1);

    // Must NOT have replied yet (neither signed nor rejected as unsupported).
    assert_eq!(bridge.sent_messages().len(), sent_before);
    let pending = backend
        .pending_sign_data()
        .expect("signData must await explicit user confirmation");
    assert_eq!(pending.request.kind, "text");
    assert_eq!(pending.dapp_domain, "demoswap.example");

    // User presses Sign -> signs with tg_wallet::tonconnect::sign_ton_connect_data & replies.
    let result = backend
        .approve_ton_connect_sign_data()
        .expect("approve text signData");
    assert!(backend.pending_sign_data().is_none());
    assert_eq!(result.address, addr.to_raw());
    assert!(
        verify_ton_connect_data(&pubkey, &result).is_ok(),
        "Ed25519 signData signature must verify"
    );
    let sent = bridge.sent_messages();
    assert_eq!(sent.len(), sent_before + 1);
    let reply_plain = dapp_session
        .decrypt_message(&sent.last().unwrap().2)
        .unwrap();
    let reply_json: serde_json::Value = serde_json::from_slice(&reply_plain).unwrap();
    assert_eq!(reply_json["id"], "sd-text-1");
    assert_eq!(reply_json["result"]["signature"], result.signature);

    // 1b. Cell signData: approve with valid BOC & schema.
    let cell = TransferMessage::simple(addr, 1, false, Some("wire1-cell-test"))
        .unwrap()
        .body
        .unwrap();
    let cell_boc = serialize_boc_base64(&cell).unwrap();
    let cell_req = serde_json::json!({
        "id": "sd-cell-2",
        "method": "signData",
        "params": [serde_json::json!({
            "type": "cell",
            "schema": "text_comment#00000000 text:SnakeData = TextComment;",
            "cell": cell_boc
        }).to_string()]
    })
    .to_string();
    bridge.push_event(BridgeEvent {
        id: Some("ev-2".to_owned()),
        from: dapp_session.wallet_client_id(),
        message: dapp_session.encrypt_message(cell_req.as_bytes()).unwrap(),
    });
    assert_eq!(backend.poll_ton_connect_events().unwrap(), 1);
    assert_eq!(backend.pending_sign_data().unwrap().request.kind, "cell");
    let cell_sig = backend
        .approve_ton_connect_sign_data()
        .expect("approve cell signData");
    assert!(verify_ton_connect_data(&pubkey, &cell_sig).is_ok());

    // 1c. Binary signData: user clicks Decline -> sends UserDeclined without signing.
    let bin_req = serde_json::json!({
        "id": "sd-bin-3",
        "method": "signData",
        "params": [serde_json::json!({
            "type": "binary",
            "bytes": "AQIDBA=="
        }).to_string()]
    })
    .to_string();
    bridge.push_event(BridgeEvent {
        id: Some("ev-3".to_owned()),
        from: dapp_session.wallet_client_id(),
        message: dapp_session.encrypt_message(bin_req.as_bytes()).unwrap(),
    });
    assert_eq!(backend.poll_ton_connect_events().unwrap(), 1);
    assert_eq!(backend.pending_sign_data().unwrap().request.kind, "binary");
    let before_decline = bridge.sent_messages().len();
    backend.decline_ton_connect_sign_data();
    assert!(backend.pending_sign_data().is_none());
    let after_decline = bridge.sent_messages();
    assert_eq!(after_decline.len(), before_decline + 1);
    let err_plain = dapp_session
        .decrypt_message(&after_decline.last().unwrap().2)
        .unwrap();
    let err_json: serde_json::Value = serde_json::from_slice(&err_plain).unwrap();
    assert_eq!(err_json["error"]["code"], 300); // UserDeclined
}

#[test]
fn wallet_live_emulate_message_populates_deltas_fees_and_warnings() {
    let dest = TonAddress::new(0, [0x74; 32]);
    let wallet_placeholder = format!("0:{}", "aa".repeat(32));
    let canned_emul = EmulationResult {
        mc_block_seqno: Some(50_000_001),
        root_tx_hash: "root_emul_hash".to_owned(),
        root_account: wallet_placeholder.clone(),
        root_fee_nano: 6_250_000,
        total_fees_nano: 6_250_000,
        total_gas_fees_nano: 4_100_000,
        total_storage_fees_nano: 350_000,
        total_fwd_fees_nano: 1_800_000,
        is_incomplete: false,
        trace_succeeded: true,
        transactions: Vec::new(),
        account_deltas: vec![AccountBalanceDelta {
            account: wallet_placeholder.clone(),
            ton_delta_nano: -1_506_250_000,
            jetton_deltas: vec![JettonDelta {
                jetton_master: TonAddress::new(0, [0xD1; 32]).to_raw(),
                jetton_wallet: None,
                delta: 9_750_000,
                symbol: Some("USDt".to_owned()),
                decimals: Some(6),
            }],
            nft_deltas: Vec::new(),
        }],
        warnings: vec![EmulationWarning::MessageBounced {
            tx_hash: "child_tx".to_owned(),
            account: dest.to_raw(),
            source: Some(wallet_placeholder.clone()),
            destination: Some(dest.to_raw()),
            value_nano: 100_000_000,
        }],
        raw_actions: vec![SendEmulationAction {
            kind: "ton_transfer".to_owned(),
            succeeded: true,
            details_json: serde_json::json!({
                "source": wallet_placeholder,
                "destination": dest.to_raw(),
                "value": "1500000000"
            })
            .to_string(),
        }],
    };

    let ConnectedWalletFixture {
        mut backend,
        toncenter,
        bridge,
        dapp_session,
        ..
    } = setup_connected_live_wallet(Some(canned_emul));

    let tx_req = serde_json::json!({
        "id": "tx-emul-1",
        "method": "sendTransaction",
        "params": [serde_json::json!({
            "valid_until": 1_900_000_000u64,
            "messages": [{
                "address": dest.to_friendly(true, true),
                "amount": "1500000000"
            }]
        }).to_string()]
    })
    .to_string();
    bridge.push_event(BridgeEvent {
        id: Some("ev-tx-1".to_owned()),
        from: dapp_session.wallet_client_id(),
        message: dapp_session.encrypt_message(tx_req.as_bytes()).unwrap(),
    });
    assert_eq!(backend.poll_ton_connect_events().unwrap(), 1);
    assert_eq!(toncenter.emulate_calls.lock().unwrap().len(), 1);
    assert!(
        toncenter.sent_bocs.lock().unwrap().is_empty(),
        "emulation must NEVER broadcast a real transaction"
    );

    let pending = backend
        .pending_tx_approval()
        .expect("pending tx approval with emulation");
    assert_eq!(pending.estimated_fee_nano, 6_250_000);
    let emul = pending.emulation.expect("EmulationResult attached");
    assert_eq!(emul.total_gas_fees_nano, 4_100_000);
    assert_eq!(emul.warnings.len(), 1);
    assert_eq!(emul.account_deltas[0].ton_delta_nano, -1_506_250_000);
    assert_eq!(emul.account_deltas[0].jetton_deltas[0].delta, 9_750_000);
    assert!(pending.ton_connect_emulation.is_some());
}

#[test]
fn wallet_shamir_backup_and_recovery_ui_roundtrip() {
    let backend = MockWalletBackend::with_demo_wallet();
    let original_words = backend.reveal_phrase().expect("demo phrase");
    assert_eq!(original_words.len(), 24);

    let mut ui_state = BackupUiState {
        threshold: 2,
        total_shares: 3,
        ..BackupUiState::default()
    };
    let count = ui_state
        .generate_from_words(&original_words)
        .expect("split 2-of-3");
    assert_eq!(count, 3);
    assert!(
        ui_state.generated_shares[0]
            .encoded
            .starts_with("tgshare1:")
    );

    // Reconstruct using shares #1 and #3 (any 2-of-3 subset).
    ui_state.tab = BackupTab::Recover;
    ui_state.recover_inputs = vec![
        ui_state.generated_shares[0].encoded.clone(),
        ui_state.generated_shares[2].encoded.clone(),
    ];
    let recovered = ui_state.try_reconstruct().expect("reconstruct 2-of-3");
    assert_eq!(recovered, original_words);

    // Corrupted share must be rejected by checksum verification.
    let mut tampered = ui_state.generated_shares[0].encoded.clone();
    tampered.push('X');
    ui_state.recover_inputs = vec![tampered, ui_state.generated_shares[1].encoded.clone()];
    assert!(ui_state.try_reconstruct().is_err());

    // Single share below threshold must fail.
    ui_state.recover_inputs = vec![ui_state.generated_shares[0].encoded.clone()];
    assert!(ui_state.try_reconstruct().is_err());
}

// ============================================================================
// 2. tg-tgw (WP-47): WidgetStorageDisk + WidgetMeter verdicts in live widgets
// ============================================================================

#[test]
fn widgets_live_disk_storage_and_meter_verdicts_enforce_budget() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("rt");
    let rpc = Wire1FakeRpc::new();
    let self_uid = DataUserId::from_tl(100_001);
    let bot_uid = DataUserId::from_tl(880_001);
    let bot_peer = DataPeerId::from_user(bot_uid);
    enqueue_startup_responses(&rpc, 100_001);

    let runtime = Arc::new(
        SessionRuntime::start_on(
            self_uid,
            ErasedRpcClient::new(rpc.clone()),
            rt.handle().clone(),
            SessionRuntimeConfig::default(),
        )
        .expect("start runtime"),
    );
    rt.block_on(async {
        tokio::time::timeout(Duration::from_secs(15), runtime.wait_ready())
            .await
            .expect("wait_ready timeout")
            .expect("startup ready");
        let mut s = runtime.session().write().await;
        let mut bot = DataUser::new(bot_uid, false);
        bot.first_name = "Widget Bot".to_owned();
        bot.access_hash = 0x1234_5678_9ABC;
        s.upsert_user(bot, PeerUpdateFlags::empty());

        let mut msg = Message::new_text(
            bot_peer,
            DataMsgId(42),
            Some(bot_peer),
            now_unix_i32(),
            tg_data::TextWithEntities::plain("Live widget"),
        );
        msg.content = MessageContent::Regular {
            text: tg_data::TextWithEntities::plain("Live widget"),
            media: MessageMedia::Widget(Box::new(WidgetData {
                widget_id: Some(77_001),
                width: 320,
                height: 200,
                payload: pack_payload_zlib(b"<title>Wire1</title>"),
                is_copy: false,
            })),
        };
        s.insert_message(msg);
    });
    rpc.clear_recorded();

    let live = LiveSession::new(runtime.clone(), 701);
    let session_arc = runtime.session();
    let peer_id = PeerId(bot_peer.0);
    let msg_id = MsgId(42);

    // 2a. Attach WidgetStorageDisk and exercise write/read/height/flush/prune/clear.
    attach_widget_disk_storage(
        session_arc,
        dir.path(),
        DiskStorageConfig {
            owner: 100,
            ..DiskStorageConfig::default()
        },
    )
    .expect("attach disk storage");

    let mut area: StorageArea = BTreeMap::new();
    area.insert("theme".to_owned(), "night".to_owned());
    area.insert("counter".to_owned(), "7".to_owned());
    let meta = StorageMeta {
        expires: 2_000_000_000,
        ..StorageMeta::default()
    };
    let write_out = widget_storage_write_live(session_arc, peer_id, msg_id, meta, &area)
        .expect("write local storage");
    assert!(write_out.changed);
    let loaded = widget_storage_read_live(session_arc, peer_id, msg_id)
        .expect("read local storage")
        .expect("persisted area");
    assert_eq!(loaded.get("counter").map(String::as_str), Some("7"));

    let height = HeightRecord {
        height: 220.0,
        ..HeightRecord::default()
    };
    assert!(widget_storage_set_height_live(session_arc, peer_id, msg_id, meta, height).unwrap());
    widget_storage_flush_live(session_arc).unwrap();
    assert_eq!(
        widget_storage_height_live(session_arc, peer_id, msg_id).map(|h| h.height),
        Some(220.0)
    );
    assert_eq!(
        widget_storage_prune_expired_live(session_arc, 2_000_000_100).unwrap(),
        1
    );
    assert!(
        widget_storage_read_live(session_arc, peer_id, msg_id)
            .unwrap()
            .is_none()
    );

    // Re-write and verify clear_chat removes entries.
    widget_storage_write_live(session_arc, peer_id, msg_id, meta, &area).unwrap();
    assert_eq!(
        widget_storage_clear_chat_live(session_arc, peer_id).unwrap(),
        1
    );

    // 2b. Exercise WidgetMeter verdicts blocking live widget RPCs when suspended.
    widget_meter_configure_live(session_arc, peer_id, msg_id, MeterConfig::default());
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();
    assert_eq!(
        widget_meter_verdict_live(session_arc, peer_id, msg_id, now_secs),
        MeterVerdict::Allow
    );
    // Single 1.5s frame breaches hard raster timeout -> Suspend!
    let verdict = widget_meter_record_frame_live(session_arc, peer_id, msg_id, now_secs, 1.5);
    assert!(verdict.is_suspended());

    // Live query & public-data poll must be blocked BEFORE hitting RPC.
    let rpc_before = rpc.recorded().len();
    let outcome = widget_query_live(&live, peer_id, msg_id, b"{}", false).unwrap();
    assert_eq!(outcome.failure, Some(WidgetRequestFailure::Failed));
    let poll_err = widget_poll_public_data_live(&live, peer_id, msg_id, 0);
    assert!(poll_err.is_err());
    assert_eq!(
        rpc.recorded().len(),
        rpc_before,
        "suspended widget must not invoke RPCs"
    );

    // Reset clears suspension.
    widget_meter_reset_live(session_arc, peer_id, msg_id);
    assert_eq!(
        widget_meter_verdict_live(session_arc, peer_id, msg_id, now_secs),
        MeterVerdict::Allow
    );
}

// ============================================================================
// 3. tg-platform (WP-48): caBLE scanner + widget sandbox host + autostart
// ============================================================================

#[test]
fn platform_cable_scanner_wired_in_passkeys_and_intro() {
    // 3a. LivePasskeysHandle with FakeWebAuthnProvider + FakeCableBleScanner.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("rt");
    let rpc = Wire1FakeRpc::new();
    enqueue_startup_responses(&rpc, 100_002);
    let opts_json = serde_json::json!({
        "publicKey": {
            "challenge": "AQIDBAUGBwgJCgsMDQ4PEA",
            "rp": { "id": "telegram.org", "name": "Telegram" },
            "user": { "id": "AQID", "name": "demo", "displayName": "Demo" },
            "pubKeyCredParams": [{ "type": "public-key", "alg": -7 }],
            "timeout": 60000
        }
    })
    .to_string();
    rpc.enqueue_ok(
        tl_fns::account::InitPasskeyRegistration::CONSTRUCTOR_ID,
        &tl_enums::account::PasskeyRegistrationOptions::PasskeyRegistrationOptions(
            tl_types::account::PasskeyRegistrationOptions {
                options: tl_enums::DataJSON::DataJSON(tl_types::DataJSON { data: opts_json }),
            },
        ),
    );

    let runtime = Arc::new(
        SessionRuntime::start_on(
            DataUserId::from_tl(100_002),
            ErasedRpcClient::new(rpc.clone()),
            rt.handle().clone(),
            SessionRuntimeConfig::default(),
        )
        .expect("start passkey runtime"),
    );
    rt.block_on(async {
        tokio::time::timeout(Duration::from_secs(15), runtime.wait_ready())
            .await
            .expect("wait_ready timeout")
            .expect("startup ready");
    });

    let bridge = LiveRuntimeBridge::spawn(&runtime, 702);
    let fake_webauthn = Arc::new(FakeWebAuthnProvider::new_supported());
    let handle = LivePasskeysHandle::with_provider(runtime, &bridge, fake_webauthn);

    // Bluetooth unavailable -> NoBluetooth status.
    let no_bt = Arc::new(FakeCableBleScanner::new(false));
    handle.set_cable_scanner(no_bt);
    let qr_uri = handle
        .start_hybrid_qr_registration()
        .expect("start hybrid qr");
    assert!(qr_uri.starts_with("FIDO:/"));
    assert_eq!(
        handle.hybrid_qr_status(),
        PasskeyHybridQrStatus::NoBluetooth
    );
    handle.cancel_hybrid_qr();
    assert_eq!(handle.hybrid_qr_status(), PasskeyHybridQrStatus::Idle);

    // Bluetooth available -> Scanning.
    let scanner = Arc::new(FakeCableBleScanner::new(true));
    handle.set_cable_scanner(scanner.clone());
    let _qr_uri2 = handle
        .start_hybrid_qr_registration()
        .expect("start hybrid qr 2");
    assert_eq!(handle.hybrid_qr_status(), PasskeyHybridQrStatus::Scanning);
    assert!(scanner.is_running());

    // Verify MockSettingsBackend also exercises the real caBLE scanner match path.
    let mut mock_settings = MockSettingsBackend::new();
    let mock_scanner = Arc::new(FakeCableBleScanner::new(true));
    mock_settings.set_cable_scanner(mock_scanner.clone());
    let mock_uri = mock_settings.start_passkey_hybrid_qr().unwrap();
    assert!(mock_uri.starts_with("FIDO:/"));
    assert_eq!(
        mock_settings.passkey_hybrid_qr_status(),
        PasskeyHybridQrStatus::Scanning
    );
    let eid_key = mock_settings.active_cable_eid_key().unwrap();
    // Construct 16-byte plaintext EID: [0x00, nonce(10), routing_id(3), domain_le(2)=0 -> cable.ua5v.com].
    let mut eid = [0u8; 16];
    eid[1..11].copy_from_slice(&[7u8; 10]);
    eid[11..14].copy_from_slice(&[1, 2, 3]);
    eid[14..16].copy_from_slice(&0u16.to_le_bytes());
    let advert = cable_encrypt_advert(&eid, &eid_key);
    mock_scanner.emit_advertisement(&BleAdvertisement {
        sections: vec![build_cable_service_data_section(
            CableServiceUuid::Fido,
            &advert,
        )],
        bluetooth_address: None,
        rssi: Some(-52),
    });
    assert_eq!(
        mock_settings.passkey_hybrid_qr_status(),
        PasskeyHybridQrStatus::Connecting {
            tunnel_domain: "cable.ua5v.com".to_owned()
        }
    );
    assert!(
        !mock_scanner.is_running(),
        "scanner must stop automatically after first authenticated match"
    );

    // 3b. IntroWidget passkey hybrid login wires FakeCableBleScanner.
    let intro_rpc = Wire1FakeRpc::new();
    let login_opts_json = serde_json::json!({
        "publicKey": {
            "challenge": "AQIDBAUGBwgJCgsMDQ4PEA",
            "rpId": "telegram.org",
            "timeout": 60000
        }
    })
    .to_string();
    intro_rpc.enqueue_ok(
        tl_fns::auth::InitPasskeyLogin::CONSTRUCTOR_ID,
        &tl_enums::auth::PasskeyLoginOptions::PasskeyLoginOptions(
            tl_types::auth::PasskeyLoginOptions {
                options: tl_enums::DataJSON::DataJSON(tl_types::DataJSON {
                    data: login_opts_json,
                }),
            },
        ),
    );
    let mut intro = IntroWidget::with_rpc_client(
        ApiCredentials {
            api_id: 1,
            api_hash: "test".to_owned(),
        },
        intro_rpc,
    );
    intro.set_passkey_provider(Arc::new(FakeWebAuthnProvider::new_supported()));
    let intro_scanner = Arc::new(FakeCableBleScanner::new(true));
    intro.set_cable_scanner(intro_scanner.clone());
    let login_qr = intro
        .start_passkey_hybrid_login(Instant::now())
        .expect("start intro hybrid login");
    assert!(login_qr.starts_with("FIDO:/"));
    assert_eq!(
        intro.passkey_hybrid_login_status(),
        PasskeyHybridQrStatus::Scanning
    );
    assert!(intro_scanner.is_running());
    intro.cancel_passkey_hybrid_login();
    assert!(!intro_scanner.is_running());
    assert_eq!(
        intro.passkey_hybrid_login_status(),
        PasskeyHybridQrStatus::Idle
    );
}

#[test]
fn platform_widget_sandbox_host_launch_ipc_and_meter_suspend() {
    let launcher = Arc::new(FakeWidgetSandboxLauncher::new());
    let mut host = LiveWidgetHost::new(PeerId(10), MsgId(99), b"<html>widget</html>", false)
        .with_sandbox_launcher(launcher.clone());
    assert!(host.is_sandbox_supported());

    let cfg = WidgetSandboxConfig::for_executable(PathBuf::from(r"C:\fake\rustgram-widget.exe"));
    let pid = host
        .launch_sandbox_helper(&cfg)
        .expect("launch fake helper");
    assert_eq!(host.sandbox_pid(), Some(pid));
    assert_eq!(host.state(), RuntimeState::Live);

    // Framed IPC round-trip with helper peer channel.
    let peer = launcher.take_peer_channel(pid).expect("peer channel");
    host.send_sandbox_frame(b"ping-from-host").unwrap();
    assert_eq!(peer.recv_frame().unwrap(), b"ping-from-host");
    peer.send_frame(b"pong-from-helper").unwrap();
    assert_eq!(host.recv_sandbox_frame().unwrap(), b"pong-from-helper");

    // Hard frame timeout -> MeterVerdict::Suspend terminates sandbox helper cleanly.
    let verdict = host.record_frame_and_enforce(5.0, 1.5);
    assert!(verdict.is_suspended());
    assert_eq!(host.sandbox_pid(), None);
    assert_eq!(host.state(), RuntimeState::Failed);
}

#[test]
fn platform_settings_advanced_autostart_provider_states() {
    let fake_entry = AutostartEntry::for_executable(PathBuf::from(r"C:\fake\rustgram.exe"));
    let provider = Arc::new(FakeAutostartProvider::new(fake_entry));

    let mut state = AdvancedState::default();
    state.set_autostart_provider(provider.clone());
    assert_eq!(state.autostart_state, StartupTaskState::Disabled);

    let mut core = CoreSettings::default();
    let mut global = GlobalConfig::default();
    let mut debug = DebugOptions::default();
    let update = UpdateSnapshot::default();
    let mut backend = MockSettingsBackend::new();
    let colors = LiveThemeColors::default();
    let layout = ColumnLayout {
        filters_width: 0,
        body_width: 900,
        dialogs_width: 300,
        chat_width: 600,
        third_width: 0,
        window_layout: WindowLayout::Normal,
        chat_layout: ChatLayout::Normal,
        dialogs_small_column: false,
    };

    // Enable -> probe syncs UI state cleanly without touching Windows Registry.
    provider.set_state(StartupTaskState::Enabled);
    state.platform_probed = false;
    let ctx = egui::Context::default();
    let mut out1 = ctx.run_ui(egui::RawInput::default(), |ui| {
        egui::CentralPanel::default().show(ui, |ui| {
            let _ = render_advanced_section(
                ui,
                &mut state,
                &mut core,
                &mut global,
                &mut debug,
                &update,
                ConnectionStatus::Connected,
                layout,
                &mut backend,
                &colors,
            );
        });
    });
    out1.textures_delta.clear();
    assert!(global.auto_start);
    assert_eq!(state.autostart_state, StartupTaskState::Enabled);

    // DisabledByPolicy locks the toggle.
    provider.set_state(StartupTaskState::DisabledByPolicy);
    state.platform_probed = false;
    let mut out2 = ctx.run_ui(egui::RawInput::default(), |ui| {
        egui::CentralPanel::default().show(ui, |ui| {
            let _ = render_advanced_section(
                ui,
                &mut state,
                &mut core,
                &mut global,
                &mut debug,
                &update,
                ConnectionStatus::Connected,
                layout,
                &mut backend,
                &colors,
            );
        });
    });
    out2.textures_delta.clear();
    assert!(!global.auto_start);
    assert!(!state.autostart_state.can_toggle());
}

// ============================================================================
// Headless wgpu Before / After Screenshot Capture (1440x900 & 390x844)
// ============================================================================

#[test]
#[ignore = "manual screenshot capture for wave 9 design before/after review"]
fn capture_wire1_before_screenshots() {
    capture_wire1_screenshots("before");
}

#[test]
#[ignore = "manual screenshot capture for wave 9 design before/after review"]
fn capture_wire1_after_screenshots() {
    capture_wire1_screenshots("after");
}

fn capture_wire1_screenshots(suffix: &str) {
    let _guard = WIRE1_WGPU_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let sizes = [
        ("1440x900", Vec2::new(1440.0, 900.0)),
        ("390x844", Vec2::new(390.0, 844.0)),
    ];

    for (tag, size) in sizes {
        // 1. TON Connect transfer preview + signData confirmation
        let mut panel = WalletPanel::with_mock_demo();
        panel.select_screen(WalletScreen::TonConnect);
        save_widget_png(
            &format!("wallet_ton_connect_{tag}_{suffix}"),
            size,
            panel,
            |ui, panel| {
                let pal = Palette::day();
                let _ = panel.show(ui, &pal, true);
            },
        );

        // 2. Shamir Phrase Backup screen with generated 2-of-3 shares
        let mut panel_backup = WalletPanel::with_mock_demo();
        if let Ok(words) = panel_backup.backend_mut().reveal_phrase() {
            let _ = panel_backup.backup_state_mut().generate_from_words(&words);
        }
        panel_backup.select_screen(WalletScreen::Backup);
        save_widget_png(
            &format!("wallet_shamir_backup_{tag}_{suffix}"),
            size,
            panel_backup,
            |ui, panel| {
                let pal = Palette::day();
                let _ = panel.show(ui, &pal, true);
            },
        );

        // 3. Settings Advanced (autostart toggle with FakeAutostartProvider in DisabledByUser state)
        struct AdvSnap {
            state: AdvancedState,
            core: CoreSettings,
            global: GlobalConfig,
            debug: DebugOptions,
            update: UpdateSnapshot,
            backend: MockSettingsBackend,
        }
        let fake_autostart = Arc::new(FakeAutostartProvider::new(AutostartEntry::for_executable(
            PathBuf::from(r"C:\Program Files\Rustgram\rustgram.exe"),
        )));
        fake_autostart.set_state(StartupTaskState::DisabledByUser);
        let mut adv_state = AdvancedState::default();
        adv_state.set_autostart_provider(fake_autostart);
        adv_state.platform_probed = true;
        let adv = AdvSnap {
            state: adv_state,
            core: CoreSettings::default(),
            global: GlobalConfig::default(),
            debug: DebugOptions::default(),
            update: UpdateSnapshot::default(),
            backend: MockSettingsBackend::new(),
        };
        let col_layout = ColumnLayout {
            filters_width: 0,
            body_width: size.x as i32,
            dialogs_width: 300,
            chat_width: (size.x as i32 - 300).max(380),
            third_width: 0,
            window_layout: if size.x < 680.0 {
                WindowLayout::OneColumn
            } else {
                WindowLayout::Normal
            },
            chat_layout: ChatLayout::Normal,
            dialogs_small_column: false,
        };
        save_widget_png(
            &format!("settings_advanced_{tag}_{suffix}"),
            size,
            adv,
            move |ui, s| {
                let colors = LiveThemeColors::default();
                let scroll_y = if size.x < 680.0 { 560.0 } else { 460.0 };
                ScrollArea::vertical()
                    .vertical_scroll_offset(scroll_y)
                    .show(ui, |ui| {
                        let _ = render_advanced_section(
                            ui,
                            &mut s.state,
                            &mut s.core,
                            &mut s.global,
                            &mut s.debug,
                            &s.update,
                            ConnectionStatus::Connected,
                            col_layout,
                            &mut s.backend,
                            &colors,
                        );
                    });
            },
        );

        // 4. Passkeys hybrid QR modal with live caBLE BLE proximity scanner status
        struct PkSnap {
            state: PasskeysState,
            backend: MockSettingsBackend,
        }
        let mut backend = MockSettingsBackend::new();
        let qr_uri = backend
            .start_passkey_hybrid_qr()
            .unwrap_or_else(|_| "FIDO:/012345678901234567890123456789".to_owned());
        let mut pk_state = PasskeysState::default();
        pk_state.loaded = true;
        pk_state.modal = PasskeysModal::HybridQr { qr_uri };
        let pk = PkSnap {
            state: pk_state,
            backend,
        };
        save_widget_png(
            &format!("passkeys_hybrid_qr_{tag}_{suffix}"),
            size,
            pk,
            |ui, s| {
                let colors = LiveThemeColors::default();
                ScrollArea::vertical().show(ui, |ui| {
                    let _ = render_passkeys_section(ui, &mut s.state, &mut s.backend, &colors);
                });
            },
        );
    }
}
