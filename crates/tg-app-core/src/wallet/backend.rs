//! Backend trait and view models for the TON Wallet UI (`crates/tg-app/src/wallet/`).
//!
//! Port origin: `Telegram/SourceFiles/wallet/wallet_session.h` and
//! `Telegram/SourceFiles/wallet/wallet_ton_connect.h`.

use tg_wallet::WalletAppConfig;
use tg_wallet::address::TonAddress;
use tg_wallet::contract::WalletVersion;
use tg_wallet::emulate::{EmulationResult, TonConnectEmulation};
use tg_wallet::jettons::JettonWalletBalance;
use tg_wallet::mtproto_api::{CardStickerCoordinates, CardStickerInput, GaslessTerms};
use tg_wallet::toncenter::{AccountState, NftItem, ToncenterTransportKind};
use tg_wallet::tonconnect::{
    DappManifest, SignDataResult, TonConnectLink, TonConnectSendTxRequest, TonConnectSessionStatus,
    TonConnectSignDataRequest,
};

/// Status of a wallet transaction row (`TransferItem::Status` in `wallet_session.h`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalletTxStatus {
    /// Confirmed on chain.
    Success,
    /// Broadcast and awaiting confirmation.
    Pending,
    /// Failed or bounced.
    Failure,
}

/// Display transaction row in the wallet history list (`Wallet::TransferItem`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalletTxRecord {
    /// Unique transaction / message hash identifier.
    pub id: String,
    /// `true` for incoming funds, `false` for outgoing funds.
    pub incoming: bool,
    /// Counterparty user-friendly address.
    pub counterparty_address: String,
    /// Optional resolved contact/dApp label (fictional in demo mode).
    pub counterparty_label: String,
    /// Magnitude in nanoTON.
    pub amount_nano: u64,
    /// Fee in nanoTON (if known).
    pub fee_nano: Option<u64>,
    /// Optional plain UTF-8 comment.
    pub comment: String,
    /// Unix timestamp (seconds).
    pub timestamp: i64,
    /// Confirmation state.
    pub status: WalletTxStatus,
}

/// Snapshot of wallet account overview state.
#[derive(Debug, Clone)]
pub struct WalletOverview {
    /// Whether a wallet has been created or imported.
    pub has_wallet: bool,
    /// Whether the local key vault is currently locked.
    pub locked: bool,
    /// Active wallet contract version (`V3R2` / `V4R2` / `V5R1`).
    pub version: WalletVersion,
    /// Canonical wallet address (when `has_wallet` is true).
    pub address: Option<TonAddress>,
    /// Account balance in nanoTON.
    pub balance_nano: u64,
    /// Current on-chain contract state.
    pub account_state: AccountState,
    /// USD price per 1 TON used for fiat display.
    pub usd_per_ton: f64,
    /// Current wallet sequence number (`seqno`).
    pub seqno: u32,
    /// Recent transactions newest-first.
    pub history: Vec<WalletTxRecord>,
}

/// Fee quote prepared before user confirms a TON transfer (`Wallet::FeeResult`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendQuote {
    /// Parsed destination address.
    pub destination: TonAddress,
    /// Bounceable flag to use on the outgoing internal message.
    pub bounce: bool,
    /// Amount in nanoTON.
    pub amount_nano: u64,
    /// Estimated network fee in nanoTON.
    pub fee_nano: u64,
    /// Optional transfer comment.
    pub comment: String,
}

/// Receipt emitted after signing and broadcasting a transfer (`Wallet::TransferReceipt`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendReceipt {
    /// Hex-encoded external message hash.
    pub message_hash_hex: String,
    /// Base64-encoded signed BOC submitted to the provider.
    pub signed_boc_base64: String,
}

/// Summary card for a connected TON Connect dApp (`Wallet::TonConnectSessionInfo`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TonConnectSessionCard {
    /// Local session ID.
    pub session_id: u64,
    /// Remote dApp client ID (64 hex chars).
    pub dapp_client_id: String,
    /// dApp manifest metadata.
    pub manifest: DappManifest,
    /// Session lifecycle state.
    pub status: TonConnectSessionStatus,
    /// Connected unix timestamp.
    pub connected_at: i64,
}

/// Pending TON Connect dApp connection approval (`Wallet::TonConnectBoxState`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingConnectApproval {
    /// Parsed connect link.
    pub link: TonConnectLink,
    /// Fetched dApp manifest.
    pub manifest: DappManifest,
    /// Whether the dApp requests a `ton_proof` ownership signature.
    pub requests_proof: bool,
}

/// Pending TON Connect transaction approval (`Wallet::TonConnectRequestBoxState`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingTxApproval {
    /// Session ID that originated the request.
    pub session_id: u64,
    /// dApp display name.
    pub dapp_name: String,
    /// dApp hostname.
    pub dapp_domain: String,
    /// Decrypted transaction request details.
    pub request: TonConnectSendTxRequest,
    /// Estimated network fee in nanoTON.
    pub estimated_fee_nano: u64,
    /// Optional full Toncenter emulation result (balance deltas, gas/storage/fwd fees, warnings).
    pub emulation: Option<EmulationResult>,
    /// Optional classified TON Connect action preview derived from `emulation`.
    pub ton_connect_emulation: Option<TonConnectEmulation>,
}

/// Pending TON Connect `signData` approval (`Wallet::TonConnectSignDataRequestBoxState`).
///
/// Every `signData` payload must be reviewed and explicitly confirmed by the user before signing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingSignDataApproval {
    /// Session ID that originated the request.
    pub session_id: u64,
    /// dApp display name.
    pub dapp_name: String,
    /// dApp hostname bound into the Ed25519 signature domain.
    pub dapp_domain: String,
    /// Decrypted `signData` request details (`text`, `binary`, or `cell`).
    pub request: TonConnectSignDataRequest,
}

/// Fee & payload quote prepared before the user confirms a TEP-74 Jetton transfer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JettonTransferQuote {
    /// Sender's Jetton wallet contract address.
    pub jetton_wallet: TonAddress,
    /// Jetton master contract canonical raw address.
    pub jetton_master: String,
    /// Token ticker symbol for confirmation UI.
    pub symbol: String,
    /// Token decimal places.
    pub decimals: u8,
    /// Recipient TON owner address.
    pub destination: TonAddress,
    /// Transfer amount in smallest jetton units.
    pub jetton_amount: u128,
    /// Attached TON gas in nanoTON (typically `0.05 TON`).
    pub attached_ton_nano: u64,
    /// Estimated network fee in nanoTON.
    pub fee_nano: u64,
    /// Optional UTF-8 transfer comment.
    pub comment: String,
}

/// Fee & payload quote prepared before the user confirms a TEP-62 NFT transfer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NftTransferQuote {
    /// NFT item contract address.
    pub nft_address: TonAddress,
    /// Display name of the collectible.
    pub nft_name: String,
    /// Collection display name.
    pub collection_name: String,
    /// Recipient TON owner address.
    pub new_owner: TonAddress,
    /// Attached TON gas in nanoTON (typically `0.05 TON`).
    pub attached_ton_nano: u64,
    /// Estimated network fee in nanoTON.
    pub fee_nano: u64,
    /// Optional UTF-8 transfer comment.
    pub comment: String,
}

/// Read-only onramp provider descriptor (`OnrampProviderInfo` in `wallet_onramp.cpp`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnrampProviderCard {
    /// Provider identifier passed to `payments.createOnrampSession`.
    pub id: String,
    /// Human-readable provider display name.
    pub name: String,
    /// Supported crypto currencies (e.g. `["GRAM", "TON"]`).
    pub crypto_currencies: Vec<String>,
    /// Whether the provider supports querying `payments.getOnrampBaseCurrencies`.
    pub supports_base_currencies: bool,
    /// Whether the provider supports querying `payments.getOnrampLimits`.
    pub supports_limits: bool,
    /// Whether the provider supports querying `payments.getOnrampQuote`.
    pub supports_quote: bool,
}

/// Read-only external fiat onramp handoff session (`OnrampSession` in `wallet_onramp.cpp`).
///
/// Never executes any payment or purchase automatically; only provides a validated external URL
/// for the user to inspect, copy, or open manually.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnrampHandoffSession {
    /// Provider identifier.
    pub provider_id: String,
    /// Provider display name.
    pub provider_name: String,
    /// Target crypto currency (e.g. `"GRAM"` / `"TON"`).
    pub crypto_currency: String,
    /// Optional selected fiat currency (e.g. `"usd"`).
    pub base_currency: Option<String>,
    /// Destination TON wallet address embedded in the read-only handoff.
    pub wallet_address: String,
    /// Validated `https://` external handoff URL.
    pub handoff_url: String,
    /// Session expiry unix timestamp (seconds).
    pub expires_date: i32,
}

/// Encrypted Telegram cloud backup status for the active wallet (`wallet.getState`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CloudBackupStatus {
    /// Whether Telegram's split encrypted cloud backup is currently enabled (`flags.0`).
    pub backup_enabled: bool,
    /// Whether the server allows enabling cloud backup on this wallet (`flags.2`).
    pub can_enable_backup: bool,
    /// Whether the recovery phrase can be exported from the cloud backup (`flags.1`).
    pub can_export_phrase: bool,
    /// Number of Telegram holder DCs participating in cloud backup shares (typically 3).
    pub holder_dc_count: usize,
}

/// Display entry for a collectible sticker placed on the user's wallet card
/// (`Data::WalletCardSticker` in `wallet_own_card.cpp`).
#[derive(Debug, Clone, PartialEq)]
pub struct WalletCardStickerEntry {
    /// Canonical NFT address of the collectible gift.
    pub nft_address: String,
    /// Human-readable collectible title (e.g. `"Desk Calendar #45754"`).
    pub title: String,
    /// Collection name (e.g. `"Telegram Gifts"`).
    pub collection_name: String,
    /// Placement rectangle & rotation on the card (`0..=100` percentage coordinates).
    pub coordinates: CardStickerCoordinates,
}

/// Current wallet card customization state (`Wallet::OwnCardState` in `wallet_own_card.h`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct WalletCardState {
    /// Stickers placed on the wallet card (at most [`tg_wallet::mtproto_api::WALLET_CARD_MAX_STICKERS`]).
    pub stickers: Vec<WalletCardStickerEntry>,
    /// Optional canonical NFT address of the sticker providing the card's background color.
    pub background_address: Option<String>,
    /// Whether the server card state is hydrated and ready to edit.
    pub ready: bool,
}

/// Verified off-chain Star Gift deposit quote awaiting explicit user confirmation
/// (`Wallet::MoveToGiftsFlow` in `wallet_move_to_gifts.cpp`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoveToGiftsQuote {
    /// Canonical raw NFT collectible address being moved to Telegram Star Gifts.
    pub nft_address: String,
    /// Unique Star Gift slug passed to `payments.depositStarGift`.
    pub slug: String,
    /// Human-readable collectible display title.
    pub title: String,
    /// Verified escrow destination extracted from the server deposit BOC (`nft_transfer_new_owner`).
    pub escrow_address: String,
    /// Base64-encoded deposit BOC payload returned by Telegram.
    pub payload_base64: String,
    /// Required attached TON amount in nanoTON.
    pub amount_nano: u64,
    /// Estimated network fee in nanoTON.
    pub fee_nano: u64,
    /// Whether sender info should be hidden (`payments.depositStarGift.hide_sender`).
    pub hide_sender: bool,
}

/// Receipt returned after the user confirms moving an NFT gift to Telegram Star Gifts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoveToGiftsReceipt {
    /// Canonical NFT collectible address moved.
    pub nft_address: String,
    /// Unique Star Gift slug.
    pub slug: String,
    /// Hex-encoded external message hash of the submitted deposit transfer.
    pub message_hash_hex: String,
    /// Whether `payments.displayStarGift` was also enabled.
    pub shown_in_gifts: bool,
}

/// Decoupled wallet backend trait implemented by [`super::mock::MockWalletBackend`] and live
/// adapters backed by `tg_wallet::toncenter::ToncenterApi` + `TonConnectBridge`.
pub trait WalletBackend: Send {
    /// Returns the active app-config / feature-flag gating for the wallet.
    fn app_config(&self) -> &WalletAppConfig;

    /// Updates the wallet app-config / feature flags (e.g. from server appConfig sync).
    fn set_app_config(&mut self, config: WalletAppConfig);

    /// Returns a snapshot of the wallet overview (balance, address, status, history).
    fn overview(&self) -> WalletOverview;

    /// Refreshes balance and transaction history from the configured backend.
    fn refresh(&mut self) -> Result<(), String>;

    /// Generates a fresh 24-word TON mnemonic phrase for onboarding.
    fn generate_phrase(&mut self) -> Result<Vec<String>, String>;

    /// Finalizes creating a new wallet from `words`, encrypting it in the local vault with `passcode`.
    fn create_wallet(
        &mut self,
        words: &[String],
        version: WalletVersion,
        passcode: &str,
    ) -> Result<TonAddress, String>;

    /// Validates and imports an existing 24-word TON mnemonic phrase into the local encrypted vault.
    fn import_wallet(
        &mut self,
        words: &[String],
        password: &str,
        version: WalletVersion,
        passcode: &str,
    ) -> Result<TonAddress, String>;

    /// Locks the local encrypted vault, wiping decrypted key material from memory.
    fn lock_vault(&mut self);

    /// Unlocks the local encrypted vault using `passcode`.
    fn unlock_vault(&mut self, passcode: &str) -> Result<(), String>;

    /// Reveals the stored recovery phrase when the vault is unlocked.
    fn reveal_phrase(&self) -> Result<Vec<String>, String>;

    /// Validates a send form and returns a fee quote ready for user confirmation.
    fn prepare_send(
        &self,
        destination_input: &str,
        amount_nano: u64,
        comment: &str,
        bounce_override: Option<bool>,
    ) -> Result<SendQuote, String>;

    /// Signs and broadcasts a previously quoted transfer.
    fn confirm_send(&mut self, quote: &SendQuote) -> Result<SendReceipt, String>;

    /// Lists connected TON Connect dApps.
    fn ton_connect_sessions(&self) -> Vec<TonConnectSessionCard>;

    /// Returns any pending TON Connect dApp connection approval box.
    fn pending_connect(&self) -> Option<PendingConnectApproval>;

    /// Returns any pending TON Connect `sendTransaction` approval box.
    fn pending_tx_approval(&self) -> Option<PendingTxApproval>;

    /// Parses a pasted `tc://` or `tonconnect-...` link and queues a connect approval box.
    fn handle_ton_connect_link(&mut self, raw_link: &str) -> Result<(), String>;

    /// Approves the pending TON Connect connection request.
    fn approve_ton_connect(&mut self) -> Result<TonConnectSessionCard, String>;

    /// Declines and dismisses the pending TON Connect connection request.
    fn decline_ton_connect(&mut self);

    /// Approves, signs, and submits the pending TON Connect transaction request.
    fn approve_ton_connect_tx(&mut self) -> Result<SendReceipt, String>;

    /// Declines and dismisses the pending TON Connect transaction request.
    fn decline_ton_connect_tx(&mut self);

    /// Returns any pending TON Connect `signData` approval box.
    fn pending_sign_data(&self) -> Option<PendingSignDataApproval> {
        None
    }

    /// Approves and signs the pending TON Connect `signData` request (only after user confirmation).
    fn approve_ton_connect_sign_data(&mut self) -> Result<SignDataResult, String> {
        Err("signData is not supported by this backend".to_owned())
    }

    /// Declines and dismisses the pending TON Connect `signData` request.
    fn decline_ton_connect_sign_data(&mut self) {}

    /// Disconnects an active TON Connect dApp session.
    fn disconnect_ton_connect(&mut self, session_id: u64) -> Result<(), String>;

    /// Non-blocking tick invoked from UI frames to drain background async updates
    /// (such as `help.getAppConfig` sync or incoming TON Connect bridge events).
    fn poll_background(&mut self) {}

    /// Lists owned NFT collectibles fetched from Toncenter v3 (`/api/v3/nft/items`).
    fn collectibles(&self) -> Vec<NftItem> {
        Vec::new()
    }

    /// Lists owned TEP-74 Jetton wallet balances fetched from Toncenter v3 (`/api/v3/jetton/wallets`).
    fn jetton_balances(&self) -> Vec<JettonWalletBalance> {
        Vec::new()
    }

    /// Refreshes NFT collectibles and Jetton balances from the configured provider.
    fn refresh_assets(&mut self) -> Result<(), String> {
        Ok(())
    }

    /// Validates and quotes a TEP-74 Jetton transfer (`transfer#0f8a7ea5`).
    fn prepare_jetton_transfer(
        &self,
        _jetton_wallet_addr: &str,
        _destination_input: &str,
        _jetton_amount: u128,
        _comment: &str,
    ) -> Result<JettonTransferQuote, String> {
        Err("Jetton transfers are not supported by this backend".to_owned())
    }

    /// Signs and broadcasts a previously quoted TEP-74 Jetton transfer.
    fn confirm_jetton_transfer(
        &mut self,
        _quote: &JettonTransferQuote,
    ) -> Result<SendReceipt, String> {
        Err("Jetton transfers are not supported by this backend".to_owned())
    }

    /// Validates and quotes a TEP-62 NFT collectible transfer (`transfer#5fcc3d14`).
    fn prepare_nft_transfer(
        &self,
        _nft_address: &str,
        _new_owner_input: &str,
        _comment: &str,
    ) -> Result<NftTransferQuote, String> {
        Err("NFT transfers are not supported by this backend".to_owned())
    }

    /// Signs and broadcasts a previously quoted TEP-62 NFT transfer.
    fn confirm_nft_transfer(&mut self, _quote: &NftTransferQuote) -> Result<SendReceipt, String> {
        Err("NFT transfers are not supported by this backend".to_owned())
    }

    /// Lists available read-only fiat onramp providers (`payments.getOnrampProviders`).
    fn onramp_providers(&self) -> Vec<OnrampProviderCard> {
        Vec::new()
    }

    /// Creates a read-only external fiat onramp handoff URL (`payments.createOnrampSession`).
    /// Never executes a payment or auto-purchases.
    fn create_onramp_handoff(
        &mut self,
        _provider_id: &str,
        _base_currency: Option<&str>,
    ) -> Result<OnrampHandoffSession, String> {
        Err("Onramp handoff is not supported by this backend".to_owned())
    }

    /// Returns the active Toncenter transport kind (`Mtproto` or `Http`), if configured.
    fn toncenter_transport_kind(&self) -> Option<ToncenterTransportKind> {
        None
    }

    /// Returns the wallet's Telegram encrypted cloud backup status (`wallet.getState`).
    fn cloud_backup_status(&self) -> CloudBackupStatus {
        CloudBackupStatus::default()
    }

    /// Enables Telegram encrypted cloud backup across holder DCs
    /// (`wallet.getBackupHolderDcs` + `wallet.enableBackup`).
    fn enable_cloud_backup(&mut self) -> Result<CloudBackupStatus, String> {
        Err("Cloud backup is not supported by this backend".to_owned())
    }

    /// Disables Telegram encrypted cloud backup (`wallet.disableBackup`).
    fn disable_cloud_backup(&mut self, _update_phrase: bool) -> Result<CloudBackupStatus, String> {
        Err("Cloud backup is not supported by this backend".to_owned())
    }

    /// Returns active gasless transfer eligibility terms (`wallet.getGaslessInfo`), if available.
    fn gasless_terms(&self) -> Option<GaslessTerms> {
        None
    }

    /// Returns the current wallet card stickers and background selection (`wallet.card`).
    fn wallet_card_state(&self) -> WalletCardState {
        WalletCardState::default()
    }

    /// Persists wallet card stickers and optional background gift (`wallet.setCardStickers`).
    fn set_card_stickers(
        &mut self,
        _stickers: &[CardStickerInput],
        _bg_nft_address: Option<&str>,
    ) -> Result<WalletCardState, String> {
        Err("Wallet card customization is not supported by this backend".to_owned())
    }

    /// Requests and verifies a Star Gift deposit payload (`payments.depositStarGift`) ready
    /// for explicit user confirmation. Never signs or broadcasts a transfer.
    fn prepare_move_to_gifts(
        &mut self,
        _nft_address: &str,
        _slug: &str,
        _hide_sender: bool,
    ) -> Result<MoveToGiftsQuote, String> {
        Err("Move to Gifts is not supported by this backend".to_owned())
    }

    /// Signs and submits a previously verified Star Gift deposit quote after explicit user
    /// confirmation, optionally also calling `payments.displayStarGift`.
    fn confirm_move_to_gifts(
        &mut self,
        _quote: &MoveToGiftsQuote,
        _also_display_in_gifts: bool,
    ) -> Result<MoveToGiftsReceipt, String> {
        Err("Move to Gifts is not supported by this backend".to_owned())
    }

    /// Toggles displaying an owned collectible NFT gift on the user's Telegram Gifts list
    /// (`payments.displayStarGift`).
    fn display_star_gift(&mut self, _nft_address: &str, _display: bool) -> Result<(), String> {
        Err("Displaying Star Gifts is not supported by this backend".to_owned())
    }
}
