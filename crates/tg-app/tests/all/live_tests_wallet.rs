//! Live TON Wallet backend, `help.getAppConfig` sync, deep-link routing, and DPAPI key
//! protection tests against strictly local in-process fake Toncenter & TON Connect bridge servers.
//!
//! Never moves real funds and never contacts any real TON network or bridge.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use egui::{Context, Pos2, RawInput, Rect, Vec2};
use serde_json::{Value, json};
use tempfile::TempDir;
use tg_api::{DcId, InvocationError, RpcClient};
use tg_data::UserId as DataUserId;
use tg_platform::wallet_protection::{FakeWalletKeyProtector, WalletKeyProtector};
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tg_wallet::address::{TonAddress, build_transfer_link};
use tg_wallet::contract::WalletVersion;
use tg_wallet::toncenter::{AccountState, BoxFuture};
use tg_wallet::tonconnect::{
    BridgeEvent, DappManifest, HttpTonConnectBridge, TonConnectBridge, TonConnectBridgeConfig,
    TonConnectKeypair, TonConnectSession,
};
use tg_wallet::vault::PasscodeKdf;
use tg_wallet::{
    DEFAULT_OLD_WALLET_BOT_USERNAME, TRANSFER_MIN_NANOS_DEFAULT, TRANSFER_MIN_NANOS_MAX,
    WalletAppConfig, WalletError,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::broadcast;

use tg_app::live::LiveSession;
use tg_app::live::host::LiveActiveWorkspace;
use tg_app::live::wallet::{
    LiveWalletBackend, parse_wallet_app_config_from_tl, transfer_min_nanos_from_config,
    wallet_dir_for_account,
};
use tg_app::stand_in::settings::ShellSettings;
use tg_app::wallet::backend::WalletBackend;
use tg_app::wallet::{AppWalletHost, WalletScreen};
use tg_app::window::deep_links::{
    DeepLink, DeepLinkOutcome, DeepLinkOverlayRequest, WalletDeepLinkAction, parse_deep_link,
};

/// Public TON spec test phrase (synthetic test vector; zero real funds).
const TEST_MNEMONIC: [&str; 24] = [
    "hospital", "stove", "relief", "fringe", "tongue", "always", "charge", "angry", "urge",
    "sentence", "again", "match", "nerve", "inquiry", "senior", "coconut", "label", "tumble",
    "carry", "category", "beauty", "bean", "road", "solution",
];

fn test_words() -> Vec<String> {
    TEST_MNEMONIC.iter().map(|w| (*w).to_owned()).collect()
}

// ============================================================================
// Local in-process Fake Toncenter HTTP Server
// ============================================================================

struct FakeToncenterState {
    sent_bocs: Mutex<Vec<String>>,
    balance_nano: Mutex<u64>,
    seqno: Mutex<u64>,
    emulated_fee: Mutex<i64>,
}

async fn spawn_local_fake_toncenter() -> (String, Arc<FakeToncenterState>) {
    let state = Arc::new(FakeToncenterState {
        sent_bocs: Mutex::new(Vec::new()),
        balance_nano: Mutex::new(2_500_000_000),
        seqno: Mutex::new(7),
        emulated_fee: Mutex::new(4_200_000),
    });

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake toncenter");
    let addr = listener.local_addr().expect("local addr");
    let state_clone = Arc::clone(&state);

    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let st = Arc::clone(&state_clone);
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
                            "last_transaction_id": { "lt": "42000001", "hash": "last_hash_1" }
                        }
                    })
                    .to_string()
                } else if first_line.contains("/api/v2/getTransactions") {
                    json!({
                        "ok": true,
                        "result": [
                            {
                                "transaction_id": { "lt": "42000001", "hash": "tx_fake_1" },
                                "utime": 1731900000,
                                "fee": "4100000",
                                "in_msg": {
                                    "source": "UQCdqXGvONLwOr3zCNX5FjapflorB6ZsOdcdfLrjsDLt3AF4",
                                    "value": "2500000000",
                                    "message": "Local faucet"
                                },
                                "out_msgs": []
                            }
                        ]
                    })
                    .to_string()
                } else if first_line.contains("/api/v2/runGetMethod") {
                    let seq = *st.seqno.lock().expect("seq");
                    json!({
                        "ok": true,
                        "result": { "exit_code": 0, "stack": [["num", format!("0x{seq:x}")]] }
                    })
                    .to_string()
                } else if first_line.contains("/api/emulate/v1/emulateTrace") {
                    let fee = *st.emulated_fee.lock().expect("fee");
                    json!({
                        "mc_block_seqno": 99,
                        "trace": { "tx_hash": "root" },
                        "transactions": {
                            "root": {
                                "account": "0:1111111111111111111111111111111111111111111111111111111111111111",
                                "total_fees": fee.to_string(),
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
                        "result": { "@type": "ok", "hash": "fake_sent_hash_01" }
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
// Local in-process Fake TON Connect HTTP/SSE Bridge + Manifest Server
// ============================================================================

struct FakeBridgeState {
    /// Messages posted by the wallet (`(query_string, encrypted_body)`).
    received_from_wallet: Mutex<Vec<(String, String)>>,
    /// Queued SSE events waiting to be delivered on `GET /bridge/events`.
    queued_events: Mutex<VecDeque<(String, String, String)>>,
    manifest: DappManifest,
}

async fn spawn_local_fake_bridge(manifest: DappManifest) -> (String, Arc<FakeBridgeState>) {
    let state = Arc::new(FakeBridgeState {
        received_from_wallet: Mutex::new(Vec::new()),
        queued_events: Mutex::new(VecDeque::new()),
        manifest,
    });

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake bridge");
    let addr = listener.local_addr().expect("bridge addr");
    let state_clone = Arc::clone(&state);

    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let st = Arc::clone(&state_clone);
            tokio::spawn(async move {
                let mut buf = vec![0_u8; 32768];
                let n = stream.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                let first_line = req.lines().next().unwrap_or("");
                let path_and_query = first_line.split_whitespace().nth(1).unwrap_or("");
                let body = req.split("\r\n\r\n").nth(1).unwrap_or("").to_owned();

                if first_line.starts_with("POST /bridge/message") {
                    let query = path_and_query.split_once('?').map_or("", |(_, q)| q);
                    st.received_from_wallet
                        .lock()
                        .expect("recv")
                        .push((query.to_owned(), body));
                    let resp =
                        "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nOK";
                    let _ = stream.write_all(resp.as_bytes()).await;
                } else if first_line.starts_with("GET /bridge/events") {
                    let mut sse = String::new();
                    {
                        let mut q = st.queued_events.lock().expect("queue");
                        while let Some((ev_id, from, msg)) = q.pop_front() {
                            sse.push_str(&format!(
                                "id: {ev_id}\ndata: {}\n\n",
                                json!({ "from": from, "message": msg })
                            ));
                        }
                    }
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        sse.len(),
                        sse
                    );
                    let _ = stream.write_all(resp.as_bytes()).await;
                } else if first_line.starts_with("GET /manifest.json") {
                    let json_body = serde_json::to_string(&st.manifest).unwrap_or_default();
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        json_body.len(),
                        json_body
                    );
                    let _ = stream.write_all(resp.as_bytes()).await;
                } else {
                    let resp =
                        "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                    let _ = stream.write_all(resp.as_bytes()).await;
                }
            });
        }
    });

    (format!("http://{addr}"), state)
}

/// Wraps real [`HttpTonConnectBridge`] so `send_message` and `poll_events` hit the local HTTP
/// bridge directly while `fetch_manifest` rewrites `https://...` manifest URLs from `tc://` links
/// to the local HTTP fake server's `/manifest.json` (never touching external hosts).
struct LocalManifestRewritingBridge {
    inner: HttpTonConnectBridge,
    local_manifest_url: String,
}

impl TonConnectBridge for LocalManifestRewritingBridge {
    fn send_message<'a>(
        &'a self,
        from_client_id: &'a str,
        to_client_id: &'a str,
        ttl_secs: u32,
        encrypted_base64: &'a str,
    ) -> BoxFuture<'a, Result<(), WalletError>> {
        self.inner
            .send_message(from_client_id, to_client_id, ttl_secs, encrypted_base64)
    }

    fn poll_events<'a>(
        &'a self,
        client_id: &'a str,
        last_event_id: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Vec<BridgeEvent>, WalletError>> {
        self.inner.poll_events(client_id, last_event_id)
    }

    fn fetch_manifest<'a>(
        &'a self,
        _manifest_url: &'a str,
    ) -> BoxFuture<'a, Result<DappManifest, WalletError>> {
        let local_url = self.local_manifest_url.clone();
        Box::pin(async move { self.inner.fetch_manifest(&local_url).await })
    }
}

// ============================================================================
// Fake Telegram RPC client for `help.getAppConfig`
// ============================================================================

type QueuedResp = (u32, Result<Vec<u8>, InvocationError>);

#[derive(Clone)]
struct WalletFakeRpc {
    responses: Arc<Mutex<VecDeque<QueuedResp>>>,
    updates_tx: broadcast::Sender<tl_enums::Updates>,
}

impl WalletFakeRpc {
    fn new() -> Self {
        let (updates_tx, _) = broadcast::channel(16);
        Self {
            responses: Arc::new(Mutex::new(VecDeque::new())),
            updates_tx,
        }
    }

    fn enqueue_ok<T: Serializable>(&self, ctor: u32, resp: &T) {
        self.responses
            .lock()
            .expect("responses")
            .push_back((ctor, Ok(resp.to_bytes())));
    }
}

impl RpcClient for WalletFakeRpc {
    type Error = InvocationError;

    async fn invoke<R: RemoteCall + Send + Sync>(&self, req: &R) -> Result<R::Return, Self::Error> {
        self.invoke_in_dc(2, req).await
    }

    async fn invoke_in_dc<R: RemoteCall + Send + Sync>(
        &self,
        _dc_id: DcId,
        req: &R,
    ) -> Result<R::Return, Self::Error> {
        let raw = req.to_bytes();
        let ctor = if raw.len() >= 4 {
            u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]])
        } else {
            0
        };
        let mut q = self.responses.lock().expect("responses");
        if let Some(pos) = q.iter().position(|(c, _)| *c == ctor) {
            let (_, res) = q.remove(pos).expect("queued");
            let bytes = res?;
            R::Return::from_bytes(&bytes).map_err(|e| InvocationError::Deserialize(e.to_string()))
        } else {
            Err(InvocationError::Rpc(tg_api::RpcError {
                code: 400,
                name: format!("UNMOCKED_RPC_{ctor:#010x}"),
                value: None,
            }))
        }
    }

    fn main_dc_id(&self) -> DcId {
        2
    }

    fn subscribe_updates(&self) -> Option<broadcast::Receiver<tl_enums::Updates>> {
        Some(self.updates_tx.subscribe())
    }
}

fn build_live_session(rpc: WalletFakeRpc, handle: tokio::runtime::Handle) -> LiveSession {
    let cfg = SessionRuntimeConfig {
        init_updates_state: false,
        run_get_difference_on_start: false,
        load_initial_dialogs: false,
        fetch_self_user_on_start: false,
        tick_interval: Duration::from_secs(3600),
        ..SessionRuntimeConfig::default()
    };
    let runtime = Arc::new(
        SessionRuntime::start_on(
            DataUserId(5001),
            tg_app::live::ErasedRpcClient::new(rpc),
            handle,
            cfg,
        )
        .expect("start fake runtime"),
    );
    LiveSession::new(runtime, 5001)
}

fn json_kv(key: &str, value: tl_enums::JSONValue) -> tl_enums::JSONObjectValue {
    tl_enums::JSONObjectValue::JsonObjectValue(tl_types::JsonObjectValue {
        key: key.to_owned(),
        value,
    })
}

// ============================================================================
// Tests
// ============================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_wallet_vault_persistence_in_tdata_and_lock_unlock_roundtrip() {
    let tmp = TempDir::new().expect("tempdir");
    let wallet_dir = wallet_dir_for_account(tmp.path(), "D877F783D5D3EF8C");

    let mut backend = LiveWalletBackend::with_dependencies(
        42,
        wallet_dir.clone(),
        tokio::runtime::Handle::current(),
        WalletAppConfig::default(),
        None,
        None,
        Arc::new(FakeWalletKeyProtector::new()),
        PasscodeKdf::fast_test_kdf(),
    );
    assert!(!backend.overview().has_wallet);

    let words = test_words();
    let addr = backend
        .create_wallet(&words, WalletVersion::V5R1, "secret-pass")
        .expect("create wallet");
    assert!(backend.overview().has_wallet);
    assert!(!backend.overview().locked);
    assert_eq!(backend.overview().address, Some(addr));
    assert_eq!(backend.reveal_phrase().expect("reveal"), words);

    // Encrypted files exist inside tdata, and plaintext phrase never appears on disk.
    let keyring_bytes = std::fs::read(wallet_dir.join("keyring.bin")).expect("keyring.bin");
    let records_bytes = std::fs::read(wallet_dir.join("records.bin")).expect("records.bin");
    let meta_bytes = std::fs::read(wallet_dir.join("wallet_meta.json")).expect("wallet_meta.json");
    assert!(!keyring_bytes.is_empty());
    assert!(!records_bytes.is_empty());
    assert!(!meta_bytes.is_empty());
    for blob in [&keyring_bytes, &records_bytes, &meta_bytes] {
        assert!(!blob.windows(8).any(|w| w == b"hospital"));
        assert!(!blob.windows(8).any(|w| w == b"solution"));
    }

    // Reopen from disk: starts locked, retains public address from non-secret metadata.
    let mut reopened = LiveWalletBackend::with_dependencies(
        42,
        wallet_dir,
        tokio::runtime::Handle::current(),
        WalletAppConfig::default(),
        None,
        None,
        Arc::new(FakeWalletKeyProtector::new()),
        PasscodeKdf::fast_test_kdf(),
    );
    assert!(reopened.overview().has_wallet);
    assert!(reopened.overview().locked);
    assert_eq!(reopened.overview().address, Some(addr));
    assert!(reopened.reveal_phrase().is_err());
    assert!(reopened.unlock_vault("wrong-pass").is_err());

    reopened.unlock_vault("secret-pass").expect("unlock");
    assert!(!reopened.overview().locked);
    assert_eq!(
        reopened.reveal_phrase().expect("reveal after reload"),
        words
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_wallet_toncenter_refresh_emulate_and_send_boc_against_local_fake() {
    let (toncenter_url, tc_state) = spawn_local_fake_toncenter().await;
    let tmp = TempDir::new().expect("tempdir");
    let wallet_dir = wallet_dir_for_account(tmp.path(), "ACCOUNT_TONCENTER");

    let cfg = WalletAppConfig {
        toncenter_base_url: toncenter_url,
        ..WalletAppConfig::default()
    };

    let mut backend = LiveWalletBackend::with_dependencies(
        100,
        wallet_dir,
        tokio::runtime::Handle::current(),
        cfg,
        None,
        None,
        Arc::new(FakeWalletKeyProtector::new()),
        PasscodeKdf::fast_test_kdf(),
    );

    let words = test_words();
    backend
        .create_wallet(&words, WalletVersion::V4R2, "pass1234")
        .expect("create wallet");

    // create_wallet automatically refreshed against the configured local fake Toncenter.
    let ov = backend.overview();
    assert_eq!(ov.balance_nano, 2_500_000_000);
    assert_eq!(ov.account_state, AccountState::Active);
    assert_eq!(ov.seqno, 7);
    assert_eq!(ov.history.len(), 1);
    assert_eq!(ov.history[0].id, "tx_fake_1");
    assert_eq!(ov.history[0].comment, "Local faucet");

    // Prepare send -> calls /api/emulate/v1/emulateTrace and returns emulated fee (4_200_000).
    let dest = TonAddress::new(0, [0x77; 32]).to_friendly(false, false);
    let quote = backend
        .prepare_send(&dest, 1_000_000_000, "Test local send", Some(false))
        .expect("prepare send");
    assert_eq!(quote.fee_nano, 4_200_000);

    // Confirm send -> signs V4R2 external BOC and broadcasts via /api/v2/sendBoc.
    let receipt = backend.confirm_send(&quote).expect("confirm send");
    assert_eq!(receipt.message_hash_hex, "fake_sent_hash_01");
    assert!(receipt.signed_boc_base64.starts_with("te6cc"));

    let bocs = tc_state.sent_bocs.lock().expect("bocs").clone();
    assert_eq!(bocs.len(), 1);
    assert_eq!(bocs[0], receipt.signed_boc_base64);

    let after = backend.overview();
    assert_eq!(after.seqno, 8);
    assert_eq!(after.balance_nano, 2_500_000_000 - 1_004_200_000);
    assert_eq!(after.history[0].id, "fake_sent_hash_01");
    assert_eq!(after.history[0].comment, "Test local send");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_wallet_ton_connect_full_flow_and_vault_persistence_against_local_fake_bridge() {
    let (toncenter_url, tc_state) = spawn_local_fake_toncenter().await;
    let manifest = DappManifest {
        url: "https://demoswap.example".to_owned(),
        name: "Local Test dApp".to_owned(),
        icon_url: "https://demoswap.example/icon.png".to_owned(),
    };
    let (bridge_origin, bridge_state) = spawn_local_fake_bridge(manifest.clone()).await;

    let inner_bridge = HttpTonConnectBridge::new(TonConnectBridgeConfig::new(format!(
        "{bridge_origin}/bridge"
    )))
    .expect("http bridge");
    let bridge: Arc<dyn TonConnectBridge> = Arc::new(LocalManifestRewritingBridge {
        inner: inner_bridge,
        local_manifest_url: format!("{bridge_origin}/manifest.json"),
    });

    let tmp = TempDir::new().expect("tempdir");
    let wallet_dir = wallet_dir_for_account(tmp.path(), "ACCOUNT_TC");
    let cfg = WalletAppConfig {
        toncenter_base_url: toncenter_url.clone(),
        tonconnect_bridge_url: format!("{bridge_origin}/bridge"),
        ..WalletAppConfig::default()
    };

    let mut backend = LiveWalletBackend::with_dependencies(
        200,
        wallet_dir.clone(),
        tokio::runtime::Handle::current(),
        cfg.clone(),
        None,
        Some(Arc::clone(&bridge)),
        Arc::new(FakeWalletKeyProtector::new()),
        PasscodeKdf::fast_test_kdf(),
    );
    backend
        .create_wallet(&test_words(), WalletVersion::V5R1, "vaultpass")
        .expect("create wallet");

    // 1. Parse tc:// link -> fetches manifest from local fake server -> queues connect approval.
    let dapp_kp = TonConnectKeypair::generate();
    let tc_link = format!(
        "tc://?v=2&id={}&r=%7B%22manifestUrl%22%3A%22https%3A%2F%2Fdemoswap.example%2Fmanifest.json%22%2C%22items%22%3A%5B%7B%22name%22%3A%22ton_addr%22%7D%2C%7B%22name%22%3A%22ton_proof%22%2C%22payload%22%3A%22local-nonce-99%22%7D%5D%7D",
        dapp_kp.client_id()
    );
    backend
        .handle_ton_connect_link(&tc_link)
        .expect("handle tc link");
    let pending_conn = backend.pending_connect().expect("pending connect");
    assert_eq!(pending_conn.manifest.name, "Local Test dApp");
    assert!(pending_conn.requests_proof);

    // 2. Approve connect -> sends NaCl-encrypted connect event with ton_proof to local bridge.
    let card = backend.approve_ton_connect().expect("approve connect");
    assert_eq!(card.manifest.name, "Local Test dApp");
    assert_eq!(backend.ton_connect_sessions().len(), 1);

    let (connect_query, connect_enc) = {
        let msgs = bridge_state.received_from_wallet.lock().expect("msgs");
        assert_eq!(msgs.len(), 1);
        msgs[0].clone()
    };
    let wallet_client_id = connect_query
        .split('&')
        .find_map(|p| p.strip_prefix("client_id="))
        .expect("wallet client_id in query")
        .to_owned();

    let dapp_session = TonConnectSession::new(dapp_kp.clone(), &wallet_client_id, manifest.clone())
        .expect("dapp session");
    let connect_dec: Value = serde_json::from_slice(
        &dapp_session
            .decrypt_message(&connect_enc)
            .expect("decrypt connect"),
    )
    .expect("json connect");
    assert_eq!(connect_dec["event"], "connect");
    assert_eq!(connect_dec["payload"]["items"][1]["name"], "ton_proof");

    // 3. Restart backend from disk -> unlock -> TON Connect session restored from encrypted vault!
    let mut reloaded = LiveWalletBackend::with_dependencies(
        200,
        wallet_dir,
        tokio::runtime::Handle::current(),
        cfg,
        None,
        Some(Arc::clone(&bridge)),
        Arc::new(FakeWalletKeyProtector::new()),
        PasscodeKdf::fast_test_kdf(),
    );
    assert!(reloaded.ton_connect_sessions().is_empty());
    reloaded.unlock_vault("vaultpass").expect("unlock reloaded");
    reloaded.refresh().expect("refresh reloaded");
    assert_eq!(reloaded.ton_connect_sessions().len(), 1);
    assert_eq!(
        reloaded.ton_connect_sessions()[0].manifest.name,
        "Local Test dApp"
    );

    // 4. Fake dApp pushes encrypted sendTransaction SSE event -> poll -> approve -> Toncenter + Bridge!
    let tx_req_json = json!({
        "id": "req-77",
        "method": "sendTransaction",
        "params": [
            json!({
                "valid_until": 1900000000,
                "network": "-239",
                "messages": [{
                    "address": "UQCdqXGvONLwOr3zCNX5FjapflorB6ZsOdcdfLrjsDLt3AF4",
                    "amount": "500000000",
                    "payload": "te6cckEBAQEADAAAFAAAAABIZWxsbyGVgYQo"
                }]
            }).to_string()
        ]
    });
    let enc_req = dapp_session
        .encrypt_message(&serde_json::to_vec(&tx_req_json).expect("tx json"))
        .expect("encrypt req");
    bridge_state
        .queued_events
        .lock()
        .expect("queue")
        .push_back(("ev-1".to_owned(), dapp_kp.client_id(), enc_req));

    let count = reloaded.poll_ton_connect_events().expect("poll events");
    assert_eq!(count, 1);
    let pending_tx = reloaded.pending_tx_approval().expect("pending tx");
    assert_eq!(pending_tx.request.request_id, "req-77");
    assert_eq!(pending_tx.request.total_nano(), 500_000_000);

    let tx_receipt = reloaded.approve_ton_connect_tx().expect("approve tx");
    assert_eq!(tc_state.sent_bocs.lock().expect("bocs").len(), 1);
    assert!(reloaded.pending_tx_approval().is_none());

    // Fake dApp decrypts tx success response.
    let tx_resp_enc = {
        let msgs = bridge_state.received_from_wallet.lock().expect("msgs");
        assert_eq!(msgs.len(), 2);
        msgs[1].1.clone()
    };
    let tx_resp_dec: Value = serde_json::from_slice(
        &dapp_session
            .decrypt_message(&tx_resp_enc)
            .expect("decrypt tx resp"),
    )
    .expect("json tx resp");
    assert_eq!(tx_resp_dec["id"], "req-77");
    assert_eq!(tx_resp_dec["result"], tx_receipt.signed_boc_base64);

    // 5. Enqueue a second tx request and decline it -> verify UserDeclined (300) reply.
    let tx_req2 = json!({
        "id": "req-78",
        "method": "sendTransaction",
        "params": [
            json!({
                "messages": [{
                    "address": "UQCdqXGvONLwOr3zCNX5FjapflorB6ZsOdcdfLrjsDLt3AF4",
                    "amount": "100000000"
                }]
            }).to_string()
        ]
    });
    let enc_req2 = dapp_session
        .encrypt_message(&serde_json::to_vec(&tx_req2).expect("tx2 json"))
        .expect("encrypt req2");
    bridge_state
        .queued_events
        .lock()
        .expect("queue")
        .push_back(("ev-2".to_owned(), dapp_kp.client_id(), enc_req2));
    assert_eq!(reloaded.poll_ton_connect_events().expect("poll 2"), 1);
    reloaded.decline_ton_connect_tx();
    assert!(reloaded.pending_tx_approval().is_none());

    let decline_enc = {
        let msgs = bridge_state.received_from_wallet.lock().expect("msgs");
        assert_eq!(msgs.len(), 3);
        msgs[2].1.clone()
    };
    let decline_dec: Value = serde_json::from_slice(
        &dapp_session
            .decrypt_message(&decline_enc)
            .expect("decrypt decline"),
    )
    .expect("json decline");
    assert_eq!(decline_dec["id"], "req-78");
    assert_eq!(decline_dec["error"]["code"], 300);

    // 6. Disconnect -> verify disconnect event sent and session removed.
    reloaded
        .disconnect_ton_connect(card.session_id)
        .expect("disconnect");
    assert!(reloaded.ton_connect_sessions().is_empty());
    let disc_enc = {
        let msgs = bridge_state.received_from_wallet.lock().expect("msgs");
        assert_eq!(msgs.len(), 4);
        msgs[3].1.clone()
    };
    let disc_dec: Value = serde_json::from_slice(
        &dapp_session
            .decrypt_message(&disc_enc)
            .expect("decrypt disconnect"),
    )
    .expect("json disconnect");
    assert_eq!(disc_dec["event"], "disconnect");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_wallet_syncs_app_config_flags_from_help_get_app_config() {
    // 1. Validate tdesktop `MinNanosFromConfig` edge cases.
    assert_eq!(transfer_min_nanos_from_config(1.0), 1);
    assert_eq!(transfer_min_nanos_from_config(250_000_000.0), 250_000_000);
    assert_eq!(
        transfer_min_nanos_from_config(TRANSFER_MIN_NANOS_MAX as f64),
        TRANSFER_MIN_NANOS_MAX
    );
    assert_eq!(
        transfer_min_nanos_from_config(0.0),
        TRANSFER_MIN_NANOS_DEFAULT
    );
    assert_eq!(
        transfer_min_nanos_from_config(-5.0),
        TRANSFER_MIN_NANOS_DEFAULT
    );
    assert_eq!(
        transfer_min_nanos_from_config(1.5),
        TRANSFER_MIN_NANOS_DEFAULT
    );
    assert_eq!(
        transfer_min_nanos_from_config(f64::NAN),
        TRANSFER_MIN_NANOS_DEFAULT
    );
    assert_eq!(
        transfer_min_nanos_from_config(f64::INFINITY),
        TRANSFER_MIN_NANOS_DEFAULT
    );
    assert_eq!(
        transfer_min_nanos_from_config((TRANSFER_MIN_NANOS_MAX as f64) + 2.0),
        TRANSFER_MIN_NANOS_DEFAULT
    );

    // 2. Empty object uses exact tdesktop defaults.
    let base = WalletAppConfig {
        toncenter_base_url: "http://127.0.0.1:19999".to_owned(),
        tonconnect_bridge_url: "http://127.0.0.1:19998/bridge".to_owned(),
        ..WalletAppConfig::default()
    };
    let empty_obj = tl_enums::JSONValue::JsonObject(tl_types::JsonObject { value: Vec::new() });
    let (defaults, rate_none) = parse_wallet_app_config_from_tl(&empty_obj, &base);
    assert!(defaults.wallet_available);
    assert_eq!(
        defaults.old_wallet_bot_username,
        DEFAULT_OLD_WALLET_BOT_USERNAME
    );
    assert_eq!(defaults.transfer_min_nanos, TRANSFER_MIN_NANOS_DEFAULT);
    assert_eq!(defaults.toncenter_base_url, "http://127.0.0.1:19999");
    assert!(rate_none.is_none());

    // 3. Full RPC sync via `help.getAppConfig` on `FakeRpcClient`.
    let rpc = WalletFakeRpc::new();
    let cfg_obj = tl_enums::JSONValue::JsonObject(tl_types::JsonObject {
        value: vec![
            json_kv(
                "wallet_available",
                tl_enums::JSONValue::JsonBool(tl_types::JsonBool { value: false }),
            ),
            json_kv(
                "old_wallet_bot_username",
                tl_enums::JSONValue::JsonString(tl_types::JsonString {
                    value: "legacywallet".to_owned(),
                }),
            ),
            json_kv(
                "wallet_transfer_min_nanos",
                tl_enums::JSONValue::JsonNumber(tl_types::JsonNumber {
                    value: 300_000_000.0,
                }),
            ),
            json_kv(
                "ton_usd_rate",
                tl_enums::JSONValue::JsonNumber(tl_types::JsonNumber { value: 6.25 }),
            ),
        ],
    });
    rpc.enqueue_ok(
        tl_fns::help::GetAppConfig::CONSTRUCTOR_ID,
        &tl_enums::help::AppConfig::AppConfig(tl_types::help::AppConfig {
            hash: 98765,
            config: cfg_obj,
        }),
    );
    rpc.enqueue_ok(
        tl_fns::help::GetAppConfig::CONSTRUCTOR_ID,
        &tl_enums::help::AppConfig::NotModified,
    );

    let session = build_live_session(rpc, tokio::runtime::Handle::current());
    let tmp = TempDir::new().expect("tempdir");
    let mut backend = LiveWalletBackend::with_dependencies(
        300,
        tmp.path().join("wallet"),
        tokio::runtime::Handle::current(),
        base,
        None,
        None,
        Arc::new(FakeWalletKeyProtector::new()),
        PasscodeKdf::fast_test_kdf(),
    );

    assert!(backend.app_config().is_available());
    let changed = backend.sync_app_config_now(&session).expect("sync 1");
    assert!(changed);
    assert_eq!(backend.app_config_hash(), 98765);
    assert!(!backend.app_config().is_available());
    assert!(
        backend
            .app_config()
            .is_old_wallet_bot_username("LegacyWallet")
    );
    assert_eq!(backend.app_config().transfer_min_nanos, 300_000_000);
    assert!((backend.overview().usd_per_ton - 6.25).abs() < 1e-9);
    // Endpoints configured locally are preserved across appConfig sync.
    assert_eq!(
        backend.app_config().toncenter_base_url,
        "http://127.0.0.1:19999"
    );

    // Second sync returns `NotModified` -> returns Ok(false) and preserves values.
    let changed_second = backend.sync_app_config_now(&session).expect("sync 2");
    assert!(!changed_second);
    assert!(!backend.app_config().is_available());
}

#[test]
fn deep_links_route_tc_sendgrams_and_ton_transfer_to_app_wallet_host() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("build test runtime");
    let handle = rt.handle().clone();

    let manifest = DappManifest {
        url: "https://connect.example".to_owned(),
        name: "Connect Example".to_owned(),
        icon_url: String::new(),
    };
    let (bridge_origin, _bridge_state) = rt.block_on(spawn_local_fake_bridge(manifest));
    let inner_bridge = HttpTonConnectBridge::new(TonConnectBridgeConfig::new(format!(
        "{bridge_origin}/bridge"
    )))
    .expect("http bridge");
    let bridge: Arc<dyn TonConnectBridge> = Arc::new(LocalManifestRewritingBridge {
        inner: inner_bridge,
        local_manifest_url: format!("{bridge_origin}/manifest.json"),
    });

    let tmp = TempDir::new().expect("tempdir");
    let mut live_backend = LiveWalletBackend::with_dependencies(
        400,
        tmp.path().join("wallet"),
        handle.clone(),
        WalletAppConfig {
            tonconnect_bridge_url: format!("{bridge_origin}/bridge"),
            ..WalletAppConfig::default()
        },
        None,
        Some(bridge),
        Arc::new(FakeWalletKeyProtector::new()),
        PasscodeKdf::fast_test_kdf(),
    );
    live_backend
        .create_wallet(&test_words(), WalletVersion::V5R1, "pass9999")
        .expect("create wallet");

    let ctx = Context::default();
    AppWalletHost::set_backend(&ctx, Box::new(live_backend));

    let rpc = WalletFakeRpc::new();
    let session = build_live_session(rpc, handle);
    let mut workspace = LiveActiveWorkspace::new_headless(
        session,
        ShellSettings::default(),
        tmp.path().join("shell.json"),
        false,
    );

    let dest_addr = TonAddress::new(0, [0x42; 32]);
    let dest_friendly = dest_addr.to_friendly(false, false);

    let run_frame = |ctx: &Context, ws: &mut LiveActiveWorkspace| {
        let mut out = ctx.run_ui(
            RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(960.0, 720.0))),
                ..RawInput::default()
            },
            |ui| {
                let _ = ws.show(ctx, ui);
            },
        );
        out.textures_delta.clear();
    };

    // 1. `ton://transfer/...` -> parsed & routed to AppWalletHost Send screen with prefilled fields.
    let transfer_uri = build_transfer_link(
        &dest_addr,
        false,
        false,
        Some(1_500_000_000),
        Some("Lunch TON"),
    );
    assert!(matches!(
        parse_deep_link(&transfer_uri),
        Some(DeepLink::SendGrams {
            transfer_uri: Some(_),
            ..
        })
    ));
    workspace.controller_mut().open_link(&transfer_uri);
    run_frame(&ctx, &mut workspace);
    assert!(AppWalletHost::is_open(&ctx));
    AppWalletHost::with_active_panel(&ctx, |panel| {
        assert_eq!(panel.active_screen(), WalletScreen::Send);
        assert_eq!(panel.send_state().destination_input, dest_friendly);
        assert_eq!(panel.send_state().amount_input, "1.5");
        assert_eq!(panel.send_state().comment_input, "Lunch TON");
    })
    .expect("panel");

    // 2. `tg://sendgrams?to=...&amount=...` -> opens Send screen with prefilled recipient & amount.
    let sendgrams_url = format!("tg://sendgrams?to={dest_friendly}&amount=2.25");
    workspace.controller_mut().open_link(&sendgrams_url);
    run_frame(&ctx, &mut workspace);
    AppWalletHost::with_active_panel(&ctx, |panel| {
        assert_eq!(panel.active_screen(), WalletScreen::Send);
        assert_eq!(panel.send_state().destination_input, dest_friendly);
        assert_eq!(panel.send_state().amount_input, "2.25");
    })
    .expect("panel");

    // 3. `tc://?...` -> converted via `ton_connect_start_param` -> opens TON Connect approval box!
    let dapp_kp = TonConnectKeypair::generate();
    let tc_url = format!(
        "tc://?v=2&id={}&r=%7B%22manifestUrl%22%3A%22https%3A%2F%2Fconnect.example%2Fmanifest.json%22%2C%22items%22%3A%5B%7B%22name%22%3A%22ton_addr%22%7D%5D%7D",
        dapp_kp.client_id()
    );
    workspace.controller_mut().open_link(&tc_url);
    run_frame(&ctx, &mut workspace);
    AppWalletHost::with_active_panel(&ctx, |panel| {
        assert_eq!(panel.active_screen(), WalletScreen::TonConnect);
    })
    .expect("panel");
    AppWalletHost::with_active_backend_mut(&ctx, |b| {
        let pending = b.pending_connect().expect("pending connect from tc://");
        assert_eq!(pending.manifest.name, "Connect Example");
    })
    .expect("backend");

    // 4. Invalid / expired links produce error toasts rather than OpenWallet overlays.
    let mut sess_ctrl =
        tg_app::window::session_controller::SessionController::new(Default::default(), true);
    let mut mock_session = tg_app::stand_in::mock_session::MockSession::new(true);
    let expired_uri = format!("{transfer_uri}&exp=1");
    let expired_outcome = tg_app::window::deep_links::dispatch_deep_link(
        &mut sess_ctrl,
        &expired_uri,
        &mut mock_session,
    );
    assert!(matches!(expired_outcome, DeepLinkOutcome::Toast(msg) if msg.contains("expired")));

    let bad_amount = tg_app::window::deep_links::dispatch_deep_link(
        &mut sess_ctrl,
        "tg://sendgrams?to=UQCdqXGvONLwOr3zCNX5FjapflorB6ZsOdcdfLrjsDLt3AF4&amount=1,5",
        &mut mock_session,
    );
    assert!(matches!(bad_amount, DeepLinkOutcome::Toast(_)));

    let missing_to = tg_app::window::deep_links::dispatch_deep_link(
        &mut sess_ctrl,
        "tg://sendgrams?amount=1.0",
        &mut mock_session,
    );
    assert!(matches!(missing_to, DeepLinkOutcome::Toast(_)));

    let bare_sendgrams = tg_app::window::deep_links::dispatch_deep_link(
        &mut sess_ctrl,
        "tg://sendgrams",
        &mut mock_session,
    );
    assert!(matches!(
        bare_sendgrams,
        DeepLinkOutcome::OpenOverlay(DeepLinkOverlayRequest::OpenWallet {
            action: WalletDeepLinkAction::OpenOverview
        })
    ));

    drop(workspace);
    drop(ctx);
    rt.shutdown_timeout(Duration::from_secs(1));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn app_wallet_host_isolates_per_account_live_backends() {
    let tmp = TempDir::new().expect("tempdir");
    let ctx = Context::default();

    let mut b0 = LiveWalletBackend::with_dependencies(
        10,
        wallet_dir_for_account(tmp.path(), "SLOT0"),
        tokio::runtime::Handle::current(),
        WalletAppConfig::default(),
        None,
        None,
        Arc::new(FakeWalletKeyProtector::new()),
        PasscodeKdf::fast_test_kdf(),
    );
    let addr0 = b0
        .create_wallet(&test_words(), WalletVersion::V4R2, "pass-0")
        .expect("wallet 0");

    let mut b1 = LiveWalletBackend::with_dependencies(
        20,
        wallet_dir_for_account(tmp.path(), "SLOT1"),
        tokio::runtime::Handle::current(),
        WalletAppConfig::default(),
        None,
        None,
        Arc::new(FakeWalletKeyProtector::new()),
        PasscodeKdf::fast_test_kdf(),
    );
    let addr1 = b1
        .create_wallet(&test_words(), WalletVersion::V5R1, "pass-1")
        .expect("wallet 1");
    assert_ne!(addr0, addr1);

    AppWalletHost::set_account_backend(&ctx, 0, Box::new(b0), true);
    AppWalletHost::set_account_backend(&ctx, 1, Box::new(b1), false);
    assert!(AppWalletHost::has_account_backend(&ctx, 0));
    assert!(AppWalletHost::has_account_backend(&ctx, 1));

    let cur0 = AppWalletHost::with_active_backend_mut(&ctx, |b| b.overview().address)
        .flatten()
        .expect("addr0");
    assert_eq!(cur0, addr0);

    AppWalletHost::activate_account(&ctx, 1);
    let cur1 = AppWalletHost::with_active_backend_mut(&ctx, |b| b.overview().address)
        .flatten()
        .expect("addr1");
    assert_eq!(cur1, addr1);

    AppWalletHost::remove_account(&ctx, 1);
    assert!(!AppWalletHost::has_account_backend(&ctx, 1));
    let back_to_0 = AppWalletHost::with_active_backend_mut(&ctx, |b| b.overview().address)
        .flatten()
        .expect("fallback addr0");
    assert_eq!(back_to_0, addr0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_wallet_key_protection_enroll_lock_reload_and_unlock() {
    let tmp = TempDir::new().expect("tempdir");
    let wallet_dir = wallet_dir_for_account(tmp.path(), "DPAPI_FAKE");
    let protector: Arc<dyn WalletKeyProtector> = Arc::new(FakeWalletKeyProtector::new());

    let mut backend = LiveWalletBackend::with_dependencies(
        701,
        wallet_dir.clone(),
        tokio::runtime::Handle::current(),
        WalletAppConfig::default(),
        None,
        None,
        Arc::clone(&protector),
        PasscodeKdf::fast_test_kdf(),
    );
    let words = test_words();
    backend
        .create_wallet(&words, WalletVersion::V5R1, "my-passcode")
        .expect("create");
    assert!(backend.is_key_protection_available());
    assert!(!backend.is_key_protection_enabled());

    // Wrong passcode refuses enrollment.
    assert!(backend.enable_key_protection("wrong").is_err());
    backend
        .enable_key_protection("my-passcode")
        .expect("enable protection");
    assert!(backend.is_key_protection_enabled());

    // Reload from disk -> locked -> unlock via protector without typing passcode.
    let mut reopened = LiveWalletBackend::with_dependencies(
        701,
        wallet_dir,
        tokio::runtime::Handle::current(),
        WalletAppConfig::default(),
        None,
        None,
        Arc::clone(&protector),
        PasscodeKdf::fast_test_kdf(),
    );
    assert!(reopened.overview().locked);
    assert!(reopened.is_key_protection_enabled());
    reopened
        .unlock_with_key_protection()
        .expect("unlock with protector");
    assert!(!reopened.overview().locked);
    assert_eq!(reopened.reveal_phrase().expect("phrase"), words);

    // Disabling protection clears the persisted blob.
    reopened.disable_key_protection().expect("disable");
    assert!(!reopened.is_key_protection_enabled());
}

#[cfg(windows)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_wallet_windows_dpapi_end_to_end_round_trip() {
    let tmp = TempDir::new().expect("tempdir");
    let wallet_dir = wallet_dir_for_account(tmp.path(), "DPAPI_WIN_REAL");
    let protector = tg_platform::wallet_protection::default_wallet_key_protector();
    assert!(protector.is_available());

    let mut backend = LiveWalletBackend::with_dependencies(
        702,
        wallet_dir.clone(),
        tokio::runtime::Handle::current(),
        WalletAppConfig::default(),
        None,
        None,
        Arc::clone(&protector),
        PasscodeKdf::fast_test_kdf(),
    );
    let words = test_words();
    backend
        .create_wallet(&words, WalletVersion::V5R1, "win-dpapi-pass")
        .expect("create");
    backend
        .enable_key_protection("win-dpapi-pass")
        .expect("enable real Windows DPAPI");

    let mut reopened = LiveWalletBackend::with_dependencies(
        702,
        wallet_dir,
        tokio::runtime::Handle::current(),
        WalletAppConfig::default(),
        None,
        None,
        protector,
        PasscodeKdf::fast_test_kdf(),
    );
    assert!(reopened.overview().locked);
    reopened
        .unlock_with_key_protection()
        .expect("unlock via real Windows DPAPI");
    assert!(!reopened.overview().locked);
    assert_eq!(reopened.reveal_phrase().expect("phrase"), words);
}
