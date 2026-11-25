//! Security review #1 (SEC-B) live wallet & TON Connect regression tests:
//! - B3: TON Connect approval swap protection (FIFO queue + `(request_id, content_hash)` binding +
//!   error responses for duplicate/superseded requests)
//! - B7 (CRITICAL): pre-approval emulation (`emulate_ton_connect_tx`) and fee estimation
//!   (`estimate_transfer_fee`) never sign with the real wallet key; sent BOCs do not verify under
//!   the wallet's real public key
//! - B8: TON Connect enforces `ton_connect_proof_domain_allowed` (no `telegram.org` spoofing),
//!   binds proof to the manifest host, and checks `network`, `valid_until`, and `from` on
//!   `sendTransaction`
//!
//! All tests use freshly generated synthetic test keys and in-process fakes only — never real
//! seeds, never real funds, never contacts TON/Toncenter/Telegram.

use std::sync::{Arc, Mutex};

use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde_json::{Value, json};
use tg_app::live::wallet::LiveWalletBackend;
use tg_app::wallet::backend::WalletBackend;
use tg_platform::wallet_protection::FakeWalletKeyProtector;
use tg_wallet::address::TonAddress;
use tg_wallet::boc::deserialize_boc_base64;
use tg_wallet::cell::CellBuilder;
use tg_wallet::contract::WalletVersion;
use tg_wallet::emulate::{EmulateMessageRequest, EmulationResult};
use tg_wallet::mnemonic::{generate_mnemonic, mnemonic_to_keypair};
use tg_wallet::toncenter::{
    AccountState, AddressInfo, BoxFuture, EmulatedTrace, GetMethodResult, SendBocResult,
    TonTransaction, ToncenterApi,
};
use tg_wallet::tonconnect::{
    BridgeEvent, DappManifest, TonConnectBridge, TonConnectKeypair, TonConnectSession,
};
use tg_wallet::vault::PasscodeKdf;
use tg_wallet::{WalletAppConfig, WalletError};

struct SecBFakeToncenter {
    emulate_message_bocs: Mutex<Vec<String>>,
    emulate_trace_bocs: Mutex<Vec<String>>,
    sent_bocs: Mutex<Vec<String>>,
}

impl SecBFakeToncenter {
    fn new() -> Self {
        Self {
            emulate_message_bocs: Mutex::new(Vec::new()),
            emulate_trace_bocs: Mutex::new(Vec::new()),
            sent_bocs: Mutex::new(Vec::new()),
        }
    }
}

impl ToncenterApi for SecBFakeToncenter {
    fn get_address_information<'a>(
        &'a self,
        _address: &'a str,
    ) -> BoxFuture<'a, Result<AddressInfo, WalletError>> {
        Box::pin(async {
            Ok(AddressInfo {
                balance_nano: 50_000_000_000,
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
                hash: Some("cc".repeat(32)),
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
                stack: vec![json!(["num", "0x5"])],
            })
        })
    }

    fn emulate_trace<'a>(
        &'a self,
        boc_base64: &'a str,
    ) -> BoxFuture<'a, Result<EmulatedTrace, WalletError>> {
        let boc = boc_base64.to_owned();
        Box::pin(async move {
            self.emulate_trace_bocs.lock().unwrap().push(boc);
            Ok(EmulatedTrace {
                fee_nano: 4_500_000,
                account: format!("0:{}", "11".repeat(32)),
            })
        })
    }

    fn emulate_message<'a>(
        &'a self,
        request: &'a EmulateMessageRequest,
    ) -> BoxFuture<'a, Result<EmulationResult, WalletError>> {
        let boc = request.boc_base64.clone();
        Box::pin(async move {
            self.emulate_message_bocs.lock().unwrap().push(boc);
            Ok(EmulationResult {
                mc_block_seqno: Some(1),
                root_tx_hash: "root_hash".to_owned(),
                root_account: format!("0:{}", "11".repeat(32)),
                total_fees_nano: 5_100_000,
                root_fee_nano: 5_100_000,
                total_gas_fees_nano: 3_000_000,
                total_storage_fees_nano: 100_000,
                total_fwd_fees_nano: 2_000_000,
                is_incomplete: false,
                trace_succeeded: true,
                transactions: Vec::new(),
                account_deltas: Vec::new(),
                warnings: Vec::new(),
                raw_actions: Vec::new(),
            })
        })
    }
}

struct SecBFakeBridge {
    manifest: Mutex<DappManifest>,
    inbox: Mutex<Vec<BridgeEvent>>,
    sent: Mutex<Vec<(String, String, String)>>,
}

impl SecBFakeBridge {
    fn new(manifest: DappManifest) -> Self {
        Self {
            manifest: Mutex::new(manifest),
            inbox: Mutex::new(Vec::new()),
            sent: Mutex::new(Vec::new()),
        }
    }

    fn set_manifest(&self, manifest: DappManifest) {
        *self.manifest.lock().unwrap() = manifest;
    }

    fn push_event(&self, ev: BridgeEvent) {
        self.inbox.lock().unwrap().push(ev);
    }

    fn sent_messages(&self) -> Vec<(String, String, String)> {
        self.sent.lock().unwrap().clone()
    }
}

impl TonConnectBridge for SecBFakeBridge {
    fn fetch_manifest<'a>(
        &'a self,
        _manifest_url: &'a str,
    ) -> BoxFuture<'a, Result<DappManifest, WalletError>> {
        let m = self.manifest.lock().unwrap().clone();
        Box::pin(async move { Ok(m) })
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

/// Checks whether the Ed25519 signature embedded in a V5R1 external-in wallet BOC verifies under
/// `pubkey`. V5R1 packs `[signing_payload_bits..., 512_bit_signature]` inside the body cell
/// (always the last ref of the root external-in message cell).
fn v5r1_external_boc_verifies_under(boc_base64: &str, pubkey: &[u8; 32]) -> bool {
    let root = deserialize_boc_base64(boc_base64).expect("valid BOC");
    let body = root.refs().last().expect("external message body cell ref");
    let total_bits = body.bit_len();
    assert!(
        total_bits >= 512,
        "V5R1 body must contain signing payload + 512-bit signature, got {total_bits} bits"
    );
    let data = body.data();
    let bit_at = |idx: usize| -> bool { (data[idx / 8] >> (7 - (idx % 8))) & 1 != 0 };

    let payload_bits = total_bits - 512;
    let mut signing_builder = CellBuilder::new();
    for i in 0..payload_bits {
        signing_builder.store_bit(bit_at(i)).unwrap();
    }
    for r in body.refs() {
        signing_builder.store_ref(Arc::clone(r)).unwrap();
    }
    let signing_hash = signing_builder.end_cell().unwrap().hash();

    let mut sig_bytes = [0_u8; 64];
    for (byte_idx, slot) in sig_bytes.iter_mut().enumerate() {
        let mut b = 0_u8;
        for bit_in_byte in 0..8 {
            b = (b << 1) | u8::from(bit_at(payload_bits + byte_idx * 8 + bit_in_byte));
        }
        *slot = b;
    }

    let vk = VerifyingKey::from_bytes(pubkey).unwrap();
    let sig = Signature::from_bytes(&sig_bytes);
    vk.verify_strict(&signing_hash, &sig).is_ok() || vk.verify(&signing_hash, &sig).is_ok()
}

struct Fixture {
    _dir: tempfile::TempDir,
    _rt: tokio::runtime::Runtime,
    backend: LiveWalletBackend,
    toncenter: Arc<SecBFakeToncenter>,
    bridge: Arc<SecBFakeBridge>,
    dapp_session: TonConnectSession,
    pubkey: [u8; 32],
    addr: TonAddress,
}

fn setup_fixture(testnet: bool) -> Fixture {
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
    let toncenter = Arc::new(SecBFakeToncenter::new());
    let bridge = Arc::new(SecBFakeBridge::new(manifest.clone()));
    let cfg = WalletAppConfig {
        testnet,
        ..WalletAppConfig::default()
    };

    let mut backend = LiveWalletBackend::with_dependencies(
        700,
        dir.path().to_path_buf(),
        rt.handle().clone(),
        cfg,
        Some(toncenter.clone()),
        Some(bridge.clone()),
        Arc::new(FakeWalletKeyProtector::new()),
        PasscodeKdf::fast_test_kdf(),
    );
    let words = generate_mnemonic("").expect("synthetic test mnemonic");
    let kp = mnemonic_to_keypair(&words, "").expect("keypair");
    let addr = backend
        .create_wallet(&words, WalletVersion::V5R1, "test-passcode")
        .expect("create synthetic wallet");
    backend.refresh().expect("refresh");

    let dapp_kp = TonConnectKeypair::generate();
    let link = format!(
        "tc://?v=2&id={}&r={{\"manifestUrl\":\"https://demoswap.example/manifest.json\",\"items\":[{{\"name\":\"ton_addr\"}},{{\"name\":\"ton_proof\",\"payload\":\"nonce-1\"}}]}}",
        dapp_kp.client_id()
    );
    backend.handle_ton_connect_link(&link).expect("handle link");
    let _card = backend.approve_ton_connect().expect("approve connect");

    let sent = bridge.sent_messages();
    let wallet_cid = sent[0].0.clone();
    let dapp_session =
        TonConnectSession::new(dapp_kp, &wallet_cid, manifest).expect("dapp session");

    Fixture {
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

#[test]
fn b7_live_wallet_emulation_and_fee_estimate_never_sign_with_real_key() {
    let Fixture {
        mut backend,
        toncenter,
        bridge,
        dapp_session,
        pubkey,
        ..
    } = setup_fixture(false);

    let dest = TonAddress::new(0, [0x44_u8; 32]).to_friendly(true, false);

    // 1. `prepare_send` -> `estimate_transfer_fee` -> `emulate_trace` while vault is UNLOCKED.
    //    Old code called `load_keypair()` and sent a real, valid, broadcastable BOC to Toncenter!
    let quote = backend
        .prepare_send(&dest, 1_000_000_000, "fee preview", None)
        .expect("prepare_send quote");
    let trace_bocs = toncenter.emulate_trace_bocs.lock().unwrap().clone();
    assert_eq!(
        trace_bocs.len(),
        1,
        "prepare_send must invoke emulate_trace"
    );
    assert!(
        !v5r1_external_boc_verifies_under(&trace_bocs[0], &pubkey),
        "B7 CRITICAL: fee-estimate BOC sent to emulate_trace MUST NOT verify under real wallet key"
    );
    assert!(
        toncenter.sent_bocs.lock().unwrap().is_empty(),
        "prepare_send must never broadcast"
    );

    // 2. Incoming TON Connect `sendTransaction` -> `emulate_ton_connect_tx` -> `emulate_message`
    //    while vault is UNLOCKED. Old code signed with real wallet key before user approval!
    let tx_req = json!({
        "id": "b7-emul-tx",
        "method": "sendTransaction",
        "params": [json!({
            "valid_until": 1_900_000_000u64,
            "network": "-239",
            "messages": [{
                "address": dest,
                "amount": "2000000000"
            }]
        }).to_string()]
    })
    .to_string();
    bridge.push_event(BridgeEvent {
        id: Some("ev-b7-1".to_owned()),
        from: dapp_session.wallet_client_id(),
        message: dapp_session.encrypt_message(tx_req.as_bytes()).unwrap(),
    });
    assert_eq!(backend.poll_ton_connect_events().unwrap(), 1);

    let emul_bocs = toncenter.emulate_message_bocs.lock().unwrap().clone();
    assert_eq!(
        emul_bocs.len(),
        1,
        "poll_ton_connect_events must emulate incoming tx"
    );
    assert!(
        !v5r1_external_boc_verifies_under(&emul_bocs[0], &pubkey),
        "B7 CRITICAL: TON Connect emulation BOC MUST NOT verify under real wallet key"
    );
    assert!(
        toncenter.sent_bocs.lock().unwrap().is_empty(),
        "pre-approval emulation must never broadcast"
    );

    // 3. Contrast: ONLY after explicit user confirmation (`approve_ton_connect_tx` and
    //    `confirm_send`) does a BOC verify under the real wallet public key.
    let pending = backend.pending_tx_approval().expect("pending tx");
    let tc_receipt = backend
        .approve_ton_connect_tx(&pending.request.request_id, &pending.content_hash())
        .expect("explicitly approved tc tx");
    assert!(
        v5r1_external_boc_verifies_under(&tc_receipt.signed_boc_base64, &pubkey),
        "approved TON Connect BOC must verify under real key"
    );

    let send_receipt = backend.confirm_send(&quote).expect("confirmed send");
    assert!(
        v5r1_external_boc_verifies_under(&send_receipt.signed_boc_base64, &pubkey),
        "confirmed send BOC must verify under real key"
    );
}

#[test]
fn b3_second_send_transaction_cannot_be_approved_by_first_click_and_signdata_bound() {
    let Fixture {
        mut backend,
        toncenter,
        bridge,
        dapp_session,
        ..
    } = setup_fixture(false);

    let dest_a = TonAddress::new(0, [0x21_u8; 32]).to_friendly(true, false);
    let dest_b = TonAddress::new(0, [0x22_u8; 32]).to_friendly(true, false);

    // 1. First `sendTransaction` arrives and is shown to the user (`req-1`, 0.5 TON).
    let tx1 = json!({
        "id": "req-1",
        "method": "sendTransaction",
        "params": [json!({
            "valid_until": 1_900_000_000u64,
            "network": "-239",
            "messages": [{"address": dest_a, "amount": "500000000"}]
        }).to_string()]
    })
    .to_string();
    bridge.push_event(BridgeEvent {
        id: Some("ev-1".to_owned()),
        from: dapp_session.wallet_client_id(),
        message: dapp_session.encrypt_message(tx1.as_bytes()).unwrap(),
    });
    assert_eq!(backend.poll_ton_connect_events().unwrap(), 1);
    let shown_first = backend.pending_tx_approval().expect("first pending tx");
    assert_eq!(shown_first.request.request_id, "req-1");
    let first_id = shown_first.request.request_id.clone();
    let first_hash = shown_first.content_hash();

    // 2. Second `sendTransaction` (`req-2`, 40 TON to attacker) arrives BEFORE the user clicks!
    let tx2 = json!({
        "id": "req-2",
        "method": "sendTransaction",
        "params": [json!({
            "valid_until": 1_900_000_000u64,
            "network": "-239",
            "messages": [{"address": dest_b, "amount": "40000000000"}]
        }).to_string()]
    })
    .to_string();
    bridge.push_event(BridgeEvent {
        id: Some("ev-2".to_owned()),
        from: dapp_session.wallet_client_id(),
        message: dapp_session.encrypt_message(tx2.as_bytes()).unwrap(),
    });
    assert_eq!(backend.poll_ton_connect_events().unwrap(), 1);

    // The head request MUST remain `req-1` (old code overwrote `pending_tx` with `req-2`!).
    let still_first = backend.pending_tx_approval().expect("still req-1");
    assert_eq!(still_first.request.request_id, "req-1");
    assert_eq!(still_first.request.total_nano(), 500_000_000);

    // Tampered hash or mismatched id MUST be refused without broadcasting.
    assert!(
        backend
            .approve_ton_connect_tx(&first_id, "00000000000000000000000000000000")
            .is_err()
    );
    assert!(
        backend
            .approve_ton_connect_tx("req-2", &first_hash)
            .is_err()
    );
    assert!(toncenter.sent_bocs.lock().unwrap().is_empty());

    // First click approves ONLY `req-1`; `req-2` is NOT approved and now advances to head.
    let receipt1 = backend
        .approve_ton_connect_tx(&first_id, &first_hash)
        .expect("first click approves req-1");
    assert!(!receipt1.signed_boc_base64.is_empty());
    assert_eq!(toncenter.sent_bocs.lock().unwrap().len(), 1);

    let now_head = backend.pending_tx_approval().expect("req-2 queued behind");
    assert_eq!(now_head.request.request_id, "req-2");
    assert_ne!(now_head.content_hash(), first_hash);

    // Reusing `req-1`'s `(id, hash)` MUST NOT approve `req-2`!
    assert!(
        backend
            .approve_ton_connect_tx(&first_id, &first_hash)
            .is_err(),
        "stale first-click approval target must be refused for req-2"
    );
    assert_eq!(toncenter.sent_bocs.lock().unwrap().len(), 1);

    // Duplicate `req-2` arriving while `req-2` is already pending is rejected with an error reply.
    let sent_before_dup = bridge.sent_messages().len();
    bridge.push_event(BridgeEvent {
        id: Some("ev-2-dup".to_owned()),
        from: dapp_session.wallet_client_id(),
        message: dapp_session.encrypt_message(tx2.as_bytes()).unwrap(),
    });
    assert_eq!(backend.poll_ton_connect_events().unwrap(), 1);
    let sent_after_dup = bridge.sent_messages();
    assert_eq!(sent_after_dup.len(), sent_before_dup + 1);
    let dup_reply: Value = serde_json::from_slice(
        &dapp_session
            .decrypt_message(&sent_after_dup.last().unwrap().2)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(dup_reply["id"], "req-2");
    assert_eq!(dup_reply["error"]["code"], 1);

    // 3. Same swap-protection guarantee for `signData` (`sd-1` followed by `sd-2`).
    let sd1 = json!({
        "id": "sd-1",
        "method": "signData",
        "params": [json!({"type": "text", "text": "Sign benign message"}).to_string()]
    })
    .to_string();
    let sd2 = json!({
        "id": "sd-2",
        "method": "signData",
        "params": [json!({"type": "text", "text": "Sign malicious drain"}).to_string()]
    })
    .to_string();
    bridge.push_event(BridgeEvent {
        id: Some("ev-sd-1".to_owned()),
        from: dapp_session.wallet_client_id(),
        message: dapp_session.encrypt_message(sd1.as_bytes()).unwrap(),
    });
    bridge.push_event(BridgeEvent {
        id: Some("ev-sd-2".to_owned()),
        from: dapp_session.wallet_client_id(),
        message: dapp_session.encrypt_message(sd2.as_bytes()).unwrap(),
    });
    assert_eq!(backend.poll_ton_connect_events().unwrap(), 2);
    let shown_sd = backend.pending_sign_data().expect("sd-1 must stay at head");
    assert_eq!(shown_sd.request.request_id, "sd-1");
    let sd1_hash = shown_sd.content_hash();

    // Tampered hash refused.
    assert!(
        backend
            .approve_ton_connect_sign_data("sd-1", "deadbeef")
            .is_err()
    );
    // Approving sd-1 signs ONLY sd-1; sd-2 then advances and refuses sd-1's click target.
    backend
        .approve_ton_connect_sign_data("sd-1", &sd1_hash)
        .expect("approve sd-1");
    assert_eq!(
        backend.pending_sign_data().unwrap().request.request_id,
        "sd-2"
    );
    assert!(
        backend
            .approve_ton_connect_sign_data("sd-1", &sd1_hash)
            .is_err(),
        "first click target must not sign sd-2"
    );

    // Disconnecting the session answers remaining queued requests (`req-2`, `sd-2`) with error replies.
    let session_id = backend.ton_connect_sessions()[0].session_id;
    let sent_before_disc = bridge.sent_messages().len();
    backend
        .disconnect_ton_connect(session_id)
        .expect("disconnect");
    assert!(backend.pending_tx_approval().is_none());
    assert!(backend.pending_sign_data().is_none());
    let sent_after_disc = bridge.sent_messages();
    assert!(
        sent_after_disc.len() >= sent_before_disc + 3,
        "disconnect must send error replies for queued req-2 and sd-2 plus disconnect event"
    );
}

#[test]
fn b8_ton_connect_blocks_telegram_spoofing_host_mismatch_and_invalid_tx_fields() {
    let Fixture {
        mut backend,
        toncenter,
        bridge,
        dapp_session,
        addr,
        ..
    } = setup_fixture(false);

    // 1. `telegram.org` / `*.telegram.org` TON Proof spoofing must be blocked.
    let spoof_kp = TonConnectKeypair::generate();
    bridge.set_manifest(DappManifest {
        url: "https://wallet.telegram.org".to_owned(),
        name: "Fake Telegram Wallet".to_owned(),
        icon_url: String::new(),
    });
    let tg_link = format!(
        "tc://?v=2&id={}&r={{\"manifestUrl\":\"https://wallet.telegram.org/manifest.json\",\"items\":[{{\"name\":\"ton_addr\"}},{{\"name\":\"ton_proof\",\"payload\":\"spoof\"}}]}}",
        spoof_kp.client_id()
    );
    backend
        .handle_ton_connect_link(&tg_link)
        .expect("parse link");
    assert!(
        backend.approve_ton_connect().is_err(),
        "B8: must reject ton_proof for wallet.telegram.org"
    );
    backend.decline_ton_connect();

    // 2. Manifest host mismatch (`manifestUrl` hosted on evil.example claiming victim.example) must be blocked.
    bridge.set_manifest(DappManifest {
        url: "https://victim.example".to_owned(),
        name: "Victim Site".to_owned(),
        icon_url: String::new(),
    });
    let mismatch_link = format!(
        "tc://?v=2&id={}&r={{\"manifestUrl\":\"https://evil.example/manifest.json\",\"items\":[{{\"name\":\"ton_addr\"}},{{\"name\":\"ton_proof\",\"payload\":\"steal\"}}]}}",
        spoof_kp.client_id()
    );
    backend
        .handle_ton_connect_link(&mismatch_link)
        .expect("parse mismatch link");
    assert!(
        backend.approve_ton_connect().is_err(),
        "B8: must reject manifest whose url host does not match manifestUrl host"
    );
    backend.decline_ton_connect();

    // 3. `sendTransaction` validation on active mainnet (`-239`) session:
    //    (a) Wrong network (`-3` testnet on mainnet wallet)
    //    (b) Expired `valid_until` (`1000` in the past)
    //    (c) Mismatched `from` address
    let dest = TonAddress::new(0, [0x77_u8; 32]).to_friendly(true, false);
    let wrong_from = TonAddress::new(0, [0x88_u8; 32]).to_raw();
    let bad_requests = [
        (
            "bad-net",
            json!({
                "id": "bad-net",
                "method": "sendTransaction",
                "params": [json!({
                    "valid_until": 1_900_000_000u64,
                    "network": "-3",
                    "messages": [{"address": dest, "amount": "100000000"}]
                }).to_string()]
            }),
        ),
        (
            "bad-exp",
            json!({
                "id": "bad-exp",
                "method": "sendTransaction",
                "params": [json!({
                    "valid_until": 1_000u64,
                    "network": "-239",
                    "messages": [{"address": dest, "amount": "100000000"}]
                }).to_string()]
            }),
        ),
        (
            "bad-from",
            json!({
                "id": "bad-from",
                "method": "sendTransaction",
                "params": [json!({
                    "valid_until": 1_900_000_000u64,
                    "network": "-239",
                    "from": wrong_from,
                    "messages": [{"address": dest, "amount": "100000000"}]
                }).to_string()]
            }),
        ),
    ];

    let sent_before = bridge.sent_messages().len();
    for (idx, (_id, req)) in bad_requests.iter().enumerate() {
        bridge.push_event(BridgeEvent {
            id: Some(format!("ev-bad-{idx}")),
            from: dapp_session.wallet_client_id(),
            message: dapp_session
                .encrypt_message(req.to_string().as_bytes())
                .unwrap(),
        });
    }
    assert_eq!(backend.poll_ton_connect_events().unwrap(), 3);
    assert!(
        backend.pending_tx_approval().is_none(),
        "invalid sendTransaction requests must not be queued"
    );
    assert!(
        toncenter.emulate_message_bocs.lock().unwrap().is_empty(),
        "invalid sendTransaction requests must not even be emulated"
    );

    let sent_after = bridge.sent_messages();
    assert_eq!(
        sent_after.len(),
        sent_before + 3,
        "each invalid sendTransaction must receive an encrypted BadRequest reply"
    );
    for (offset, (expected_id, _)) in bad_requests.iter().enumerate() {
        let dec: Value = serde_json::from_slice(
            &dapp_session
                .decrypt_message(&sent_after[sent_before + offset].2)
                .unwrap(),
        )
        .unwrap();
        assert_eq!(dec["id"], *expected_id);
        assert_eq!(dec["error"]["code"], 1);
    }

    // 4. Valid `sendTransaction` with matching `network`, future `valid_until`, and matching `from`
    //    succeeds and queues for user confirmation.
    let valid_tx = json!({
        "id": "good-tx-1",
        "method": "sendTransaction",
        "params": [json!({
            "valid_until": 1_900_000_000u64,
            "network": "-239",
            "from": addr.to_raw(),
            "messages": [{"address": dest, "amount": "100000000"}]
        }).to_string()]
    })
    .to_string();
    bridge.push_event(BridgeEvent {
        id: Some("ev-good-1".to_owned()),
        from: dapp_session.wallet_client_id(),
        message: dapp_session.encrypt_message(valid_tx.as_bytes()).unwrap(),
    });
    assert_eq!(backend.poll_ton_connect_events().unwrap(), 1);
    let pending = backend
        .pending_tx_approval()
        .expect("valid sendTransaction must be queued");
    assert_eq!(pending.request.request_id, "good-tx-1");
}
