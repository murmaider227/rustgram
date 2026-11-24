//! Live TON Wallet backend (`LiveWalletBackend`), `help.getAppConfig` synchronization,
//! `tdata`-persisted encrypted vault + DPAPI key protection, TON Connect 2.0 session manager,
//! and minimal live-host wiring helpers.
//!
//! Port origin: `Telegram/SourceFiles/wallet/wallet_session.cpp`,
//! `Telegram/SourceFiles/wallet/wallet_ton_connect.cpp`,
//! `Telegram/SourceFiles/wallet/wallet_vault.cpp`,
//! `Telegram/SourceFiles/wallet/wallet_rates.cpp`,
//! `Telegram/SourceFiles/main/main_app_config.cpp`, and
//! `Telegram/SourceFiles/platform/win/wallet_protection_win.cpp`.
//!
//! Endpoints always come from [`WalletAppConfig`] (or environment configuration). Tests run
//! exclusively against local in-process fake Toncenter and TON Connect bridge servers.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tg_api::{DcId, RpcClient};
use tg_platform::wallet_protection::{WalletKeyProtector, default_wallet_key_protector};
use tg_tl::enums as tl_enums;
use tg_tl::functions as tl_functions;
use tg_tl::types as tl_types;
use tg_wallet::address::TonAddress;
use tg_wallet::boc::deserialize_boc_base64;
use tg_wallet::contract::{DEFAULT_SEND_MODE, TransferMessage, WalletContract, WalletVersion};
use tg_wallet::emulate::{EmulateMessageRequest, EmulationResult, TonConnectEmulation};
use tg_wallet::jettons::{
    DEFAULT_JETTON_FORWARD_NANO, DEFAULT_JETTON_GAS_NANO, DEFAULT_NFT_FORWARD_NANO,
    DEFAULT_NFT_GAS_NANO, JettonTransferParams, JettonWalletBalance, NftTransferParams,
    build_jetton_transfer_message, build_nft_transfer_message, format_jetton_amount,
};
use tg_wallet::mnemonic::{TonKeypair, generate_mnemonic, mnemonic_to_keypair, validate_mnemonic};
use tg_wallet::mtproto_api::{
    CardStickerInput, DepositStarGiftRequest, DisableBackupRequest, FiatRateCache, GaslessTerms,
    ServerTransactionPeer, ServerWalletState, TransactionDirectionFilter, WalletCardInfo,
    WalletMtprotoClient, select_toncenter_api,
};
use tg_wallet::toncenter::{
    AccountState, HttpToncenterClient, NftItem, ToncenterApi, ToncenterConfig,
    ToncenterTransportKind, ToncenterTransportPolicy,
};
use tg_wallet::tonconnect::{
    DappManifest, HttpTonConnectBridge, SignDataResult, TonConnectAppRequest, TonConnectBridge,
    TonConnectBridgeConfig, TonConnectErrorCode, TonConnectKeypair, TonConnectLinkKind,
    TonConnectSendTxRequest, TonConnectSession, TonConnectSessionStatus, parse_ton_connect_link,
    sign_ton_connect_data,
};
use tg_wallet::vault::{LocalKeyVault, PasscodeKdf};
use tg_wallet::{
    DEFAULT_OLD_WALLET_BOT_USERNAME, TRANSFER_MIN_NANOS_DEFAULT, TRANSFER_MIN_NANOS_MAX,
    WalletAppConfig,
};
use tracing::{debug, warn};

use crate::media::{block_on_runtime, with_rpc_timeout};
use crate::session::LiveSession;
use tg_app_core::core_types::dto::collectible_info::CollectibleInfoData;
use tg_app_core::core_types::erased_rpc::ErasedRpcClient;
use tg_app_core::core_types::misc::nft_display_title;
use tg_app_core::info::backend::InfoBackend;
use tg_app_core::info::values::{
    CollectibleEntityKind, detect_collectible_type, normalize_username,
};
use tg_app_core::stand_in::types::PeerId;
use tg_app_core::wallet::backend::{
    CloudBackupStatus, JettonTransferQuote, MoveToGiftsQuote, MoveToGiftsReceipt, NftTransferQuote,
    OnrampHandoffSession, OnrampProviderCard, PendingConnectApproval, PendingSignDataApproval,
    PendingTxApproval, SendQuote, SendReceipt, TonConnectSessionCard, WalletBackend,
    WalletCardState, WalletCardStickerEntry, WalletOverview, WalletTxRecord, WalletTxStatus,
};

/// Environment variable configuring the Toncenter HTTP API base URL.
pub const ENV_TONCENTER_URL: &str = "RUSTGRAM_TONCENTER_URL";
/// Environment variable configuring an optional Toncenter `X-API-Key` header value.
pub const ENV_TONCENTER_API_KEY: &str = "RUSTGRAM_TONCENTER_API_KEY";
/// Environment variable configuring the TON Connect 2.0 HTTP bridge base URL.
pub const ENV_TONCONNECT_BRIDGE_URL: &str = "RUSTGRAM_TONCONNECT_BRIDGE_URL";
/// Environment variable toggling TON testnet mode (`1` / `true`).
pub const ENV_TON_TESTNET: &str = "RUSTGRAM_TON_TESTNET";

const LEGACY_ENV_TONCENTER_URL: &str = "TGFORK_TONCENTER_URL";
const LEGACY_ENV_TONCENTER_API_KEY: &str = "TGFORK_TONCENTER_API_KEY";
const LEGACY_ENV_TONCONNECT_BRIDGE_URL: &str = "TGFORK_TONCONNECT_BRIDGE_URL";
const LEGACY_ENV_TON_TESTNET: &str = "TGFORK_TON_TESTNET";

/// Fallback estimated network fee (`0.0055 TON`) when `emulateTrace` is unavailable.
const DEFAULT_ESTIMATED_FEE_NANO: u64 = 5_500_000;
/// Default TON Connect bridge message TTL in seconds (TON Connect 2.0 spec).
const TON_CONNECT_TTL_SECS: u32 = 300;
/// Network timeout for live `help.getAppConfig` synchronization.
const APP_CONFIG_RPC_TIMEOUT: Duration = Duration::from_secs(10);
/// File storing non-secret wallet metadata (public key, contract version, optional DPAPI blob).
const WALLET_META_FILENAME: &str = "wallet_meta.json";
/// Vault secret name storing the 32-byte Ed25519 seed.
const VAULT_SECRET_SEED: &str = "seed";
/// Vault secret name storing the space-separated 24-word TON recovery phrase.
const VAULT_SECRET_MNEMONIC: &str = "mnemonic";
/// Vault secret name storing serialized TON Connect sessions (including X25519 private keys).
const VAULT_SECRET_TON_CONNECT: &str = "tonconnect_sessions";

/// Builds a [`WalletAppConfig`] from process environment variables.
///
/// Never embeds or hardcodes remote endpoints: unset variables leave the endpoint strings empty.
#[must_use]
pub fn wallet_app_config_from_env() -> WalletAppConfig {
    let env_or_legacy = |primary: &str, legacy: &str| {
        std::env::var(primary)
            .ok()
            .filter(|v| !v.trim().is_empty())
            .or_else(|| std::env::var(legacy).ok().filter(|v| !v.trim().is_empty()))
    };
    let toncenter_base_url = env_or_legacy(ENV_TONCENTER_URL, LEGACY_ENV_TONCENTER_URL)
        .map(|s| s.trim().to_owned())
        .unwrap_or_default();
    let toncenter_api_key = env_or_legacy(ENV_TONCENTER_API_KEY, LEGACY_ENV_TONCENTER_API_KEY)
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty());
    let tonconnect_bridge_url =
        env_or_legacy(ENV_TONCONNECT_BRIDGE_URL, LEGACY_ENV_TONCONNECT_BRIDGE_URL)
            .map(|s| s.trim().to_owned())
            .unwrap_or_default();
    let testnet = env_or_legacy(ENV_TON_TESTNET, LEGACY_ENV_TON_TESTNET)
        .is_some_and(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"));
    WalletAppConfig {
        toncenter_base_url,
        toncenter_api_key,
        tonconnect_bridge_url,
        testnet,
        ..WalletAppConfig::default()
    }
}

/// Computes the per-account wallet persistence directory inside `tdata`:
/// `<working_dir>/tdata/<account_key_file_part>/wallet`.
#[must_use]
pub fn wallet_dir_for_account(working_dir: &Path, account_key_file_part: &str) -> PathBuf {
    working_dir
        .join("tdata")
        .join(account_key_file_part)
        .join("wallet")
}

/// Result of syncing `help.getAppConfig` for wallet flags and TON USD rate.
#[derive(Debug, Clone, PartialEq)]
pub struct SyncedWalletAppConfig {
    /// Updated wallet app config preserving caller-configured endpoints and `support_mode`.
    pub config: WalletAppConfig,
    /// Optional positive `ton_usd_rate` from `help.getAppConfig` (`wallet_rates.cpp:24`).
    pub usd_per_ton: Option<f64>,
    /// Server config hash returned by `help.appConfig` for subsequent conditional refreshes.
    pub hash: i32,
}

/// Mirrors `MinNanosFromConfig` in `wallet_session.cpp:297-314,8431-8439`.
#[must_use]
pub fn transfer_min_nanos_from_config(configured: f64) -> i64 {
    let valid = configured.is_finite()
        && configured >= 1.0
        && configured <= (TRANSFER_MIN_NANOS_MAX as f64)
        && configured == configured.floor();
    if valid {
        configured as i64
    } else {
        TRANSFER_MIN_NANOS_DEFAULT
    }
}

/// Parses wallet availability flags and parameters from a TL `JSONValue` returned by
/// `help.getAppConfig`, matching tdesktop's `AppConfig` getters:
/// - `wallet_available` (`bool`, default `true` — `main_app_config.cpp:279`)
/// - `old_wallet_bot_username` (`string`, default `"wallet"` — `main_app_config.cpp:275`)
/// - `wallet_transfer_min_nanos` (`double`, validated integer — `wallet_session.cpp:8436`)
/// - `ton_usd_rate` (`double`, positive finite — `wallet_rates.cpp:24`)
///
/// Endpoints (`toncenter_base_url`, `toncenter_api_key`, `tonconnect_bridge_url`), `testnet`,
/// and `support_mode` are preserved from `base`.
#[must_use]
pub fn parse_wallet_app_config_from_tl(
    value: &tl_enums::JSONValue,
    base: &WalletAppConfig,
) -> (WalletAppConfig, Option<f64>) {
    let mut out = base.clone();
    out.wallet_available = true;
    out.old_wallet_bot_username = DEFAULT_OLD_WALLET_BOT_USERNAME.to_owned();
    out.transfer_min_nanos = TRANSFER_MIN_NANOS_DEFAULT;
    let mut usd_per_ton = None;

    let tl_enums::JSONValue::JsonObject(obj) = value else {
        return (out, usd_per_ton);
    };

    for entry in &obj.value {
        let tl_enums::JSONObjectValue::JsonObjectValue(kv) = entry;
        match kv.key.as_str() {
            "wallet_available" => {
                if let tl_enums::JSONValue::JsonBool(b) = &kv.value {
                    out.wallet_available = b.value;
                }
            }
            "old_wallet_bot_username" => {
                if let tl_enums::JSONValue::JsonString(s) = &kv.value {
                    out.old_wallet_bot_username = s.value.clone();
                }
            }
            "wallet_transfer_min_nanos" => {
                if let tl_enums::JSONValue::JsonNumber(n) = &kv.value {
                    out.transfer_min_nanos = transfer_min_nanos_from_config(n.value);
                }
            }
            "ton_usd_rate" => {
                if let tl_enums::JSONValue::JsonNumber(n) = &kv.value
                    && n.value.is_finite()
                    && n.value > 0.0
                {
                    usd_per_ton = Some(n.value);
                }
            }
            _ => {}
        }
    }

    (out, usd_per_ton)
}

/// Fetches `help.getAppConfig` over `rpc` and parses the wallet flags into
/// [`SyncedWalletAppConfig`]. Returns `Ok(None)` when the server answers
/// `help.appConfigNotModified`.
pub async fn sync_wallet_app_config_async<C: RpcClient>(
    rpc: &C,
    base: &WalletAppConfig,
    hash: i32,
) -> Result<Option<SyncedWalletAppConfig>, String> {
    let req = tl_functions::help::GetAppConfig { hash };
    let resp = with_rpc_timeout(APP_CONFIG_RPC_TIMEOUT, async {
        rpc.invoke(&req)
            .await
            .map_err(Into::<tg_api::InvocationError>::into)
    })
    .await?;
    match resp {
        tl_enums::help::AppConfig::NotModified => Ok(None),
        tl_enums::help::AppConfig::AppConfig(cfg) => {
            let (config, usd_per_ton) = parse_wallet_app_config_from_tl(&cfg.config, base);
            Ok(Some(SyncedWalletAppConfig {
                config,
                usd_per_ton,
                hash: cfg.hash,
            }))
        }
    }
}

/// Synchronous wrapper over [`sync_wallet_app_config_async`] for a [`LiveSession`].
pub fn sync_wallet_app_config_from_session(
    session: &LiveSession,
    base: &WalletAppConfig,
    hash: i32,
) -> Result<Option<SyncedWalletAppConfig>, String> {
    let rpc = session.runtime().rpc().clone();
    let base_clone = base.clone();
    block_on_runtime(session.runtime().tokio_handle(), async move {
        sync_wallet_app_config_async(&rpc, &base_clone, hash).await
    })
}

/// Non-secret metadata persisted in `<wallet_dir>/wallet_meta.json` alongside the encrypted vault.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedWalletMeta {
    version: WalletVersion,
    public_key_hex: String,
    testnet: bool,
    #[serde(default)]
    dpapi_wrapped_passcode_hex: Option<String>,
}

/// Serializable TON Connect session state stored **encrypted inside `LocalKeyVault`**.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedTonConnectSession {
    session_id: u64,
    wallet_secret_hex: String,
    dapp_client_id: String,
    manifest: DappManifest,
    status: TonConnectSessionStatus,
    connected_at: i64,
    last_event_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedTonConnectBundle {
    next_session_id: u64,
    sessions: Vec<PersistedTonConnectSession>,
}

struct LiveTonConnectSession {
    card: TonConnectSessionCard,
    session: TonConnectSession,
}

/// Pending background updates produced by async tasks (e.g., `help.getAppConfig` sync).
#[derive(Debug, Default)]
struct PendingAsyncWalletUpdates {
    synced_app_config: Option<SyncedWalletAppConfig>,
    onramp_providers: Option<Vec<OnrampProviderCard>>,
}

/// Async provider trait for read-only fiat onramp handoff (`wallet_onramp.cpp`).
///
/// Backed by MTProto `payments.getOnrampProviders`, `payments.getOnrampBaseCurrencies`, and
/// `payments.createOnrampSession` via [`RpcOnrampProvider`], or an in-process fake in tests.
pub trait LiveOnrampProvider: Send + Sync {
    /// Lists available fiat onramp providers for `crypto_currency` (default `"GRAM"`).
    fn fetch_providers(
        &self,
        crypto_currency: String,
    ) -> tg_wallet::toncenter::BoxFuture<'_, Result<Vec<OnrampProviderCard>, String>>;

    /// Resolves a read-only external handoff URL (`https://...`) for `address`.
    /// Never performs a purchase or payment.
    fn create_handoff(
        &self,
        address: String,
        provider_id: Option<String>,
        base_currency: Option<String>,
    ) -> tg_wallet::toncenter::BoxFuture<'_, Result<OnrampHandoffSession, String>>;
}

/// Default crypto currency identifier passed to `payments.*Onramp*` (`kCryptoCurrency = "GRAM"`
/// in `wallet_onramp.cpp:15`).
pub const ONRAMP_CRYPTO_CURRENCY: &str = "GRAM";

/// Fetches available fiat onramp providers over MTProto (`payments.getOnrampProviders`).
pub async fn fetch_onramp_providers_async<C: RpcClient>(
    rpc: &C,
    crypto_currency: &str,
) -> Result<Vec<OnrampProviderCard>, String> {
    let req = tl_functions::payments::GetOnrampProviders {
        crypto_currency: (!crypto_currency.is_empty()).then(|| crypto_currency.to_owned()),
    };
    let resp = with_rpc_timeout(APP_CONFIG_RPC_TIMEOUT, async {
        rpc.invoke(&req)
            .await
            .map_err(Into::<tg_api::InvocationError>::into)
    })
    .await?;
    let mut out = Vec::with_capacity(resp.len());
    for item in resp {
        let tl_enums::OnrampProviderInfo::OnrampProviderInfo(info) = item;
        if info.id.is_empty() {
            continue;
        }
        out.push(OnrampProviderCard {
            id: info.id,
            name: info.name,
            crypto_currencies: info.crypto_currencies,
            supports_base_currencies: info.supports_base_currencies,
            supports_limits: info.supports_limits,
            supports_quote: info.supports_quote,
        });
    }
    Ok(out)
}

/// Runs tdesktop's 3-step read-only onramp handoff flow (`Wallet::Onramp::requestSessionUrl`
/// in `wallet_onramp.cpp:39-118`):
/// 1. `payments.getOnrampProviders(crypto_currency = "GRAM")`
/// 2. If `base_currency` is given and provider `supports_base_currencies`:
///    `payments.getOnrampBaseCurrencies(provider, "GRAM")`
/// 3. `payments.createOnrampSession(...)` -> validates `https://` URL and returns
///    [`OnrampHandoffSession`].
///
/// Strictly read-only: returns a link for the user to inspect/copy/open; never auto-purchases.
pub async fn request_onramp_session_url_async<C: RpcClient>(
    rpc: &C,
    address: &str,
    base_currency: Option<&str>,
    preferred_provider: Option<&str>,
) -> Result<OnrampHandoffSession, String> {
    let providers = fetch_onramp_providers_async(rpc, ONRAMP_CRYPTO_CURRENCY).await?;
    let selected = if let Some(pref) = preferred_provider.filter(|s| !s.is_empty()) {
        providers
            .iter()
            .find(|p| p.id == pref)
            .ok_or_else(|| format!("Onramp provider '{pref}' is unavailable"))?
    } else {
        providers
            .first()
            .ok_or_else(|| "No fiat onramp providers available".to_owned())?
    };

    let normalized_currency = base_currency
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty());

    let use_base_currency = if let Some(ref cur) = normalized_currency
        && selected.supports_base_currencies
    {
        let req = tl_functions::payments::GetOnrampBaseCurrencies {
            provider: selected.id.clone(),
            crypto_currency: ONRAMP_CRYPTO_CURRENCY.to_owned(),
        };
        match with_rpc_timeout(APP_CONFIG_RPC_TIMEOUT, async {
            rpc.invoke(&req)
                .await
                .map_err(Into::<tg_api::InvocationError>::into)
        })
        .await
        {
            Ok(list) => list.iter().any(|c| c.eq_ignore_ascii_case(cur)),
            Err(_) => false,
        }
    } else {
        false
    };

    let effective_currency = if use_base_currency {
        normalized_currency
    } else {
        None
    };

    let create_req = tl_functions::payments::CreateOnrampSession {
        provider: selected.id.clone(),
        crypto_currency: ONRAMP_CRYPTO_CURRENCY.to_owned(),
        address: address.to_owned(),
        payment_method: None,
        base_currency: effective_currency.clone(),
        base_amount: None,
        memo: None,
        theme: None,
        success_return_url: None,
        fail_return_url: None,
        crypto_amount: None,
    };
    let tl_enums::OnrampSession::OnrampSession(session) =
        with_rpc_timeout(APP_CONFIG_RPC_TIMEOUT, async {
            rpc.invoke(&create_req)
                .await
                .map_err(Into::<tg_api::InvocationError>::into)
        })
        .await?;

    let validated_url = validate_handoff_url(&session.url)?;
    Ok(OnrampHandoffSession {
        provider_id: session.provider,
        provider_name: selected.name.clone(),
        crypto_currency: ONRAMP_CRYPTO_CURRENCY.to_owned(),
        base_currency: effective_currency,
        wallet_address: address.to_owned(),
        handoff_url: validated_url,
        expires_date: session.expires_date,
    })
}

/// MTProto-backed [`LiveOnrampProvider`] wrapping any [`RpcClient`].
pub struct RpcOnrampProvider<C: RpcClient + Clone + 'static> {
    rpc: C,
}

impl<C: RpcClient + Clone + 'static> RpcOnrampProvider<C> {
    /// Creates a new [`RpcOnrampProvider`] using `rpc`.
    #[must_use]
    pub const fn new(rpc: C) -> Self {
        Self { rpc }
    }
}

impl<C: RpcClient + Clone + 'static> LiveOnrampProvider for RpcOnrampProvider<C> {
    fn fetch_providers(
        &self,
        crypto_currency: String,
    ) -> tg_wallet::toncenter::BoxFuture<'_, Result<Vec<OnrampProviderCard>, String>> {
        let rpc = self.rpc.clone();
        Box::pin(async move { fetch_onramp_providers_async(&rpc, &crypto_currency).await })
    }

    fn create_handoff(
        &self,
        address: String,
        provider_id: Option<String>,
        base_currency: Option<String>,
    ) -> tg_wallet::toncenter::BoxFuture<'_, Result<OnrampHandoffSession, String>> {
        let rpc = self.rpc.clone();
        Box::pin(async move {
            request_onramp_session_url_async(
                &rpc,
                &address,
                base_currency.as_deref(),
                provider_id.as_deref(),
            )
            .await
        })
    }
}

/// Fetches Fragment collectible metadata via `fragment.getCollectibleInfo`
/// (`SessionNavigation::resolveCollectible` in `window_session_controller.cpp:1044-1070`).
pub async fn fetch_collectible_info_async<C: RpcClient>(
    rpc: &C,
    peer_id: PeerId,
    owner_name: &str,
    owner_color_index: u8,
    entity: &str,
) -> Result<CollectibleInfoData, String> {
    let trimmed = entity.trim();
    if trimmed.is_empty() {
        return Err("Collectible entity is empty".to_owned());
    }
    let kind = detect_collectible_type(trimmed);
    let collectible = match kind {
        CollectibleEntityKind::Phone => {
            tl_enums::InputCollectible::Phone(tl_types::InputCollectiblePhone {
                phone: trimmed.to_owned(),
            })
        }
        CollectibleEntityKind::Username => {
            let clean = normalize_username(trimmed).to_owned();
            tl_enums::InputCollectible::Username(tl_types::InputCollectibleUsername {
                username: clean,
            })
        }
    };
    let req = tl_functions::fragment::GetCollectibleInfo { collectible };
    let tl_enums::fragment::CollectibleInfo::CollectibleInfo(info) =
        with_rpc_timeout(APP_CONFIG_RPC_TIMEOUT, async {
            rpc.invoke(&req)
                .await
                .map_err(Into::<tg_api::InvocationError>::into)
        })
        .await?;

    Ok(CollectibleInfoData::from_fragment_fields(
        trimmed,
        peer_id,
        owner_name,
        owner_color_index,
        info.purchase_date,
        info.currency,
        u64::try_from(info.amount).unwrap_or(0),
        info.crypto_currency,
        u64::try_from(info.crypto_amount).unwrap_or(0),
        info.url,
    ))
}

/// Synchronous `InfoBackend::get_collectible_info` hook for [`LiveSession`].
pub fn get_collectible_info_from_session(
    session: &LiveSession,
    peer_id: PeerId,
    entity: &str,
) -> Result<CollectibleInfoData, String> {
    let (owner_name, owner_color_index) = InfoBackend::peer_profile(session, peer_id).map_or_else(
        || ("Telegram User".to_owned(), 0),
        |p| (p.title, p.color_index),
    );
    let rpc = session.runtime().rpc().clone();
    let entity_owned = entity.to_owned();
    block_on_runtime(session.runtime().tokio_handle(), async move {
        fetch_collectible_info_async(&rpc, peer_id, &owner_name, owner_color_index, &entity_owned)
            .await
    })
}

/// Live implementation of [`WalletBackend`] backed by `tg-wallet`:
/// - [`ToncenterApi`] HTTP client (`HttpToncenterClient` or injected fake)
/// - [`LocalKeyVault`] persisted encrypted inside the account's `tdata` directory
/// - [`WalletKeyProtector`] (Windows DPAPI or test fake) for optional OS key wrapping
/// - [`TonConnectBridge`] session manager with NaCl-encrypted bridge messaging and vault-sealed
///   session persistence
/// - Asynchronous `help.getAppConfig` synchronization for tdesktop's wallet feature flags.
pub struct LiveWalletBackend {
    account_id: u64,
    wallet_dir: PathBuf,
    rt_handle: tokio::runtime::Handle,
    app_config: WalletAppConfig,
    app_config_hash: i32,
    kdf: PasscodeKdf,
    vault: LocalKeyVault,
    protector: Arc<dyn WalletKeyProtector>,
    dpapi_wrapped_passcode: Option<Vec<u8>>,
    toncenter: Option<Arc<dyn ToncenterApi>>,
    toncenter_transport_kind: Option<ToncenterTransportKind>,
    custom_toncenter: bool,
    mtproto_rpc: Option<ErasedRpcClient>,
    webfile_dc_id: DcId,
    mtproto: Option<WalletMtprotoClient<ErasedRpcClient>>,
    bridge: Option<Arc<dyn TonConnectBridge>>,
    custom_bridge: bool,
    version: WalletVersion,
    public_key: Option<[u8; 32]>,
    contract: Option<WalletContract>,
    balance_nano: u64,
    account_state: AccountState,
    usd_per_ton: f64,
    fiat_rates: FiatRateCache,
    cloud_backup: CloudBackupStatus,
    gasless: Option<GaslessTerms>,
    card_state: WalletCardState,
    seqno: u32,
    history: Vec<WalletTxRecord>,
    sessions: Vec<LiveTonConnectSession>,
    next_session_id: u64,
    pending_connect: Option<PendingConnectApproval>,
    pending_tx: Option<PendingTxApproval>,
    pending_sign_data: Option<PendingSignDataApproval>,
    collectibles: Vec<NftItem>,
    jettons: Vec<JettonWalletBalance>,
    onramp_providers: Vec<OnrampProviderCard>,
    onramp_provider: Option<Arc<dyn LiveOnrampProvider>>,
    pending_async: Arc<Mutex<PendingAsyncWalletUpdates>>,
}

impl LiveWalletBackend {
    /// Creates a new [`LiveWalletBackend`] persisted in `wallet_dir`, loading any existing
    /// encrypted vault and metadata from disk (in a locked state).
    #[must_use]
    pub fn new(
        account_id: u64,
        wallet_dir: PathBuf,
        rt_handle: tokio::runtime::Handle,
        app_config: WalletAppConfig,
    ) -> Self {
        Self::with_dependencies(
            account_id,
            wallet_dir,
            rt_handle,
            app_config,
            None,
            None,
            default_wallet_key_protector(),
            PasscodeKdf::default_argon2id(),
        )
    }

    /// Creates a [`LiveWalletBackend`] with explicit injectable [`ToncenterApi`],
    /// [`TonConnectBridge`], [`WalletKeyProtector`], and [`PasscodeKdf`] (used by tests and
    /// custom endpoint configurations).
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn with_dependencies(
        account_id: u64,
        wallet_dir: PathBuf,
        rt_handle: tokio::runtime::Handle,
        app_config: WalletAppConfig,
        toncenter: Option<Arc<dyn ToncenterApi>>,
        bridge: Option<Arc<dyn TonConnectBridge>>,
        protector: Arc<dyn WalletKeyProtector>,
        kdf: PasscodeKdf,
    ) -> Self {
        let normalized_id = account_id.max(1);
        let custom_toncenter = toncenter.is_some();
        let custom_bridge = bridge.is_some();

        let effective_toncenter = toncenter.or_else(|| build_http_toncenter(&app_config));
        let toncenter_transport_kind = if !custom_toncenter && effective_toncenter.is_some() {
            Some(ToncenterTransportKind::Http)
        } else {
            None
        };
        let effective_bridge = bridge.or_else(|| build_http_bridge(&app_config));

        let mut backend = Self {
            account_id: normalized_id,
            wallet_dir,
            rt_handle,
            app_config,
            app_config_hash: 0,
            kdf,
            vault: LocalKeyVault::new(normalized_id),
            protector,
            dpapi_wrapped_passcode: None,
            toncenter: effective_toncenter,
            toncenter_transport_kind,
            custom_toncenter,
            mtproto_rpc: None,
            webfile_dc_id: 4,
            mtproto: None,
            bridge: effective_bridge,
            custom_bridge,
            version: WalletVersion::V5R1,
            public_key: None,
            contract: None,
            balance_nano: 0,
            account_state: AccountState::Uninitialized,
            usd_per_ton: 0.0,
            fiat_rates: FiatRateCache::new("USD", 0.0),
            cloud_backup: CloudBackupStatus::default(),
            gasless: None,
            card_state: WalletCardState::default(),
            seqno: 0,
            history: Vec::new(),
            sessions: Vec::new(),
            next_session_id: 1,
            pending_connect: None,
            pending_tx: None,
            pending_sign_data: None,
            collectibles: Vec::new(),
            jettons: Vec::new(),
            onramp_providers: Vec::new(),
            onramp_provider: None,
            pending_async: Arc::new(Mutex::new(PendingAsyncWalletUpdates::default())),
        };
        backend.load_from_disk_if_present();
        backend
    }

    /// Attaches an injectable read-only [`LiveOnrampProvider`] (backed by MTProto or a test fake).
    pub fn set_onramp_provider(&mut self, provider: Arc<dyn LiveOnrampProvider>) {
        self.onramp_provider = Some(provider);
    }

    /// Attaches a live Telegram MTProto [`ErasedRpcClient`] and selects the active Toncenter
    /// transport using tdesktop's policy ([`ToncenterTransportPolicy::AutoPreferMtproto`]):
    /// native `toncenter.performApiRequest` on the shifted webfile DC by default, falling back to
    /// HTTP only when an explicit `toncenter_base_url` override is configured.
    pub fn attach_mtproto_rpc(&mut self, rpc: ErasedRpcClient, webfile_dc_id: DcId) {
        self.webfile_dc_id = webfile_dc_id.max(1);
        self.mtproto = Some(WalletMtprotoClient::new(rpc.clone(), self.webfile_dc_id));
        self.mtproto_rpc = Some(rpc);
        if !self.custom_toncenter {
            self.rebuild_toncenter_transport();
        }
    }

    /// Re-runs tdesktop's Toncenter transport selection (`AutoPreferMtproto`).
    fn rebuild_toncenter_transport(&mut self) {
        if self.custom_toncenter {
            return;
        }
        match select_toncenter_api(
            ToncenterTransportPolicy::AutoPreferMtproto,
            self.mtproto_rpc.clone(),
            self.webfile_dc_id,
            &self.app_config,
        ) {
            Ok((kind, api)) => {
                self.toncenter_transport_kind = Some(kind);
                self.toncenter = Some(api);
            }
            Err(_) => {
                self.toncenter = build_http_toncenter(&self.app_config);
                self.toncenter_transport_kind = self
                    .toncenter
                    .as_ref()
                    .map(|_| ToncenterTransportKind::Http);
            }
        }
    }

    /// Returns the cached fiat currency exchange rate table (`payments.getCurrencyRates`).
    #[must_use]
    pub const fn fiat_rates(&self) -> &FiatRateCache {
        &self.fiat_rates
    }

    fn apply_server_wallet_state(&mut self, state: &ServerWalletState) {
        if let ServerWalletState::Ready {
            backup_enabled,
            can_export_phrase,
            can_enable_backup,
            balance_nano,
            ..
        } = state
        {
            self.cloud_backup.backup_enabled = *backup_enabled;
            self.cloud_backup.can_export_phrase = *can_export_phrase;
            self.cloud_backup.can_enable_backup = *can_enable_backup;
            if *balance_nano >= 0 && self.balance_nano == 0 {
                self.balance_nano = *balance_nano as u64;
            }
            self.card_state.ready = true;
        }
    }

    /// Synchronously refreshes available read-only fiat onramp providers via the configured
    /// [`LiveOnrampProvider`].
    pub fn refresh_onramp_providers(&mut self) -> Result<Vec<OnrampProviderCard>, String> {
        let Some(provider) = self.onramp_provider.clone() else {
            return Ok(self.onramp_providers.clone());
        };
        let list = block_on_runtime(&self.rt_handle, async move {
            provider
                .fetch_providers(ONRAMP_CRYPTO_CURRENCY.to_owned())
                .await
        })?;
        self.onramp_providers = list.clone();
        Ok(list)
    }

    /// Returns the account's wallet persistence directory inside `tdata`.
    #[must_use]
    pub fn wallet_dir(&self) -> &Path {
        &self.wallet_dir
    }

    /// Returns the last `help.getAppConfig` hash applied to this backend.
    #[must_use]
    pub const fn app_config_hash(&self) -> i32 {
        self.app_config_hash
    }

    /// Spawns a non-blocking `help.getAppConfig` sync on the live session's Tokio runtime.
    /// The result is drained automatically on the next [`WalletBackend::poll_background`] tick.
    pub fn spawn_app_config_sync(&self, session: &LiveSession) {
        let rpc = session.runtime().rpc().clone();
        let bridge_state = session.bridge_state().clone();
        let base = self.app_config.clone();
        let hash = self.app_config_hash;
        let pending = Arc::clone(&self.pending_async);
        session.runtime().tokio_handle().spawn(async move {
            match sync_wallet_app_config_async(&rpc, &base, hash).await {
                Ok(Some(synced)) => {
                    if let Ok(mut guard) = pending.lock() {
                        guard.synced_app_config = Some(synced);
                    }
                    bridge_state.notify_ui();
                }
                Ok(None) => {
                    debug!("wallet help.getAppConfig not modified");
                }
                Err(err) => {
                    debug!(error = %err, "wallet help.getAppConfig sync skipped");
                }
            }
        });
    }

    /// Synchronously invokes `help.getAppConfig` on `session` and applies the returned wallet
    /// availability flags (`wallet_available`, `old_wallet_bot_username`,
    /// `wallet_transfer_min_nanos`, `ton_usd_rate`).
    pub fn sync_app_config_now(&mut self, session: &LiveSession) -> Result<bool, String> {
        let Some(synced) =
            sync_wallet_app_config_from_session(session, &self.app_config, self.app_config_hash)?
        else {
            return Ok(false);
        };
        self.apply_synced_app_config(synced);
        Ok(true)
    }

    /// Applies a [`SyncedWalletAppConfig`] while preserving local endpoints and `support_mode`.
    pub fn apply_synced_app_config(&mut self, synced: SyncedWalletAppConfig) {
        let mut next = synced.config;
        next.support_mode = self.app_config.support_mode;
        next.toncenter_base_url = self.app_config.toncenter_base_url.clone();
        next.toncenter_api_key = self.app_config.toncenter_api_key.clone();
        next.tonconnect_bridge_url = self.app_config.tonconnect_bridge_url.clone();
        next.testnet = self.app_config.testnet;
        self.app_config = next;
        self.app_config_hash = synced.hash;
        if let Some(rate) = synced.usd_per_ton {
            self.usd_per_ton = rate;
            self.fiat_rates.set_usd_per_ton(rate);
        }
    }

    /// Returns `true` if the OS [`WalletKeyProtector`] (Windows DPAPI) is available.
    #[must_use]
    pub fn is_key_protection_available(&self) -> bool {
        self.protector.is_available()
    }

    /// Returns `true` if DPAPI key protection is currently enrolled for this wallet.
    #[must_use]
    pub fn is_key_protection_enabled(&self) -> bool {
        self.dpapi_wrapped_passcode.is_some()
    }

    /// Enrolls Windows DPAPI protection for the current wallet vault passcode.
    ///
    /// Verifies `passcode` against the vault first, seals it via [`WalletKeyProtector::protect`]
    /// using per-account entropy, and persists the protected blob to `wallet_meta.json`.
    pub fn enable_key_protection(&mut self, passcode: &str) -> Result<(), String> {
        if !self.vault.is_initialized() {
            return Err("No wallet configured".to_owned());
        }
        if !self.protector.is_available() {
            return Err("Platform key protection is unavailable".to_owned());
        }
        // Verify passcode opens the vault before wrapping it.
        self.vault
            .unlock(passcode)
            .map_err(|e| format!("Invalid passcode: {e}"))?;
        let entropy = self.dpapi_entropy();
        let protected = self
            .protector
            .protect(passcode.as_bytes(), &entropy)
            .map_err(|e| format!("DPAPI protect failed: {e}"))?;
        self.dpapi_wrapped_passcode = Some(protected);
        self.persist_meta()?;
        Ok(())
    }

    /// Unlocks the encrypted vault using the stored Windows DPAPI protected blob.
    pub fn unlock_with_key_protection(&mut self) -> Result<(), String> {
        let Some(blob) = self.dpapi_wrapped_passcode.as_ref() else {
            return Err("Platform key protection is not enabled for this wallet".to_owned());
        };
        let entropy = self.dpapi_entropy();
        let mut raw = self
            .protector
            .unprotect(blob, &entropy)
            .map_err(|e| format!("DPAPI unprotect failed: {e}"))?;
        let res = match std::str::from_utf8(&raw) {
            Ok(passcode) => self.unlock_vault(passcode),
            Err(_) => Err("Corrupt DPAPI protected passcode UTF-8".to_owned()),
        };
        raw.fill(0);
        res
    }

    /// Disables DPAPI protection and removes the protected blob from disk.
    pub fn disable_key_protection(&mut self) -> Result<(), String> {
        self.dpapi_wrapped_passcode = None;
        if self.public_key.is_some() {
            self.persist_meta()?;
        }
        Ok(())
    }

    /// Builds [`TransferMessage`] values from a TON Connect `sendTransaction` request.
    fn build_ton_connect_messages(
        request: &TonConnectSendTxRequest,
    ) -> Result<Vec<TransferMessage>, String> {
        let mut msgs = Vec::with_capacity(request.messages.len());
        for m in &request.messages {
            let parsed = TonAddress::parse(&m.address).map_err(|e| e.to_string())?;
            let body = if let Some(ref payload_b64) = m.payload_boc {
                Some(deserialize_boc_base64(payload_b64).map_err(|e| e.to_string())?)
            } else if let Some(ref comment) = m.decoded_comment {
                TransferMessage::simple(
                    parsed.address,
                    m.amount_nano,
                    parsed.bounceable,
                    Some(comment.as_str()),
                )
                .map_err(|e| e.to_string())?
                .body
            } else {
                None
            };
            let state_init = match m.state_init_boc.as_deref() {
                Some(b64) => Some(deserialize_boc_base64(b64).map_err(|e| e.to_string())?),
                None => None,
            };
            msgs.push(TransferMessage {
                destination: parsed.address,
                amount_nano: m.amount_nano,
                bounce: parsed.bounceable,
                send_mode: DEFAULT_SEND_MODE,
                body,
                state_init,
            });
        }
        Ok(msgs)
    }

    /// Runs `ToncenterApi::emulate_message` with `ignore_chksig = true` for a TON Connect
    /// transaction preview, returning `(estimated_fee_nano, emulation, ton_connect_emulation)`.
    fn emulate_ton_connect_tx(
        &self,
        tx_req: &TonConnectSendTxRequest,
    ) -> (u64, Option<EmulationResult>, Option<TonConnectEmulation>) {
        let (Some(toncenter), Some(contract)) = (self.toncenter.as_ref(), self.contract.as_ref())
        else {
            return (DEFAULT_ESTIMATED_FEE_NANO, None, None);
        };
        let Ok(msgs) = Self::build_ton_connect_messages(tx_req) else {
            return (DEFAULT_ESTIMATED_FEE_NANO, None, None);
        };
        // Pre-sign emulation uses `ignore_chksig = true`, so we do not require unlocking the
        // vault just to preview balance deltas, fees, and warnings.
        let emul_kp = self
            .load_keypair()
            .unwrap_or_else(|_| TonKeypair::from_seed([0u8; 32]));
        let now = now_unix_u64();
        let valid_until = tx_req.valid_until.unwrap_or(now.saturating_add(300)) as u32;
        let Ok(signed) = contract.create_signed_transfer(&emul_kp, self.seqno, valid_until, &msgs)
        else {
            return (DEFAULT_ESTIMATED_FEE_NANO, None, None);
        };

        let req = EmulateMessageRequest::new(signed.boc_base64).with_metadata(true);
        let api = Arc::clone(toncenter);
        match block_on_runtime(
            &self.rt_handle,
            async move { api.emulate_message(&req).await },
        ) {
            Ok(emul) => {
                let fee = if emul.total_fees_nano > 0 {
                    emul.total_fees_nano
                } else if emul.root_fee_nano > 0 {
                    emul.root_fee_nano
                } else {
                    DEFAULT_ESTIMATED_FEE_NANO
                };
                let tc_emul = contract.address().ok().map(|addr| {
                    emul.to_ton_connect_emulation(&addr.to_raw(), tx_req.total_nano() as i64)
                });
                (fee, Some(emul), tc_emul)
            }
            Err(err) => {
                debug!(error = %err, "TON Connect emulate_message preview unavailable");
                (DEFAULT_ESTIMATED_FEE_NANO, None, None)
            }
        }
    }

    /// Polls all active TON Connect sessions on the configured bridge for incoming dApp requests
    /// (`sendTransaction`, `signData`, `disconnect`, etc.), returning the number of bridge events processed.
    pub fn poll_ton_connect_events(&mut self) -> Result<usize, String> {
        let Some(bridge) = self.bridge.clone() else {
            return Ok(0);
        };
        if self.sessions.is_empty() {
            return Ok(0);
        }

        let mut total_processed = 0_usize;
        let mut sessions_dirty = false;
        let mut disconnected_ids: Vec<u64> = Vec::new();
        let mut replies_to_send: Vec<(String, String, String)> = Vec::new();
        let mut incoming_txs: Vec<(u64, String, String, TonConnectSendTxRequest)> = Vec::new();
        let mut incoming_sign_data: Vec<PendingSignDataApproval> = Vec::new();

        for stored in &mut self.sessions {
            let wallet_cid = stored.session.wallet_client_id();
            let last_id = stored.session.last_event_id.clone();
            let bridge_ref = Arc::clone(&bridge);
            let events = block_on_runtime(&self.rt_handle, async move {
                bridge_ref
                    .poll_events(&wallet_cid, last_id.as_deref())
                    .await
            })
            .map_err(|e| e.to_string())?;

            for ev in events {
                if let Some(id) = ev.id {
                    stored.session.last_event_id = Some(id);
                    sessions_dirty = true;
                }
                if !ev.from.eq_ignore_ascii_case(&stored.card.dapp_client_id) {
                    continue;
                }
                total_processed += 1;
                match stored.session.decrypt_app_request(&ev.message) {
                    Ok(TonConnectAppRequest::SendTransaction(tx_req)) => {
                        let domain = stored.card.manifest.host();
                        incoming_txs.push((
                            stored.card.session_id,
                            stored.card.manifest.name.clone(),
                            domain,
                            tx_req,
                        ));
                    }
                    Ok(TonConnectAppRequest::Disconnect { .. }) => {
                        disconnected_ids.push(stored.card.session_id);
                        sessions_dirty = true;
                    }
                    Ok(TonConnectAppRequest::SignData(sd)) => {
                        // Validate payload shape early; never sign until user confirms in UI.
                        if let Err(err) = sd.to_sign_data_payload() {
                            if let Ok(enc) = stored.session.build_error_response(
                                &sd.request_id,
                                TonConnectErrorCode::BadRequest,
                                &err.to_string(),
                            ) {
                                replies_to_send.push((
                                    stored.session.wallet_client_id(),
                                    stored.session.dapp_client_id(),
                                    enc,
                                ));
                            }
                        } else {
                            let domain = stored.card.manifest.host();
                            incoming_sign_data.push(PendingSignDataApproval {
                                session_id: stored.card.session_id,
                                dapp_name: stored.card.manifest.name.clone(),
                                dapp_domain: domain,
                                request: sd,
                            });
                        }
                    }
                    Ok(TonConnectAppRequest::Unsupported { request_id, method }) => {
                        if let Ok(enc) = stored.session.build_error_response(
                            &request_id,
                            TonConnectErrorCode::MethodNotSupported,
                            &format!("Unsupported method: {method}"),
                        ) {
                            replies_to_send.push((
                                stored.session.wallet_client_id(),
                                stored.session.dapp_client_id(),
                                enc,
                            ));
                        }
                    }
                    Err(err) => {
                        warn!(error = %err, "failed to decrypt incoming TON Connect request");
                    }
                }
            }
        }

        for (session_id, dapp_name, dapp_domain, tx_req) in incoming_txs {
            let (estimated_fee_nano, emulation, ton_connect_emulation) =
                self.emulate_ton_connect_tx(&tx_req);
            self.pending_tx = Some(PendingTxApproval {
                session_id,
                dapp_name,
                dapp_domain,
                request: tx_req,
                estimated_fee_nano,
                emulation,
                ton_connect_emulation,
            });
        }

        for sd in incoming_sign_data {
            self.pending_sign_data = Some(sd);
        }

        for (from_cid, to_cid, enc) in replies_to_send {
            let bridge_ref = Arc::clone(&bridge);
            let _ = block_on_runtime(&self.rt_handle, async move {
                bridge_ref
                    .send_message(&from_cid, &to_cid, TON_CONNECT_TTL_SECS, &enc)
                    .await
            });
        }

        if !disconnected_ids.is_empty() {
            self.sessions
                .retain(|s| !disconnected_ids.contains(&s.card.session_id));
            if self
                .pending_tx
                .as_ref()
                .is_some_and(|p| disconnected_ids.contains(&p.session_id))
            {
                self.pending_tx = None;
            }
            if self
                .pending_sign_data
                .as_ref()
                .is_some_and(|p| disconnected_ids.contains(&p.session_id))
            {
                self.pending_sign_data = None;
            }
        }

        if sessions_dirty && self.vault.is_unlocked() {
            let _ = self.persist_ton_connect_sessions();
        }

        Ok(total_processed)
    }

    fn dpapi_entropy(&self) -> Vec<u8> {
        format!("tgfork-account:{}", self.account_id).into_bytes()
    }

    fn load_from_disk_if_present(&mut self) {
        if !self.wallet_dir.join("keyring.bin").exists() {
            return;
        }
        match LocalKeyVault::load_from_dir(self.account_id, &self.wallet_dir) {
            Ok(loaded) => {
                self.vault = loaded;
            }
            Err(err) => {
                warn!(
                    dir = %self.wallet_dir.display(),
                    error = %err,
                    "failed to load encrypted wallet vault from tdata"
                );
                return;
            }
        }

        let meta_path = self.wallet_dir.join(WALLET_META_FILENAME);
        let Ok(meta_bytes) = std::fs::read(&meta_path) else {
            return;
        };
        let Ok(meta) = serde_json::from_slice::<PersistedWalletMeta>(&meta_bytes) else {
            warn!(path = %meta_path.display(), "corrupt wallet_meta.json");
            return;
        };
        if let Some(pk) = decode_hex_32(&meta.public_key_hex) {
            self.version = meta.version;
            self.public_key = Some(pk);
            self.contract = Some(WalletContract::new(
                meta.version,
                0,
                pk,
                self.app_config.testnet,
            ));
        }
        if let Some(hex_blob) = meta.dpapi_wrapped_passcode_hex {
            self.dpapi_wrapped_passcode = decode_hex_vec(&hex_blob);
        }
    }

    fn persist_meta(&self) -> Result<(), String> {
        let Some(pk) = self.public_key else {
            return Ok(());
        };
        std::fs::create_dir_all(&self.wallet_dir)
            .map_err(|e| format!("create wallet tdata dir: {e}"))?;
        let meta = PersistedWalletMeta {
            version: self.version,
            public_key_hex: encode_hex(&pk),
            testnet: self.app_config.testnet,
            dpapi_wrapped_passcode_hex: self.dpapi_wrapped_passcode.as_deref().map(encode_hex),
        };
        let bytes = serde_json::to_vec_pretty(&meta)
            .map_err(|e| format!("serialize wallet metadata: {e}"))?;
        std::fs::write(self.wallet_dir.join(WALLET_META_FILENAME), bytes)
            .map_err(|e| format!("write wallet metadata: {e}"))
    }

    fn persist_vault_and_meta(&self) -> Result<(), String> {
        self.vault
            .save_to_dir(&self.wallet_dir)
            .map_err(|e| format!("persist encrypted wallet vault: {e}"))?;
        self.persist_meta()
    }

    fn persist_ton_connect_sessions(&mut self) -> Result<(), String> {
        if !self.vault.is_unlocked() {
            return Ok(());
        }
        let bundle = PersistedTonConnectBundle {
            next_session_id: self.next_session_id,
            sessions: self
                .sessions
                .iter()
                .map(|s| PersistedTonConnectSession {
                    session_id: s.card.session_id,
                    wallet_secret_hex: encode_hex(s.session.wallet_keypair.secret_key_bytes()),
                    dapp_client_id: s.card.dapp_client_id.clone(),
                    manifest: s.card.manifest.clone(),
                    status: s.card.status,
                    connected_at: s.card.connected_at,
                    last_event_id: s.session.last_event_id.clone(),
                })
                .collect(),
        };
        let bytes = serde_json::to_vec(&bundle)
            .map_err(|e| format!("serialize TON Connect sessions: {e}"))?;
        self.vault
            .store_secret(VAULT_SECRET_TON_CONNECT, &bytes)
            .map_err(|e| format!("seal TON Connect sessions: {e}"))?;
        self.vault
            .save_to_dir(&self.wallet_dir)
            .map_err(|e| format!("persist wallet vault: {e}"))
    }

    fn load_ton_connect_sessions_from_vault(&mut self) {
        let Ok(bytes) = self.vault.read_secret(VAULT_SECRET_TON_CONNECT) else {
            return;
        };
        let Ok(bundle) = serde_json::from_slice::<PersistedTonConnectBundle>(bytes.as_slice())
        else {
            warn!("corrupt encrypted TON Connect sessions bundle in vault");
            return;
        };
        self.next_session_id = bundle.next_session_id.max(1);
        let mut restored = Vec::with_capacity(bundle.sessions.len());
        for item in bundle.sessions {
            let Some(secret) = decode_hex_32(&item.wallet_secret_hex) else {
                continue;
            };
            let kp = TonConnectKeypair::from_secret_bytes(secret);
            let Ok(mut tc_session) =
                TonConnectSession::new(kp, &item.dapp_client_id, item.manifest.clone())
            else {
                continue;
            };
            tc_session.status = item.status;
            tc_session.last_event_id = item.last_event_id;
            restored.push(LiveTonConnectSession {
                card: TonConnectSessionCard {
                    session_id: item.session_id,
                    dapp_client_id: item.dapp_client_id,
                    manifest: item.manifest,
                    status: item.status,
                    connected_at: item.connected_at,
                },
                session: tc_session,
            });
        }
        self.sessions = restored;
    }

    fn load_keypair(&self) -> Result<TonKeypair, String> {
        let secret = self
            .vault
            .read_secret(VAULT_SECRET_SEED)
            .map_err(|e| format!("Vault locked or unreadable: {e}"))?;
        let seed: [u8; 32] = secret
            .as_slice()
            .try_into()
            .map_err(|_| "Corrupt seed in vault".to_owned())?;
        Ok(TonKeypair::from_seed(seed))
    }

    fn estimate_transfer_fee(&self, msgs: &[TransferMessage]) -> u64 {
        let (Some(toncenter), Some(contract)) = (self.toncenter.as_ref(), self.contract.as_ref())
        else {
            return DEFAULT_ESTIMATED_FEE_NANO;
        };
        let Ok(kp) = self.load_keypair() else {
            return DEFAULT_ESTIMATED_FEE_NANO;
        };
        let valid_until = now_unix_u64().saturating_add(300) as u32;
        let Ok(signed) = contract.create_signed_transfer(&kp, self.seqno, valid_until, msgs) else {
            return DEFAULT_ESTIMATED_FEE_NANO;
        };
        let api = Arc::clone(toncenter);
        let boc = signed.boc_base64;
        match block_on_runtime(
            &self.rt_handle,
            async move { api.emulate_trace(&boc).await },
        ) {
            Ok(trace) if trace.fee_nano > 0 => trace.fee_nano as u64,
            _ => DEFAULT_ESTIMATED_FEE_NANO,
        }
    }

    fn initialize_wallet_from_words(
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
            .initialize_with_passcode(passcode, self.kdf)
            .map_err(|e| e.to_string())?;
        self.vault
            .store_secret(VAULT_SECRET_SEED, kp.seed_bytes())
            .map_err(|e| e.to_string())?;
        self.vault
            .store_secret(VAULT_SECRET_MNEMONIC, words.join(" ").as_bytes())
            .map_err(|e| e.to_string())?;
        let pk = kp.public_key_bytes();
        let contract = WalletContract::new(version, 0, pk, self.app_config.testnet);
        let addr = contract.address().map_err(|e| e.to_string())?;

        self.version = version;
        self.public_key = Some(pk);
        self.contract = Some(contract);
        self.dpapi_wrapped_passcode = None;
        self.balance_nano = 0;
        self.account_state = AccountState::Uninitialized;
        self.seqno = 0;
        self.history.clear();
        self.sessions.clear();
        self.pending_connect = None;
        self.pending_tx = None;

        self.persist_vault_and_meta()?;
        if self.toncenter.is_some() {
            let _ = self.refresh();
        }
        Ok(addr)
    }
}

impl WalletBackend for LiveWalletBackend {
    fn app_config(&self) -> &WalletAppConfig {
        &self.app_config
    }

    fn set_app_config(&mut self, config: WalletAppConfig) {
        let toncenter_changed = self.app_config.toncenter_base_url != config.toncenter_base_url
            || self.app_config.toncenter_api_key != config.toncenter_api_key;
        let bridge_changed = self.app_config.tonconnect_bridge_url != config.tonconnect_bridge_url;
        let testnet_changed = self.app_config.testnet != config.testnet;

        self.app_config = config;

        if !self.custom_toncenter && toncenter_changed {
            self.rebuild_toncenter_transport();
        }
        if !self.custom_bridge && bridge_changed {
            self.bridge = build_http_bridge(&self.app_config);
        }
        if testnet_changed && let Some(pk) = self.public_key {
            self.contract = Some(WalletContract::new(
                self.version,
                0,
                pk,
                self.app_config.testnet,
            ));
        }
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
        let contract = self
            .contract
            .as_ref()
            .ok_or_else(|| "No wallet configured".to_owned())?;
        let addr = contract.address().map_err(|e| e.to_string())?;
        let own_raw = addr.to_raw();
        let friendly = addr.to_friendly(false, self.app_config.testnet);

        // 1. Hydrate native Layer-233 MTProto wallet state, gasless terms, FX rates, and history.
        let mut server_history: Option<Vec<WalletTxRecord>> = None;
        if let Some(mtproto) = self.mtproto.clone() {
            let transfer_min = self.app_config.transfer_min_nanos;
            let testnet = self.app_config.testnet;
            let own_raw_clone = own_raw.clone();
            let (state_res, gasless_res, rates_res, txs_res) =
                block_on_runtime(&self.rt_handle, async move {
                    let st = mtproto.get_state().await;
                    let gl = mtproto.get_gasless_info().await;
                    let fx = mtproto.get_currency_rates().await;
                    let txs = mtproto
                        .get_transactions(TransactionDirectionFilter::All, "", 50)
                        .await;
                    (st, gl, fx, txs)
                });
            if let Ok(state) = state_res {
                self.apply_server_wallet_state(&state);
            }
            if let Ok(info) = gasless_res {
                self.gasless = Some(GaslessTerms::compute(
                    info,
                    Some(own_raw_clone),
                    transfer_min,
                    transfer_min,
                ));
            }
            if let Ok(rates) = rates_res {
                self.fiat_rates.apply_rates(rates);
                if let Some(per_ton) = self.fiat_rates.current_per_ton()
                    && per_ton > 0.0
                {
                    self.usd_per_ton = per_ton;
                }
            }
            if let Ok(page) = txs_res {
                if page.balance_nano >= 0 {
                    self.balance_nano = page.balance_nano as u64;
                }
                if !page.transactions.is_empty() {
                    server_history = Some(
                        page.transactions
                            .into_iter()
                            .map(|tx| {
                                let (cp_addr, cp_label) = match tx.peer {
                                    Some(ServerTransactionPeer::User {
                                        user_id,
                                        address,
                                        domain,
                                        ..
                                    }) => (domain.unwrap_or(address), format!("User #{user_id}")),
                                    Some(ServerTransactionPeer::Address {
                                        address,
                                        domain,
                                        ..
                                    }) => (
                                        domain.unwrap_or_else(|| {
                                            TonAddress::parse(&address)
                                                .map(|p| p.address.to_friendly(false, testnet))
                                                .unwrap_or(address)
                                        }),
                                        String::new(),
                                    ),
                                    Some(ServerTransactionPeer::Onramp {
                                        address,
                                        provider_name,
                                        ..
                                    }) => (address, provider_name),
                                    Some(ServerTransactionPeer::Channel {
                                        channel_id,
                                        address,
                                        ..
                                    }) => (address, format!("Channel #{channel_id}")),
                                    Some(ServerTransactionPeer::TelegramGift { .. }) => {
                                        (String::new(), "Telegram Gift".to_owned())
                                    }
                                    Some(ServerTransactionPeer::Unsupported) | None => {
                                        (String::new(), String::new())
                                    }
                                };
                                WalletTxRecord {
                                    id: tx.id,
                                    incoming: tx.incoming,
                                    counterparty_address: cp_addr,
                                    counterparty_label: cp_label,
                                    amount_nano: tx.amount_nano.unsigned_abs(),
                                    fee_nano: Some(tx.fee_nano.max(0) as u64),
                                    comment: tx.comment.unwrap_or_default(),
                                    timestamp: i64::from(tx.date),
                                    status: if tx.failed {
                                        WalletTxStatus::Failure
                                    } else {
                                        WalletTxStatus::Success
                                    },
                                }
                            })
                            .collect(),
                    );
                }
            }
        }

        // 2. Query Toncenter (over MTProto `toncenter.performApiRequest` by default, or HTTP override).
        let Some(toncenter) = self.toncenter.as_ref() else {
            if let Some(hist) = server_history {
                self.history = hist;
                return Ok(());
            }
            return Err("Toncenter endpoint is not configured".to_owned());
        };

        let api = Arc::clone(toncenter);
        let friendly_clone = friendly.clone();
        let need_tc_txs = server_history.is_none();
        let (info, txs, seqno_opt) = block_on_runtime(&self.rt_handle, async move {
            let info = api
                .get_address_information(&friendly_clone)
                .await
                .map_err(|e| e.to_string())?;
            let txs = if need_tc_txs {
                api.get_transactions(&friendly_clone, 20, None, None)
                    .await
                    .map_err(|e| e.to_string())?
            } else {
                Vec::new()
            };
            let seqno = if info.state == AccountState::Active {
                match api.run_get_method(&friendly_clone, "seqno", &[]).await {
                    Ok(res) if res.exit_code == 0 => res.first_u64().map(|v| v as u32),
                    _ => None,
                }
            } else {
                Some(0)
            };
            Ok::<_, String>((info, txs, seqno))
        })?;

        self.balance_nano = info.balance_nano;
        self.account_state = info.state;
        if let Some(seq) = seqno_opt {
            self.seqno = seq;
        }
        if let Some(hist) = server_history {
            self.history = hist;
        } else {
            self.history = txs
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
        }
        self.card_state.ready = true;

        if self.bridge.is_some() && !self.sessions.is_empty() {
            let _ = self.poll_ton_connect_events();
        }

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
        self.initialize_wallet_from_words(words, "", version, passcode)
    }

    fn import_wallet(
        &mut self,
        words: &[String],
        password: &str,
        version: WalletVersion,
        passcode: &str,
    ) -> Result<TonAddress, String> {
        self.initialize_wallet_from_words(words, password, version, passcode)
    }

    fn lock_vault(&mut self) {
        self.vault.lock();
    }

    fn unlock_vault(&mut self, passcode: &str) -> Result<(), String> {
        self.vault.unlock(passcode).map_err(|e| e.to_string())?;
        // Rebuild contract from seed if metadata file was absent or out of sync.
        if self.contract.is_none()
            && let Ok(kp) = self.load_keypair()
        {
            let pk = kp.public_key_bytes();
            self.public_key = Some(pk);
            self.contract = Some(WalletContract::new(
                self.version,
                0,
                pk,
                self.app_config.testnet,
            ));
        }
        self.load_ton_connect_sessions_from_vault();
        Ok(())
    }

    fn reveal_phrase(&self) -> Result<Vec<String>, String> {
        let bytes = self
            .vault
            .read_secret(VAULT_SECRET_MNEMONIC)
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
        let bounce = bounce_override.unwrap_or(parsed.bounceable);
        let msg = TransferMessage::simple(
            parsed.address,
            amount_nano,
            bounce,
            (!comment.is_empty()).then_some(comment),
        )
        .map_err(|e| e.to_string())?;
        let fee_nano = self.estimate_transfer_fee(&[msg]);
        let total = amount_nano.saturating_add(fee_nano);
        if total > self.balance_nano {
            return Err("Insufficient TON balance (including network fee)".to_owned());
        }
        Ok(SendQuote {
            destination: parsed.address,
            bounce,
            amount_nano,
            fee_nano,
            comment: comment.to_owned(),
        })
    }

    fn confirm_send(&mut self, quote: &SendQuote) -> Result<SendReceipt, String> {
        let contract = self
            .contract
            .as_ref()
            .ok_or_else(|| "No wallet configured".to_owned())?;
        let toncenter = self
            .toncenter
            .as_ref()
            .ok_or_else(|| "Toncenter endpoint is not configured".to_owned())?;
        let kp = self.load_keypair()?;
        let msg = TransferMessage::simple(
            quote.destination,
            quote.amount_nano,
            quote.bounce,
            (!quote.comment.is_empty()).then_some(quote.comment.as_str()),
        )
        .map_err(|e| e.to_string())?;

        let now = now_unix_u64();
        let valid_until = now.saturating_add(300) as u32;
        let signed = contract
            .create_signed_transfer(&kp, self.seqno, valid_until, &[msg])
            .map_err(|e| e.to_string())?;

        let api = Arc::clone(toncenter);
        let boc_to_send = signed.boc_base64.clone();
        let send_res =
            block_on_runtime(
                &self.rt_handle,
                async move { api.send_boc(&boc_to_send).await },
            )
            .map_err(|e| e.to_string())?;
        if !send_res.ok {
            return Err("Toncenter rejected signed BOC".to_owned());
        }

        let hash_hex = send_res
            .hash
            .filter(|h| !h.is_empty())
            .unwrap_or_else(|| encode_hex(&signed.message_hash));
        let total = quote.amount_nano.saturating_add(quote.fee_nano);
        self.balance_nano = self.balance_nano.saturating_sub(total);
        self.seqno = self.seqno.saturating_add(1);
        self.account_state = AccountState::Active;

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
                timestamp: now as i64,
                status: WalletTxStatus::Pending,
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
        self.pending_tx.clone()
    }

    fn handle_ton_connect_link(&mut self, raw_link: &str) -> Result<(), String> {
        let link = parse_ton_connect_link(raw_link).map_err(|e| e.to_string())?;
        match link.kind {
            TonConnectLinkKind::Bare => Ok(()),
            TonConnectLinkKind::OpenPending => {
                let _ = self.poll_ton_connect_events()?;
                Ok(())
            }
            TonConnectLinkKind::Connect => {
                let bridge = self
                    .bridge
                    .as_ref()
                    .ok_or_else(|| "TON Connect bridge endpoint is not configured".to_owned())?;
                let bridge_ref = Arc::clone(bridge);
                let manifest_url = link.manifest_url.clone();
                let manifest = block_on_runtime(&self.rt_handle, async move {
                    bridge_ref.fetch_manifest(&manifest_url).await
                })
                .map_err(|e| format!("Failed to fetch dApp manifest: {e}"))?;

                self.pending_connect = Some(PendingConnectApproval {
                    requests_proof: link.proof_payload.is_some(),
                    link,
                    manifest,
                });
                Ok(())
            }
        }
    }

    fn approve_ton_connect(&mut self) -> Result<TonConnectSessionCard, String> {
        let pending = self
            .pending_connect
            .clone()
            .ok_or_else(|| "No pending TON Connect request".to_owned())?;
        let contract = self
            .contract
            .as_ref()
            .ok_or_else(|| "Create or import a wallet first".to_owned())?;
        let bridge = self
            .bridge
            .as_ref()
            .ok_or_else(|| "TON Connect bridge endpoint is not configured".to_owned())?;
        let kp = self.load_keypair()?;

        let wallet_tc_kp = TonConnectKeypair::generate();
        let mut tc_session = TonConnectSession::new(
            wallet_tc_kp,
            &pending.link.client_id,
            pending.manifest.clone(),
        )
        .map_err(|e| e.to_string())?;

        let now = now_unix_u64();
        let session_id = self.next_session_id;
        let encrypted_reply = tc_session
            .build_connect_event(
                session_id,
                contract,
                &kp,
                pending.link.proof_payload.as_deref(),
                now,
            )
            .map_err(|e| e.to_string())?;

        let from_cid = tc_session.wallet_client_id();
        let to_cid = tc_session.dapp_client_id();
        let bridge_ref = Arc::clone(bridge);
        block_on_runtime(&self.rt_handle, async move {
            bridge_ref
                .send_message(&from_cid, &to_cid, TON_CONNECT_TTL_SECS, &encrypted_reply)
                .await
        })
        .map_err(|e| e.to_string())?;

        tc_session.status = TonConnectSessionStatus::Active;
        self.pending_connect = None;
        self.next_session_id = session_id.saturating_add(1);

        let card = TonConnectSessionCard {
            session_id,
            dapp_client_id: pending.link.client_id,
            manifest: pending.manifest,
            status: TonConnectSessionStatus::Active,
            connected_at: now as i64,
        };
        self.sessions.push(LiveTonConnectSession {
            card: card.clone(),
            session: tc_session,
        });
        let _ = self.persist_ton_connect_sessions();
        Ok(card)
    }

    fn decline_ton_connect(&mut self) {
        self.pending_connect = None;
    }

    fn approve_ton_connect_tx(&mut self) -> Result<SendReceipt, String> {
        let pending = self
            .pending_tx
            .clone()
            .ok_or_else(|| "No pending TON Connect transaction".to_owned())?;
        let contract = self
            .contract
            .as_ref()
            .ok_or_else(|| "No wallet configured".to_owned())?;
        let toncenter = self
            .toncenter
            .as_ref()
            .ok_or_else(|| "Toncenter endpoint is not configured".to_owned())?;
        let bridge = self
            .bridge
            .as_ref()
            .ok_or_else(|| "TON Connect bridge endpoint is not configured".to_owned())?;
        let kp = self.load_keypair()?;

        let msgs = Self::build_ton_connect_messages(&pending.request)?;

        let now = now_unix_u64();
        let valid_until = pending
            .request
            .valid_until
            .unwrap_or(now.saturating_add(300)) as u32;
        let signed = contract
            .create_signed_transfer(&kp, self.seqno, valid_until, &msgs)
            .map_err(|e| e.to_string())?;

        // 1. Broadcast signed BOC to Toncenter.
        let api = Arc::clone(toncenter);
        let boc_to_send = signed.boc_base64.clone();
        let send_res =
            block_on_runtime(
                &self.rt_handle,
                async move { api.send_boc(&boc_to_send).await },
            )
            .map_err(|e| e.to_string())?;
        if !send_res.ok {
            return Err("Toncenter rejected signed TON Connect BOC".to_owned());
        }

        // 2. Send encrypted JSON-RPC success response over the TON Connect bridge.
        if let Some(stored) = self
            .sessions
            .iter()
            .find(|s| s.card.session_id == pending.session_id)
        {
            let encrypted_resp = stored
                .session
                .build_tx_success_response(&pending.request.request_id, &signed.boc_base64)
                .map_err(|e| e.to_string())?;
            let from_cid = stored.session.wallet_client_id();
            let to_cid = stored.session.dapp_client_id();
            let bridge_ref = Arc::clone(bridge);
            block_on_runtime(&self.rt_handle, async move {
                bridge_ref
                    .send_message(&from_cid, &to_cid, TON_CONNECT_TTL_SECS, &encrypted_resp)
                    .await
            })
            .map_err(|e| e.to_string())?;
        }

        let hash_hex = send_res
            .hash
            .filter(|h| !h.is_empty())
            .unwrap_or_else(|| encode_hex(&signed.message_hash));
        let total = pending
            .request
            .total_nano()
            .saturating_add(pending.estimated_fee_nano);
        self.balance_nano = self.balance_nano.saturating_sub(total);
        self.seqno = self.seqno.saturating_add(1);

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
                timestamp: now as i64,
                status: WalletTxStatus::Pending,
            },
        );
        self.pending_tx = None;

        Ok(SendReceipt {
            message_hash_hex: hash_hex,
            signed_boc_base64: signed.boc_base64,
        })
    }

    fn decline_ton_connect_tx(&mut self) {
        let Some(pending) = self.pending_tx.take() else {
            return;
        };
        let Some(bridge) = self.bridge.as_ref() else {
            return;
        };
        if let Some(stored) = self
            .sessions
            .iter()
            .find(|s| s.card.session_id == pending.session_id)
            && let Ok(enc) = stored.session.build_error_response(
                &pending.request.request_id,
                TonConnectErrorCode::UserDeclined,
                "User declined transaction",
            )
        {
            let from_cid = stored.session.wallet_client_id();
            let to_cid = stored.session.dapp_client_id();
            let bridge_ref = Arc::clone(bridge);
            let _ = block_on_runtime(&self.rt_handle, async move {
                bridge_ref
                    .send_message(&from_cid, &to_cid, TON_CONNECT_TTL_SECS, &enc)
                    .await
            });
        }
    }

    fn pending_sign_data(&self) -> Option<PendingSignDataApproval> {
        self.pending_sign_data.clone()
    }

    fn approve_ton_connect_sign_data(&mut self) -> Result<SignDataResult, String> {
        let pending = self
            .pending_sign_data
            .clone()
            .ok_or_else(|| "No pending TON Connect signData request".to_owned())?;
        let contract = self
            .contract
            .as_ref()
            .ok_or_else(|| "No wallet configured".to_owned())?;
        let bridge = self
            .bridge
            .as_ref()
            .ok_or_else(|| "TON Connect bridge endpoint is not configured".to_owned())?;
        let kp = self.load_keypair()?;
        let address = contract.address().map_err(|e| e.to_string())?;
        let payload = pending
            .request
            .to_sign_data_payload()
            .map_err(|e| e.to_string())?;
        let now = now_unix_u64();
        let signed = sign_ton_connect_data(&kp, &address, &pending.dapp_domain, now, &payload)
            .map_err(|e| e.to_string())?;

        if let Some(stored) = self
            .sessions
            .iter()
            .find(|s| s.card.session_id == pending.session_id)
        {
            let encrypted_resp = stored
                .session
                .build_sign_data_success_response(&pending.request.request_id, &signed)
                .map_err(|e| e.to_string())?;
            let from_cid = stored.session.wallet_client_id();
            let to_cid = stored.session.dapp_client_id();
            let bridge_ref = Arc::clone(bridge);
            block_on_runtime(&self.rt_handle, async move {
                bridge_ref
                    .send_message(&from_cid, &to_cid, TON_CONNECT_TTL_SECS, &encrypted_resp)
                    .await
            })
            .map_err(|e| e.to_string())?;
        }

        self.pending_sign_data = None;
        Ok(signed)
    }

    fn decline_ton_connect_sign_data(&mut self) {
        let Some(pending) = self.pending_sign_data.take() else {
            return;
        };
        let Some(bridge) = self.bridge.as_ref() else {
            return;
        };
        if let Some(stored) = self
            .sessions
            .iter()
            .find(|s| s.card.session_id == pending.session_id)
            && let Ok(enc) = stored.session.build_error_response(
                &pending.request.request_id,
                TonConnectErrorCode::UserDeclined,
                "User declined signData request",
            )
        {
            let from_cid = stored.session.wallet_client_id();
            let to_cid = stored.session.dapp_client_id();
            let bridge_ref = Arc::clone(bridge);
            let _ = block_on_runtime(&self.rt_handle, async move {
                bridge_ref
                    .send_message(&from_cid, &to_cid, TON_CONNECT_TTL_SECS, &enc)
                    .await
            });
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
        if let Some(bridge) = self.bridge.as_ref()
            && let Ok(enc) = stored.session.build_disconnect_event(session_id)
        {
            let from_cid = stored.session.wallet_client_id();
            let to_cid = stored.session.dapp_client_id();
            let bridge_ref = Arc::clone(bridge);
            let _ = block_on_runtime(&self.rt_handle, async move {
                bridge_ref
                    .send_message(&from_cid, &to_cid, TON_CONNECT_TTL_SECS, &enc)
                    .await
            });
        }
        if self
            .pending_tx
            .as_ref()
            .is_some_and(|p| p.session_id == session_id)
        {
            self.pending_tx = None;
        }
        if self
            .pending_sign_data
            .as_ref()
            .is_some_and(|p| p.session_id == session_id)
        {
            self.pending_sign_data = None;
        }
        if self.vault.is_unlocked() {
            let _ = self.persist_ton_connect_sessions();
        }
        Ok(())
    }

    fn poll_background(&mut self) {
        let (synced, providers) = self
            .pending_async
            .lock()
            .ok()
            .map(|mut g| (g.synced_app_config.take(), g.onramp_providers.take()))
            .unwrap_or((None, None));
        if let Some(synced) = synced {
            self.apply_synced_app_config(synced);
        }
        if let Some(providers) = providers {
            self.onramp_providers = providers;
        }
    }

    fn collectibles(&self) -> Vec<NftItem> {
        self.collectibles.clone()
    }

    fn jetton_balances(&self) -> Vec<JettonWalletBalance> {
        self.jettons.clone()
    }

    fn refresh_assets(&mut self) -> Result<(), String> {
        let contract = self
            .contract
            .as_ref()
            .ok_or_else(|| "No wallet configured".to_owned())?;
        let addr = contract.address().map_err(|e| e.to_string())?;
        let owner_raw = addr.to_raw();

        let mut mtproto_nfts: Option<Vec<NftItem>> = None;
        if let Some(mtproto) = self.mtproto.clone()
            && let Ok(page) =
                block_on_runtime(
                    &self.rt_handle,
                    async move { mtproto.get_nfts("", 50).await },
                )
        {
            mtproto_nfts = Some(page.items.into_iter().map(|s| s.item).collect());
        }

        let Some(toncenter) = self.toncenter.as_ref() else {
            if let Some(nfts) = mtproto_nfts {
                self.collectibles = nfts;
                return Ok(());
            }
            return Err("Toncenter endpoint is not configured".to_owned());
        };
        let api = Arc::clone(toncenter);
        let need_tc_nfts = mtproto_nfts.is_none();

        let (nft_page, mut jetton_page) = block_on_runtime(&self.rt_handle, async move {
            let nfts = if need_tc_nfts {
                api.get_nft_items(&owner_raw, 50, 0)
                    .await
                    .map(|p| p.list)
                    .map_err(|e| e.to_string())?
            } else {
                Vec::new()
            };
            let mut jw = api
                .get_jetton_wallets(&owner_raw, 50, 0)
                .await
                .map_err(|e| e.to_string())?;
            // Enrich any Jetton wallet lacking symbol/name from `/api/v3/jetton/masters`.
            for entry in &mut jw.wallets {
                if (entry.symbol.is_empty() || entry.name.is_empty())
                    && !entry.jetton_master.is_empty()
                    && let Ok(masters) = api.get_jetton_masters(&entry.jetton_master).await
                    && let Some(master) = masters.first()
                {
                    entry.apply_master_info(master);
                }
            }
            Ok::<_, String>((nfts, jw))
        })?;

        self.collectibles = mtproto_nfts.unwrap_or(nft_page);
        jetton_page.wallets.retain(|w| w.balance > 0);
        self.jettons = jetton_page.wallets;
        Ok(())
    }

    fn prepare_jetton_transfer(
        &self,
        jetton_wallet_addr: &str,
        destination_input: &str,
        jetton_amount: u128,
        comment: &str,
    ) -> Result<JettonTransferQuote, String> {
        let contract = self
            .contract
            .as_ref()
            .ok_or_else(|| "No wallet configured".to_owned())?;
        let token = self
            .jettons
            .iter()
            .find(|t| {
                t.wallet_address.eq_ignore_ascii_case(jetton_wallet_addr)
                    || t.user_friendly_wallet == jetton_wallet_addr
            })
            .ok_or_else(|| "Jetton wallet not found".to_owned())?;
        if jetton_amount == 0 {
            return Err("Jetton transfer amount must be greater than zero".to_owned());
        }
        if jetton_amount > token.balance {
            return Err("Insufficient Jetton balance".to_owned());
        }
        if comment.len() > 256 {
            return Err("Comment exceeds 256-byte limit".to_owned());
        }
        let jw = TonAddress::parse(&token.wallet_address)
            .map_err(|e| e.to_string())?
            .address;
        let dest = TonAddress::parse(destination_input)
            .map_err(|e| e.to_string())?
            .address;
        let self_addr = contract.address().map_err(|e| e.to_string())?;
        let msg = build_jetton_transfer_message(&JettonTransferParams {
            jetton_wallet: jw,
            destination: dest,
            response_destination: self_addr,
            jetton_amount,
            query_id: now_unix_u64(),
            attached_ton_nano: DEFAULT_JETTON_GAS_NANO,
            forward_ton_nano: DEFAULT_JETTON_FORWARD_NANO,
            forward_comment: (!comment.is_empty()).then(|| comment.to_owned()),
        })
        .map_err(|e| e.to_string())?;
        let fee_nano = self.estimate_transfer_fee(&[msg]);
        let needed_ton = DEFAULT_JETTON_GAS_NANO.saturating_add(fee_nano);
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
            fee_nano,
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
        let toncenter = self
            .toncenter
            .as_ref()
            .ok_or_else(|| "Toncenter endpoint is not configured".to_owned())?;
        let kp = self.load_keypair()?;
        let self_addr = contract.address().map_err(|e| e.to_string())?;
        let now = now_unix_u64();
        let msg = build_jetton_transfer_message(&JettonTransferParams {
            jetton_wallet: quote.jetton_wallet,
            destination: quote.destination,
            response_destination: self_addr,
            jetton_amount: quote.jetton_amount,
            query_id: now,
            attached_ton_nano: quote.attached_ton_nano,
            forward_ton_nano: DEFAULT_JETTON_FORWARD_NANO,
            forward_comment: (!quote.comment.is_empty()).then(|| quote.comment.clone()),
        })
        .map_err(|e| e.to_string())?;
        let valid_until = now.saturating_add(300) as u32;
        let signed = contract
            .create_signed_transfer(&kp, self.seqno, valid_until, &[msg])
            .map_err(|e| e.to_string())?;

        let api = Arc::clone(toncenter);
        let boc_to_send = signed.boc_base64.clone();
        let send_res =
            block_on_runtime(
                &self.rt_handle,
                async move { api.send_boc(&boc_to_send).await },
            )
            .map_err(|e| e.to_string())?;
        if !send_res.ok {
            return Err("Toncenter rejected signed Jetton transfer BOC".to_owned());
        }

        let hash_hex = send_res
            .hash
            .filter(|h| !h.is_empty())
            .unwrap_or_else(|| encode_hex(&signed.message_hash));
        let jw_raw = quote.jetton_wallet.to_raw();
        if let Some(token) = self
            .jettons
            .iter_mut()
            .find(|t| t.wallet_address.eq_ignore_ascii_case(&jw_raw))
        {
            token.balance = token.balance.saturating_sub(quote.jetton_amount);
        }
        self.balance_nano = self
            .balance_nano
            .saturating_sub(quote.attached_ton_nano.saturating_add(quote.fee_nano));
        self.seqno = self.seqno.saturating_add(1);
        self.account_state = AccountState::Active;
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
                timestamp: now as i64,
                status: WalletTxStatus::Pending,
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
        let contract = self
            .contract
            .as_ref()
            .ok_or_else(|| "No wallet configured".to_owned())?;
        let item = self
            .collectibles
            .iter()
            .find(|n| n.address.eq_ignore_ascii_case(nft_address))
            .ok_or_else(|| "Collectible NFT not found in wallet".to_owned())?;
        if comment.len() > 256 {
            return Err("Comment exceeds 256-byte limit".to_owned());
        }
        let nft_addr = TonAddress::parse(&item.address)
            .map_err(|e| e.to_string())?
            .address;
        let new_owner = TonAddress::parse(new_owner_input)
            .map_err(|e| e.to_string())?
            .address;
        let self_addr = contract.address().map_err(|e| e.to_string())?;
        let msg = build_nft_transfer_message(&NftTransferParams {
            nft_item: nft_addr,
            new_owner,
            response_destination: self_addr,
            query_id: now_unix_u64(),
            attached_ton_nano: DEFAULT_NFT_GAS_NANO,
            forward_ton_nano: DEFAULT_NFT_FORWARD_NANO,
            forward_comment: (!comment.is_empty()).then(|| comment.to_owned()),
        })
        .map_err(|e| e.to_string())?;
        let fee_nano = self.estimate_transfer_fee(&[msg]);
        let needed_ton = DEFAULT_NFT_GAS_NANO.saturating_add(fee_nano);
        if needed_ton > self.balance_nano {
            return Err("Insufficient TON balance for NFT transfer gas".to_owned());
        }
        Ok(NftTransferQuote {
            nft_address: nft_addr,
            nft_name: nft_display_title(item),
            collection_name: item.collection_name.clone(),
            new_owner,
            attached_ton_nano: DEFAULT_NFT_GAS_NANO,
            fee_nano,
            comment: comment.to_owned(),
        })
    }

    fn confirm_nft_transfer(&mut self, quote: &NftTransferQuote) -> Result<SendReceipt, String> {
        let contract = self
            .contract
            .as_ref()
            .ok_or_else(|| "No wallet configured".to_owned())?;
        let toncenter = self
            .toncenter
            .as_ref()
            .ok_or_else(|| "Toncenter endpoint is not configured".to_owned())?;
        let kp = self.load_keypair()?;
        let self_addr = contract.address().map_err(|e| e.to_string())?;
        let now = now_unix_u64();
        let msg = build_nft_transfer_message(&NftTransferParams {
            nft_item: quote.nft_address,
            new_owner: quote.new_owner,
            response_destination: self_addr,
            query_id: now,
            attached_ton_nano: quote.attached_ton_nano,
            forward_ton_nano: DEFAULT_NFT_FORWARD_NANO,
            forward_comment: (!quote.comment.is_empty()).then(|| quote.comment.clone()),
        })
        .map_err(|e| e.to_string())?;
        let valid_until = now.saturating_add(300) as u32;
        let signed = contract
            .create_signed_transfer(&kp, self.seqno, valid_until, &[msg])
            .map_err(|e| e.to_string())?;

        let api = Arc::clone(toncenter);
        let boc_to_send = signed.boc_base64.clone();
        let send_res =
            block_on_runtime(
                &self.rt_handle,
                async move { api.send_boc(&boc_to_send).await },
            )
            .map_err(|e| e.to_string())?;
        if !send_res.ok {
            return Err("Toncenter rejected signed NFT transfer BOC".to_owned());
        }

        let hash_hex = send_res
            .hash
            .filter(|h| !h.is_empty())
            .unwrap_or_else(|| encode_hex(&signed.message_hash));
        let raw_nft = quote.nft_address.to_raw();
        self.collectibles
            .retain(|n| !n.address.eq_ignore_ascii_case(&raw_nft));
        self.balance_nano = self
            .balance_nano
            .saturating_sub(quote.attached_ton_nano.saturating_add(quote.fee_nano));
        self.seqno = self.seqno.saturating_add(1);
        self.account_state = AccountState::Active;
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
                timestamp: now as i64,
                status: WalletTxStatus::Pending,
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
            .onramp_provider
            .as_ref()
            .ok_or_else(|| "Onramp handoff provider is not configured".to_owned())?;
        let wallet_address = contract
            .address()
            .map_err(|e| e.to_string())?
            .to_friendly(false, self.app_config.testnet);
        let provider_ref = Arc::clone(provider);
        let pid_opt = (!provider_id.trim().is_empty()).then(|| provider_id.to_owned());
        let cur_opt = base_currency
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToOwned::to_owned);
        block_on_runtime(&self.rt_handle, async move {
            provider_ref
                .create_handoff(wallet_address, pid_opt, cur_opt)
                .await
        })
    }

    fn toncenter_transport_kind(&self) -> Option<ToncenterTransportKind> {
        self.toncenter_transport_kind
    }

    fn cloud_backup_status(&self) -> CloudBackupStatus {
        self.cloud_backup
    }

    fn enable_cloud_backup(&mut self) -> Result<CloudBackupStatus, String> {
        let mtproto = self
            .mtproto
            .clone()
            .ok_or_else(|| "MTProto wallet session is not attached".to_owned())?;
        let words = self.reveal_phrase()?;
        let state = block_on_runtime(&self.rt_handle, async move {
            mtproto
                .enable_cloud_backup_from_words(&words, None, None)
                .await
                .map_err(|e| e.to_string())
        })?;
        self.apply_server_wallet_state(&state);
        self.cloud_backup.backup_enabled = true;
        Ok(self.cloud_backup)
    }

    fn disable_cloud_backup(&mut self, _update_phrase: bool) -> Result<CloudBackupStatus, String> {
        let mtproto = self
            .mtproto
            .clone()
            .ok_or_else(|| "MTProto wallet session is not attached".to_owned())?;
        let state = block_on_runtime(&self.rt_handle, async move {
            mtproto
                .disable_backup(DisableBackupRequest {
                    password: None,
                    new_public_key: None,
                    proof: None,
                })
                .await
                .map_err(|e| e.to_string())
        })?;
        self.apply_server_wallet_state(&state);
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
        let mtproto = self
            .mtproto
            .clone()
            .ok_or_else(|| "MTProto wallet session is not attached".to_owned())?;
        let stickers_owned = stickers.to_vec();
        let bg_owned = bg_nft_address.map(str::to_owned);
        let card_info = block_on_runtime(&self.rt_handle, async move {
            mtproto
                .set_card_stickers(&stickers_owned, bg_owned.as_deref())
                .await
                .map_err(|e| e.to_string())
        })?;
        self.card_state = wallet_card_state_from_info(&card_info);
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
        let mtproto = self
            .mtproto
            .clone()
            .ok_or_else(|| "MTProto wallet session is not attached".to_owned())?;
        let req = DepositStarGiftRequest {
            hide_sender,
            peer: tl_enums::InputPeer::Self_,
            slug: slug.to_owned(),
        };
        let deposit = block_on_runtime(&self.rt_handle, async move {
            mtproto
                .deposit_star_gift(req)
                .await
                .map_err(|e| e.to_string())
        })?;
        let verified = deposit
            .verify_for_collectible(nft_address)
            .map_err(|e| e.to_string())?;
        let amount_nano = u64::try_from(verified.amount_nano).unwrap_or(DEFAULT_NFT_GAS_NANO);
        let nft_ton = TonAddress::parse(&verified.collectible_nft)
            .map_err(|e| e.to_string())?
            .address;
        let payload_cell = deserialize_boc_base64(&verified.payload_base64)
            .map_err(|e| format!("Invalid deposit payload BOC: {e}"))?;
        let msg = TransferMessage {
            destination: nft_ton,
            amount_nano,
            bounce: verified.bounce,
            send_mode: DEFAULT_SEND_MODE,
            body: Some(payload_cell),
            state_init: None,
        };
        let fee_nano = self.estimate_transfer_fee(&[msg]);
        let title = self
            .collectibles
            .iter()
            .find(|n| n.address.eq_ignore_ascii_case(&verified.collectible_nft))
            .map_or_else(|| slug.to_owned(), nft_display_title);
        Ok(MoveToGiftsQuote {
            nft_address: verified.collectible_nft,
            slug: slug.to_owned(),
            title,
            escrow_address: verified.destination_escrow,
            payload_base64: verified.payload_base64,
            amount_nano,
            fee_nano,
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
        let toncenter = self
            .toncenter
            .as_ref()
            .ok_or_else(|| "Toncenter transport is not configured".to_owned())?;
        let kp = self.load_keypair()?;
        let nft_addr = TonAddress::parse(&quote.nft_address)
            .map_err(|e| e.to_string())?
            .address;
        let body_cell = deserialize_boc_base64(&quote.payload_base64)
            .map_err(|e| format!("Invalid deposit payload BOC: {e}"))?;
        let msg = TransferMessage {
            destination: nft_addr,
            amount_nano: quote.amount_nano,
            bounce: true,
            send_mode: DEFAULT_SEND_MODE,
            body: Some(body_cell),
            state_init: None,
        };
        let now = now_unix_u64();
        let valid_until = now.saturating_add(300) as u32;
        let signed = contract
            .create_signed_transfer(&kp, self.seqno, valid_until, &[msg])
            .map_err(|e| e.to_string())?;

        let api = Arc::clone(toncenter);
        let boc_to_send = signed.boc_base64.clone();
        let send_res =
            block_on_runtime(
                &self.rt_handle,
                async move { api.send_boc(&boc_to_send).await },
            )
            .map_err(|e| e.to_string())?;
        if !send_res.ok {
            return Err("Toncenter rejected signed Move-to-Gifts deposit BOC".to_owned());
        }

        let mut shown_in_gifts = false;
        if also_display_in_gifts && let Some(mtproto) = self.mtproto.clone() {
            let slug = quote.slug.clone();
            if block_on_runtime(&self.rt_handle, async move {
                mtproto
                    .display_star_gift(&slug, Some(tl_enums::InputPeer::Self_))
                    .await
            })
            .is_ok()
            {
                shown_in_gifts = true;
            }
        }

        let hash_hex = send_res
            .hash
            .filter(|h| !h.is_empty())
            .unwrap_or_else(|| encode_hex(&signed.message_hash));
        let raw_nft = nft_addr.to_raw();
        self.collectibles
            .retain(|n| !n.address.eq_ignore_ascii_case(&raw_nft));
        self.balance_nano = self
            .balance_nano
            .saturating_sub(quote.amount_nano.saturating_add(quote.fee_nano));
        self.seqno = self.seqno.saturating_add(1);
        Ok(MoveToGiftsReceipt {
            nft_address: raw_nft,
            slug: quote.slug.clone(),
            message_hash_hex: hash_hex,
            shown_in_gifts,
        })
    }

    fn display_star_gift(&mut self, nft_address: &str, display: bool) -> Result<(), String> {
        let mtproto = self
            .mtproto
            .clone()
            .ok_or_else(|| "MTProto wallet session is not attached".to_owned())?;
        let slug = self
            .collectibles
            .iter()
            .find(|n| n.address.eq_ignore_ascii_case(nft_address))
            .and_then(|n| {
                let candidate = n.name.trim().replace(' ', "").replace('#', "-");
                (!candidate.is_empty()).then_some(candidate)
            })
            .unwrap_or_else(|| nft_address.to_owned());
        let peer = display.then_some(tl_enums::InputPeer::Self_);
        block_on_runtime(&self.rt_handle, async move {
            mtproto
                .display_star_gift(&slug, peer)
                .await
                .map(|_| ())
                .map_err(|e| e.to_string())
        })
    }
}

fn wallet_card_state_from_info(info: &WalletCardInfo) -> WalletCardState {
    WalletCardState {
        stickers: info
            .stickers
            .iter()
            .map(|s| WalletCardStickerEntry {
                nft_address: s.nft.item.address.clone(),
                title: nft_display_title(&s.nft.item),
                collection_name: s.nft.item.collection_name.clone(),
                coordinates: s.coordinates,
            })
            .collect(),
        background_address: info.background_address.clone(),
        ready: info.stickers_complete,
    }
}

// ============================================================================
// Live host integration glue (called from minimal call lines in `live/host.rs`)
// ============================================================================

fn build_http_toncenter(cfg: &WalletAppConfig) -> Option<Arc<dyn ToncenterApi>> {
    if cfg.toncenter_base_url.trim().is_empty() {
        return None;
    }
    let mut tc_cfg = ToncenterConfig::new(cfg.toncenter_base_url.clone());
    tc_cfg.api_key = cfg.toncenter_api_key.clone();
    HttpToncenterClient::new(tc_cfg)
        .ok()
        .map(|c| Arc::new(c) as Arc<dyn ToncenterApi>)
}

fn build_http_bridge(cfg: &WalletAppConfig) -> Option<Arc<dyn TonConnectBridge>> {
    if cfg.tonconnect_bridge_url.trim().is_empty() {
        return None;
    }
    HttpTonConnectBridge::new(TonConnectBridgeConfig::new(
        cfg.tonconnect_bridge_url.clone(),
    ))
    .ok()
    .map(|b| Arc::new(b) as Arc<dyn TonConnectBridge>)
}

fn now_unix_u64() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn encode_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        let _ = write!(&mut out, "{b:02x}");
    }
    out
}

fn decode_hex_vec(hex_str: &str) -> Option<Vec<u8>> {
    let trimmed = hex_str.trim();
    if !trimmed.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(trimmed.len() / 2);
    for pair in trimmed.as_bytes().as_chunks::<2>().0 {
        let s = std::str::from_utf8(pair).ok()?;
        let byte = u8::from_str_radix(s, 16).ok()?;
        out.push(byte);
    }
    Some(out)
}

fn decode_hex_32(hex_str: &str) -> Option<[u8; 32]> {
    decode_hex_vec(hex_str)?.try_into().ok()
}

fn validate_handoff_url(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("Handoff URL is empty".to_owned());
    }
    let parsed = reqwest::Url::parse(trimmed).map_err(|e| format!("Invalid handoff URL: {e}"))?;
    if parsed.scheme() != "https" {
        return Err(format!(
            "Refusing non-HTTPS handoff URL scheme: {}",
            parsed.scheme()
        ));
    }
    if parsed.host_str().is_none_or(str::is_empty) {
        return Err("Handoff URL is missing host".to_owned());
    }
    Ok(parsed.to_string())
}
