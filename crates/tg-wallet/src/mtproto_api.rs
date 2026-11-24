//! Native Layer-233 MTProto TON Wallet client, Toncenter MTProto transport, cloud backup
//! share coordinator, gasless fee terms, server-side TON Connect, Star Gift deposit/display,
//! wallet card stickers, and FX currency rates.
//!
//! Port origin (tdesktop commit `c2d0ad9`, Layer 233):
//! - `Telegram/SourceFiles/wallet/wallet_api.{h,cpp}` (`toncenter.performApiRequest`, `toncenter.getStreamingUrl`)
//! - `Telegram/SourceFiles/wallet/wallet_engine.cpp` (MTProto transport bridge & routed `sendBoc`)
//! - `Telegram/SourceFiles/wallet/wallet_session.{h,cpp}` (`wallet.*` state, history, NFTs, backup, transfer, gasless, proof, replace)
//! - `Telegram/SourceFiles/wallet/wallet_user_addresses.{h,cpp}` (`wallet.getUserAddresses`)
//! - `Telegram/SourceFiles/wallet/wallet_ton_connect.{h,cpp}`, `wallet_ton_connect_request.cpp` (`wallet.tonConnect*`)
//! - `Telegram/SourceFiles/wallet/wallet_own_card.cpp`, `data/data_user_wallet_card.cpp` (`wallet.setCardStickers`)
//! - `Telegram/SourceFiles/wallet/wallet_rates.{h,cpp}` (`payments.getCurrencyRates`)
//! - `Telegram/SourceFiles/wallet/wallet_session_deposit.{h,cpp}` (`payments.depositStarGift`)
//! - `Telegram/SourceFiles/wallet/wallet_collectibles.cpp` (`payments.displayStarGift`)

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE, URL_SAFE_NO_PAD};
use crypto_box::SecretKey as BoxSecretKey;
use rand_core::OsRng;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tg_api::{DcId, InvocationError, RpcClient, RpcError};
use tg_tl::{Serializable, enums as tl_enums, functions as tl_fns, types as tl_types};
use thiserror::Error;

use crate::address::{TonAddress, canonical_address};
use crate::boc::nft_transfer_new_owner;
use crate::phrase_shares::{
    PHRASE_SHARE_PUBLIC_KEY_SIZE, combine_shares_xor, decrypt_share_for_holder,
    encrypt_share_for_holder, seed_from_words, split_seed_xor, words_from_seed,
};
use crate::toncenter::{
    BoxFuture, CurrencyRates, GramHttpRequest, HttpToncenterTransport, NftItem, NftKind,
    StreamingUrlInfo, ToncenterApi, ToncenterClient, ToncenterConfig, ToncenterTransport,
    ToncenterTransportKind, ToncenterTransportPolicy, classify_nft_kind, parse_api_error,
    parse_stream_endpoint,
};
use crate::tonconnect::TonConnectSessionStatus;
use crate::{TRANSFER_MIN_NANOS_DEFAULT, WalletAppConfig, WalletError};

// ============================================================================
// Constants matching tdesktop (`mtproto/core_types.h`, `wallet/*`)
// ============================================================================

/// Base DC shift multiplier separating bare DC ID from session slot (`MTP::kDcShift`).
pub const DC_SHIFT: DcId = 10_000;
/// Session shift slot for `toncenter.*` proxy RPCs (`MTP::kToncenterDcShift`).
pub const TONCENTER_DC_SHIFT: i32 = 0x08;
/// Session shift slot for `wallet.fetchEncryptedSecretPhrasePart` (`MTP::kWalletShareDcShift`).
pub const WALLET_SHARE_DC_SHIFT: i32 = 0x09;

/// Maximum users per `wallet.getUserAddresses` request (`kUserAddressesPerRequest`).
pub const USER_ADDRESSES_PER_REQUEST: usize = 100;
/// Default/maximum page size for `wallet.getTransactions` (`kTransactionsPerPage`).
pub const TRANSACTIONS_PER_PAGE: i32 = 50;
/// Maximum consecutive hidden-row pages fetched in a row (`kMaxHiddenPagesInRow`).
pub const MAX_HIDDEN_PAGES_IN_ROW: usize = 20;
/// Default page size for `wallet.getNfts` (`kCollectiblesPerPage`).
pub const COLLECTIBLES_PER_PAGE: i32 = 20;
/// Maximum stickers allowed on a wallet card (`Data::kWalletCardMaxStickers`).
pub const WALLET_CARD_MAX_STICKERS: usize = 10;
/// Expected Ed25519 / X25519 public key byte length (`kCustodyPublicKeySize`).
pub const CUSTODY_PUBLIC_KEY_SIZE: usize = 32;
/// Expected Ed25519 ownership proof signature length (`kOwnershipProofSignatureSize`).
pub const OWNERSHIP_PROOF_SIGNATURE_SIZE: usize = 64;
/// Default minimum amount in nanoTON for gasless transfers (`kGaslessMinNanosDefault`).
pub const GASLESS_MIN_NANOS_DEFAULT: i64 = 100_000_000;
/// Default per-request timeout for `toncenter.*` proxy calls (`kRequestTimeout` in `wallet_api.cpp`).
pub const DEFAULT_TONCENTER_TIMEOUT: Duration = Duration::from_secs(30);

/// Error token returned when `webfile_dc_id` is invalid (`kInvalidWebFileDcError`).
pub const WEBFILE_DC_INVALID: &str = "WEBFILE_DC_INVALID";
/// Error token returned when a toncenter request times out (`kTimeoutErrorMessage`).
pub const TONCENTER_TIMEOUT_ERROR: &str = "TIMEOUT";
/// Toncenter error message translated to an empty pending list (`wallet_engine.cpp:751`).
pub const EMULATED_TRACES_NOT_FOUND: &str = "emulated traces not found";

/// Combines a bare DC ID and shift slot (`MTP::ShiftDcId`).
#[must_use]
pub const fn shift_dc_id(dc_id: DcId, shift: i32) -> DcId {
    dc_id + DC_SHIFT * shift
}

/// Computes the shifted DC ID for `toncenter.*` calls (`Wallet::Api::shiftedDcId`).
///
/// Returns `None` when `webfile_dc_id <= 0` or `webfile_dc_id >= DC_SHIFT`.
#[must_use]
pub const fn toncenter_shifted_dc_id(webfile_dc_id: DcId) -> Option<DcId> {
    if webfile_dc_id <= 0 || webfile_dc_id >= DC_SHIFT {
        None
    } else {
        Some(shift_dc_id(webfile_dc_id, TONCENTER_DC_SHIFT))
    }
}

/// Computes the shifted DC ID for `wallet.fetchEncryptedSecretPhrasePart`.
///
/// Returns `None` when `dc_id <= 0` or `dc_id >= DC_SHIFT`.
#[must_use]
pub const fn wallet_share_shifted_dc_id(dc_id: DcId) -> Option<DcId> {
    if dc_id <= 0 || dc_id >= DC_SHIFT {
        None
    } else {
        Some(shift_dc_id(dc_id, WALLET_SHARE_DC_SHIFT))
    }
}

// ============================================================================
// Errors & tdesktop error classification
// ============================================================================

/// Definite 4xx refusal classification matching `DefiniteTransferRefusal` (`wallet_session.cpp:1356`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferRefusalKind {
    /// Error code `406` (`MTP::IgnoreError`), silently ignored.
    Silent,
    /// `WALLET_TRANSFER_SEND_FAILED`.
    Rejected,
    /// `WALLET_TRANSFER_DATA_INVALID`.
    DataInvalid,
    /// `WALLET_KEY_MISMATCH`.
    KeyMismatch,
    /// Any other 4xx RPC error.
    Failed,
}

/// Transfer submission failure outcome matching `TransferSubmissionOutcome`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransferSubmissionError {
    /// Server definitely refused the transfer (4xx RPC error).
    Definite {
        /// Classified refusal kind.
        kind: TransferRefusalKind,
        /// Raw Telegram RPC error token.
        token: String,
    },
    /// Transport/5xx/receipt failure; transfer may have been broadcast.
    Uncertain {
        /// Diagnostic error string.
        diagnostic: String,
    },
}

/// Outcome of `wallet.tonConnectSubmitResponse` matching `SubmitResult` (`wallet_ton_connect_request.cpp:57`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TonConnectSubmitOutcome {
    /// Server accepted the response (`boolTrue`).
    Delivered,
    /// Server refused or the session/request is permanently gone (`boolFalse` or `RequestDropped`).
    Refused,
    /// Transient error; caller should retry later.
    RetryLater,
}

/// Unified error type for Layer-233 MTProto wallet operations.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum WalletMtprotoError {
    /// Underlying MTProto RPC / transport / deserialization error.
    #[error("MTProto invocation error: {0}")]
    Invocation(#[from] InvocationError),
    /// Provider returned an application-level JSON error inside `toncenter.apiResponse`.
    #[error("toncenter API error ({code}): {message}")]
    ToncenterApi {
        /// Provider code (`0` when omitted).
        code: i32,
        /// Provider error string.
        message: String,
    },
    /// Client-side validation failed before sending the request.
    #[error("invalid wallet request ({code}): {detail}")]
    Validation {
        /// Tdesktop-style symbolic error token (e.g. `"WEBFILE_DC_INVALID"`).
        code: &'static str,
        /// Human-readable diagnostic.
        detail: String,
    },
    /// Server replied with a structurally unusable payload.
    #[error("invalid server response ({code}): {detail}")]
    InvalidResponse {
        /// Tdesktop-style symbolic error token (e.g. `"PHRASE_PARTS_INVALID"`).
        code: &'static str,
        /// Human-readable diagnostic.
        detail: String,
    },
    /// Local cryptographic or phrase-share error.
    #[error("wallet crypto error: {0}")]
    Crypto(String),
}

impl WalletMtprotoError {
    /// Returns the underlying Telegram [`RpcError`] if this error came from `rpc_error`.
    #[must_use]
    pub const fn rpc_error(&self) -> Option<&RpcError> {
        match self {
            Self::Invocation(InvocationError::Rpc(rpc)) => Some(rpc),
            _ => None,
        }
    }

    /// Returns the Telegram RPC error type token (e.g. `"WALLET_UNAVAILABLE"`), or the local
    /// symbolic token for validation/invalid-response errors.
    #[must_use]
    pub fn error_type(&self) -> Option<&str> {
        match self {
            Self::Invocation(InvocationError::Rpc(rpc)) => Some(rpc.name.as_str()),
            Self::ToncenterApi { message, .. } => Some(message.as_str()),
            Self::Validation { code, .. } | Self::InvalidResponse { code, .. } => Some(*code),
            Self::Invocation(_) | Self::Crypto(_) => None,
        }
    }

    /// Returns `true` for code `406` (`MTP::IgnoreError`).
    #[must_use]
    pub fn is_ignored(&self) -> bool {
        self.rpc_error().is_some_and(RpcError::is_ignored)
    }

    /// Returns `true` for temporary errors (`MTP::IsTemporaryError`).
    #[must_use]
    pub fn is_temporary(&self) -> bool {
        self.rpc_error().is_some_and(RpcError::is_temporary)
    }

    /// Returns `true` for `FLOOD_WAIT_X` / `FLOOD_PREMIUM_WAIT_X`.
    #[must_use]
    pub fn is_flood(&self) -> bool {
        self.rpc_error().is_some_and(RpcError::is_flood_error)
    }

    /// Returns `true` when the server replied `WALLET_UNAVAILABLE`.
    #[must_use]
    pub fn is_wallet_unavailable(&self) -> bool {
        self.error_type() == Some("WALLET_UNAVAILABLE")
    }

    /// Returns `true` when the server replied `PASSWORD_MISSING`.
    #[must_use]
    pub fn is_password_missing(&self) -> bool {
        self.error_type() == Some("PASSWORD_MISSING")
    }

    /// Returns `true` when a TON Connect session is permanently gone (`SessionGone`).
    #[must_use]
    pub fn is_ton_connect_session_gone(&self) -> bool {
        matches!(
            self.error_type(),
            Some("TONCONNECT_SESSION_CLOSED" | "TONCONNECT_SESSION_NOT_FOUND")
        )
    }

    /// Returns `true` when a TON Connect request is permanently dropped (`RequestDropped`).
    #[must_use]
    pub fn is_ton_connect_request_dropped(&self) -> bool {
        self.is_ton_connect_session_gone()
            || matches!(
                self.error_type(),
                Some(
                    "TONCONNECT_REQUEST_EXPIRED"
                        | "TONCONNECT_REQUEST_NOT_FOUND"
                        | "TONCONNECT_SESSION_NOT_ACTIVE"
                )
            )
    }

    /// Classifies a `wallet.sendTransfer` error matching `DefiniteTransferRefusal`
    /// (`wallet_session.cpp:1356-1372`).
    #[must_use]
    pub fn classify_transfer_refusal(&self) -> TransferSubmissionError {
        if let Some(rpc) = self.rpc_error()
            && (400..500).contains(&rpc.code)
        {
            let kind = if rpc.is_ignored() {
                TransferRefusalKind::Silent
            } else {
                match rpc.name.as_str() {
                    "WALLET_TRANSFER_SEND_FAILED" => TransferRefusalKind::Rejected,
                    "WALLET_TRANSFER_DATA_INVALID" => TransferRefusalKind::DataInvalid,
                    "WALLET_KEY_MISMATCH" => TransferRefusalKind::KeyMismatch,
                    _ => TransferRefusalKind::Failed,
                }
            };
            return TransferSubmissionError::Definite {
                kind,
                token: rpc.name.clone(),
            };
        }
        TransferSubmissionError::Uncertain {
            diagnostic: self
                .error_type()
                .map_or_else(|| self.to_string(), ToOwned::to_owned),
        }
    }
}

impl From<WalletError> for WalletMtprotoError {
    fn from(err: WalletError) -> Self {
        Self::Crypto(err.to_string())
    }
}

impl From<WalletMtprotoError> for WalletError {
    fn from(err: WalletMtprotoError) -> Self {
        match err {
            WalletMtprotoError::ToncenterApi { message, .. } => Self::Toncenter(message),
            other => Self::Mtproto(other.to_string()),
        }
    }
}

// ============================================================================
// Domain DTOs matching tdesktop Layer-233 wallet types
// ============================================================================

/// Parsed `WalletState` (`walletStateEmpty#9cb9b2ec` | `walletState#95c5346b`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServerWalletState {
    /// Account has no wallet yet (or one is currently being provisioned).
    Empty {
        /// `flags.0` (`creating`).
        creating: bool,
    },
    /// Active server wallet with canonicalized TON address.
    Ready {
        /// `flags.0` (`backup_enabled`).
        backup_enabled: bool,
        /// `flags.1` (`can_export_phrase`).
        can_export_phrase: bool,
        /// `flags.2` (`can_enable_backup`).
        can_enable_backup: bool,
        /// Canonical raw TON address (`workchain:64-hex`).
        address: String,
        /// Raw address string as received from the server.
        raw_address: String,
        /// Active wallet signing public key (32 bytes).
        public_key: Vec<u8>,
        /// Cached balance in nanoTON.
        balance_nano: i64,
    },
    /// Server returned `walletState`, but its `address` could not be parsed
    /// (`Presence::AddressUnreadable` in `wallet_session.cpp:2907-2913`).
    AddressUnreadable {
        /// Original unparseable address string from the server.
        raw_address: String,
    },
}

/// Entry from `wallet.getUserAddresses` (`walletUserAddress#fe78eecc`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserAddressEntry {
    /// Optional Telegram user ID (`flags.0`).
    pub user_id: Option<i64>,
    /// Canonical raw TON address (`workchain:64-hex`), empty if unparseable.
    pub address: String,
    /// Raw address string from the server.
    pub raw_address: String,
    /// Wallet public key (32 bytes when valid).
    pub public_key: Vec<u8>,
}

/// Response from `wallet.getUserAddresses` (`wallet.userAddresses#928e7b55`).
#[derive(Debug, Clone, PartialEq)]
pub struct UserAddressesResponse {
    /// Parsed user address entries.
    pub addresses: Vec<UserAddressEntry>,
    /// Accompanying Telegram `User` objects for peer resolution.
    pub users: Vec<tl_enums::User>,
}

/// Direction filter for `wallet.getTransactions#a8830a83`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TransactionDirectionFilter {
    /// Both flags unset (tdesktop default undivided feed).
    #[default]
    All,
    /// Only incoming transactions (`flags.0 = inbound`).
    InboundOnly,
    /// Only outgoing transactions (`flags.1 = outbound`).
    OutboundOnly,
}

/// Proxied web document metadata (`Gram::NftWebDocument` in `wallet_session.cpp:1692-1707`).
///
/// `webDocumentNoProxy` is intentionally dropped so clients never leak IP addresses directly to
/// third-party media hosts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProxiedWebDocument {
    /// Remote media URL proxied via Telegram's webfile DC.
    pub url: String,
    /// Telegram `access_hash` for `inputWebFileLocation`.
    pub access_hash: i64,
    /// Reported byte size.
    pub size: i32,
    /// MIME type (e.g. `"image/png"`, `"application/x-tgsticker"`).
    pub mime_type: String,
}

/// Key/value NFT trait (`wallet.nftAttribute#4c1ef10e`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerNftAttribute {
    /// Attribute name (`trait_type`).
    pub trait_type: String,
    /// Attribute value.
    pub value: String,
}

/// Parsed `wallet.nftItem#173dfaf3` (`CollectibleRecord` in `wallet_session.cpp:1709-1735`).
#[derive(Debug, Clone, PartialEq)]
pub struct ServerNftItem {
    /// Core `tg-wallet` [`NftItem`] metadata (canonicalized & classified).
    pub item: NftItem,
    /// Optional description text.
    pub description: Option<String>,
    /// Proxied primary image (`webDocument` only; `webDocumentNoProxy` dropped).
    pub image: Option<ProxiedWebDocument>,
    /// Proxied thumbnail image.
    pub image_small: Option<ProxiedWebDocument>,
    /// Proxied content URL.
    pub content_url: Option<ProxiedWebDocument>,
    /// Proxied Lottie animation document.
    pub lottie: Option<ProxiedWebDocument>,
    /// Optional trait attributes.
    pub attributes: Vec<ServerNftAttribute>,
    /// Optional raw JSON string from `extra: DataJSON`.
    pub extra_json: Option<String>,
    /// Optional accompanying `StarGift` payload (for unique Telegram gifts).
    pub gift: Option<tl_enums::StarGift>,
}

/// Paginated response from `wallet.getNfts` (`wallet.nftItems#794d486f`).
#[derive(Debug, Clone, PartialEq)]
pub struct ServerNftPage {
    /// Deduplicated collectibles on this page (invalid addresses dropped).
    pub items: Vec<ServerNftItem>,
    /// Opaque next-page cursor (`None` when exhausted or repeated).
    pub next_offset: Option<String>,
}

/// Classified transaction kind matching `TransferItem::Kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ServerTransactionKind {
    /// Standard TON address transfer.
    #[default]
    Transfer,
    /// Transfer to/from a Telegram user.
    PeerTransfer,
    /// NFT / collectible transfer.
    Collectible,
    /// Fiat onramp purchase.
    Onramp,
    /// Wallet contract key-rotation transaction.
    KeyChange,
}

/// Gift movement classification for `walletTransactionPeerTelegramGift`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GiftMoveTarget {
    /// Gift moved to/from Telegram generally (`own_profile = false`).
    Telegram,
    /// Gift moved to/from the user's own Telegram profile (`own_profile = true`).
    OwnProfile,
}

/// Parsed `WalletTransactionPeer`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServerTransactionPeer {
    /// Telegram user (`walletTransactionPeerUser#d4ea706d`).
    User {
        /// Telegram user ID.
        user_id: i64,
        /// Canonical raw TON address (empty if server sent unparseable address).
        address: String,
        /// Whether the raw input was a friendly bounceable address.
        bounceable: bool,
        /// Optional TON DNS / `.t.me` domain.
        domain: Option<String>,
    },
    /// Raw TON address (`walletTransactionPeerAddress#062cc1bc`).
    Address {
        /// Canonical raw TON address.
        address: String,
        /// Whether the raw input was a friendly bounceable address.
        bounceable: bool,
        /// Optional domain.
        domain: Option<String>,
    },
    /// Fiat onramp provider (`walletTransactionPeerOnramp#e595186e`).
    Onramp {
        /// Canonical raw TON address.
        address: String,
        /// Whether the raw input was a friendly bounceable address.
        bounceable: bool,
        /// Optional domain.
        domain: Option<String>,
        /// Trimmed provider name.
        provider_name: String,
    },
    /// Telegram channel (`walletTransactionPeerChannel#54e70c31`).
    Channel {
        /// Telegram channel ID.
        channel_id: i64,
        /// Canonical raw TON address.
        address: String,
        /// Whether the raw input was a friendly bounceable address.
        bounceable: bool,
        /// Optional domain.
        domain: Option<String>,
    },
    /// Telegram Star Gift move (`walletTransactionPeerTelegramGift#f4729499`).
    TelegramGift {
        /// Target profile classification.
        target: GiftMoveTarget,
    },
    /// Forward-compatible unknown peer (`walletTransactionPeerUnsupported#728bed5a`).
    Unsupported,
}

/// Parsed `walletTransaction#952dc143` matching `HistoryItemFromServer` (`wallet_session.cpp:1761`).
#[derive(Debug, Clone, PartialEq)]
pub struct ServerWalletTransaction {
    /// Server-assigned opaque transaction ID (typically `lt:hash`).
    pub id: String,
    /// Transaction category.
    pub kind: ServerTransactionKind,
    /// Direction after `SetDirectedAmount` and `key_change` reconciliation.
    pub incoming: bool,
    /// Non-negative nanoTON magnitude (or `i64::MIN` if server sent `i64::MIN`).
    pub amount_nano: i64,
    /// Fee in nanoTON.
    pub fee_nano: i64,
    /// Unix timestamp in seconds.
    pub date: i32,
    /// Whether this transfer was sponsored by the gasless relayer.
    pub gasless: bool,
    /// Whether on-chain execution failed.
    pub failed: bool,
    /// Whether this transaction rotated the wallet key (`flags.5`).
    pub key_change: bool,
    /// Whether the comment is encrypted (`flags.6`).
    pub comment_encrypted: bool,
    /// Plaintext comment (only populated when `!comment_encrypted`).
    pub comment: Option<String>,
    /// Decoded server encrypted comment payload (only when `comment_encrypted` and valid).
    pub encrypted_comment_payload: Option<Vec<u8>>,
    /// Decoded 32-byte transaction hash (from 64-hex or 32-byte base64/base64url `tx_hash`).
    pub tx_hash: Option<[u8; 32]>,
    /// Counterparty peer (`None` for `KeyChange`, matching `wallet_session.cpp:1777`).
    pub peer: Option<ServerTransactionPeer>,
    /// Optional NFT attached to this transaction (`flags.7`).
    pub nft: Option<ServerNftItem>,
}

/// Response from `wallet.getTransactions*` (`wallet.transactions#4322d5a5`).
#[derive(Debug, Clone, PartialEq)]
pub struct WalletTransactionsPage {
    /// Cached wallet balance in nanoTON.
    pub balance_nano: i64,
    /// Converted transactions.
    pub transactions: Vec<ServerWalletTransaction>,
    /// Opaque next-page cursor (`None` when absent, empty, or identical to requested offset).
    pub next_offset: Option<String>,
    /// Accompanying `Chat` objects for peer resolution.
    pub chats: Vec<tl_enums::Chat>,
    /// Accompanying `User` objects for peer resolution.
    pub users: Vec<tl_enums::User>,
}

/// Validated `wallet.secretPhraseParts#e6d0ef01` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretPhrasePartsInfo {
    /// One-time export token passed to `wallet.fetchEncryptedSecretPhrasePart`.
    pub token: String,
    /// Unique, validated holder DC IDs (`1..DC_SHIFT`).
    pub dcs: Vec<DcId>,
}

/// Validated `wallet.holderDc#f9d612ef` entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupHolderDc {
    /// Holder data center ID (`> 0`).
    pub dc: DcId,
    /// 32-byte X25519 public key used to seal the XOR phrase share for this holder DC.
    pub public_key: [u8; PHRASE_SHARE_PUBLIC_KEY_SIZE],
}

/// Validated `wallet.proofChallenge#99e41707` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProofChallengeInfo {
    /// Challenge payload string to sign in TON Connect proof format.
    pub payload: String,
    /// Challenge expiration timestamp in seconds.
    pub expires: i32,
    /// Ownership proof domain (e.g. `"telegram.org"`).
    pub domain: String,
}

/// Ownership proof payload (`walletOwnershipProof#60bccb0d`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnershipProofInput {
    /// Unix timestamp in seconds when the proof was signed.
    pub timestamp: i32,
    /// 64-byte Ed25519 signature.
    pub signature: Vec<u8>,
}

impl OwnershipProofInput {
    fn to_tl(&self) -> Result<tl_enums::WalletOwnershipProof, WalletMtprotoError> {
        if self.signature.len() != OWNERSHIP_PROOF_SIGNATURE_SIZE {
            return Err(WalletMtprotoError::Validation {
                code: "WALLET_PROOF_INVALID",
                detail: format!(
                    "ownership proof signature must be {OWNERSHIP_PROOF_SIGNATURE_SIZE} bytes, got {}",
                    self.signature.len()
                ),
            });
        }
        Ok(tl_enums::WalletOwnershipProof::WalletOwnershipProof(
            tl_types::WalletOwnershipProof {
                timestamp: self.timestamp,
                signature: self.signature.clone(),
            },
        ))
    }
}

/// Wallet replacement input (`InputWalletReplacement`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WalletReplacement {
    /// Provision a brand-new server wallet (`inputWalletNew#63a440dc`).
    New,
    /// Import an existing wallet with one-shot ownership proof (`inputWalletImported#66a66a3b`).
    Imported {
        /// Active 32-byte signing public key.
        public_key: Vec<u8>,
        /// Original 32-byte anchor public key (only set when the wallet key was rotated).
        anchor_public_key: Option<Vec<u8>>,
        /// One-shot ownership proof signed under `public_key`.
        proof: OwnershipProofInput,
    },
}

/// Request arguments for `wallet.enableBackup#4500c525`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnableBackupRequest {
    /// Sealed holder share blobs (one per holder DC returned by `wallet.getBackupHolderDcs`).
    pub parts: Vec<Vec<u8>>,
    /// Optional 32-byte signing public key (`flags.1`).
    pub new_public_key: Option<Vec<u8>>,
    /// Optional one-shot ownership proof (`flags.2`).
    pub proof: Option<OwnershipProofInput>,
}

/// Request arguments for `wallet.disableBackup#0b0bf0da`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DisableBackupRequest {
    /// Optional 2FA SRP password check (`flags.0`).
    pub password: Option<tl_enums::InputCheckPasswordSRP>,
    /// Optional 32-byte signing public key (`flags.1`).
    pub new_public_key: Option<Vec<u8>>,
    /// Optional one-shot ownership proof (`flags.2`).
    pub proof: Option<OwnershipProofInput>,
}

/// Request arguments for `wallet.sendTransfer#d9e5d9e2`.
#[derive(Debug, Clone, PartialEq)]
pub struct SendTransferRequest {
    /// Signed normal (paid-fee) external message BOC bytes (mandatory).
    pub data_normal: Vec<u8>,
    /// Optional signed gasless (relayer-sponsored) external message BOC bytes (`flags.0`).
    pub data_gasless: Option<Vec<u8>>,
    /// Optional Telegram recipient (`InputUser::Empty` when sending to a raw TON address).
    pub user_id: tl_enums::InputUser,
    /// Non-zero idempotency / chat-message random ID.
    pub random_id: i64,
}

/// Receipt extracted from `updateSentWalletTransaction#b1434d4a` in `wallet.sendTransfer`'s `Updates`.
#[derive(Debug, Clone, PartialEq)]
pub struct TransferReceipt {
    /// Opaque server message hash token echoed verbatim into `wallet.getTransactionsByMsgHash`.
    pub msg_hash: String,
    /// Whether the server accepted the gasless variant (`flags.0`).
    pub gasless: bool,
    /// Optional immediate transaction record (`flags.1`).
    pub transaction: Option<ServerWalletTransaction>,
}

/// Full result of `wallet.sendTransfer#d9e5d9e2`.
#[derive(Debug, Clone, PartialEq)]
pub struct SendTransferResult {
    /// Parsed transfer receipt from `updateSentWalletTransaction`.
    pub receipt: TransferReceipt,
    /// Full raw `Updates` container so the caller can forward it to the session update processor.
    pub updates: tl_enums::Updates,
}

/// Parsed `updateWalletGaslessInfo#a8ae1cac` (`Wallet::GaslessInfo` in `wallet_session.cpp:323`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct GaslessInfo {
    /// Whether gasless transfers are currently available (`flags.0`).
    pub available: bool,
    /// Remaining sponsored transfers in the current window.
    pub left: i32,
    /// Unix timestamp when the quota resets.
    pub reset_at: i32,
    /// Minimum transfer amount in nanoTON required by the server.
    pub min_amount: i64,
    /// Canonical raw mainnet relayer address (empty if unparseable or testnet).
    pub relayer_address: String,
}

/// Effective gasless eligibility terms (`Wallet::GaslessTerms` in `wallet_session.cpp:2479-2495`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GaslessTerms {
    /// Server-reported gasless quota and relayer info.
    pub info: GaslessInfo,
    /// Minimum transfer amount from app config (`wallet_transfer_min_nanos`).
    pub transfer_min_nanos: i64,
    /// Minimum gasless amount from app config (`wallet_gasless_min_nanos`).
    pub configured_min_nanos: i64,
    /// Effective minimum (`max(transfer_min_nanos, configured_min_nanos, info.min_amount)`).
    pub effective_min_nanos: i64,
    /// Canonical raw address of the active wallet (used to disallow self-transfers).
    pub own_address: Option<String>,
}

impl GaslessTerms {
    /// Computes [`GaslessTerms`] from server [`GaslessInfo`] and configured minimums.
    #[must_use]
    pub fn compute(
        info: GaslessInfo,
        own_address: Option<String>,
        transfer_min_nanos: i64,
        configured_min_nanos: i64,
    ) -> Self {
        let effective_min_nanos = transfer_min_nanos
            .max(configured_min_nanos)
            .max(info.min_amount);
        Self {
            info,
            transfer_min_nanos,
            configured_min_nanos,
            effective_min_nanos,
            own_address: own_address.and_then(|a| canonical_address(&a)),
        }
    }

    /// Returns `true` when gasless sponsorship is usable at all.
    #[must_use]
    pub fn is_usable(&self) -> bool {
        self.info.available && self.info.left > 0 && !self.info.relayer_address.is_empty()
    }

    /// Mirrors `GaslessTerms::eligible(amountNano)` (`wallet_session.cpp:2479`).
    #[must_use]
    pub fn is_amount_eligible(&self, amount_nano: i64) -> bool {
        self.is_usable() && amount_nano > 0 && amount_nano >= self.effective_min_nanos
    }

    /// Mirrors `GaslessTerms::eligible(amountNano, destination)` (`wallet_session.cpp:2488-2495`):
    /// the relayer rejects self-transfers, so destinations matching `own_address` are ineligible.
    #[must_use]
    pub fn is_eligible_for(&self, amount_nano: i64, destination: &str) -> bool {
        if !self.is_amount_eligible(amount_nano) {
            return false;
        }
        let Some(own) = self.own_address.as_deref() else {
            return false;
        };
        let Some(dest_canonical) = canonical_address(destination) else {
            return false;
        };
        dest_canonical != own
    }
}

/// Parsed `wallet.existingBalance#bde90a8d`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExistingWaltBalance {
    /// Whether the legacy `@wallet` bot holds an existing balance (`flags.0`).
    pub has_balance: bool,
    /// Migration URL (cleared when `!has_balance`, matching `wallet_session.cpp:2875-2877`).
    pub url: Option<String>,
}

/// Parsed `tonConnectManifest#12229264` (`Wallet::TonConnectManifest`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerTonConnectManifest {
    /// Canonical dApp URL.
    pub url: String,
    /// Display name of the dApp.
    pub name: String,
    /// Proxied icon (`webDocument` only; `webDocumentNoProxy` dropped to avoid IP leak).
    pub icon: Option<ProxiedWebDocument>,
}

/// Parsed `tonConnectSession#126556c6` (`Wallet::TonConnectSessionInfo`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerTonConnectSession {
    /// Server-assigned session ID.
    pub id: i64,
    /// Remote dApp X25519 public key (hex).
    pub dapp_client_id: String,
    /// Wallet-side X25519 public key registered for this session (`flags.3`).
    pub client_id: Option<String>,
    /// Server-provided nonce bytes used to derive the session key.
    pub nonce: Vec<u8>,
    /// Fetched dApp manifest (`flags.4`).
    pub manifest: Option<ServerTonConnectManifest>,
    /// Manifest fetch/validation error code (`flags.5`).
    pub manifest_error: Option<i32>,
    /// Session creation timestamp (seconds).
    pub date: i32,
    /// Resolved lifecycle status (`closed > closing > pending > active`).
    pub status: TonConnectSessionStatus,
}

/// Parsed `wallet.tonConnectChallenge#4bc89693`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerTonConnectChallenge {
    /// Challenge bytes to sign/answer with the session key.
    pub challenge: Vec<u8>,
    /// Monotonic event ID allocated for the connect event.
    pub event_id: i64,
}

/// Parsed `tonConnectRequest#a15f8513`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerTonConnectRequest {
    /// Session ID this request belongs to.
    pub session_id: i64,
    /// Server message ID for claiming and submitting a response.
    pub msg_id: i32,
    /// Encrypted TON Connect JSON-RPC request body bytes.
    pub body: Vec<u8>,
    /// Expiration unix timestamp (seconds).
    pub expires: i32,
    /// Optional request topic (`flags.0`, e.g. `"sendTransaction"` or `"signData"`).
    pub topic: Option<String>,
    /// Optional UUID trace ID (`flags.1`).
    pub trace_id: Option<String>,
}

/// Parsed `wallet.tonConnectPending#85c0f124`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerTonConnectPending {
    /// Current session state.
    pub session: ServerTonConnectSession,
    /// Pending requests awaiting claim/response.
    pub requests: Vec<ServerTonConnectRequest>,
}

/// Selector for `wallet.tonConnectGetPending#11aee065`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TonConnectPendingQuery {
    /// Query by dApp client ID (`flags.0`).
    ByDappClientId(String),
    /// Query by server session ID (`flags.1`).
    BySessionId(i64),
    /// Query with both fields explicitly specified.
    Both {
        /// dApp client ID (`flags.0`).
        dapp_client_id: String,
        /// Session ID (`flags.1`).
        session_id: i64,
    },
}

/// Request arguments for `wallet.tonConnectSubmitConnectResult#c2c00779`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TonConnectSubmitConnectResultRequest {
    /// `true` when submitting a rejection / error response (`flags.0`).
    pub error: bool,
    /// Session ID.
    pub session_id: i64,
    /// Signed/answered challenge bytes.
    pub challenge_answer: Vec<u8>,
    /// Encrypted connect event or connect error body bytes.
    pub body: Vec<u8>,
    /// Optional trace ID (`flags.1`, omitted if empty).
    pub trace_id: Option<String>,
}

/// Request arguments for `wallet.tonConnectClaimRequest#66b57b41`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TonConnectClaimRequest {
    /// `true` when claiming in order to decline the request (`flags.1`).
    pub declined: bool,
    /// Session ID.
    pub session_id: i64,
    /// Request message ID.
    pub msg_id: i32,
    /// Decrypted JSON-RPC `id` string from the dApp request.
    pub app_request_id: String,
    /// Challenge answer bytes (`flags.0`, omitted for late claims per `wallet_ton_connect_request.cpp:1861`).
    pub challenge_answer: Option<Vec<u8>>,
}

/// Request arguments for `wallet.tonConnectSubmitResponse#fac67d3c`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TonConnectSubmitResponseRequest {
    /// Session ID.
    pub session_id: i64,
    /// Request message ID.
    pub msg_id: i32,
    /// Encrypted JSON-RPC response body bytes.
    pub body: Vec<u8>,
    /// Optional trace ID (`flags.0`, omitted if empty).
    pub trace_id: Option<String>,
}

/// Normalized placement rectangle for a wallet card sticker (`MediaAreaCoordinates`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CardStickerCoordinates {
    /// Horizontal center percentage (`0.0..=100.0`).
    pub x: f64,
    /// Vertical center percentage (`0.0..=100.0`).
    pub y: f64,
    /// Width percentage (`(0.0..=100.0]`).
    pub w: f64,
    /// Height percentage (`(0.0..=100.0]`).
    pub h: f64,
    /// Clockwise rotation in degrees (finite).
    pub rotation: f64,
    /// Optional corner radius (`>= 0.0`, `flags.0`).
    pub radius: Option<f64>,
}

impl CardStickerCoordinates {
    /// Validates coordinate ranges matching `ValidCoordinates` (`data_user_wallet_card.cpp:18-34`).
    #[must_use]
    pub fn is_valid(&self) -> bool {
        let pos_ok = |v: f64| v.is_finite() && (0.0..=100.0).contains(&v);
        let ext_ok = |v: f64| v.is_finite() && v > 0.0 && v <= 100.0;
        let radius_ok = self.radius.is_none_or(|r| r.is_finite() && r >= 0.0);
        pos_ok(self.x)
            && pos_ok(self.y)
            && ext_ok(self.w)
            && ext_ok(self.h)
            && self.rotation.is_finite()
            && radius_ok
    }

    fn to_tl(self) -> tl_enums::MediaAreaCoordinates {
        tl_enums::MediaAreaCoordinates::MediaAreaCoordinates(tl_types::MediaAreaCoordinates {
            x: self.x,
            y: self.y,
            w: self.w,
            h: self.h,
            rotation: self.rotation,
            radius: self.radius,
        })
    }

    fn from_tl(coords: &tl_enums::MediaAreaCoordinates) -> Self {
        let tl_enums::MediaAreaCoordinates::MediaAreaCoordinates(c) = coords;
        Self {
            x: c.x,
            y: c.y,
            w: c.w,
            h: c.h,
            rotation: c.rotation,
            radius: c.radius,
        }
    }
}

/// Input sticker item for `wallet.setCardStickers#068509ac`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CardStickerInput {
    /// Placement coordinates on the card.
    pub coordinates: CardStickerCoordinates,
    /// NFT address of the collectible gift.
    pub nft_address: String,
}

/// Parsed sticker from `wallet.cardSticker#f3010cba`.
#[derive(Debug, Clone, PartialEq)]
pub struct WalletCardStickerInfo {
    /// Placement coordinates.
    pub coordinates: CardStickerCoordinates,
    /// Underlying NFT item.
    pub nft: ServerNftItem,
}

/// Parsed `wallet.card#8ed043b1` (`Data::WalletCard` in `data_user_wallet_card.cpp:95-126`).
#[derive(Debug, Clone, PartialEq)]
pub struct WalletCardInfo {
    /// Card wallet address string.
    pub address: String,
    /// Card balance in nanoTON.
    pub balance_nano: i64,
    /// Validated stickers (capped at [`WALLET_CARD_MAX_STICKERS`]).
    pub stickers: Vec<WalletCardStickerInfo>,
    /// Optional background NFT item (canonical address matched against `stickers`).
    pub bg_nft_item: Option<ServerNftItem>,
    /// Canonical address of the matched background sticker, if valid.
    pub background_address: Option<String>,
    /// Whether every server sticker and background parsed cleanly with unique addresses.
    pub stickers_complete: bool,
}

/// Request arguments for `payments.depositStarGift#c0a2ea9f`.
#[derive(Debug, Clone, PartialEq)]
pub struct DepositStarGiftRequest {
    /// Whether to hide sender info (`flags.0`).
    pub hide_sender: bool,
    /// Recipient peer (`InputPeer::PeerSelf` in `wallet_session_deposit.cpp:92`).
    pub peer: tl_enums::InputPeer,
    /// Unique Star Gift slug (e.g. `"PlushPepe-42"`).
    pub slug: String,
}

impl DepositStarGiftRequest {
    /// Convenience constructor targeting `InputPeerSelf` with `hide_sender = false`
    /// (matching `RequestGiftDeposit` in `wallet_session_deposit.cpp:90-94`).
    #[must_use]
    pub fn for_self(slug: impl Into<String>) -> Self {
        Self {
            hide_sender: false,
            peer: tl_enums::InputPeer::Self_,
            slug: slug.into(),
        }
    }
}

/// Parsed `payments.starGiftDepositPayload#725bb4eb`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StarGiftDepositInfo {
    /// Canonical raw NFT address (or raw string if unparseable).
    pub nft_address: String,
    /// Raw BOC bytes returned by Telegram.
    pub payload_bytes: Vec<u8>,
    /// Standard base64-encoded BOC payload.
    pub payload_base64: String,
    /// Escrow destination extracted from the TEP-62 NFT transfer payload (`nft_transfer_new_owner`).
    pub escrow_address: Option<String>,
    /// Required attached TON amount in nanoTON.
    pub amount_nano: i64,
}

impl StarGiftDepositInfo {
    /// Validates the server deposit payload against the expected collectible NFT address
    /// (`GiftDepositFromServer` in `wallet_session_deposit.cpp:107-130`).
    pub fn verify_for_collectible(
        &self,
        expected_collectible: &str,
    ) -> Result<VerifiedGiftDeposit, WalletMtprotoError> {
        let Some(expected) = canonical_address(expected_collectible) else {
            return Err(WalletMtprotoError::InvalidResponse {
                code: "GIFT_DEPOSIT_ADDRESS_MISMATCH",
                detail: "expected collectible address is not parseable".to_owned(),
            });
        };
        let Some(served) = canonical_address(&self.nft_address) else {
            return Err(WalletMtprotoError::InvalidResponse {
                code: "GIFT_DEPOSIT_ADDRESS_MISMATCH",
                detail: "server nft_address is not parseable".to_owned(),
            });
        };
        if served != expected {
            return Err(WalletMtprotoError::InvalidResponse {
                code: "GIFT_DEPOSIT_ADDRESS_MISMATCH",
                detail: format!("nft_address mismatch: expected {expected}, got {served}"),
            });
        }
        if self.amount_nano <= 0 {
            return Err(WalletMtprotoError::InvalidResponse {
                code: "GIFT_DEPOSIT_INVALID_ANSWER",
                detail: format!("non-positive deposit amount: {}", self.amount_nano),
            });
        }
        let escrow = self
            .escrow_address
            .as_deref()
            .and_then(canonical_address)
            .ok_or_else(|| WalletMtprotoError::InvalidResponse {
                code: "GIFT_DEPOSIT_INVALID_ANSWER",
                detail: "deposit BOC payload does not contain a valid TEP-62 new_owner address"
                    .to_owned(),
            })?;
        Ok(VerifiedGiftDeposit {
            destination_escrow: escrow,
            collectible_nft: served,
            payload_base64: self.payload_base64.clone(),
            amount_nano: self.amount_nano,
            bounce: true,
        })
    }
}

/// Validated gift deposit send arguments (`SendArgs` from `GiftDepositFromServer`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifiedGiftDeposit {
    /// Canonical escrow destination extracted from the TEP-62 NFT transfer payload.
    pub destination_escrow: String,
    /// Canonical NFT collectible address.
    pub collectible_nft: String,
    /// Standard base64-encoded BOC payload.
    pub payload_base64: String,
    /// Attached TON amount in nanoTON (`> 0`).
    pub amount_nano: i64,
    /// Always `true` in tdesktop (`wallet_session_deposit.cpp:127`).
    pub bounce: bool,
}

/// Cached fiat exchange rates (`Wallet::Rates` in `wallet_rates.cpp`).
#[derive(Debug, Clone, Default)]
pub struct FiatRateCache {
    rates: Option<CurrencyRates>,
    selected_currency: String,
    usd_per_ton: f64,
}

impl FiatRateCache {
    /// Creates a new cache defaulting to `"USD"` (`kDefaultFiatCurrency`).
    #[must_use]
    pub fn new(selected_currency: impl Into<String>, usd_per_ton: f64) -> Self {
        let norm = selected_currency.into().trim().to_ascii_uppercase();
        Self {
            rates: None,
            selected_currency: if norm.is_empty() {
                "USD".to_owned()
            } else {
                norm
            },
            usd_per_ton,
        }
    }

    /// Updates the cached `payments.currencyRates` table.
    pub fn apply_rates(&mut self, rates: CurrencyRates) {
        self.rates = Some(rates);
    }

    /// Updates `ton_usd_rate` from `help.getAppConfig`.
    pub const fn set_usd_per_ton(&mut self, usd_per_ton: f64) {
        self.usd_per_ton = usd_per_ton;
    }

    /// Changes the selected fiat currency code (normalized to ASCII uppercase).
    pub fn set_currency(&mut self, code: &str) {
        let norm = code.trim().to_ascii_uppercase();
        if !norm.is_empty() {
            self.selected_currency = norm;
        }
    }

    /// Returns the currently selected ISO currency code.
    #[must_use]
    pub fn selected_currency(&self) -> &str {
        &self.selected_currency
    }

    /// Computes fiat value per 1 TON in the currently selected currency.
    #[must_use]
    pub fn current_per_ton(&self) -> Option<f64> {
        self.rates
            .as_ref()?
            .compute_per_ton(&self.selected_currency, self.usd_per_ton)
    }

    /// Returns sorted available currency codes.
    #[must_use]
    pub fn available_currencies(&self) -> Vec<String> {
        let Some(rates) = self.rates.as_ref() else {
            return vec![self.selected_currency.clone()];
        };
        let mut codes: Vec<String> = rates.values.keys().cloned().collect();
        if !codes.iter().any(|c| c == &self.selected_currency) {
            codes.push(self.selected_currency.clone());
        }
        codes.sort();
        codes
    }
}

/// Parsed server-pushed wallet update (`Update::Wallet*` & `Update::SentWalletTransaction`).
#[derive(Debug, Clone, PartialEq)]
pub enum WalletServerUpdate {
    /// `updateWalletState#6ac3f2aa`.
    WalletState(ServerWalletState),
    /// `updateSentWalletTransaction#b1434d4a`.
    SentWalletTransaction(Box<TransferReceipt>),
    /// `updateWalletGaslessInfo#a8ae1cac`.
    GaslessInfo(GaslessInfo),
    /// `updateWalletTonConnectSession#50a38e0e`.
    TonConnectSession(ServerTonConnectSession),
    /// `updateWalletTonConnectPendingDisconnect#d1ee8199`.
    TonConnectPendingDisconnect(Vec<i64>),
}

// ============================================================================
// MTProto Toncenter Transport + tdesktop Transport Selection
// ============================================================================

/// Returns `true` if `req` is a Toncenter JSON-RPC `sendBoc` call (`IsSendBocRequest` in
/// `wallet_engine.cpp:107-114`), which tdesktop intercepts and routes through `wallet.sendTransfer`.
#[must_use]
pub fn is_send_boc_json_rpc(req: &GramHttpRequest) -> bool {
    if !req.post || req.endpoint != "/api/v2/jsonRPC" {
        return false;
    }
    serde_json::from_slice::<Value>(&req.payload)
        .ok()
        .and_then(|v| v.get("method")?.as_str().map(|m| m == "sendBoc"))
        .unwrap_or(false)
}

/// Extracts decoded BOC bytes from a Toncenter JSON-RPC `sendBoc` request (`RoutedSendBoc` in
/// `wallet_engine.cpp:116-126`).
#[must_use]
pub fn extract_routed_send_boc(req: &GramHttpRequest) -> Option<Vec<u8>> {
    if !is_send_boc_json_rpc(req) {
        return None;
    }
    let val: Value = serde_json::from_slice(&req.payload).ok()?;
    let boc_b64 = val.get("params")?.get("boc")?.as_str()?;
    STANDARD.decode(boc_b64).ok()
}

/// Returns `true` if an error represents a Toncenter timeout (`Wallet::Api::IsTimeoutError`).
#[must_use]
pub fn is_toncenter_timeout_error(message: &str) -> bool {
    message == TONCENTER_TIMEOUT_ERROR
}

/// Returns `true` if a Toncenter error message indicates an empty pending traces list
/// (`wallet_engine.cpp:750-754`).
#[must_use]
pub fn is_empty_pending_traces_error(message: &str) -> bool {
    message == EMULATED_TRACES_NOT_FOUND
}

/// [`ToncenterTransport`] implementation routing `GramHttpRequest` through Telegram's native
/// MTProto proxy (`toncenter.performApiRequest` & `toncenter.getStreamingUrl`) on
/// `shift_dc_id(webfile_dc_id, TONCENTER_DC_SHIFT)`.
///
/// Mirrors `Wallet::Api` (`Telegram/SourceFiles/wallet/wallet_api.cpp`).
#[derive(Debug)]
pub struct MtprotoToncenterTransport<C> {
    rpc: C,
    webfile_dc_id: AtomicI32,
    timeout: Duration,
}

impl<C: Clone> Clone for MtprotoToncenterTransport<C> {
    fn clone(&self) -> Self {
        Self {
            rpc: self.rpc.clone(),
            webfile_dc_id: AtomicI32::new(self.webfile_dc_id.load(Ordering::Relaxed)),
            timeout: self.timeout,
        }
    }
}

impl<C: RpcClient> MtprotoToncenterTransport<C> {
    /// Creates a new MTProto Toncenter transport targeting `webfile_dc_id`.
    #[must_use]
    pub const fn new(rpc: C, webfile_dc_id: DcId) -> Self {
        Self {
            rpc,
            webfile_dc_id: AtomicI32::new(webfile_dc_id),
            timeout: DEFAULT_TONCENTER_TIMEOUT,
        }
    }

    /// Overrides the per-request timeout (defaults to 30 seconds, matching `wallet_api.cpp:18`).
    #[must_use]
    pub const fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Updates `webfile_dc_id` (e.g. after `help.getConfig` refreshes server configuration).
    pub fn set_webfile_dc_id(&self, webfile_dc_id: DcId) {
        self.webfile_dc_id.store(webfile_dc_id, Ordering::Relaxed);
    }

    /// Returns the current bare `webfile_dc_id`.
    #[must_use]
    pub fn webfile_dc_id(&self) -> DcId {
        self.webfile_dc_id.load(Ordering::Relaxed)
    }

    /// Computes `shift_dc_id(webfile_dc_id, TONCENTER_DC_SHIFT)`, or `None` if invalid.
    #[must_use]
    pub fn shifted_dc_id(&self) -> Option<DcId> {
        toncenter_shifted_dc_id(self.webfile_dc_id())
    }

    /// Invokes `toncenter.performApiRequest#8d7bdd61` on the shifted webfile DC.
    pub async fn perform_api_request(
        &self,
        request: GramHttpRequest,
    ) -> Result<Vec<u8>, WalletMtprotoError> {
        let Some(dc_id) = self.shifted_dc_id() else {
            return Err(WalletMtprotoError::Validation {
                code: WEBFILE_DC_INVALID,
                detail: format!("invalid webfile_dc_id {}", self.webfile_dc_id()),
            });
        };
        let payload = if request.payload.is_empty() {
            None
        } else {
            Some(String::from_utf8(request.payload).map_err(|e| {
                WalletMtprotoError::Validation {
                    code: "TONCENTER_PAYLOAD_INVALID",
                    detail: format!("toncenter payload must be UTF-8: {e}"),
                }
            })?)
        };
        let tl_req = tl_fns::toncenter::PerformApiRequest {
            post: request.post,
            endpoint: request.endpoint,
            query: (!request.query.is_empty()).then_some(request.query),
            payload,
        };
        let resp = tokio::time::timeout(self.timeout, self.rpc.invoke_in_dc(dc_id, &tl_req))
            .await
            .map_err(|_| WalletMtprotoError::ToncenterApi {
                code: 0,
                message: TONCENTER_TIMEOUT_ERROR.to_owned(),
            })?
            .map_err(Into::<InvocationError>::into)?;

        let tl_enums::toncenter::ApiResponse::ApiResponse(inner) = resp;
        let tl_enums::DataJSON::DataJSON(json_data) = inner.response;
        let bytes = json_data.data.into_bytes();
        if let Some(api_err) = parse_api_error(&bytes) {
            return Err(WalletMtprotoError::ToncenterApi {
                code: api_err.code,
                message: api_err.message,
            });
        }
        Ok(bytes)
    }

    /// Invokes `toncenter.getStreamingUrl#cdaf63c7` on the shifted webfile DC.
    pub async fn request_streaming_url(&self) -> Result<StreamingUrlInfo, WalletMtprotoError> {
        let Some(dc_id) = self.shifted_dc_id() else {
            return Err(WalletMtprotoError::Validation {
                code: WEBFILE_DC_INVALID,
                detail: format!("invalid webfile_dc_id {}", self.webfile_dc_id()),
            });
        };
        let tl_req = tl_fns::toncenter::GetStreamingUrl;
        let resp = tokio::time::timeout(self.timeout, self.rpc.invoke_in_dc(dc_id, &tl_req))
            .await
            .map_err(|_| WalletMtprotoError::ToncenterApi {
                code: 0,
                message: TONCENTER_TIMEOUT_ERROR.to_owned(),
            })?
            .map_err(Into::<InvocationError>::into)?;

        let tl_enums::toncenter::StreamingUrl::StreamingUrl(inner) = resp;
        let endpoint = parse_stream_endpoint(&inner.url);
        Ok(StreamingUrlInfo {
            url: inner.url,
            expires: inner.expires,
            endpoint,
        })
    }
}

impl<C: RpcClient + 'static> ToncenterTransport for MtprotoToncenterTransport<C> {
    fn execute<'a>(
        &'a self,
        request: GramHttpRequest,
    ) -> BoxFuture<'a, Result<Vec<u8>, WalletError>> {
        Box::pin(async move {
            self.perform_api_request(request)
                .await
                .map_err(WalletError::from)
        })
    }

    fn get_streaming_url<'a>(&'a self) -> BoxFuture<'a, Result<StreamingUrlInfo, WalletError>> {
        Box::pin(async move {
            self.request_streaming_url()
                .await
                .map_err(WalletError::from)
        })
    }
}

/// Selected transport plus its active [`ToncenterTransportKind`].
pub struct SelectedToncenterTransport {
    /// Which path was chosen (`Mtproto` or `Http`).
    pub kind: ToncenterTransportKind,
    /// Object-safe transport ready to wrap in [`ToncenterClient`].
    pub transport: Arc<dyn ToncenterTransport>,
}

/// Chooses between Telegram's native MTProto `toncenter.*` transport and the direct HTTP
/// Toncenter transport using tdesktop's selection rules:
/// - [`ToncenterTransportPolicy::AutoPreferMtproto`] (default): uses MTProto unless an explicit
///   non-empty `config.toncenter_base_url` override is provided (or `rpc` is `None`).
/// - [`ToncenterTransportPolicy::ForceMtproto`]: always uses MTProto (requires `rpc: Some(_)`).
/// - [`ToncenterTransportPolicy::ForceHttp`]: always uses direct HTTP (requires non-empty
///   `config.toncenter_base_url`).
pub fn select_toncenter_transport<C: RpcClient + 'static>(
    policy: ToncenterTransportPolicy,
    rpc: Option<C>,
    webfile_dc_id: DcId,
    config: &WalletAppConfig,
) -> Result<SelectedToncenterTransport, WalletError> {
    let has_http_override = !config.toncenter_base_url.trim().is_empty();
    let use_http = match policy {
        ToncenterTransportPolicy::ForceHttp => true,
        ToncenterTransportPolicy::ForceMtproto => false,
        ToncenterTransportPolicy::AutoPreferMtproto => has_http_override || rpc.is_none(),
    };

    if use_http {
        let mut tc_cfg = ToncenterConfig::new(config.toncenter_base_url.clone());
        tc_cfg.api_key = config.toncenter_api_key.clone();
        let transport = HttpToncenterTransport::new(tc_cfg)?;
        Ok(SelectedToncenterTransport {
            kind: ToncenterTransportKind::Http,
            transport: Arc::new(transport),
        })
    } else {
        let rpc = rpc.ok_or_else(|| {
            WalletError::Toncenter(
                "MTProto RPC client is required for native toncenter transport".to_owned(),
            )
        })?;
        let transport = MtprotoToncenterTransport::new(rpc, webfile_dc_id);
        Ok(SelectedToncenterTransport {
            kind: ToncenterTransportKind::Mtproto,
            transport: Arc::new(transport),
        })
    }
}

/// Convenience wrapper returning a ready-to-use `Arc<dyn ToncenterApi>` and its
/// [`ToncenterTransportKind`].
pub fn select_toncenter_api<C: RpcClient + 'static>(
    policy: ToncenterTransportPolicy,
    rpc: Option<C>,
    webfile_dc_id: DcId,
    config: &WalletAppConfig,
) -> Result<(ToncenterTransportKind, Arc<dyn ToncenterApi>), WalletError> {
    let selected = select_toncenter_transport(policy, rpc, webfile_dc_id, config)?;
    let client = ToncenterClient::new(selected.transport);
    Ok((selected.kind, Arc::new(client)))
}

// ============================================================================
// Typed Layer-233 MTProto Wallet Client (all 26 wallet.* + 2 toncenter.* + 3 payments.*)
// ============================================================================

/// Typed client covering every Layer-233 MTProto wallet RPC called by Telegram Desktop:
/// - 2 `toncenter.*` methods (`performApiRequest`, `getStreamingUrl`) on shifted webfile DC
/// - 26 `wallet.*` methods (state, addresses, transactions, secret-phrase backup/export,
///   ownership proof, replacement, transfer, gasless info, legacy `@wallet` balance, NFTs,
///   all 9 server-side TON Connect RPCs, and wallet card stickers)
/// - 3 wallet-related `payments.*` methods (`getCurrencyRates`, `depositStarGift`, `displayStarGift`)
#[derive(Debug)]
pub struct WalletMtprotoClient<C> {
    rpc: C,
    toncenter: MtprotoToncenterTransport<C>,
}

impl<C: Clone> Clone for WalletMtprotoClient<C> {
    fn clone(&self) -> Self {
        Self {
            rpc: self.rpc.clone(),
            toncenter: self.toncenter.clone(),
        }
    }
}

impl<C: RpcClient + Clone> WalletMtprotoClient<C> {
    /// Creates a new Layer-233 wallet client with `webfile_dc_id` for `toncenter.*` calls.
    #[must_use]
    pub fn new(rpc: C, webfile_dc_id: DcId) -> Self {
        let toncenter = MtprotoToncenterTransport::new(rpc.clone(), webfile_dc_id);
        Self { rpc, toncenter }
    }

    /// Returns a reference to the underlying [`RpcClient`].
    #[must_use]
    pub const fn rpc(&self) -> &C {
        &self.rpc
    }

    /// Returns a reference to the [`MtprotoToncenterTransport`] used for `toncenter.*` RPCs.
    #[must_use]
    pub const fn toncenter_transport(&self) -> &MtprotoToncenterTransport<C> {
        &self.toncenter
    }

    /// Updates `webfile_dc_id` for subsequent `toncenter.*` requests.
    pub fn set_webfile_dc_id(&self, webfile_dc_id: DcId) {
        self.toncenter.set_webfile_dc_id(webfile_dc_id);
    }

    /// Builds a [`ToncenterClient`] backed by this client's MTProto `toncenter.*` transport.
    #[must_use]
    pub fn toncenter_client(&self) -> ToncenterClient<MtprotoToncenterTransport<C>>
    where
        C: 'static,
    {
        ToncenterClient::new(self.toncenter.clone())
    }

    // ------------------------------------------------------------------------
    // 1-2. toncenter.* (2 methods)
    // ------------------------------------------------------------------------

    /// `toncenter.performApiRequest#8d7bdd61` on `shift_dc_id(webfile_dc_id, TONCENTER_DC_SHIFT)`.
    pub async fn toncenter_perform_api_request(
        &self,
        request: GramHttpRequest,
    ) -> Result<Vec<u8>, WalletMtprotoError> {
        self.toncenter.perform_api_request(request).await
    }

    /// `toncenter.getStreamingUrl#cdaf63c7` on `shift_dc_id(webfile_dc_id, TONCENTER_DC_SHIFT)`.
    pub async fn toncenter_get_streaming_url(
        &self,
    ) -> Result<StreamingUrlInfo, WalletMtprotoError> {
        self.toncenter.request_streaming_url().await
    }

    // ------------------------------------------------------------------------
    // 3. wallet.getState#ab83b33a
    // ------------------------------------------------------------------------

    /// `wallet.getState#ab83b33a` (`wallet_session.cpp:2437`).
    pub async fn get_state(&self) -> Result<ServerWalletState, WalletMtprotoError> {
        let resp = self
            .rpc
            .invoke(&tl_fns::wallet::GetState)
            .await
            .map_err(Into::<InvocationError>::into)?;
        Ok(parse_wallet_state(resp))
    }

    // ------------------------------------------------------------------------
    // 4. wallet.getUserAddresses#5275dfdd
    // ------------------------------------------------------------------------

    /// `wallet.getUserAddresses#5275dfdd` (`wallet_user_addresses.cpp`).
    pub async fn get_user_addresses(
        &self,
        force: bool,
        users: Vec<tl_enums::InputUser>,
        addresses: Vec<String>,
    ) -> Result<UserAddressesResponse, WalletMtprotoError> {
        let req = tl_fns::wallet::GetUserAddresses {
            force,
            id: users,
            addresses,
        };
        let resp = self
            .rpc
            .invoke(&req)
            .await
            .map_err(Into::<InvocationError>::into)?;
        Ok(parse_user_addresses(resp))
    }

    /// Resolves user TON addresses in deduplicated chunks of [`USER_ADDRESSES_PER_REQUEST`] (100),
    /// matching `UserAddresses::resolve` (`wallet_user_addresses.cpp:25-40, 302-327`).
    pub async fn resolve_user_addresses_chunked(
        &self,
        users: Vec<(i64, tl_enums::InputUser)>,
    ) -> Result<BTreeMap<i64, Option<UserAddressEntry>>, WalletMtprotoError> {
        let mut seen = HashSet::with_capacity(users.len());
        let mut deduped = Vec::with_capacity(users.len());
        for (uid, input) in users {
            if seen.insert(uid) {
                deduped.push((uid, input));
            }
        }
        let mut out: BTreeMap<i64, Option<UserAddressEntry>> = BTreeMap::new();
        for chunk in deduped.chunks(USER_ADDRESSES_PER_REQUEST) {
            let asked_ids: Vec<i64> = chunk.iter().map(|(id, _)| *id).collect();
            let input_users: Vec<tl_enums::InputUser> =
                chunk.iter().map(|(_, u)| u.clone()).collect();
            for &id in &asked_ids {
                out.insert(id, None);
            }
            let resp = self
                .get_user_addresses(false, input_users, Vec::new())
                .await?;
            for entry in resp.addresses {
                if let Some(uid) = entry.user_id
                    && out.contains_key(&uid)
                    && !entry.address.is_empty()
                {
                    out.insert(uid, Some(entry));
                }
            }
        }
        Ok(out)
    }

    /// Forces creation/lookup of a single recipient user's TON address (`force = true`),
    /// matching `UserAddresses::forceResolve` (`wallet_user_addresses.cpp:131-156`).
    pub async fn force_resolve_user_address(
        &self,
        user_id: i64,
        input_user: tl_enums::InputUser,
    ) -> Result<UserAddressEntry, WalletMtprotoError> {
        let resp = self
            .get_user_addresses(true, vec![input_user], Vec::new())
            .await?;
        if resp.addresses.len() == 1 {
            let entry = resp.addresses.into_iter().next().expect("len == 1");
            if entry.user_id == Some(user_id) && !entry.address.is_empty() {
                return Ok(entry);
            }
        }
        Err(WalletMtprotoError::InvalidResponse {
            code: "WALLET_ADDRESS_INVALID",
            detail: format!("server returned unusable forced address for user {user_id}"),
        })
    }

    /// Reverse-resolves a canonical TON address to its Telegram owner (`UserAddresses::resolveOwner`,
    /// `wallet_user_addresses.cpp:201-255`).
    pub async fn resolve_address_owner(
        &self,
        address: &str,
    ) -> Result<Option<UserAddressEntry>, WalletMtprotoError> {
        let Some(canonical) = canonical_address(address) else {
            return Ok(None);
        };
        let resp = self
            .get_user_addresses(false, Vec::new(), vec![canonical.clone()])
            .await?;
        let mut owner = None;
        for entry in resp.addresses {
            if entry.address == canonical {
                owner = Some(entry);
            }
        }
        Ok(owner)
    }

    // ------------------------------------------------------------------------
    // 5-7. wallet.getTransactions / ByIDs / ByMsgHash
    // ------------------------------------------------------------------------

    /// `wallet.getTransactions#a8830a83` (`wallet_session.cpp:7281-7422`).
    ///
    /// Guards against empty or repeated `next_offset` cursors identically to tdesktop.
    pub async fn get_transactions(
        &self,
        filter: TransactionDirectionFilter,
        offset: &str,
        limit: i32,
    ) -> Result<WalletTransactionsPage, WalletMtprotoError> {
        let (inbound, outbound) = match filter {
            TransactionDirectionFilter::All => (false, false),
            TransactionDirectionFilter::InboundOnly => (true, false),
            TransactionDirectionFilter::OutboundOnly => (false, true),
        };
        let effective_limit = if limit <= 0 {
            TRANSACTIONS_PER_PAGE
        } else {
            limit
        };
        let req = tl_fns::wallet::GetTransactions {
            inbound,
            outbound,
            offset: offset.to_owned(),
            limit: effective_limit,
        };
        let resp = self
            .rpc
            .invoke(&req)
            .await
            .map_err(Into::<InvocationError>::into)?;
        Ok(parse_transactions_page(resp, Some(offset)))
    }

    /// `wallet.getTransactionsByIDs#811ceab6` (`wallet_session.cpp:7350`).
    pub async fn get_transactions_by_ids(
        &self,
        ids: Vec<String>,
    ) -> Result<WalletTransactionsPage, WalletMtprotoError> {
        let req = tl_fns::wallet::GetTransactionsByIDs { id: ids };
        let resp = self
            .rpc
            .invoke(&req)
            .await
            .map_err(Into::<InvocationError>::into)?;
        Ok(parse_transactions_page(resp, None))
    }

    /// `wallet.getTransactionsByMsgHash#a6bb795d` (`wallet_session.cpp:10420`).
    pub async fn get_transactions_by_msg_hash(
        &self,
        msg_hashes: Vec<String>,
    ) -> Result<WalletTransactionsPage, WalletMtprotoError> {
        let req = tl_fns::wallet::GetTransactionsByMsgHash {
            msg_hash: msg_hashes,
        };
        let resp = self
            .rpc
            .invoke(&req)
            .await
            .map_err(Into::<InvocationError>::into)?;
        Ok(parse_transactions_page(resp, None))
    }

    // ------------------------------------------------------------------------
    // 8-14. Cloud backup, secret phrase export, ownership proof, wallet replacement
    // ------------------------------------------------------------------------

    /// `wallet.exportSecretPhrase#9077c9ac` (`wallet_session.cpp:3636-3705`).
    ///
    /// Uses `invoke_handle_flood` (matching `.handleFloodErrors()` in tdesktop) and validates
    /// that `token` is non-empty and every DC in `dcs` is unique and in `1..DC_SHIFT`.
    pub async fn export_secret_phrase(
        &self,
        password: Option<tl_enums::InputCheckPasswordSRP>,
    ) -> Result<SecretPhrasePartsInfo, WalletMtprotoError> {
        let req = tl_fns::wallet::ExportSecretPhrase { password };
        let resp = self
            .rpc
            .invoke_handle_flood(&req)
            .await
            .map_err(Into::<InvocationError>::into)?;
        parse_secret_phrase_parts(resp)
    }

    /// `wallet.fetchEncryptedSecretPhrasePart#f53925cf` (`wallet_session.cpp:3763-3848`).
    ///
    /// Routed to `shift_dc_id(dc_id, WALLET_SHARE_DC_SHIFT)` with `invoke_in_dc_handle_flood`.
    pub async fn fetch_encrypted_secret_phrase_part(
        &self,
        dc_id: DcId,
        token: &str,
        ephemeral_public_key: &[u8; PHRASE_SHARE_PUBLIC_KEY_SIZE],
    ) -> Result<Vec<u8>, WalletMtprotoError> {
        let Some(shifted_dc) = wallet_share_shifted_dc_id(dc_id) else {
            return Err(WalletMtprotoError::Validation {
                code: "PHRASE_PARTS_INVALID",
                detail: format!("invalid holder dc_id {dc_id}"),
            });
        };
        if token.is_empty() {
            return Err(WalletMtprotoError::Validation {
                code: "PHRASE_PARTS_INVALID",
                detail: "secret phrase export token must not be empty".to_owned(),
            });
        }
        let req = tl_fns::wallet::FetchEncryptedSecretPhrasePart {
            token: token.to_owned(),
            public_key: ephemeral_public_key.to_vec(),
        };
        let resp = self
            .rpc
            .invoke_in_dc_handle_flood(shifted_dc, &req)
            .await
            .map_err(Into::<InvocationError>::into)?;
        let tl_enums::wallet::EncryptedSecretPhrasePart::EncryptedSecretPhrasePart(inner) = resp;
        Ok(inner.data)
    }

    /// `wallet.replaceWallet#d8c72eec` (`wallet_session.cpp:6623-6652`).
    ///
    /// `WalletReplacement::New` uses `invoke_handle_flood`; `WalletReplacement::Imported` carries
    /// a one-shot ownership proof and uses `invoke` directly (caller handles errors without resend).
    pub async fn replace_wallet(
        &self,
        replacement: WalletReplacement,
        password: Option<tl_enums::InputCheckPasswordSRP>,
    ) -> Result<ServerWalletState, WalletMtprotoError> {
        let (tl_replacement, one_shot) = match replacement {
            WalletReplacement::New => (tl_enums::InputWalletReplacement::InputWalletNew, false),
            WalletReplacement::Imported {
                public_key,
                anchor_public_key,
                proof,
            } => {
                if public_key.len() != CUSTODY_PUBLIC_KEY_SIZE {
                    return Err(WalletMtprotoError::Validation {
                        code: "REPLACE_KEY_INVALID",
                        detail: format!(
                            "public_key must be {CUSTODY_PUBLIC_KEY_SIZE} bytes, got {}",
                            public_key.len()
                        ),
                    });
                }
                if let Some(ref anchor) = anchor_public_key
                    && anchor.len() != CUSTODY_PUBLIC_KEY_SIZE
                {
                    return Err(WalletMtprotoError::Validation {
                        code: "REPLACE_KEY_INVALID",
                        detail: format!(
                            "anchor_public_key must be {CUSTODY_PUBLIC_KEY_SIZE} bytes, got {}",
                            anchor.len()
                        ),
                    });
                }
                let tl_proof = proof.to_tl()?;
                (
                    tl_enums::InputWalletReplacement::InputWalletImported(
                        tl_types::InputWalletImported {
                            public_key,
                            anchor_public_key,
                            proof: tl_proof,
                        },
                    ),
                    true,
                )
            }
        };
        let req = tl_fns::wallet::ReplaceWallet {
            wallet: tl_replacement,
            password,
        };
        let resp = if one_shot {
            self.rpc.invoke(&req).await
        } else {
            self.rpc.invoke_handle_flood(&req).await
        }
        .map_err(Into::<InvocationError>::into)?;
        Ok(parse_wallet_state(resp))
    }

    /// `wallet.disableBackup#0b0bf0da` (`wallet_session.cpp:4842-4858`, `4944-4971`).
    ///
    /// Password-only mode uses `invoke_handle_flood`; proof mode carries a one-shot ownership
    /// proof and uses `invoke` directly.
    pub async fn disable_backup(
        &self,
        request: DisableBackupRequest,
    ) -> Result<ServerWalletState, WalletMtprotoError> {
        let one_shot = request.proof.is_some();
        if let Some(ref key) = request.new_public_key
            && key.len() != CUSTODY_PUBLIC_KEY_SIZE
        {
            return Err(WalletMtprotoError::Validation {
                code: "BACKUP_KEY_INVALID",
                detail: format!(
                    "new_public_key must be {CUSTODY_PUBLIC_KEY_SIZE} bytes, got {}",
                    key.len()
                ),
            });
        }
        let tl_proof = request
            .proof
            .as_ref()
            .map(OwnershipProofInput::to_tl)
            .transpose()?;
        let req = tl_fns::wallet::DisableBackup {
            password: request.password,
            new_public_key: request.new_public_key,
            proof: tl_proof,
        };
        let resp = if one_shot {
            self.rpc.invoke(&req).await
        } else {
            self.rpc.invoke_handle_flood(&req).await
        }
        .map_err(Into::<InvocationError>::into)?;
        Ok(parse_wallet_state(resp))
    }

    /// `wallet.getBackupHolderDcs#d179d494` (`wallet_session.cpp:4724-4732`).
    ///
    /// Validates `>= 2` holders, positive unique DC IDs, and 32-byte X25519 public keys
    /// (`ParseBackupHolderKeys` in `wallet_session.cpp:1162-1184`).
    pub async fn get_backup_holder_dcs(&self) -> Result<Vec<BackupHolderDc>, WalletMtprotoError> {
        let resp = self
            .rpc
            .invoke(&tl_fns::wallet::GetBackupHolderDcs)
            .await
            .map_err(Into::<InvocationError>::into)?;
        parse_backup_holder_dcs(resp)
    }

    /// `wallet.enableBackup#4500c525` (`wallet_session.cpp:4686-4715`).
    pub async fn enable_backup(
        &self,
        request: EnableBackupRequest,
    ) -> Result<ServerWalletState, WalletMtprotoError> {
        if request.parts.len() < 2 {
            return Err(WalletMtprotoError::Validation {
                code: "BACKUP_HOLDERS_INVALID",
                detail: format!(
                    "enableBackup requires at least 2 sealed parts, got {}",
                    request.parts.len()
                ),
            });
        }
        if let Some(ref key) = request.new_public_key
            && key.len() != CUSTODY_PUBLIC_KEY_SIZE
        {
            return Err(WalletMtprotoError::Validation {
                code: "BACKUP_KEY_INVALID",
                detail: format!(
                    "new_public_key must be {CUSTODY_PUBLIC_KEY_SIZE} bytes, got {}",
                    key.len()
                ),
            });
        }
        let tl_proof = request
            .proof
            .as_ref()
            .map(OwnershipProofInput::to_tl)
            .transpose()?;
        let req = tl_fns::wallet::EnableBackup {
            parts: request.parts,
            new_public_key: request.new_public_key,
            proof: tl_proof,
        };
        let resp = self
            .rpc
            .invoke(&req)
            .await
            .map_err(Into::<InvocationError>::into)?;
        Ok(parse_wallet_state(resp))
    }

    /// `wallet.getProofChallenge#2025e697` (`wallet_session.cpp:6034-6088`).
    ///
    /// Uses `invoke_handle_flood` matching `.handleFloodErrors()` in tdesktop.
    pub async fn get_proof_challenge(&self) -> Result<ProofChallengeInfo, WalletMtprotoError> {
        let resp = self
            .rpc
            .invoke_handle_flood(&tl_fns::wallet::GetProofChallenge)
            .await
            .map_err(Into::<InvocationError>::into)?;
        let tl_enums::wallet::ProofChallenge::ProofChallenge(inner) = resp;
        Ok(ProofChallengeInfo {
            payload: inner.payload,
            expires: inner.expires,
            domain: inner.domain,
        })
    }

    /// High-level cloud backup enable flow (`Session::enableBackup` in `wallet_session.cpp:4724-4762`):
    /// fetches holder DCs, encodes `words` into tdesktop's 215-byte space-padded seed, splits into
    /// `N` XOR shares, seals each share for its holder's X25519 key (`mnemonic.decryptedKeyPart`),
    /// and uploads via `wallet.enableBackup`.
    pub async fn enable_cloud_backup_from_words(
        &self,
        words: &[String],
        new_public_key: Option<Vec<u8>>,
        proof: Option<OwnershipProofInput>,
    ) -> Result<ServerWalletState, WalletMtprotoError> {
        let holders = self.get_backup_holder_dcs().await?;
        let seed = seed_from_words(words)?;
        let shares = split_seed_xor(&seed, holders.len())?;
        let mut sealed = Vec::with_capacity(holders.len());
        for (holder, share) in holders.iter().zip(shares.iter()) {
            sealed.push(encrypt_share_for_holder(&holder.public_key, share)?);
        }
        self.enable_backup(EnableBackupRequest {
            parts: sealed,
            new_public_key,
            proof,
        })
        .await
    }

    /// High-level cloud backup export flow (`Session::exportSecretPhrase` + `fetchShareParts` in
    /// `wallet_session.cpp:3636-3856`): requests export token & holder DCs, generates an ephemeral
    /// X25519 keypair, fetches encrypted parts from every holder DC on
    /// `shift_dc_id(dc, WALLET_SHARE_DC_SHIFT)`, decrypts each part, XOR-combines, and decodes the
    /// recovery phrase words.
    pub async fn export_cloud_backup_words(
        &self,
        password: Option<tl_enums::InputCheckPasswordSRP>,
    ) -> Result<Vec<String>, WalletMtprotoError> {
        let parts_info = self.export_secret_phrase(password).await?;
        let eph_sk = BoxSecretKey::generate(&mut OsRng);
        let eph_pk = *eph_sk.public_key().as_bytes();
        let eph_sk_bytes = eph_sk.to_bytes();

        let mut shares = Vec::with_capacity(parts_info.dcs.len());
        for &dc in &parts_info.dcs {
            let encrypted = self
                .fetch_encrypted_secret_phrase_part(dc, &parts_info.token, &eph_pk)
                .await?;
            let decrypted = decrypt_share_for_holder(&eph_sk_bytes, &encrypted).map_err(|e| {
                WalletMtprotoError::InvalidResponse {
                    code: "PHRASE_PART_INVALID",
                    detail: format!("holder DC {dc} share decryption failed: {e}"),
                }
            })?;
            shares.push(decrypted);
        }
        let seed =
            combine_shares_xor(&shares).map_err(|e| WalletMtprotoError::InvalidResponse {
                code: "PHRASE_PART_INVALID",
                detail: format!("holder shares failed to combine: {e}"),
            })?;
        words_from_seed(&seed).map_err(|e| WalletMtprotoError::InvalidResponse {
            code: "PHRASE_PART_INVALID",
            detail: format!("combined seed failed to decode words: {e}"),
        })
    }

    // ------------------------------------------------------------------------
    // 15-19. sendTransfer, getGaslessInfo, getExistingWaltBalance, getNfts, setCardStickers
    // ------------------------------------------------------------------------

    /// `wallet.sendTransfer#d9e5d9e2` (`wallet_session.cpp:9883-9929`).
    pub async fn send_transfer(
        &self,
        request: SendTransferRequest,
    ) -> Result<SendTransferResult, WalletMtprotoError> {
        if request.data_normal.is_empty() {
            return Err(WalletMtprotoError::Validation {
                code: "WALLET_TRANSFER_DATA_INVALID",
                detail: "data_normal must not be empty".to_owned(),
            });
        }
        if request.random_id == 0 {
            return Err(WalletMtprotoError::Validation {
                code: "WALLET_TRANSFER_DATA_INVALID",
                detail: "random_id must be non-zero".to_owned(),
            });
        }
        let req = tl_fns::wallet::SendTransfer {
            data_normal: request.data_normal,
            data_gasless: request.data_gasless,
            user_id: request.user_id,
            random_id: request.random_id,
        };
        let updates = self
            .rpc
            .invoke(&req)
            .await
            .map_err(Into::<InvocationError>::into)?;
        let receipt = extract_sent_transfer_receipt(&updates).ok_or_else(|| {
            WalletMtprotoError::InvalidResponse {
                code: "WALLET_TRANSFER_RECEIPT_INVALID",
                detail: "wallet.sendTransfer response lacked an unambiguous updateSentWalletTransaction with non-empty msg_hash".to_owned(),
            }
        })?;
        Ok(SendTransferResult { receipt, updates })
    }

    /// `wallet.getGaslessInfo#742df0a1` (`wallet_session.cpp:2616-2638`).
    pub async fn get_gasless_info(&self) -> Result<GaslessInfo, WalletMtprotoError> {
        let updates = self
            .rpc
            .invoke(&tl_fns::wallet::GetGaslessInfo)
            .await
            .map_err(Into::<InvocationError>::into)?;
        extract_gasless_info(&updates).ok_or_else(|| WalletMtprotoError::InvalidResponse {
            code: "WALLET_GASLESS_INFO_MISSING",
            detail: "wallet.getGaslessInfo response lacked updateWalletGaslessInfo".to_owned(),
        })
    }

    /// `wallet.getExistingWaltBalance#691f4360` (`wallet_session.cpp:2869-2884`).
    pub async fn get_existing_walt_balance(
        &self,
    ) -> Result<ExistingWaltBalance, WalletMtprotoError> {
        let resp = self
            .rpc
            .invoke(&tl_fns::wallet::GetExistingWaltBalance)
            .await
            .map_err(Into::<InvocationError>::into)?;
        let tl_enums::wallet::ExistingBalance::ExistingBalance(inner) = resp;
        Ok(ExistingWaltBalance {
            has_balance: inner.has_balance,
            url: (inner.has_balance && !inner.url.is_empty()).then_some(inner.url),
        })
    }

    /// `wallet.getNfts#8c12f3a4` (`wallet_session.cpp:7706-7799`).
    pub async fn get_nfts(
        &self,
        offset: &str,
        limit: i32,
    ) -> Result<ServerNftPage, WalletMtprotoError> {
        let effective_limit = if limit <= 0 {
            COLLECTIBLES_PER_PAGE
        } else {
            limit
        };
        let req = tl_fns::wallet::GetNfts {
            offset: offset.to_owned(),
            limit: effective_limit,
        };
        let resp = self
            .rpc
            .invoke(&req)
            .await
            .map_err(Into::<InvocationError>::into)?;
        Ok(parse_nft_page(resp, offset))
    }

    /// `wallet.setCardStickers#068509ac` (`wallet_own_card.cpp:94-109, 210-227`).
    ///
    /// Enforces `stickers.len() <= WALLET_CARD_MAX_STICKERS`, unique canonical sticker addresses,
    /// finite valid coordinates, and only sends `bg_nft_address` when it matches a listed sticker
    /// (matching `SentBackground` in `wallet_own_card.cpp:34-39`).
    pub async fn set_card_stickers(
        &self,
        stickers: &[CardStickerInput],
        bg_nft_address: Option<&str>,
    ) -> Result<WalletCardInfo, WalletMtprotoError> {
        if stickers.len() > WALLET_CARD_MAX_STICKERS {
            return Err(WalletMtprotoError::Validation {
                code: "WALLET_CARD_STICKER_LIMIT",
                detail: format!(
                    "wallet card supports at most {WALLET_CARD_MAX_STICKERS} stickers, got {}",
                    stickers.len()
                ),
            });
        }
        let mut seen = HashSet::with_capacity(stickers.len());
        let mut tl_stickers = Vec::with_capacity(stickers.len());
        let mut matched_bg: Option<String> = None;
        let desired_bg_canonical = bg_nft_address
            .filter(|s| !s.is_empty())
            .and_then(canonical_address);
        if let Some(raw_bg) = bg_nft_address
            && !raw_bg.is_empty()
            && desired_bg_canonical.is_none()
        {
            return Err(WalletMtprotoError::Validation {
                code: "WALLET_CARD_INVALID",
                detail: format!("unparseable bg_nft_address: {raw_bg}"),
            });
        }
        for sticker in stickers {
            if !sticker.coordinates.is_valid() {
                return Err(WalletMtprotoError::Validation {
                    code: "WALLET_CARD_INVALID",
                    detail: "sticker coordinates out of range".to_owned(),
                });
            }
            let Some(canonical) = canonical_address(&sticker.nft_address) else {
                return Err(WalletMtprotoError::Validation {
                    code: "WALLET_CARD_INVALID",
                    detail: format!("unparseable sticker nft_address: {}", sticker.nft_address),
                });
            };
            if !seen.insert(canonical.clone()) {
                return Err(WalletMtprotoError::Validation {
                    code: "WALLET_CARD_INVALID",
                    detail: format!("duplicate sticker address: {canonical}"),
                });
            }
            if desired_bg_canonical.as_deref() == Some(canonical.as_str()) {
                matched_bg = Some(sticker.nft_address.clone());
            }
            tl_stickers.push(tl_enums::InputWalletCardSticker::InputWalletCardSticker(
                tl_types::InputWalletCardSticker {
                    coordinates: sticker.coordinates.to_tl(),
                    nft_address: sticker.nft_address.clone(),
                },
            ));
        }
        if desired_bg_canonical.is_some() && matched_bg.is_none() {
            return Err(WalletMtprotoError::Validation {
                code: "WALLET_CARD_INVALID",
                detail: "bg_nft_address must match one of the stickers".to_owned(),
            });
        }
        let req = tl_fns::wallet::SetCardStickers {
            stickers: tl_stickers,
            bg_nft_address: matched_bg,
        };
        let resp = self
            .rpc
            .invoke(&req)
            .await
            .map_err(Into::<InvocationError>::into)?;
        Ok(parse_wallet_card(resp))
    }

    // ------------------------------------------------------------------------
    // 20-28. Server-side TON Connect (9 methods)
    // ------------------------------------------------------------------------

    /// `wallet.tonConnectCreateSession#cc931046` (`wallet_ton_connect.cpp:1291`).
    pub async fn ton_connect_create_session(
        &self,
        dapp_client_id: &str,
        manifest_url: &str,
    ) -> Result<ServerTonConnectSession, WalletMtprotoError> {
        let req = tl_fns::wallet::TonConnectCreateSession {
            dapp_client_id: dapp_client_id.to_owned(),
            manifest_url: manifest_url.to_owned(),
        };
        let resp = self
            .rpc
            .invoke(&req)
            .await
            .map_err(Into::<InvocationError>::into)?;
        Ok(parse_ton_connect_session(resp))
    }

    /// `wallet.tonConnectRegisterKey#338bcf1c` (`wallet_ton_connect.cpp:1560`).
    pub async fn ton_connect_register_key(
        &self,
        session_id: i64,
        client_id: &str,
    ) -> Result<ServerTonConnectChallenge, WalletMtprotoError> {
        let req = tl_fns::wallet::TonConnectRegisterKey {
            session_id,
            client_id: client_id.to_owned(),
        };
        let resp = self
            .rpc
            .invoke(&req)
            .await
            .map_err(Into::<InvocationError>::into)?;
        let tl_enums::wallet::TonConnectChallenge::TonConnectChallenge(inner) = resp;
        Ok(ServerTonConnectChallenge {
            challenge: inner.challenge,
            event_id: inner.event_id,
        })
    }

    /// `wallet.tonConnectSubmitConnectResult#c2c00779` (`wallet_ton_connect.cpp:1639-1653`).
    pub async fn ton_connect_submit_connect_result(
        &self,
        request: TonConnectSubmitConnectResultRequest,
    ) -> Result<bool, WalletMtprotoError> {
        let trace_id = request.trace_id.filter(|s| !s.is_empty());
        let req = tl_fns::wallet::TonConnectSubmitConnectResult {
            error: request.error,
            session_id: request.session_id,
            challenge_answer: request.challenge_answer,
            body: request.body,
            trace_id,
        };
        self.rpc
            .invoke(&req)
            .await
            .map_err(|e| WalletMtprotoError::Invocation(e.into()))
    }

    /// `wallet.tonConnectGetPending#11aee065` (`wallet_ton_connect.cpp:1428`,
    /// `wallet_ton_connect_request.cpp:532, 599, 1044`).
    pub async fn ton_connect_get_pending(
        &self,
        query: TonConnectPendingQuery,
    ) -> Result<ServerTonConnectPending, WalletMtprotoError> {
        let (dapp_client_id, session_id) = match query {
            TonConnectPendingQuery::ByDappClientId(id) => (Some(id), None),
            TonConnectPendingQuery::BySessionId(id) => (None, Some(id)),
            TonConnectPendingQuery::Both {
                dapp_client_id,
                session_id,
            } => (Some(dapp_client_id), Some(session_id)),
        };
        let req = tl_fns::wallet::TonConnectGetPending {
            dapp_client_id,
            session_id,
        };
        let resp = self
            .rpc
            .invoke(&req)
            .await
            .map_err(Into::<InvocationError>::into)?;
        Ok(parse_ton_connect_pending(resp))
    }

    /// `wallet.tonConnectClaimRequest#66b57b41` (`wallet_ton_connect_request.cpp:1859-1872`).
    pub async fn ton_connect_claim_request(
        &self,
        request: TonConnectClaimRequest,
    ) -> Result<bool, WalletMtprotoError> {
        let req = tl_fns::wallet::TonConnectClaimRequest {
            declined: request.declined,
            session_id: request.session_id,
            msg_id: request.msg_id,
            app_request_id: request.app_request_id,
            challenge_answer: request.challenge_answer,
        };
        self.rpc
            .invoke(&req)
            .await
            .map_err(|e| WalletMtprotoError::Invocation(e.into()))
    }

    /// `wallet.tonConnectSubmitResponse#fac67d3c` (`wallet_ton_connect_request.cpp:63-92`).
    pub async fn ton_connect_submit_response(
        &self,
        request: TonConnectSubmitResponseRequest,
    ) -> Result<bool, WalletMtprotoError> {
        let trace_id = request.trace_id.filter(|s| !s.is_empty());
        let req = tl_fns::wallet::TonConnectSubmitResponse {
            session_id: request.session_id,
            msg_id: request.msg_id,
            body: request.body,
            trace_id,
        };
        self.rpc
            .invoke(&req)
            .await
            .map_err(|e| WalletMtprotoError::Invocation(e.into()))
    }

    /// Classifies `wallet.tonConnectSubmitResponse` into tdesktop's `SubmitResult`
    /// (`Delivered`, `Refused`, `RetryLater`).
    pub async fn ton_connect_submit_response_classified(
        &self,
        request: TonConnectSubmitResponseRequest,
    ) -> TonConnectSubmitOutcome {
        match self.ton_connect_submit_response(request).await {
            Ok(true) => TonConnectSubmitOutcome::Delivered,
            Ok(false) => TonConnectSubmitOutcome::Refused,
            Err(err) if err.is_ton_connect_request_dropped() => TonConnectSubmitOutcome::Refused,
            Err(_) => TonConnectSubmitOutcome::RetryLater,
        }
    }

    /// `wallet.tonConnectNextEventId#775d7244` (`wallet_ton_connect.cpp:1050`).
    pub async fn ton_connect_next_event_id(
        &self,
        session_id: i64,
    ) -> Result<i64, WalletMtprotoError> {
        let req = tl_fns::wallet::TonConnectNextEventId { session_id };
        let resp = self
            .rpc
            .invoke(&req)
            .await
            .map_err(Into::<InvocationError>::into)?;
        let tl_enums::TonConnectNextEventId::TonConnectNextEventId(inner) = resp;
        Ok(inner.event_id)
    }

    /// `wallet.tonConnectCloseSession#d63f61ce` (`wallet_ton_connect.cpp:1073-1091`).
    ///
    /// Empty `body` clears `flags.0` identically to tdesktop (`body.isEmpty() ? Flag(0) : Flag::f_body`).
    pub async fn ton_connect_close_session(
        &self,
        session_id: i64,
        body: Option<Vec<u8>>,
    ) -> Result<bool, WalletMtprotoError> {
        let body = body.filter(|b| !b.is_empty());
        let req = tl_fns::wallet::TonConnectCloseSession { session_id, body };
        self.rpc
            .invoke(&req)
            .await
            .map_err(|e| WalletMtprotoError::Invocation(e.into()))
    }

    /// `wallet.tonConnectGetSessions#a7ffb56e` (`wallet_ton_connect.cpp:751-761`).
    pub async fn ton_connect_get_sessions(
        &self,
    ) -> Result<Vec<ServerTonConnectSession>, WalletMtprotoError> {
        let resp = self
            .rpc
            .invoke(&tl_fns::wallet::TonConnectGetSessions)
            .await
            .map_err(Into::<InvocationError>::into)?;
        let tl_enums::wallet::TonConnectSessions::TonConnectSessions(inner) = resp;
        Ok(inner
            .sessions
            .into_iter()
            .map(parse_ton_connect_session)
            .collect())
    }

    // ------------------------------------------------------------------------
    // 29-31. Wallet-related payments.* RPCs (3 methods)
    // ------------------------------------------------------------------------

    /// `payments.getCurrencyRates#d35983e8` (`wallet_rates.cpp:105-136`).
    pub async fn get_currency_rates(&self) -> Result<CurrencyRates, WalletMtprotoError> {
        let resp = self
            .rpc
            .invoke(&tl_fns::payments::GetCurrencyRates)
            .await
            .map_err(Into::<InvocationError>::into)?;
        parse_currency_rates(resp)
    }

    /// `payments.depositStarGift#c0a2ea9f` (`wallet_session_deposit.cpp:90-130`).
    pub async fn deposit_star_gift(
        &self,
        request: DepositStarGiftRequest,
    ) -> Result<StarGiftDepositInfo, WalletMtprotoError> {
        let req = tl_fns::payments::DepositStarGift {
            hide_sender: request.hide_sender,
            peer: request.peer,
            slug: request.slug,
        };
        let resp = self
            .rpc
            .invoke(&req)
            .await
            .map_err(Into::<InvocationError>::into)?;
        Ok(parse_star_gift_deposit_payload(resp))
    }

    /// `payments.displayStarGift#4aa5f0fe` (`wallet_collectibles.cpp:574-586`).
    ///
    /// Pass `Some(tl_enums::InputPeer::Self_)` to display the gift on the user's profile
    /// (`flags.0` set), or `None` to hide it (`flags.0` cleared).
    pub async fn display_star_gift(
        &self,
        slug: &str,
        peer: Option<tl_enums::InputPeer>,
    ) -> Result<bool, WalletMtprotoError> {
        let req = tl_fns::payments::DisplayStarGift {
            peer,
            slug: slug.to_owned(),
        };
        self.rpc
            .invoke(&req)
            .await
            .map_err(|e| WalletMtprotoError::Invocation(e.into()))
    }
}

// ============================================================================
// Pure Conversion & Validation Helpers (exact tdesktop parity)
// ============================================================================

/// Converts a TL `WalletState` into [`ServerWalletState`] (`Session::applyState`).
#[must_use]
pub fn parse_wallet_state(state: tl_enums::WalletState) -> ServerWalletState {
    match state {
        tl_enums::WalletState::Empty(empty) => ServerWalletState::Empty {
            creating: empty.creating,
        },
        tl_enums::WalletState::WalletState(data) => match canonical_address(&data.address) {
            Some(address) => ServerWalletState::Ready {
                backup_enabled: data.backup_enabled,
                can_export_phrase: data.can_export_phrase,
                can_enable_backup: data.can_enable_backup,
                address,
                raw_address: data.address,
                public_key: data.public_key,
                balance_nano: data.balance,
            },
            None => ServerWalletState::AddressUnreadable {
                raw_address: data.address,
            },
        },
    }
}

/// Converts a TL `wallet.UserAddresses` into [`UserAddressesResponse`].
#[must_use]
pub fn parse_user_addresses(resp: tl_enums::wallet::UserAddresses) -> UserAddressesResponse {
    let tl_enums::wallet::UserAddresses::UserAddresses(inner) = resp;
    let addresses = inner
        .addresses
        .into_iter()
        .map(|entry| {
            let tl_enums::WalletUserAddress::WalletUserAddress(a) = entry;
            let address = canonical_address(&a.address).unwrap_or_default();
            UserAddressEntry {
                user_id: a.user_id,
                address,
                raw_address: a.address,
                public_key: a.public_key,
            }
        })
        .collect();
    UserAddressesResponse {
        addresses,
        users: inner.users,
    }
}

/// Decodes a server-provided transaction hash (`TransactionHashFromServer` in
/// `wallet_session.cpp:1922-1949`): accepts 64 hex chars or 32-byte standard/URL-safe base64.
#[must_use]
pub fn parse_server_tx_hash(value: &str) -> Option<[u8; 32]> {
    if value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit()) {
        let bytes = hex::decode(value).ok()?;
        let arr: [u8; 32] = bytes.try_into().ok()?;
        return Some(arr);
    }
    if let Ok(bytes) = STANDARD.decode(value)
        && let Ok(arr) = <[u8; 32]>::try_from(bytes)
    {
        return Some(arr);
    }
    if let Ok(bytes) = URL_SAFE.decode(value)
        && let Ok(arr) = <[u8; 32]>::try_from(bytes)
    {
        return Some(arr);
    }
    if let Ok(bytes) = URL_SAFE_NO_PAD.decode(value)
        && let Ok(arr) = <[u8; 32]>::try_from(bytes)
    {
        return Some(arr);
    }
    None
}

/// Decodes a server-provided encrypted comment payload (`DecodeServerEncryptedComment` in
/// `wallet_session.cpp:1973-1996`): canonical padded standard base64, 64..=1024 bytes,
/// `(len - 48) % 16 == 0`.
#[must_use]
pub fn decode_server_encrypted_comment(encoded: &str) -> Option<Vec<u8>> {
    const MIN_BYTES: usize = 64;
    const MAX_BYTES: usize = 1024;
    const ENCODED_LIMIT: usize = 4 * MAX_BYTES.div_ceil(3);
    const HEADER_BYTES: usize = 48;
    const BLOCK_BYTES: usize = 16;

    if encoded.is_empty() || encoded.len() > ENCODED_LIMIT || !encoded.len().is_multiple_of(4) {
        return None;
    }
    let decoded = STANDARD.decode(encoded).ok()?;
    if decoded.len() < MIN_BYTES
        || decoded.len() > MAX_BYTES
        || !(decoded.len() - HEADER_BYTES).is_multiple_of(BLOCK_BYTES)
        || STANDARD.encode(&decoded) != encoded
    {
        return None;
    }
    Some(decoded)
}

fn parse_proxied_web_document(doc: Option<tl_enums::WebDocument>) -> Option<ProxiedWebDocument> {
    match doc? {
        tl_enums::WebDocument::WebDocument(web) => Some(ProxiedWebDocument {
            url: web.url,
            access_hash: web.access_hash,
            size: web.size,
            mime_type: web.mime_type,
        }),
        // Intentionally dropped to prevent leaking the user's IP to third-party hosts.
        tl_enums::WebDocument::NoProxy(_) => None,
    }
}

/// Converts a TL `wallet.NftItem` into [`ServerNftItem`] (`CollectibleFromServer` in
/// `wallet_session.cpp:1709-1735`). Returns `None` if `address` is not parseable.
#[must_use]
pub fn parse_server_nft_item(nft: tl_enums::wallet::NftItem) -> Option<ServerNftItem> {
    let tl_enums::wallet::NftItem::NftItem(data) = nft;
    let address = canonical_address(&data.address)?;
    let collection = data
        .collection_address
        .as_deref()
        .and_then(canonical_address)
        .unwrap_or_default();
    let owner = canonical_address(&data.owner_address).unwrap_or_default();
    let mut item = NftItem {
        address,
        collection,
        owner,
        index: data.index,
        name: data.name.unwrap_or_default(),
        collection_name: String::new(),
        content_uri: String::new(),
        domain: String::new(),
        key: String::new(),
        on_sale: false,
        kind: NftKind::Generic,
    };
    if let Some(tl_enums::StarGift::Unique(ref unique)) = data.gift {
        item.kind = NftKind::TelegramGift;
        item.key = unique.slug.clone();
        if item.name.is_empty() {
            item.name = unique.title.clone();
        }
    } else {
        classify_nft_kind(&mut item);
    }
    let attributes = data
        .attributes
        .unwrap_or_default()
        .into_iter()
        .map(|attr| {
            let tl_enums::wallet::NftAttribute::NftAttribute(a) = attr;
            ServerNftAttribute {
                trait_type: a.trait_type,
                value: a.value,
            }
        })
        .collect();
    let extra_json = data.extra.map(|e| {
        let tl_enums::DataJSON::DataJSON(j) = e;
        j.data
    });
    Some(ServerNftItem {
        item,
        description: data.description,
        image: parse_proxied_web_document(data.image),
        image_small: parse_proxied_web_document(data.image_small),
        content_url: parse_proxied_web_document(data.content_url),
        lottie: parse_proxied_web_document(data.lottie),
        attributes,
        extra_json,
        gift: data.gift,
    })
}

/// Converts a TL `wallet.NftItems` page, deduplicating by canonical address and rejecting
/// repeated `next_offset == requested_offset` (`wallet_session.cpp:2010-2024, 7770-7774`).
#[must_use]
pub fn parse_nft_page(resp: tl_enums::wallet::NftItems, requested_offset: &str) -> ServerNftPage {
    let tl_enums::wallet::NftItems::NftItems(data) = resp;
    let mut seen = HashSet::with_capacity(data.items.len());
    let mut items = Vec::with_capacity(data.items.len());
    for raw in data.items {
        if let Some(parsed) = parse_server_nft_item(raw)
            && seen.insert(parsed.item.address.clone())
        {
            items.push(parsed);
        }
    }
    let next_offset = data
        .next_offset
        .filter(|next| !next.is_empty() && next != requested_offset);
    ServerNftPage { items, next_offset }
}

/// Converts a TL `WalletTransaction` into [`ServerWalletTransaction`] (`HistoryItemFromServer` in
/// `wallet_session.cpp:1745-1856`).
#[must_use]
pub fn parse_server_transaction(tx: tl_enums::WalletTransaction) -> ServerWalletTransaction {
    let tl_enums::WalletTransaction::WalletTransaction(data) = tx;

    // `SetDirectedAmount`: negative amount folds sign into outgoing direction (`i64::MIN` preserved).
    let fold = data.amount < 0 && data.amount != i64::MIN;
    let amount_nano = if fold { -data.amount } else { data.amount };
    let mut incoming = if fold { false } else { data.incoming };
    let mut kind = ServerTransactionKind::Transfer;
    let mut peer = None;
    let mut nft = None;

    if data.key_change {
        kind = ServerTransactionKind::KeyChange;
        incoming = false;
    } else {
        let parse_addr = |raw: &str| -> (String, bool) {
            match TonAddress::parse(raw) {
                Ok(parsed) => (
                    parsed.address.to_raw(),
                    parsed.friendly && parsed.bounceable,
                ),
                Err(_) => (String::new(), false),
            }
        };
        peer = Some(match data.peer {
            tl_enums::WalletTransactionPeer::User(u) => {
                kind = ServerTransactionKind::PeerTransfer;
                let (address, bounceable) = parse_addr(&u.address);
                ServerTransactionPeer::User {
                    user_id: u.user_id,
                    address,
                    bounceable,
                    domain: u.domain,
                }
            }
            tl_enums::WalletTransactionPeer::Address(a) => {
                let (address, bounceable) = parse_addr(&a.address);
                ServerTransactionPeer::Address {
                    address,
                    bounceable,
                    domain: a.domain,
                }
            }
            tl_enums::WalletTransactionPeer::Onramp(o) => {
                let (address, bounceable) = parse_addr(&o.address);
                let provider_name = o.provider_name.trim().to_owned();
                if !provider_name.is_empty() && !address.is_empty() {
                    kind = ServerTransactionKind::Onramp;
                    ServerTransactionPeer::Onramp {
                        address,
                        bounceable,
                        domain: o.domain,
                        provider_name,
                    }
                } else {
                    ServerTransactionPeer::Address {
                        address,
                        bounceable,
                        domain: o.domain,
                    }
                }
            }
            tl_enums::WalletTransactionPeer::Channel(c) => {
                let (address, bounceable) = parse_addr(&c.address);
                ServerTransactionPeer::Channel {
                    channel_id: c.channel_id,
                    address,
                    bounceable,
                    domain: c.domain,
                }
            }
            tl_enums::WalletTransactionPeer::TelegramGift(g) => {
                ServerTransactionPeer::TelegramGift {
                    target: if g.own_profile {
                        GiftMoveTarget::OwnProfile
                    } else {
                        GiftMoveTarget::Telegram
                    },
                }
            }
            tl_enums::WalletTransactionPeer::Unsupported => ServerTransactionPeer::Unsupported,
        });

        if let Some(raw_nft) = data.nft
            && let Some(parsed_nft) = parse_server_nft_item(raw_nft)
        {
            kind = ServerTransactionKind::Collectible;
            nft = Some(parsed_nft);
        }
    }

    let (comment, encrypted_comment_payload) = match data.comment {
        Some(raw) if data.comment_encrypted => (None, decode_server_encrypted_comment(&raw)),
        Some(raw) => (Some(raw), None),
        None => (None, None),
    };
    let tx_hash = data.tx_hash.as_deref().and_then(parse_server_tx_hash);

    ServerWalletTransaction {
        id: data.id,
        kind,
        incoming,
        amount_nano,
        fee_nano: data.fee,
        date: data.date,
        gasless: data.gasless,
        failed: data.failed,
        key_change: data.key_change,
        comment_encrypted: data.comment_encrypted,
        comment,
        encrypted_comment_payload,
        tx_hash,
        peer,
        nft,
    }
}

/// Converts a TL `wallet.Transactions` page, applying tdesktop's empty/repeated cursor guard
/// (`wallet_session.cpp:7409-7422`).
#[must_use]
pub fn parse_transactions_page(
    resp: tl_enums::wallet::Transactions,
    requested_offset: Option<&str>,
) -> WalletTransactionsPage {
    let tl_enums::wallet::Transactions::Transactions(data) = resp;
    let next_offset = data.next_offset.filter(|next| {
        if next.is_empty() {
            return false;
        }
        requested_offset != Some(next.as_str())
    });
    let transactions = data
        .transactions
        .into_iter()
        .map(parse_server_transaction)
        .collect();
    WalletTransactionsPage {
        balance_nano: data.balance,
        transactions,
        next_offset,
        chats: data.chats,
        users: data.users,
    }
}

/// Extracts the single unambiguous submitted transaction ID from a `getTransactionsByMsgHash`
/// response (`Session::applySubmittedLookup` in `wallet_session.cpp:10488-10510`).
///
/// Returns `Ok(Some(tx_id))` when a unique outgoing non-key-change transaction is found,
/// `Ok(None)` when none has appeared yet, or `Err` when multiple distinct candidate IDs collide.
pub fn extract_submitted_lookup_tx_id(
    page: &WalletTransactionsPage,
) -> Result<Option<String>, WalletMtprotoError> {
    let mut found_id: Option<&str> = None;
    for tx in &page.transactions {
        if tx.incoming || tx.id.is_empty() || tx.kind == ServerTransactionKind::KeyChange {
            continue;
        }
        match found_id {
            None => found_id = Some(tx.id.as_str()),
            Some(existing) if existing == tx.id => {}
            Some(_) => {
                return Err(WalletMtprotoError::InvalidResponse {
                    code: "WALLET_LOOKUP_AMBIGUOUS",
                    detail: "wallet.getTransactionsByMsgHash returned ambiguous transaction IDs"
                        .to_owned(),
                });
            }
        }
    }
    Ok(found_id.map(ToOwned::to_owned))
}

/// Validates `wallet.secretPhraseParts#e6d0ef01` (`ParseHolderDcs` in `wallet_session.cpp:1135`).
pub fn parse_secret_phrase_parts(
    resp: tl_enums::wallet::SecretPhraseParts,
) -> Result<SecretPhrasePartsInfo, WalletMtprotoError> {
    let tl_enums::wallet::SecretPhraseParts::SecretPhraseParts(data) = resp;
    if data.token.is_empty() || data.dcs.is_empty() {
        return Err(WalletMtprotoError::InvalidResponse {
            code: "PHRASE_PARTS_INVALID",
            detail: "secretPhraseParts has empty token or dc list".to_owned(),
        });
    }
    let mut seen = HashSet::with_capacity(data.dcs.len());
    for &dc in &data.dcs {
        if dc <= 0 || dc >= DC_SHIFT || !seen.insert(dc) {
            return Err(WalletMtprotoError::InvalidResponse {
                code: "PHRASE_PARTS_INVALID",
                detail: format!("invalid or duplicate holder dc {dc}"),
            });
        }
    }
    Ok(SecretPhrasePartsInfo {
        token: data.token,
        dcs: data.dcs,
    })
}

/// Validates `Vector<wallet.HolderDc>` (`ParseBackupHolderKeys` in `wallet_session.cpp:1162`).
pub fn parse_backup_holder_dcs(
    list: Vec<tl_enums::wallet::HolderDc>,
) -> Result<Vec<BackupHolderDc>, WalletMtprotoError> {
    if list.len() < 2 {
        return Err(WalletMtprotoError::InvalidResponse {
            code: "BACKUP_HOLDERS_INVALID",
            detail: format!("expected at least 2 backup holder DCs, got {}", list.len()),
        });
    }
    let mut seen = HashSet::with_capacity(list.len());
    let mut out = Vec::with_capacity(list.len());
    for entry in list {
        let tl_enums::wallet::HolderDc::HolderDc(h) = entry;
        if h.dc <= 0 || !seen.insert(h.dc) {
            return Err(WalletMtprotoError::InvalidResponse {
                code: "BACKUP_HOLDERS_INVALID",
                detail: format!("invalid or duplicate backup holder dc {}", h.dc),
            });
        }
        let public_key: [u8; PHRASE_SHARE_PUBLIC_KEY_SIZE] =
            h.public_key
                .try_into()
                .map_err(|_| WalletMtprotoError::InvalidResponse {
                    code: "BACKUP_HOLDERS_INVALID",
                    detail: format!("holder dc {} public key must be 32 bytes", h.dc),
                })?;
        out.push(BackupHolderDc {
            dc: h.dc,
            public_key,
        });
    }
    Ok(out)
}

/// Extracts an unambiguous [`TransferReceipt`] from `Updates` (`SentUpdateFromServer` +
/// `ReceiptFromServer` in `wallet_session.cpp:1858-1910`).
#[must_use]
pub fn extract_sent_transfer_receipt(updates: &tl_enums::Updates) -> Option<TransferReceipt> {
    let slice: &[tl_enums::Update] = match updates {
        tl_enums::Updates::Updates(u) => &u.updates,
        tl_enums::Updates::Combined(u) => &u.updates,
        tl_enums::Updates::UpdateShort(u) => std::slice::from_ref(&u.update),
        _ => return None,
    };

    let mut found: Option<&tl_types::UpdateSentWalletTransaction> = None;
    let mut found_bytes: Option<Vec<u8>> = None;

    for item in slice {
        if let tl_enums::Update::SentWalletTransaction(sent) = item {
            let encoded = sent.to_bytes();
            if let Some(ref prev) = found_bytes {
                if *prev != encoded {
                    return None;
                }
            } else {
                found = Some(sent);
                found_bytes = Some(encoded);
            }
        }
    }

    let sent = found?;
    if sent.msg_hash.is_empty() {
        return None;
    }
    Some(TransferReceipt {
        msg_hash: sent.msg_hash.clone(),
        gasless: sent.gasless,
        transaction: sent.transaction.clone().map(parse_server_transaction),
    })
}

/// Converts `updateWalletGaslessInfo#a8ae1cac` into [`GaslessInfo`] (`GaslessInfoFromServer` in
/// `wallet_session.cpp:323-335`). Rejects unparseable or testnet relayer addresses.
#[must_use]
pub fn parse_gasless_info_update(update: &tl_types::UpdateWalletGaslessInfo) -> GaslessInfo {
    let relayer_address = match TonAddress::parse(&update.relayer_address) {
        Ok(parsed) if !parsed.testnet => parsed.address.to_raw(),
        _ => String::new(),
    };
    GaslessInfo {
        available: update.available,
        left: update.left,
        reset_at: update.reset_at,
        min_amount: update.min_amount,
        relayer_address,
    }
}

/// Extracts [`GaslessInfo`] from an `Updates` container returned by `wallet.getGaslessInfo`.
#[must_use]
pub fn extract_gasless_info(updates: &tl_enums::Updates) -> Option<GaslessInfo> {
    let mut found = None;
    let mut inspect = |u: &tl_enums::Update| {
        if let tl_enums::Update::WalletGaslessInfo(info) = u {
            found = Some(parse_gasless_info_update(info));
        }
    };
    match updates {
        tl_enums::Updates::Updates(u) => {
            for item in &u.updates {
                inspect(item);
            }
        }
        tl_enums::Updates::Combined(u) => {
            for item in &u.updates {
                inspect(item);
            }
        }
        tl_enums::Updates::UpdateShort(u) => inspect(&u.update),
        _ => {}
    }
    found
}

/// Converts a TL `TonConnectSession` (`TonConnect::Parse` in `wallet_ton_connect.cpp:711-745`).
#[must_use]
pub fn parse_ton_connect_session(session: tl_enums::TonConnectSession) -> ServerTonConnectSession {
    let tl_enums::TonConnectSession::TonConnectSession(data) = session;
    let status = if data.closed {
        TonConnectSessionStatus::Closed
    } else if data.closing {
        TonConnectSessionStatus::Closing
    } else if data.pending {
        TonConnectSessionStatus::Pending
    } else {
        TonConnectSessionStatus::Active
    };
    let manifest = data.manifest.map(|m| {
        let tl_enums::TonConnectManifest::TonConnectManifest(fields) = m;
        ServerTonConnectManifest {
            url: fields.url,
            name: fields.name,
            icon: parse_proxied_web_document(fields.icon),
        }
    });
    ServerTonConnectSession {
        id: data.id,
        dapp_client_id: data.dapp_client_id,
        client_id: data.client_id,
        nonce: data.nonce,
        manifest,
        manifest_error: data.manifest_error,
        date: data.date,
        status,
    }
}

/// Converts a TL `wallet.TonConnectPending` into [`ServerTonConnectPending`].
#[must_use]
pub fn parse_ton_connect_pending(
    pending: tl_enums::wallet::TonConnectPending,
) -> ServerTonConnectPending {
    let tl_enums::wallet::TonConnectPending::TonConnectPending(data) = pending;
    let requests = data
        .requests
        .into_iter()
        .map(|r| {
            let tl_enums::TonConnectRequest::TonConnectRequest(req) = r;
            ServerTonConnectRequest {
                session_id: req.session_id,
                msg_id: req.msg_id,
                body: req.body,
                expires: req.expires,
                topic: req.topic,
                trace_id: req.trace_id,
            }
        })
        .collect();
    ServerTonConnectPending {
        session: parse_ton_connect_session(data.session),
        requests,
    }
}

/// Converts a TL `wallet.Card` (`Data::ParseWalletCard` in `data_user_wallet_card.cpp:95-126`).
#[must_use]
pub fn parse_wallet_card(card: tl_enums::wallet::Card) -> WalletCardInfo {
    let tl_enums::wallet::Card::Card(data) = card;
    let mut stickers = Vec::new();
    let mut complete = true;
    let mut seen_addresses = HashSet::new();

    if let Some(raw_stickers) = data.stickers {
        if raw_stickers.len() > WALLET_CARD_MAX_STICKERS {
            complete = false;
        }
        for raw_sticker in raw_stickers.into_iter().take(WALLET_CARD_MAX_STICKERS) {
            let tl_enums::wallet::CardSticker::CardSticker(cs) = raw_sticker;
            let coords = CardStickerCoordinates::from_tl(&cs.coordinates);
            if !coords.is_valid() {
                complete = false;
                continue;
            }
            let Some(nft) = parse_server_nft_item(cs.nft) else {
                complete = false;
                continue;
            };
            if !matches!(nft.gift, Some(tl_enums::StarGift::Unique(_))) {
                complete = false;
                continue;
            }
            if !seen_addresses.insert(nft.item.address.clone()) {
                complete = false;
            }
            stickers.push(WalletCardStickerInfo {
                coordinates: coords,
                nft,
            });
        }
    }

    let mut background_address = None;
    let bg_nft_item = data.bg_nft_item.and_then(parse_server_nft_item);
    if let Some(ref bg) = bg_nft_item {
        if stickers
            .iter()
            .any(|s| s.nft.item.address == bg.item.address)
        {
            background_address = Some(bg.item.address.clone());
        } else {
            complete = false;
        }
    }

    WalletCardInfo {
        address: data.address,
        balance_nano: data.balance,
        stickers,
        bg_nft_item,
        background_address,
        stickers_complete: complete,
    }
}

/// Converts a TL `payments.CurrencyRates` into [`CurrencyRates`] (`Rates::applyRates` in
/// `wallet_rates.cpp:117-136`).
pub fn parse_currency_rates(
    resp: tl_enums::payments::CurrencyRates,
) -> Result<CurrencyRates, WalletMtprotoError> {
    let tl_enums::payments::CurrencyRates::CurrencyRates(inner) = resp;
    let pairs: Vec<(String, f64)> = inner
        .rates
        .into_iter()
        .map(|entry| {
            let tl_enums::CurrencyRate::CurrencyRate(c) = entry;
            (c.currency, c.rate)
        })
        .collect();
    let refs: Vec<(&str, f64)> = pairs.iter().map(|(c, r)| (c.as_str(), *r)).collect();
    CurrencyRates::from_entries(&refs).ok_or_else(|| WalletMtprotoError::InvalidResponse {
        code: "CURRENCY_RATES_EMPTY",
        detail: "payments.currencyRates contained no valid positive finite rates".to_owned(),
    })
}

/// Converts a TL `payments.StarGiftDepositPayload` into [`StarGiftDepositInfo`].
#[must_use]
pub fn parse_star_gift_deposit_payload(
    resp: tl_enums::payments::StarGiftDepositPayload,
) -> StarGiftDepositInfo {
    let tl_enums::payments::StarGiftDepositPayload::StarGiftDepositPayload(data) = resp;
    let payload_base64 = STANDARD.encode(&data.payload);
    let escrow_address = nft_transfer_new_owner(&payload_base64)
        .as_deref()
        .and_then(canonical_address);
    let nft_address = canonical_address(&data.nft_address).unwrap_or(data.nft_address);
    StarGiftDepositInfo {
        nft_address,
        payload_bytes: data.payload,
        payload_base64,
        escrow_address,
        amount_nano: data.amount,
    }
}

/// Parses any of the 5 Layer-233 wallet `Update` variants into [`WalletServerUpdate`].
#[must_use]
pub fn parse_wallet_update(update: &tl_enums::Update) -> Option<WalletServerUpdate> {
    match update {
        tl_enums::Update::WalletState(u) => Some(WalletServerUpdate::WalletState(
            parse_wallet_state(u.state.clone()),
        )),
        tl_enums::Update::SentWalletTransaction(u) => {
            if u.msg_hash.is_empty() {
                None
            } else {
                Some(WalletServerUpdate::SentWalletTransaction(Box::new(
                    TransferReceipt {
                        msg_hash: u.msg_hash.clone(),
                        gasless: u.gasless,
                        transaction: u.transaction.clone().map(parse_server_transaction),
                    },
                )))
            }
        }
        tl_enums::Update::WalletGaslessInfo(u) => Some(WalletServerUpdate::GaslessInfo(
            parse_gasless_info_update(u),
        )),
        tl_enums::Update::WalletTonConnectSession(u) => Some(
            WalletServerUpdate::TonConnectSession(parse_ton_connect_session(u.session.clone())),
        ),
        tl_enums::Update::WalletTonConnectPendingDisconnect(u) => Some(
            WalletServerUpdate::TonConnectPendingDisconnect(u.session_ids.clone()),
        ),
        _ => None,
    }
}

/// Helper computing default [`GaslessTerms`] using tdesktop's default minimum constants
/// (`TRANSFER_MIN_NANOS_DEFAULT`, `GASLESS_MIN_NANOS_DEFAULT`).
#[must_use]
pub fn default_gasless_terms(info: GaslessInfo, own_address: Option<String>) -> GaslessTerms {
    GaslessTerms::compute(
        info,
        own_address,
        TRANSFER_MIN_NANOS_DEFAULT,
        GASLESS_MIN_NANOS_DEFAULT,
    )
}
