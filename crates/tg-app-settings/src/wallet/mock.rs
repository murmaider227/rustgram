//! `MockWalletBackend` with `MockSession` demo data and real `tg-wallet` crypto/vault, plus
//! a generic `ConfiguredWalletBackend` wiring `ToncenterApi` + `TonConnectBridge`.
//!
//! Port origin: `Telegram/SourceFiles/wallet/wallet_session.cpp` and
//! `Telegram/SourceFiles/wallet/wallet_ton_connect.cpp`.
//!
//! All demo data uses strictly synthetic test keys, fictional names, and local in-memory state —
//! never real funds, never real user phrases, never real TON network requests.

use std::sync::Arc;

use tg_wallet::address::TonAddress;
use tg_wallet::contract::{TransferMessage, WalletContract, WalletVersion};
use tg_wallet::emulate::{AccountBalanceDelta, EmulationResult, JettonDelta, SendEmulationAction};
use tg_wallet::jettons::{
    DEFAULT_JETTON_FORWARD_NANO, DEFAULT_JETTON_GAS_NANO, DEFAULT_NFT_FORWARD_NANO,
    DEFAULT_NFT_GAS_NANO, JettonTransferParams, JettonWalletBalance, NftTransferParams,
    build_jetton_transfer_message, build_nft_transfer_message, format_jetton_amount,
};
use tg_wallet::mnemonic::{TonKeypair, generate_mnemonic, mnemonic_to_keypair, validate_mnemonic};
use tg_wallet::toncenter::{AccountState, NftItem, NftKind, ToncenterApi};
use tg_wallet::tonconnect::{
    DappManifest, SignDataResult, TonConnectErrorCode, TonConnectKeypair, TonConnectLink,
    TonConnectLinkKind, TonConnectSendTxRequest, TonConnectSession, TonConnectSessionStatus,
    TonConnectSignDataRequest, TonConnectTxMessage, parse_ton_connect_link, sign_ton_connect_data,
};
use tg_wallet::vault::{LocalKeyVault, PasscodeKdf};
use tg_wallet::{NANO_PER_TON, WalletAppConfig};

use super::backend::{
    CloudBackupStatus, JettonTransferQuote, MoveToGiftsQuote, MoveToGiftsReceipt, NftTransferQuote,
    OnrampHandoffSession, OnrampProviderCard, PendingConnectApproval, PendingSignDataApproval,
    PendingTxApproval, SendQuote, SendReceipt, TonConnectSessionCard, WalletBackend,
    WalletCardState, WalletCardStickerEntry, WalletOverview, WalletTxRecord, WalletTxStatus,
};
use tg_wallet::mtproto_api::{
    CardStickerInput, GaslessInfo, GaslessTerms, WALLET_CARD_MAX_STICKERS,
};

/// Default demo passcode for unlocking the pre-populated [`MockWalletBackend`] vault.
pub const DEMO_WALLET_PASSCODE: &str = "1234";

/// Public TON spec test phrase used strictly for offline demo data (zero real funds).
const DEMO_SYNTHETIC_WORDS: [&str; 24] = [
    "hospital", "stove", "relief", "fringe", "tongue", "always", "charge", "angry", "urge",
    "sentence", "again", "match", "nerve", "inquiry", "senior", "coconut", "label", "tumble",
    "carry", "category", "beauty", "bean", "road", "solution",
];

/// Estimated flat network fee in nanoTON for mock/offline fee preview (`0.0055 TON`).
const DEMO_ESTIMATED_FEE_NANO: u64 = 5_500_000;

struct StoredSession {
    card: TonConnectSessionCard,
    session: TonConnectSession,
}

/// Offline demo [`WalletBackend`] backed by real `tg-wallet` crypto, BOC signing, NaCl box
/// TON Connect sessions, and [`LocalKeyVault`] encryption, with synthetic `MockSession` data.
pub struct MockWalletBackend {
    app_config: WalletAppConfig,
    vault: LocalKeyVault,
    version: WalletVersion,
    contract: Option<WalletContract>,
    balance_nano: u64,
    account_state: AccountState,
    usd_per_ton: f64,
    seqno: u32,
    history: Vec<WalletTxRecord>,
    sessions: Vec<StoredSession>,
    next_session_id: u64,
    pending_connect: Option<PendingConnectApproval>,
    pending_tx_queue: std::collections::VecDeque<PendingTxApproval>,
    pending_sign_data_queue: std::collections::VecDeque<PendingSignDataApproval>,
    collectibles: Vec<NftItem>,
    jettons: Vec<JettonWalletBalance>,
    onramp_providers: Vec<OnrampProviderCard>,
    cloud_backup: CloudBackupStatus,
    gasless: Option<GaslessTerms>,
    card_state: WalletCardState,
    displayed_gifts: Vec<String>,
    now_unix: i64,
}

impl Default for MockWalletBackend {
    fn default() -> Self {
        Self::with_demo_wallet()
    }
}

impl MockWalletBackend {
    /// Creates an empty (uninitialized) mock backend for testing Create / Import onboarding flows.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            app_config: WalletAppConfig::default(),
            vault: LocalKeyVault::new(1),
            version: WalletVersion::V5R1,
            contract: None,
            balance_nano: 0,
            account_state: AccountState::Uninitialized,
            usd_per_ton: 5.40,
            seqno: 0,
            history: Vec::new(),
            sessions: Vec::new(),
            next_session_id: 1,
            pending_connect: None,
            pending_tx_queue: std::collections::VecDeque::new(),
            pending_sign_data_queue: std::collections::VecDeque::new(),
            collectibles: Vec::new(),
            jettons: Vec::new(),
            onramp_providers: Vec::new(),
            cloud_backup: CloudBackupStatus::default(),
            gasless: None,
            card_state: WalletCardState::default(),
            displayed_gifts: Vec::new(),
            now_unix: 1_732_000_000,
        }
    }

    /// Creates a pre-populated `MockSession` demo wallet with fictional history & TON Connect apps.
    #[must_use]
    pub fn with_demo_wallet() -> Self {
        let mut backend = Self::empty();
        let words: Vec<String> = DEMO_SYNTHETIC_WORDS
            .iter()
            .map(|w| (*w).to_owned())
            .collect();
        let _ = backend.create_wallet(&words, WalletVersion::V5R1, DEMO_WALLET_PASSCODE);
        backend.balance_nano = 14_750_000_000; // 14.75 TON (synthetic demo balance)
        backend.account_state = AccountState::Active;
        backend.seqno = 3;
        let own_raw = backend
            .contract
            .as_ref()
            .and_then(|c| c.address().ok())
            .map(|a| a.to_raw());
        backend.cloud_backup = CloudBackupStatus {
            backup_enabled: false,
            can_enable_backup: true,
            can_export_phrase: true,
            holder_dc_count: 3,
        };
        backend.gasless = Some(GaslessTerms::compute(
            GaslessInfo {
                available: true,
                left: 3,
                reset_at: 1_732_086_400,
                min_amount: 100_000_000,
                relayer_address: TonAddress::new(0, [0xE1; 32]).to_raw(),
            },
            own_raw,
            1,
            100_000_000,
        ));
        backend.card_state = WalletCardState {
            ready: true,
            ..WalletCardState::default()
        };

        let mira_addr = TonAddress::new(0, [0x31; 32]).to_friendly(false, false);
        let theo_addr = TonAddress::new(0, [0x52; 32]).to_friendly(false, false);
        let dapp_addr = TonAddress::new(0, [0x74; 32]).to_friendly(true, false);

        backend.history = vec![
            WalletTxRecord {
                id: "demo_tx_03".to_owned(),
                incoming: true,
                counterparty_address: mira_addr,
                counterparty_label: "Mira Solis".to_owned(),
                amount_nano: 8_500_000_000,
                fee_nano: Some(4_100_000),
                comment: "Design sprint reimbursement".to_owned(),
                timestamp: 1_731_998_400,
                status: WalletTxStatus::Success,
            },
            WalletTxRecord {
                id: "demo_tx_02".to_owned(),
                incoming: false,
                counterparty_address: theo_addr,
                counterparty_label: "Theo Vance".to_owned(),
                amount_nano: 2_250_000_000,
                fee_nano: Some(5_200_000),
                comment: "Lunch split".to_owned(),
                timestamp: 1_731_982_000,
                status: WalletTxStatus::Success,
            },
            WalletTxRecord {
                id: "demo_tx_01".to_owned(),
                incoming: true,
                counterparty_address: dapp_addr.clone(),
                counterparty_label: "Demo Swap dApp".to_owned(),
                amount_nano: 8_500_000_000,
                fee_nano: Some(3_900_000),
                comment: "Demo liquidity return".to_owned(),
                timestamp: 1_731_900_000,
                status: WalletTxStatus::Success,
            },
        ];

        // Add one active synthetic TON Connect dApp session and one pending transaction approval
        // so the user can inspect and test TON Connect approval boxes immediately in demo mode.
        let dapp_kp = TonConnectKeypair::generate();
        let wallet_kp = TonConnectKeypair::generate();
        let manifest = DappManifest {
            url: "https://demoswap.example".to_owned(),
            name: "Demo Swap".to_owned(),
            icon_url: "https://demoswap.example/icon.png".to_owned(),
        };
        if let Ok(mut tc_session) =
            TonConnectSession::new(wallet_kp, &dapp_kp.client_id(), manifest.clone())
        {
            tc_session.status = TonConnectSessionStatus::Active;
            let session_id = backend.next_session_id;
            backend.next_session_id += 1;
            backend.sessions.push(StoredSession {
                card: TonConnectSessionCard {
                    session_id,
                    dapp_client_id: dapp_kp.client_id(),
                    manifest,
                    status: TonConnectSessionStatus::Active,
                    connected_at: 1_731_950_000,
                },
                session: tc_session,
            });
            let wallet_raw_for_emul = backend
                .contract
                .as_ref()
                .and_then(|c| c.address().ok())
                .map(|a| a.to_raw())
                .unwrap_or_else(|| format!("0:{}", "11".repeat(32)));
            let dapp_raw = TonAddress::new(0, [0x74; 32]).to_raw();
            let demo_emul = EmulationResult {
                mc_block_seqno: Some(42_100_000),
                root_tx_hash: "demo_emul_root_01".to_owned(),
                root_account: wallet_raw_for_emul.clone(),
                root_fee_nano: 4_800_000,
                total_fees_nano: 4_800_000,
                total_gas_fees_nano: 3_100_000,
                total_storage_fees_nano: 200_000,
                total_fwd_fees_nano: 1_500_000,
                is_incomplete: false,
                trace_succeeded: true,
                transactions: Vec::new(),
                account_deltas: vec![AccountBalanceDelta {
                    account: wallet_raw_for_emul.clone(),
                    ton_delta_nano: -1_204_800_000,
                    jetton_deltas: vec![JettonDelta {
                        jetton_master: TonAddress::new(0, [0xD1; 32]).to_raw(),
                        jetton_wallet: Some(TonAddress::new(0, [0xB1; 32]).to_raw()),
                        delta: 6_420_000,
                        symbol: Some("USDt".to_owned()),
                        decimals: Some(6),
                    }],
                    nft_deltas: Vec::new(),
                }],
                warnings: Vec::new(),
                raw_actions: vec![SendEmulationAction {
                    kind: "ton_transfer".to_owned(),
                    succeeded: true,
                    details_json: serde_json::json!({
                        "source": wallet_raw_for_emul,
                        "destination": dapp_raw,
                        "value": "1200000000"
                    })
                    .to_string(),
                }],
            };
            let demo_tc_emul =
                demo_emul.to_ton_connect_emulation(&wallet_raw_for_emul, 1_200_000_000);
            backend.pending_tx_queue.push_back(PendingTxApproval {
                session_id,
                dapp_name: "Demo Swap".to_owned(),
                dapp_domain: "demoswap.example".to_owned(),
                request: TonConnectSendTxRequest {
                    request_id: "demo-req-1".to_owned(),
                    valid_until: Some(1_890_000_000),
                    network: Some("-239".to_owned()),
                    from: None,
                    messages: vec![TonConnectTxMessage {
                        address: dapp_addr,
                        amount_nano: 1_200_000_000,
                        payload_boc: None,
                        decoded_comment: Some("Demo swap deposit".to_owned()),
                        state_init_boc: None,
                    }],
                },
                estimated_fee_nano: 4_800_000,
                emulation: Some(demo_emul),
                ton_connect_emulation: Some(demo_tc_emul),
            });
            backend
                .pending_sign_data_queue
                .push_back(PendingSignDataApproval {
                    session_id,
                    dapp_name: "Demo Swap".to_owned(),
                    dapp_domain: "demoswap.example".to_owned(),
                    request: TonConnectSignDataRequest {
                        request_id: "demo-sign-1".to_owned(),
                        kind: "text".to_owned(),
                        payload: "Authorize Demo Swap limit order #4821 (1.2 TON -> 6.42 USDt)"
                            .to_owned(),
                        schema: None,
                    },
                });
        }

        let owner_raw = backend
            .contract
            .as_ref()
            .and_then(|c| c.address().ok())
            .map(|a| a.to_raw())
            .unwrap_or_else(|| format!("0:{}", "11".repeat(32)));

        backend.collectibles = vec![
            NftItem {
                address: TonAddress::new(0, [0xA1; 32]).to_raw(),
                collection: "0:80d78a35f955a14b679faa887ff4cd5bfc0f43b4a4eea2a7e6927f3701b273c2"
                    .to_owned(),
                owner: owner_raw.clone(),
                index: "1001".to_owned(),
                name: "@aurora_demo".to_owned(),
                collection_name: "Telegram Usernames".to_owned(),
                content_uri: "https://nft.fragment.com/username/aurora_demo.json".to_owned(),
                domain: "aurora_demo.t.me".to_owned(),
                key: "aurora_demo".to_owned(),
                on_sale: false,
                kind: NftKind::TelegramUsername,
            },
            NftItem {
                address: TonAddress::new(0, [0xA2; 32]).to_raw(),
                collection: "0:0e41dc1dc3c9067ed24248580e12b3359818d83dee0304fabcf80845eafafdb2"
                    .to_owned(),
                owner: owner_raw.clone(),
                index: "1002".to_owned(),
                name: "+888 0123 4567".to_owned(),
                collection_name: "Anonymous Telegram Numbers".to_owned(),
                content_uri: "https://nft.fragment.com/number/88801234567.json".to_owned(),
                domain: String::new(),
                key: "88801234567".to_owned(),
                on_sale: false,
                kind: NftKind::TelegramNumber,
            },
            NftItem {
                address: TonAddress::new(0, [0xA3; 32]).to_raw(),
                collection: TonAddress::new(0, [0xC3; 32]).to_raw(),
                owner: owner_raw.clone(),
                index: "45754".to_owned(),
                name: "Desk Calendar #45754".to_owned(),
                collection_name: "Telegram Gifts".to_owned(),
                content_uri: "https://nft.fragment.com/gift/deskcalendar-45754.json".to_owned(),
                domain: String::new(),
                key: "deskcalendar-45754".to_owned(),
                on_sale: false,
                kind: NftKind::TelegramGift,
            },
        ];

        backend.jettons = vec![
            JettonWalletBalance {
                wallet_address: TonAddress::new(0, [0xB1; 32]).to_raw(),
                jetton_master: TonAddress::new(0, [0xD1; 32]).to_raw(),
                owner: owner_raw.clone(),
                balance: 250_500_000, // 250.5 USDt (6 decimals)
                last_transaction_lt: 95_000_001,
                symbol: "USDt".to_owned(),
                name: "Tether USD (Demo)".to_owned(),
                decimals: 6,
                user_friendly_wallet: TonAddress::new(0, [0xB1; 32]).to_friendly(true, false),
                user_friendly_master: TonAddress::new(0, [0xD1; 32]).to_friendly(true, false),
            },
            JettonWalletBalance {
                wallet_address: TonAddress::new(0, [0xB2; 32]).to_raw(),
                jetton_master: TonAddress::new(0, [0xD2; 32]).to_raw(),
                owner: owner_raw,
                balance: 42_000_000_000_000, // 42,000 NOT (9 decimals)
                last_transaction_lt: 94_000_001,
                symbol: "NOT".to_owned(),
                name: "Notcoin (Demo)".to_owned(),
                decimals: 9,
                user_friendly_wallet: TonAddress::new(0, [0xB2; 32]).to_friendly(true, false),
                user_friendly_master: TonAddress::new(0, [0xD2; 32]).to_friendly(true, false),
            },
        ];

        backend.onramp_providers = vec![OnrampProviderCard {
            id: "demo_onramp".to_owned(),
            name: "Demo Fiat Onramp (Read-Only)".to_owned(),
            crypto_currencies: vec!["GRAM".to_owned(), "TON".to_owned()],
            supports_base_currencies: true,
            supports_limits: true,
            supports_quote: true,
        }];

        backend
    }

    /// Triggers a synthetic pending TON Connect connect-approval dialog for UI testing.
    pub fn queue_demo_connect_approval(&mut self) {
        let dapp_kp = TonConnectKeypair::generate();
        let link = TonConnectLink {
            kind: TonConnectLinkKind::Connect,
            client_id: dapp_kp.client_id(),
            manifest_url: "https://fragment-demo.example/tonconnect-manifest.json".to_owned(),
            proof_payload: Some("demo-proof-nonce-2026".to_owned()),
            ret: "none".to_owned(),
            trace_id: None,
        };
        self.pending_connect = Some(PendingConnectApproval {
            link,
            manifest: DappManifest {
                url: "https://fragment-demo.example".to_owned(),
                name: "Demo Collectibles".to_owned(),
                icon_url: String::new(),
            },
            requests_proof: true,
        });
    }

    /// Returns the list of collectible gift addresses marked as displayed in My Gifts.
    #[must_use]
    pub fn displayed_gifts(&self) -> &[String] {
        &self.displayed_gifts
    }

    fn load_keypair(&self) -> Result<TonKeypair, String> {
        let secret = self
            .vault
            .read_secret("seed")
            .map_err(|e| format!("Vault locked or unreadable: {e}"))?;
        let seed: [u8; 32] = secret
            .as_slice()
            .try_into()
            .map_err(|_| "Corrupt seed in vault".to_owned())?;
        Ok(TonKeypair::from_seed(seed))
    }
}

impl WalletBackend for MockWalletBackend {
    fn app_config(&self) -> &WalletAppConfig {
        &self.app_config
    }

    fn set_app_config(&mut self, config: WalletAppConfig) {
        self.app_config = config;
    }

    fn overview(&self) -> WalletOverview {
        WalletOverview {
            has_wallet: self.contract.is_some(),
            locked: self.vault.is_initialized() && !self.vault.is_unlocked(),
            version: self.version,
            address: self.contract.as_ref().and_then(|c| c.address().ok()),
            balance_nano: self.balance_nano,
            account_state: self.account_state,
            usd_per_ton: self.usd_per_ton,
            seqno: self.seqno,
            history: self.history.clone(),
        }
    }

    fn refresh(&mut self) -> Result<(), String> {
        // Offline demo backend keeps deterministic state.
        Ok(())
    }

    fn generate_phrase(&mut self) -> Result<Vec<String>, String> {
        generate_mnemonic("").map_err(|e| e.to_string())
    }

    fn create_wallet(
        &mut self,
        words: &[String],
        version: WalletVersion,
        passcode: &str,
    ) -> Result<TonAddress, String> {
        if passcode.len() < 4 {
            return Err("Passcode must be at least 4 characters".to_owned());
        }
        if !validate_mnemonic(words, "") {
            return Err("Recovery phrase failed TON validation".to_owned());
        }
        let kp = mnemonic_to_keypair(words, "").map_err(|e| e.to_string())?;
        self.vault
            .initialize_with_passcode(passcode, PasscodeKdf::fast_test_kdf())
            .map_err(|e| e.to_string())?;
        self.vault
            .store_secret("seed", kp.seed_bytes())
            .map_err(|e| e.to_string())?;
        self.vault
            .store_secret("mnemonic", words.join(" ").as_bytes())
            .map_err(|e| e.to_string())?;
        let contract =
            WalletContract::new(version, 0, kp.public_key_bytes(), self.app_config.testnet);
        let addr = contract.address().map_err(|e| e.to_string())?;
        self.version = version;
        self.contract = Some(contract);
        self.balance_nano = 5 * NANO_PER_TON; // Synthetic faucet balance for newly created demo wallets.
        self.account_state = AccountState::Uninitialized;
        self.seqno = 0;
        Ok(addr)
    }

    fn import_wallet(
        &mut self,
        words: &[String],
        password: &str,
        version: WalletVersion,
        passcode: &str,
    ) -> Result<TonAddress, String> {
        if passcode.len() < 4 {
            return Err("Passcode must be at least 4 characters".to_owned());
        }
        if !validate_mnemonic(words, password) {
            return Err("Invalid 24-word TON recovery phrase".to_owned());
        }
        let kp = mnemonic_to_keypair(words, password).map_err(|e| e.to_string())?;
        self.vault
            .initialize_with_passcode(passcode, PasscodeKdf::fast_test_kdf())
            .map_err(|e| e.to_string())?;
        self.vault
            .store_secret("seed", kp.seed_bytes())
            .map_err(|e| e.to_string())?;
        self.vault
            .store_secret("mnemonic", words.join(" ").as_bytes())
            .map_err(|e| e.to_string())?;
        let contract =
            WalletContract::new(version, 0, kp.public_key_bytes(), self.app_config.testnet);
        let addr = contract.address().map_err(|e| e.to_string())?;
        self.version = version;
        self.contract = Some(contract);
        self.balance_nano = 10 * NANO_PER_TON;
        self.account_state = AccountState::Active;
        self.seqno = 1;
        Ok(addr)
    }

    fn lock_vault(&mut self) {
        self.vault.lock();
    }

    fn unlock_vault(&mut self, passcode: &str) -> Result<(), String> {
        self.vault.unlock(passcode).map_err(|e| e.to_string())
    }

    fn reveal_phrase(&self) -> Result<Vec<String>, String> {
        let bytes = self
            .vault
            .read_secret("mnemonic")
            .map_err(|e| e.to_string())?;
        let text = std::str::from_utf8(bytes.as_slice())
            .map_err(|_| "Corrupt mnemonic UTF-8".to_owned())?;
        Ok(text.split_whitespace().map(ToOwned::to_owned).collect())
    }

    fn prepare_send(
        &self,
        destination_input: &str,
        amount_nano: u64,
        comment: &str,
        bounce_override: Option<bool>,
    ) -> Result<SendQuote, String> {
        if self.contract.is_none() {
            return Err("No wallet configured".to_owned());
        }
        let parsed = TonAddress::parse(destination_input).map_err(|e| e.to_string())?;
        if amount_nano == 0 {
            return Err("Transfer amount must be greater than zero".to_owned());
        }
        if comment.len() > 256 {
            return Err("Comment exceeds 256-byte limit".to_owned());
        }
        let total = amount_nano.saturating_add(DEMO_ESTIMATED_FEE_NANO);
        if total > self.balance_nano {
            return Err("Insufficient TON balance (including network fee)".to_owned());
        }
        Ok(SendQuote {
            destination: parsed.address,
            bounce: bounce_override.unwrap_or(parsed.bounceable),
            amount_nano,
            fee_nano: DEMO_ESTIMATED_FEE_NANO,
            comment: comment.to_owned(),
        })
    }

    fn confirm_send(&mut self, quote: &SendQuote) -> Result<SendReceipt, String> {
        let contract = self
            .contract
            .as_ref()
            .ok_or_else(|| "No wallet configured".to_owned())?;
        let kp = self.load_keypair()?;
        let msg = TransferMessage::simple(
            quote.destination,
            quote.amount_nano,
            quote.bounce,
            (!quote.comment.is_empty()).then_some(quote.comment.as_str()),
        )
        .map_err(|e| e.to_string())?;

        let valid_until = (self.now_unix as u32).saturating_add(300);
        let signed = contract
            .create_signed_transfer(&kp, self.seqno, valid_until, &[msg])
            .map_err(|e| e.to_string())?;
        let hash_hex = encode_hex(&signed.message_hash);

        let total = quote.amount_nano.saturating_add(quote.fee_nano);
        self.balance_nano = self.balance_nano.saturating_sub(total);
        self.seqno = self.seqno.saturating_add(1);
        self.account_state = AccountState::Active;
        self.now_unix += 15;

        self.history.insert(
            0,
            WalletTxRecord {
                id: hash_hex.clone(),
                incoming: false,
                counterparty_address: quote
                    .destination
                    .to_friendly(quote.bounce, self.app_config.testnet),
                counterparty_label: String::new(),
                amount_nano: quote.amount_nano,
                fee_nano: Some(quote.fee_nano),
                comment: quote.comment.clone(),
                timestamp: self.now_unix,
                status: WalletTxStatus::Success,
            },
        );

        Ok(SendReceipt {
            message_hash_hex: hash_hex,
            signed_boc_base64: signed.boc_base64,
        })
    }

    fn ton_connect_sessions(&self) -> Vec<TonConnectSessionCard> {
        self.sessions.iter().map(|s| s.card.clone()).collect()
    }

    fn pending_connect(&self) -> Option<PendingConnectApproval> {
        self.pending_connect.clone()
    }

    fn pending_tx_approval(&self) -> Option<PendingTxApproval> {
        self.pending_tx_queue.front().cloned()
    }

    fn handle_ton_connect_link(&mut self, raw_link: &str) -> Result<(), String> {
        let link = parse_ton_connect_link(raw_link).map_err(|e| e.to_string())?;
        if link.kind != TonConnectLinkKind::Connect {
            return Err("Expected a TON Connect v2 connect link (`v=2&id=...&r=...`)".to_owned());
        }
        let host = {
            let probe = DappManifest {
                url: link.manifest_url.clone(),
                name: String::new(),
                icon_url: String::new(),
            };
            let h = probe.host();
            if h.is_empty() {
                "unknown.dapp".to_owned()
            } else {
                h
            }
        };
        let manifest = DappManifest {
            url: format!("https://{host}"),
            name: host.clone(),
            icon_url: String::new(),
        };
        self.pending_connect = Some(PendingConnectApproval {
            requests_proof: link.proof_payload.is_some(),
            link,
            manifest,
        });
        Ok(())
    }

    fn approve_ton_connect(&mut self) -> Result<TonConnectSessionCard, String> {
        let pending = self
            .pending_connect
            .take()
            .ok_or_else(|| "No pending TON Connect request".to_owned())?;
        let contract = self
            .contract
            .as_ref()
            .ok_or_else(|| "Create or import a wallet first".to_owned())?;
        let kp = self.load_keypair()?;
        let wallet_tc_kp = TonConnectKeypair::generate();
        let mut tc_session = TonConnectSession::new(
            wallet_tc_kp,
            &pending.link.client_id,
            pending.manifest.clone(),
        )
        .map_err(|e| e.to_string())?;
        // Build the real NaCl-encrypted connect event (including ton_proof when requested).
        let _encrypted_reply = tc_session
            .build_connect_event(
                self.next_session_id,
                contract,
                &kp,
                pending.link.proof_payload.as_deref(),
                self.now_unix as u64,
            )
            .map_err(|e| e.to_string())?;
        tc_session.status = TonConnectSessionStatus::Active;

        let session_id = self.next_session_id;
        self.next_session_id += 1;
        let card = TonConnectSessionCard {
            session_id,
            dapp_client_id: pending.link.client_id,
            manifest: pending.manifest,
            status: TonConnectSessionStatus::Active,
            connected_at: self.now_unix,
        };
        self.sessions.push(StoredSession {
            card: card.clone(),
            session: tc_session,
        });
        Ok(card)
    }

    fn decline_ton_connect(&mut self) {
        self.pending_connect = None;
    }

    fn approve_ton_connect_tx(
        &mut self,
        request_id: &str,
        content_hash: &str,
    ) -> Result<SendReceipt, String> {
        let pending = self
            .pending_tx_queue
            .front()
            .cloned()
            .ok_or_else(|| "No pending TON Connect transaction".to_owned())?;
        if pending.request.request_id != request_id || pending.content_hash() != content_hash {
            return Err(
                "Pending TON Connect transaction changed before approval; review the current request"
                    .to_owned(),
            );
        }
        let contract = self
            .contract
            .as_ref()
            .ok_or_else(|| "No wallet configured".to_owned())?;
        let expected_network = if contract.testnet { "-3" } else { "-239" };
        if let Some(ref net) = pending.request.network
            && net.trim() != expected_network
        {
            return Err(format!(
                "Transaction network '{}' does not match active wallet network '{}'",
                net.trim(),
                expected_network
            ));
        }
        let now_u64 = self.now_unix.max(0) as u64;
        if let Some(vu) = pending.request.valid_until
            && vu <= now_u64
        {
            return Err("Transaction request has expired".to_owned());
        }
        if let Some(ref from_str) = pending.request.from {
            let parsed_from = TonAddress::parse(from_str.trim()).map_err(|e| e.to_string())?;
            let wallet_addr = contract.address().map_err(|e| e.to_string())?;
            if parsed_from.address.to_raw() != wallet_addr.to_raw() {
                return Err("Transaction 'from' address does not match active wallet".to_owned());
            }
        }
        let kp = self.load_keypair()?;

        let mut msgs = Vec::with_capacity(pending.request.messages.len());
        for m in &pending.request.messages {
            let parsed = TonAddress::parse(&m.address).map_err(|e| e.to_string())?;
            msgs.push(
                TransferMessage::simple(
                    parsed.address,
                    m.amount_nano,
                    parsed.bounceable,
                    m.decoded_comment.as_deref(),
                )
                .map_err(|e| e.to_string())?,
            );
        }
        let valid_until = pending
            .request
            .valid_until
            .unwrap_or(now_u64.saturating_add(300))
            .min(u64::from(u32::MAX)) as u32;
        let signed = contract
            .create_signed_transfer(&kp, self.seqno, valid_until, &msgs)
            .map_err(|e| e.to_string())?;

        // Build encrypted bridge reply using the session's NaCl box.
        if let Some(stored) = self
            .sessions
            .iter()
            .find(|s| s.card.session_id == pending.session_id)
        {
            let _encrypted_resp = stored
                .session
                .build_tx_success_response(&pending.request.request_id, &signed.boc_base64)
                .map_err(|e| e.to_string())?;
        }

        let total = pending
            .request
            .total_nano()
            .saturating_add(pending.estimated_fee_nano);
        self.balance_nano = self.balance_nano.saturating_sub(total);
        self.seqno = self.seqno.saturating_add(1);
        self.now_unix += 15;
        let hash_hex = encode_hex(&signed.message_hash);

        let first_dest = pending
            .request
            .messages
            .first()
            .map(|m| m.address.clone())
            .unwrap_or_default();
        let first_comment = pending
            .request
            .messages
            .first()
            .and_then(|m| m.decoded_comment.clone())
            .unwrap_or_default();

        self.history.insert(
            0,
            WalletTxRecord {
                id: hash_hex.clone(),
                incoming: false,
                counterparty_address: first_dest,
                counterparty_label: pending.dapp_name,
                amount_nano: pending.request.total_nano(),
                fee_nano: Some(pending.estimated_fee_nano),
                comment: first_comment,
                timestamp: self.now_unix,
                status: WalletTxStatus::Success,
            },
        );
        self.pending_tx_queue.pop_front();

        Ok(SendReceipt {
            message_hash_hex: hash_hex,
            signed_boc_base64: signed.boc_base64,
        })
    }

    fn decline_ton_connect_tx(&mut self) {
        if let Some(pending) = self.pending_tx_queue.pop_front()
            && let Some(stored) = self
                .sessions
                .iter()
                .find(|s| s.card.session_id == pending.session_id)
        {
            let _ = stored.session.build_error_response(
                &pending.request.request_id,
                TonConnectErrorCode::UserDeclined,
                "User declined transaction",
            );
        }
    }

    fn pending_sign_data(&self) -> Option<PendingSignDataApproval> {
        self.pending_sign_data_queue.front().cloned()
    }

    fn approve_ton_connect_sign_data(
        &mut self,
        request_id: &str,
        content_hash: &str,
    ) -> Result<SignDataResult, String> {
        let pending = self
            .pending_sign_data_queue
            .front()
            .cloned()
            .ok_or_else(|| "No pending TON Connect signData request".to_owned())?;
        if pending.request.request_id != request_id || pending.content_hash() != content_hash {
            return Err(
                "Pending TON Connect signData request changed before approval; review the current request"
                    .to_owned(),
            );
        }
        if !tg_wallet::tonconnect::ton_connect_proof_domain_allowed(&pending.dapp_domain, "") {
            return Err(format!(
                "TON Connect signData domain '{}' is not allowed",
                pending.dapp_domain
            ));
        }
        let contract = self
            .contract
            .as_ref()
            .ok_or_else(|| "No wallet configured".to_owned())?;
        let kp = self.load_keypair()?;
        let addr = contract.address().map_err(|e| e.to_string())?;
        let payload = pending
            .request
            .to_sign_data_payload()
            .map_err(|e| e.to_string())?;
        let signed = sign_ton_connect_data(
            &kp,
            &addr,
            &pending.dapp_domain,
            self.now_unix as u64,
            &payload,
        )
        .map_err(|e| e.to_string())?;
        if let Some(stored) = self
            .sessions
            .iter()
            .find(|s| s.card.session_id == pending.session_id)
        {
            let _ = stored
                .session
                .build_sign_data_success_response(&pending.request.request_id, &signed)
                .map_err(|e| e.to_string())?;
        }
        self.pending_sign_data_queue.pop_front();
        Ok(signed)
    }

    fn decline_ton_connect_sign_data(&mut self) {
        if let Some(pending) = self.pending_sign_data_queue.pop_front()
            && let Some(stored) = self
                .sessions
                .iter()
                .find(|s| s.card.session_id == pending.session_id)
        {
            let _ = stored.session.build_error_response(
                &pending.request.request_id,
                TonConnectErrorCode::UserDeclined,
                "User declined signData request",
            );
        }
    }

    fn disconnect_ton_connect(&mut self, session_id: u64) -> Result<(), String> {
        let Some(pos) = self
            .sessions
            .iter()
            .position(|s| s.card.session_id == session_id)
        else {
            return Err("Session not found".to_owned());
        };
        let stored = self.sessions.remove(pos);
        let _ = stored.session.build_disconnect_event(session_id);
        self.pending_tx_queue.retain(|p| p.session_id != session_id);
        self.pending_sign_data_queue
            .retain(|p| p.session_id != session_id);
        Ok(())
    }

    fn collectibles(&self) -> Vec<NftItem> {
        self.collectibles.clone()
    }

    fn jetton_balances(&self) -> Vec<JettonWalletBalance> {
        self.jettons.clone()
    }

    fn refresh_assets(&mut self) -> Result<(), String> {
        Ok(())
    }

    fn prepare_jetton_transfer(
        &self,
        jetton_wallet_addr: &str,
        destination_input: &str,
        jetton_amount: u128,
        comment: &str,
    ) -> Result<JettonTransferQuote, String> {
        if self.contract.is_none() {
            return Err("No wallet configured".to_owned());
        }
        let token = self
            .jettons
            .iter()
            .find(|t| {
                t.wallet_address.eq_ignore_ascii_case(jetton_wallet_addr)
                    || t.user_friendly_wallet == jetton_wallet_addr
            })
            .ok_or_else(|| "Jetton wallet not found".to_owned())?;
        if jetton_amount == 0 {
            return Err("Jetton amount must be greater than zero".to_owned());
        }
        if jetton_amount > token.balance {
            return Err("Insufficient Jetton balance".to_owned());
        }
        let jw = TonAddress::parse(&token.wallet_address)
            .map_err(|e| e.to_string())?
            .address;
        let dest = TonAddress::parse(destination_input)
            .map_err(|e| e.to_string())?
            .address;
        let needed_ton = DEFAULT_JETTON_GAS_NANO.saturating_add(DEMO_ESTIMATED_FEE_NANO);
        if needed_ton > self.balance_nano {
            return Err("Insufficient TON balance for Jetton transfer gas".to_owned());
        }
        Ok(JettonTransferQuote {
            jetton_wallet: jw,
            jetton_master: token.jetton_master.clone(),
            symbol: token.display_symbol(),
            decimals: token.decimals,
            destination: dest,
            jetton_amount,
            attached_ton_nano: DEFAULT_JETTON_GAS_NANO,
            fee_nano: DEMO_ESTIMATED_FEE_NANO,
            comment: comment.to_owned(),
        })
    }

    fn confirm_jetton_transfer(
        &mut self,
        quote: &JettonTransferQuote,
    ) -> Result<SendReceipt, String> {
        let contract = self
            .contract
            .as_ref()
            .ok_or_else(|| "No wallet configured".to_owned())?;
        let kp = self.load_keypair()?;
        let self_addr = contract.address().map_err(|e| e.to_string())?;
        let msg = build_jetton_transfer_message(&JettonTransferParams {
            jetton_wallet: quote.jetton_wallet,
            destination: quote.destination,
            response_destination: self_addr,
            jetton_amount: quote.jetton_amount,
            query_id: self.now_unix as u64,
            attached_ton_nano: quote.attached_ton_nano,
            forward_ton_nano: DEFAULT_JETTON_FORWARD_NANO,
            forward_comment: (!quote.comment.is_empty()).then(|| quote.comment.clone()),
        })
        .map_err(|e| e.to_string())?;
        let valid_until = (self.now_unix + 300) as u32;
        let signed = contract
            .create_signed_transfer(&kp, self.seqno, valid_until, &[msg])
            .map_err(|e| e.to_string())?;

        if let Some(token) = self.jettons.iter_mut().find(|t| {
            t.wallet_address
                .eq_ignore_ascii_case(&quote.jetton_wallet.to_raw())
        }) {
            token.balance = token.balance.saturating_sub(quote.jetton_amount);
        }
        self.balance_nano = self
            .balance_nano
            .saturating_sub(quote.attached_ton_nano.saturating_add(quote.fee_nano));
        self.seqno = self.seqno.saturating_add(1);
        self.now_unix += 15;
        let hash_hex = encode_hex(&signed.message_hash);
        self.history.insert(
            0,
            WalletTxRecord {
                id: hash_hex.clone(),
                incoming: false,
                counterparty_address: quote
                    .destination
                    .to_friendly(false, self.app_config.testnet),
                counterparty_label: format!(
                    "Jetton {} {}",
                    format_jetton_amount(quote.jetton_amount, quote.decimals),
                    quote.symbol
                ),
                amount_nano: quote.attached_ton_nano,
                fee_nano: Some(quote.fee_nano),
                comment: quote.comment.clone(),
                timestamp: self.now_unix,
                status: WalletTxStatus::Success,
            },
        );
        Ok(SendReceipt {
            message_hash_hex: hash_hex,
            signed_boc_base64: signed.boc_base64,
        })
    }

    fn prepare_nft_transfer(
        &self,
        nft_address: &str,
        new_owner_input: &str,
        comment: &str,
    ) -> Result<NftTransferQuote, String> {
        if self.contract.is_none() {
            return Err("No wallet configured".to_owned());
        }
        let item = self
            .collectibles
            .iter()
            .find(|n| n.address.eq_ignore_ascii_case(nft_address))
            .ok_or_else(|| "Collectible NFT not found in wallet".to_owned())?;
        let nft_addr = TonAddress::parse(&item.address)
            .map_err(|e| e.to_string())?
            .address;
        let new_owner = TonAddress::parse(new_owner_input)
            .map_err(|e| e.to_string())?
            .address;
        let needed_ton = DEFAULT_NFT_GAS_NANO.saturating_add(DEMO_ESTIMATED_FEE_NANO);
        if needed_ton > self.balance_nano {
            return Err("Insufficient TON balance for NFT transfer gas".to_owned());
        }
        Ok(NftTransferQuote {
            nft_address: nft_addr,
            nft_name: super::collectibles::nft_display_title(item),
            collection_name: item.collection_name.clone(),
            new_owner,
            attached_ton_nano: DEFAULT_NFT_GAS_NANO,
            fee_nano: DEMO_ESTIMATED_FEE_NANO,
            comment: comment.to_owned(),
        })
    }

    fn confirm_nft_transfer(&mut self, quote: &NftTransferQuote) -> Result<SendReceipt, String> {
        let contract = self
            .contract
            .as_ref()
            .ok_or_else(|| "No wallet configured".to_owned())?;
        let kp = self.load_keypair()?;
        let self_addr = contract.address().map_err(|e| e.to_string())?;
        let msg = build_nft_transfer_message(&NftTransferParams {
            nft_item: quote.nft_address,
            new_owner: quote.new_owner,
            response_destination: self_addr,
            query_id: self.now_unix as u64,
            attached_ton_nano: quote.attached_ton_nano,
            forward_ton_nano: DEFAULT_NFT_FORWARD_NANO,
            forward_comment: (!quote.comment.is_empty()).then(|| quote.comment.clone()),
        })
        .map_err(|e| e.to_string())?;
        let valid_until = (self.now_unix + 300) as u32;
        let signed = contract
            .create_signed_transfer(&kp, self.seqno, valid_until, &[msg])
            .map_err(|e| e.to_string())?;

        let raw_nft = quote.nft_address.to_raw();
        self.collectibles
            .retain(|n| !n.address.eq_ignore_ascii_case(&raw_nft));
        self.balance_nano = self
            .balance_nano
            .saturating_sub(quote.attached_ton_nano.saturating_add(quote.fee_nano));
        self.seqno = self.seqno.saturating_add(1);
        self.now_unix += 15;
        let hash_hex = encode_hex(&signed.message_hash);
        self.history.insert(
            0,
            WalletTxRecord {
                id: hash_hex.clone(),
                incoming: false,
                counterparty_address: quote.new_owner.to_friendly(false, self.app_config.testnet),
                counterparty_label: format!("NFT {}", quote.nft_name),
                amount_nano: quote.attached_ton_nano,
                fee_nano: Some(quote.fee_nano),
                comment: quote.comment.clone(),
                timestamp: self.now_unix,
                status: WalletTxStatus::Success,
            },
        );
        Ok(SendReceipt {
            message_hash_hex: hash_hex,
            signed_boc_base64: signed.boc_base64,
        })
    }

    fn onramp_providers(&self) -> Vec<OnrampProviderCard> {
        self.onramp_providers.clone()
    }

    fn create_onramp_handoff(
        &mut self,
        provider_id: &str,
        base_currency: Option<&str>,
    ) -> Result<OnrampHandoffSession, String> {
        let contract = self
            .contract
            .as_ref()
            .ok_or_else(|| "Create or import a wallet first".to_owned())?;
        let provider = self
            .onramp_providers
            .iter()
            .find(|p| p.id == provider_id)
            .ok_or_else(|| format!("Unknown onramp provider: {provider_id}"))?;
        let wallet_address = contract
            .address()
            .map_err(|e| e.to_string())?
            .to_friendly(false, self.app_config.testnet);
        let currency = base_currency
            .map(|c| c.trim().to_ascii_lowercase())
            .filter(|c| !c.is_empty());
        let mut url = reqwest::Url::parse("https://onramp.example/handoff")
            .map_err(|e| format!("Invalid demo handoff URL: {e}"))?;
        {
            let mut pairs = url.query_pairs_mut();
            pairs.append_pair("provider", &provider.id);
            pairs.append_pair("crypto", "GRAM");
            pairs.append_pair("address", &wallet_address);
            if let Some(ref cur) = currency {
                pairs.append_pair("fiat", cur);
            }
            pairs.append_pair("readonly", "1");
        }
        Ok(OnrampHandoffSession {
            provider_id: provider.id.clone(),
            provider_name: provider.name.clone(),
            crypto_currency: "GRAM".to_owned(),
            base_currency: currency,
            wallet_address,
            handoff_url: super::onramp::validate_handoff_url(url.as_str())?,
            expires_date: (self.now_unix + 900) as i32,
        })
    }

    fn cloud_backup_status(&self) -> CloudBackupStatus {
        self.cloud_backup
    }

    fn enable_cloud_backup(&mut self) -> Result<CloudBackupStatus, String> {
        if self.contract.is_none() {
            return Err("No wallet configured".to_owned());
        }
        let _ = self.load_keypair()?;
        self.cloud_backup.backup_enabled = true;
        self.cloud_backup.holder_dc_count = self.cloud_backup.holder_dc_count.max(3);
        Ok(self.cloud_backup)
    }

    fn disable_cloud_backup(&mut self, _update_phrase: bool) -> Result<CloudBackupStatus, String> {
        if self.contract.is_none() {
            return Err("No wallet configured".to_owned());
        }
        self.cloud_backup.backup_enabled = false;
        Ok(self.cloud_backup)
    }

    fn gasless_terms(&self) -> Option<GaslessTerms> {
        self.gasless.clone()
    }

    fn wallet_card_state(&self) -> WalletCardState {
        self.card_state.clone()
    }

    fn set_card_stickers(
        &mut self,
        stickers: &[CardStickerInput],
        bg_nft_address: Option<&str>,
    ) -> Result<WalletCardState, String> {
        if !self.card_state.ready {
            return Err(tg_lang::tr::lng_wallet_card_not_ready());
        }
        if stickers.len() > WALLET_CARD_MAX_STICKERS {
            return Err(tg_lang::tr::lng_wallet_card_limit(
                WALLET_CARD_MAX_STICKERS as i64,
            ));
        }
        let entries = stickers
            .iter()
            .map(|input| {
                let found = self
                    .collectibles
                    .iter()
                    .find(|n| n.address.eq_ignore_ascii_case(&input.nft_address));
                WalletCardStickerEntry {
                    nft_address: input.nft_address.clone(),
                    title: found.map_or_else(
                        || input.nft_address.clone(),
                        super::collectibles::nft_display_title,
                    ),
                    collection_name: found.map(|n| n.collection_name.clone()).unwrap_or_default(),
                    coordinates: input.coordinates,
                }
            })
            .collect();
        self.card_state.stickers = entries;
        self.card_state.background_address = bg_nft_address.map(str::to_owned);
        Ok(self.card_state.clone())
    }

    fn prepare_move_to_gifts(
        &mut self,
        nft_address: &str,
        slug: &str,
        hide_sender: bool,
    ) -> Result<MoveToGiftsQuote, String> {
        if self.contract.is_none() {
            return Err("No wallet configured".to_owned());
        }
        let item = self
            .collectibles
            .iter()
            .find(|n| n.address.eq_ignore_ascii_case(nft_address))
            .ok_or_else(|| "Collectible gift not found in wallet".to_owned())?;
        if !matches!(item.kind, NftKind::TelegramGift) {
            return Err("Only Telegram collectible gifts can be moved to My Gifts".to_owned());
        }
        let deposit_address = TonAddress::new(0, [0x9A; 32]);
        let needed_ton = DEFAULT_NFT_GAS_NANO.saturating_add(DEMO_ESTIMATED_FEE_NANO);
        if needed_ton > self.balance_nano {
            return Err("Insufficient TON balance for Move to Gifts gas".to_owned());
        }
        Ok(MoveToGiftsQuote {
            nft_address: item.address.clone(),
            slug: slug.to_owned(),
            title: super::collectibles::nft_display_title(item),
            escrow_address: deposit_address.to_friendly(false, self.app_config.testnet),
            payload_base64: "te6ccgEBAQEAAgAAAA==".to_owned(),
            amount_nano: DEFAULT_NFT_GAS_NANO,
            fee_nano: DEMO_ESTIMATED_FEE_NANO,
            hide_sender,
        })
    }

    fn confirm_move_to_gifts(
        &mut self,
        quote: &MoveToGiftsQuote,
        also_display_in_gifts: bool,
    ) -> Result<MoveToGiftsReceipt, String> {
        let contract = self
            .contract
            .as_ref()
            .ok_or_else(|| "No wallet configured".to_owned())?;
        let kp = self.load_keypair()?;
        let self_addr = contract.address().map_err(|e| e.to_string())?;
        let nft_addr = TonAddress::parse(&quote.nft_address)
            .map_err(|e| e.to_string())?
            .address;
        let escrow_addr = TonAddress::parse(&quote.escrow_address)
            .map_err(|e| e.to_string())?
            .address;
        let msg = build_nft_transfer_message(&NftTransferParams {
            nft_item: nft_addr,
            new_owner: escrow_addr,
            response_destination: self_addr,
            query_id: self.now_unix as u64,
            attached_ton_nano: quote.amount_nano,
            forward_ton_nano: DEFAULT_NFT_FORWARD_NANO,
            forward_comment: Some("Move to My Gifts".to_owned()),
        })
        .map_err(|e| e.to_string())?;
        let valid_until = (self.now_unix + 300) as u32;
        let signed = contract
            .create_signed_transfer(&kp, self.seqno, valid_until, &[msg])
            .map_err(|e| e.to_string())?;

        let raw_nft = nft_addr.to_raw();
        self.collectibles
            .retain(|n| !n.address.eq_ignore_ascii_case(&raw_nft));
        self.card_state
            .stickers
            .retain(|s| !s.nft_address.eq_ignore_ascii_case(&raw_nft));
        if self
            .card_state
            .background_address
            .as_deref()
            .is_some_and(|a| a.eq_ignore_ascii_case(&raw_nft))
        {
            self.card_state.background_address = None;
        }
        if also_display_in_gifts
            && !self
                .displayed_gifts
                .iter()
                .any(|a| a.eq_ignore_ascii_case(&raw_nft))
        {
            self.displayed_gifts.push(raw_nft.clone());
        }
        self.balance_nano = self
            .balance_nano
            .saturating_sub(quote.amount_nano.saturating_add(quote.fee_nano));
        self.seqno = self.seqno.saturating_add(1);
        self.now_unix += 15;
        let hash_hex = encode_hex(&signed.message_hash);
        self.history.insert(
            0,
            WalletTxRecord {
                id: hash_hex.clone(),
                incoming: false,
                counterparty_address: quote.escrow_address.clone(),
                counterparty_label: format!("Move to Gifts · {}", quote.title),
                amount_nano: quote.amount_nano,
                fee_nano: Some(quote.fee_nano),
                comment: "Telegram Collectible Gift deposit".to_owned(),
                timestamp: self.now_unix,
                status: WalletTxStatus::Success,
            },
        );
        Ok(MoveToGiftsReceipt {
            nft_address: raw_nft,
            slug: quote.slug.clone(),
            message_hash_hex: hash_hex,
            shown_in_gifts: also_display_in_gifts,
        })
    }

    fn display_star_gift(&mut self, nft_address: &str, display: bool) -> Result<(), String> {
        if self.contract.is_none() {
            return Err("No wallet configured".to_owned());
        }
        if display {
            if !self
                .displayed_gifts
                .iter()
                .any(|a| a.eq_ignore_ascii_case(nft_address))
            {
                self.displayed_gifts.push(nft_address.to_owned());
            }
        } else {
            self.displayed_gifts
                .retain(|a| !a.eq_ignore_ascii_case(nft_address));
        }
        Ok(())
    }
}

fn encode_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        let _ = write!(&mut out, "{b:02x}");
    }
    out
}

/// Helper adapter wiring a configured [`ToncenterApi`] provider into a [`WalletOverview`] refresh
/// (used when a real/local Toncenter endpoint is configured via [`WalletAppConfig`]).
pub async fn sync_overview_from_toncenter(
    api: Arc<dyn ToncenterApi>,
    address: &TonAddress,
    testnet: bool,
) -> Result<(u64, AccountState, Vec<WalletTxRecord>), String> {
    let friendly = address.to_friendly(false, testnet);
    let info = api
        .get_address_information(&friendly)
        .await
        .map_err(|e| e.to_string())?;
    let txs = api
        .get_transactions(&friendly, 20, None, None)
        .await
        .map_err(|e| e.to_string())?;
    let mapped = txs
        .into_iter()
        .map(|tx| WalletTxRecord {
            id: tx.hash,
            incoming: tx.incoming,
            counterparty_address: tx.counterparty,
            counterparty_label: String::new(),
            amount_nano: tx.amount_nano,
            fee_nano: Some(tx.fee_nano),
            comment: tx.comment,
            timestamp: tx.utime,
            status: WalletTxStatus::Success,
        })
        .collect();
    Ok((info.balance_nano, info.state, mapped))
}
