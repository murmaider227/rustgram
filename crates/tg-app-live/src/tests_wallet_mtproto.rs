//! `FakeRpcClient` integration tests for WP-56 native MTProto TON Wallet wiring in
//! [`crate::wallet::LiveWalletBackend`].
//!
//! Zero real network, TON, or Telegram calls — every RPC (`toncenter.performApiRequest`,
//! `wallet.getState`, `wallet.getGaslessInfo`, `payments.getCurrencyRates`,
//! `wallet.getTransactions`, `wallet.getBackupHolderDcs`, `wallet.enableBackup`,
//! `wallet.disableBackup`, `wallet.setCardStickers`, `payments.depositStarGift`,
//! `payments.displayStarGift`) is served by an in-process `FakeRpcClient`.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use tg_api::{DcId, InvocationError, RpcClient};
use tg_platform::wallet_protection::default_wallet_key_protector;
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tg_wallet::WalletAppConfig;
use tg_wallet::address::TonAddress;
use tg_wallet::boc::serialize_boc;
use tg_wallet::contract::WalletVersion;
use tg_wallet::jettons::{
    DEFAULT_NFT_FORWARD_NANO, DEFAULT_NFT_GAS_NANO, NftTransferParams, build_nft_transfer_message,
};
use tg_wallet::mtproto_api::{
    CardStickerCoordinates, CardStickerInput, DC_SHIFT, TONCENTER_DC_SHIFT,
};
use tg_wallet::toncenter::ToncenterTransportKind;
use tg_wallet::vault::PasscodeKdf;
use tokio::sync::broadcast;

use crate::wallet::LiveWalletBackend;
use tg_app_core::core_types::erased_rpc::ErasedRpcClient;
use tg_app_core::wallet::backend::WalletBackend;

/// Public TON spec test phrase used strictly for offline unit tests (zero real funds).
const TEST_WORDS: [&str; 24] = [
    "hospital", "stove", "relief", "fringe", "tongue", "always", "charge", "angry", "urge",
    "sentence", "again", "match", "nerve", "inquiry", "senior", "coconut", "label", "tumble",
    "carry", "category", "beauty", "bean", "road", "solution",
];

type RecordedCall = (Option<DcId>, u32, Vec<u8>);
type QueuedResponse = (u32, Result<Vec<u8>, InvocationError>);

#[derive(Clone)]
struct FakeRpcClient {
    recorded: Arc<Mutex<Vec<RecordedCall>>>,
    responses: Arc<Mutex<VecDeque<QueuedResponse>>>,
    updates_tx: broadcast::Sender<tl_enums::Updates>,
}

impl FakeRpcClient {
    fn new() -> Self {
        let (updates_tx, _) = broadcast::channel(32);
        Self {
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

    fn recorded_calls(&self) -> Vec<RecordedCall> {
        self.recorded.lock().expect("lock recorded").clone()
    }

    fn count_ctor(&self, ctor: u32) -> usize {
        self.recorded
            .lock()
            .expect("lock recorded")
            .iter()
            .filter(|(_, c, _)| *c == ctor)
            .count()
    }

    fn invoke_impl<R: RemoteCall>(
        &self,
        dc: Option<DcId>,
        req: &R,
    ) -> Result<R::Return, InvocationError> {
        let req_bytes = req.to_bytes();
        let ctor = if req_bytes.len() >= 4 {
            u32::from_le_bytes([req_bytes[0], req_bytes[1], req_bytes[2], req_bytes[3]])
        } else {
            0
        };
        self.recorded
            .lock()
            .expect("lock recorded")
            .push((dc, ctor, req_bytes));
        let mut queue = self.responses.lock().expect("lock responses");
        let pos = queue
            .iter()
            .position(|(expected, _)| *expected == ctor)
            .ok_or_else(|| {
                InvocationError::Deserialize(format!(
                    "unexpected RPC ctor 0x{ctor:08x} (dc={dc:?})"
                ))
            })?;
        let (_, res) = queue.remove(pos).expect("queued entry exists");
        let bytes = res?;
        R::Return::from_bytes(&bytes)
            .map_err(|e| InvocationError::Deserialize(format!("decode 0x{ctor:08x}: {e}")))
    }
}

impl RpcClient for FakeRpcClient {
    type Error = InvocationError;

    async fn invoke<R: RemoteCall>(&self, request: &R) -> Result<R::Return, Self::Error> {
        self.invoke_impl(None, request)
    }

    async fn invoke_in_dc<R: RemoteCall>(
        &self,
        dc_id: DcId,
        request: &R,
    ) -> Result<R::Return, Self::Error> {
        self.invoke_impl(Some(dc_id), request)
    }

    fn subscribe_updates(&self) -> Option<broadcast::Receiver<tl_enums::Updates>> {
        Some(self.updates_tx.subscribe())
    }

    fn main_dc_id(&self) -> DcId {
        2
    }
}

fn test_words() -> Vec<String> {
    TEST_WORDS.iter().map(|s| (*s).to_owned()).collect()
}

fn make_backend(
    rt: &tokio::runtime::Runtime,
    cfg: WalletAppConfig,
    rpc: Option<&FakeRpcClient>,
) -> (tempfile::TempDir, LiveWalletBackend) {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut backend = LiveWalletBackend::with_dependencies(
        42,
        dir.path().to_path_buf(),
        rt.handle().clone(),
        cfg,
        None,
        None,
        default_wallet_key_protector(),
        PasscodeKdf::fast_test_kdf(),
    );
    if let Some(fake) = rpc {
        backend.attach_mtproto_rpc(ErasedRpcClient::new(fake.clone()), 4);
    }
    (dir, backend)
}

fn enqueue_toncenter_json(fake: &FakeRpcClient, body_json: &serde_json::Value) {
    let resp = tl_enums::toncenter::ApiResponse::ApiResponse(tl_types::toncenter::ApiResponse {
        response: tl_enums::DataJSON::DataJSON(tl_types::DataJSON {
            data: body_json.to_string(),
        }),
    });
    fake.enqueue_ok(tl_fns::toncenter::PerformApiRequest::CONSTRUCTOR_ID, &resp);
}

#[test]
fn transport_selection_defaults_to_mtproto_and_uses_http_on_override() {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let fake = FakeRpcClient::new();

    // Default config (empty `toncenter_base_url`) selects native MTProto transport.
    let (_dir1, mut backend) = make_backend(&rt, WalletAppConfig::default(), Some(&fake));
    assert_eq!(
        backend.toncenter_transport_kind(),
        Some(ToncenterTransportKind::Mtproto)
    );

    // Explicitly configuring `toncenter_base_url` switches to HTTP override.
    let http_cfg = WalletAppConfig {
        toncenter_base_url: "http://127.0.0.1:9999".to_owned(),
        ..Default::default()
    };
    backend.set_app_config(http_cfg);
    assert_eq!(
        backend.toncenter_transport_kind(),
        Some(ToncenterTransportKind::Http)
    );

    // Clearing the override returns to MTProto transport.
    backend.set_app_config(WalletAppConfig::default());
    assert_eq!(
        backend.toncenter_transport_kind(),
        Some(ToncenterTransportKind::Mtproto)
    );
}

#[test]
fn refresh_uses_mtproto_wallet_rpcs_and_shifted_webfile_dc_toncenter() {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let fake = FakeRpcClient::new();
    let (_dir, mut backend) = make_backend(&rt, WalletAppConfig::default(), None);

    let addr = backend
        .create_wallet(&test_words(), WalletVersion::V5R1, "1234")
        .expect("create wallet");
    backend.apply_synced_app_config(crate::wallet::SyncedWalletAppConfig {
        config: WalletAppConfig::default(),
        usd_per_ton: Some(6.25),
        hash: 1,
    });
    backend.attach_mtproto_rpc(ErasedRpcClient::new(fake.clone()), 4);
    assert_eq!(
        backend.toncenter_transport_kind(),
        Some(ToncenterTransportKind::Mtproto)
    );

    // Queue wallet.getState
    fake.enqueue_ok(
        tl_fns::wallet::GetState::CONSTRUCTOR_ID,
        &tl_enums::WalletState::WalletState(tl_types::WalletState {
            backup_enabled: true,
            can_export_phrase: true,
            can_enable_backup: true,
            address: addr.to_raw(),
            public_key: vec![0xAA; 32],
            balance: 9_000_000_000,
        }),
    );
    // Queue wallet.getGaslessInfo
    let relayer = TonAddress::new(0, [0x44; 32]).to_raw();
    fake.enqueue_ok(
        tl_fns::wallet::GetGaslessInfo::CONSTRUCTOR_ID,
        &tl_enums::Updates::UpdateShort(tl_types::UpdateShort {
            update: tl_enums::Update::WalletGaslessInfo(tl_types::UpdateWalletGaslessInfo {
                available: true,
                left: 5,
                reset_at: 1_732_100_000,
                min_amount: 100_000_000,
                relayer_address: relayer,
            }),
            date: 1_732_000_000,
        }),
    );
    // Queue payments.getCurrencyRates
    fake.enqueue_ok(
        tl_fns::payments::GetCurrencyRates::CONSTRUCTOR_ID,
        &tl_enums::payments::CurrencyRates::CurrencyRates(tl_types::payments::CurrencyRates {
            rates: vec![
                tl_enums::CurrencyRate::CurrencyRate(tl_types::CurrencyRate {
                    currency: "USD".to_owned(),
                    rate: 1.0,
                }),
                tl_enums::CurrencyRate::CurrencyRate(tl_types::CurrencyRate {
                    currency: "EUR".to_owned(),
                    rate: 0.92,
                }),
            ],
        }),
    );
    // Queue wallet.getTransactions
    let cp = TonAddress::new(0, [0x22; 32]).to_raw();
    fake.enqueue_ok(
        tl_fns::wallet::GetTransactions::CONSTRUCTOR_ID,
        &tl_enums::wallet::Transactions::Transactions(tl_types::wallet::Transactions {
            balance: 12_500_000_000,
            transactions: vec![tl_enums::WalletTransaction::WalletTransaction(
                tl_types::WalletTransaction {
                    incoming: true,
                    gasless: false,
                    failed: false,
                    key_change: false,
                    comment_encrypted: false,
                    id: "1001:deadbeef".to_owned(),
                    amount: 3_500_000_000,
                    fee: 4_500_000,
                    date: 1_732_000_100,
                    comment: Some("MTProto tx".to_owned()),
                    tx_hash: Some("ab".repeat(32)),
                    peer: tl_enums::WalletTransactionPeer::Address(
                        tl_types::WalletTransactionPeerAddress {
                            address: cp,
                            domain: Some("alice.ton".to_owned()),
                        },
                    ),
                    nft: None,
                },
            )],
            next_offset: None,
            chats: Vec::new(),
            users: Vec::new(),
        }),
    );
    // Queue toncenter.performApiRequest responses: getAddressInformation + runGetMethod(seqno)
    enqueue_toncenter_json(
        &fake,
        &serde_json::json!({
            "ok": true,
            "result": {
                "balance": "12500000000",
                "state": "active",
                "last_transaction_id": { "lt": "1001", "hash": "deadbeef" }
            }
        }),
    );
    enqueue_toncenter_json(
        &fake,
        &serde_json::json!({
            "ok": true,
            "result": {
                "exit_code": 0,
                "stack": [["num", "0x7"]]
            }
        }),
    );

    backend.refresh().expect("refresh via MTProto");

    let ov = backend.overview();
    assert_eq!(ov.balance_nano, 12_500_000_000);
    assert_eq!(ov.seqno, 7);
    assert!((ov.usd_per_ton - 6.25).abs() < 1e-6);
    assert!(
        backend
            .fiat_rates()
            .available_currencies()
            .iter()
            .any(|c| c == "EUR")
    );
    assert_eq!(ov.history.len(), 1);
    assert_eq!(ov.history[0].comment, "MTProto tx");
    assert_eq!(ov.history[0].counterparty_address, "alice.ton");

    let backup = backend.cloud_backup_status();
    assert!(backup.backup_enabled);
    assert!(backup.can_enable_backup);

    let gasless = backend.gasless_terms().expect("gasless terms hydrated");
    assert!(gasless.is_usable());
    assert_eq!(gasless.info.left, 5);

    // Verify toncenter.performApiRequest ran on shifted webfile DC (4 + 8 * 10_000 = 80_004).
    let expected_dc = 4 + TONCENTER_DC_SHIFT * DC_SHIFT;
    let tc_calls: Vec<_> = fake
        .recorded_calls()
        .into_iter()
        .filter(|(_, ctor, _)| *ctor == tl_fns::toncenter::PerformApiRequest::CONSTRUCTOR_ID)
        .collect();
    assert_eq!(tc_calls.len(), 2);
    for (dc, _, _) in tc_calls {
        assert_eq!(dc, Some(expected_dc));
    }
}

#[test]
fn cloud_backup_enable_and_disable_call_mtproto_rpcs() {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let fake = FakeRpcClient::new();
    let (_dir, mut backend) = make_backend(&rt, WalletAppConfig::default(), None);

    let addr = backend
        .create_wallet(&test_words(), WalletVersion::V5R1, "1234")
        .expect("create wallet");
    backend.apply_synced_app_config(crate::wallet::SyncedWalletAppConfig {
        config: WalletAppConfig::default(),
        usd_per_ton: Some(6.25),
        hash: 1,
    });
    backend.attach_mtproto_rpc(ErasedRpcClient::new(fake.clone()), 4);

    // Enable cloud backup: wallet.getBackupHolderDcs -> wallet.enableBackup
    fake.enqueue_ok(
        tl_fns::wallet::GetBackupHolderDcs::CONSTRUCTOR_ID,
        &vec![
            tl_enums::wallet::HolderDc::HolderDc(tl_types::wallet::HolderDc {
                dc: 1,
                public_key: vec![0x11; 32],
            }),
            tl_enums::wallet::HolderDc::HolderDc(tl_types::wallet::HolderDc {
                dc: 2,
                public_key: vec![0x22; 32],
            }),
        ],
    );
    fake.enqueue_ok(
        tl_fns::wallet::EnableBackup::CONSTRUCTOR_ID,
        &tl_enums::WalletState::WalletState(tl_types::WalletState {
            backup_enabled: true,
            can_export_phrase: true,
            can_enable_backup: true,
            address: addr.to_raw(),
            public_key: vec![0xAA; 32],
            balance: 0,
        }),
    );

    let enabled = backend.enable_cloud_backup().expect("enable cloud backup");
    assert!(enabled.backup_enabled);
    assert_eq!(
        fake.count_ctor(tl_fns::wallet::GetBackupHolderDcs::CONSTRUCTOR_ID),
        1
    );
    assert_eq!(
        fake.count_ctor(tl_fns::wallet::EnableBackup::CONSTRUCTOR_ID),
        1
    );

    // Disable cloud backup: wallet.disableBackup
    fake.enqueue_ok(
        tl_fns::wallet::DisableBackup::CONSTRUCTOR_ID,
        &tl_enums::WalletState::WalletState(tl_types::WalletState {
            backup_enabled: false,
            can_export_phrase: false,
            can_enable_backup: true,
            address: addr.to_raw(),
            public_key: vec![0xAA; 32],
            balance: 0,
        }),
    );

    let disabled = backend
        .disable_cloud_backup(false)
        .expect("disable cloud backup");
    assert!(!disabled.backup_enabled);
    assert_eq!(
        fake.count_ctor(tl_fns::wallet::DisableBackup::CONSTRUCTOR_ID),
        1
    );
}

#[test]
fn card_stickers_and_move_to_gifts_require_confirmation_and_call_mtproto_rpcs() {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let fake = FakeRpcClient::new();
    let (_dir, mut backend) = make_backend(&rt, WalletAppConfig::default(), None);

    let own_addr = backend
        .create_wallet(&test_words(), WalletVersion::V5R1, "1234")
        .expect("create wallet");
    backend.attach_mtproto_rpc(ErasedRpcClient::new(fake.clone()), 4);

    let gift_nft = TonAddress::new(0, [0x77; 32]);
    let gift_raw = gift_nft.to_raw();
    let escrow_addr = TonAddress::new(0, [0x88; 32]);

    // 1. wallet.setCardStickers
    let coords = CardStickerCoordinates {
        x: 42.0,
        y: 50.0,
        w: 22.0,
        h: 22.0,
        rotation: 0.0,
        radius: None,
    };
    let tl_coords =
        tl_enums::MediaAreaCoordinates::MediaAreaCoordinates(tl_types::MediaAreaCoordinates {
            x: coords.x,
            y: coords.y,
            w: coords.w,
            h: coords.h,
            rotation: coords.rotation,
            radius: coords.radius,
        });
    let tl_nft = tl_enums::wallet::NftItem::NftItem(tl_types::wallet::NftItem {
        address: gift_raw.clone(),
        index: "42".to_owned(),
        owner_address: own_addr.to_raw(),
        collection_address: Some(TonAddress::new(0, [0x66; 32]).to_raw()),
        name: Some("Plush Pepe #42".to_owned()),
        description: None,
        image: None,
        image_small: None,
        content_url: None,
        lottie: None,
        attributes: None,
        extra: None,
        gift: Some(tl_enums::StarGift::Unique(tl_types::StarGiftUnique {
            require_premium: false,
            resale_ton_only: false,
            theme_available: false,
            burned: false,
            crafted: false,
            id: 42,
            gift_id: 1042,
            title: "Plush Pepe".to_owned(),
            slug: "PlushPepe-42".to_owned(),
            num: 42,
            owner_id: None,
            owner_name: None,
            owner_address: None,
            attributes: Vec::new(),
            availability_issued: 100,
            availability_total: 1000,
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
        })),
    });
    fake.enqueue_ok(
        tl_fns::wallet::SetCardStickers::CONSTRUCTOR_ID,
        &tl_enums::wallet::Card::Card(tl_types::wallet::Card {
            address: own_addr.to_raw(),
            balance: 5_000_000_000,
            stickers: Some(vec![tl_enums::wallet::CardSticker::CardSticker(
                tl_types::wallet::CardSticker {
                    coordinates: tl_coords,
                    nft: tl_nft.clone(),
                },
            )]),
            bg_nft_item: Some(tl_nft),
        }),
    );

    let card = backend
        .set_card_stickers(
            &[CardStickerInput {
                coordinates: coords,
                nft_address: gift_raw.clone(),
            }],
            Some(&gift_raw),
        )
        .expect("set_card_stickers");
    assert_eq!(card.stickers.len(), 1);
    assert_eq!(card.background_address.as_deref(), Some(gift_raw.as_str()));
    assert_eq!(
        fake.count_ctor(tl_fns::wallet::SetCardStickers::CONSTRUCTOR_ID),
        1
    );

    // 2. Move to Gifts step 1 (prepare_move_to_gifts) — builds a real TEP-62 deposit payload BOC
    //    and verifies escrow address WITHOUT calling sendBoc!
    let transfer_msg = build_nft_transfer_message(&NftTransferParams {
        nft_item: gift_nft,
        new_owner: escrow_addr,
        response_destination: own_addr,
        query_id: 99,
        attached_ton_nano: DEFAULT_NFT_GAS_NANO,
        forward_ton_nano: DEFAULT_NFT_FORWARD_NANO,
        forward_comment: None,
    })
    .expect("build TEP-62 body");
    let body_boc =
        serialize_boc(&transfer_msg.body.expect("TEP-62 body cell")).expect("serialize boc");

    fake.enqueue_ok(
        tl_fns::payments::DepositStarGift::CONSTRUCTOR_ID,
        &tl_enums::payments::StarGiftDepositPayload::StarGiftDepositPayload(
            tl_types::payments::StarGiftDepositPayload {
                nft_address: gift_raw.clone(),
                payload: body_boc,
                amount: DEFAULT_NFT_GAS_NANO as i64,
            },
        ),
    );
    // Optional emulateTrace call inside prepare_move_to_gifts
    enqueue_toncenter_json(
        &fake,
        &serde_json::json!({
            "ok": true,
            "result": { "fee": 5500000 }
        }),
    );

    let quote = backend
        .prepare_move_to_gifts(&gift_raw, "PlushPepe-42", false)
        .expect("prepare_move_to_gifts");
    assert_eq!(quote.escrow_address, escrow_addr.to_raw());
    assert_eq!(
        fake.count_ctor(tl_fns::payments::DepositStarGift::CONSTRUCTOR_ID),
        1
    );
    // Crucial safety check: prepare_move_to_gifts must NOT call payments.displayStarGift or broadcast!
    assert_eq!(
        fake.count_ctor(tl_fns::payments::DisplayStarGift::CONSTRUCTOR_ID),
        0
    );

    // 3. Explicit user confirmation (confirm_move_to_gifts) submits BOC via MTProto toncenter & calls displayStarGift.
    enqueue_toncenter_json(
        &fake,
        &serde_json::json!({
            "ok": true,
            "result": { "hash": "cafebabe" }
        }),
    );
    fake.enqueue_ok(tl_fns::payments::DisplayStarGift::CONSTRUCTOR_ID, &true);

    let receipt = backend
        .confirm_move_to_gifts(&quote, true)
        .expect("confirm_move_to_gifts");
    assert_eq!(receipt.message_hash_hex, "cafebabe");
    assert!(receipt.shown_in_gifts);
    assert_eq!(
        fake.count_ctor(tl_fns::payments::DisplayStarGift::CONSTRUCTOR_ID),
        1
    );
}
