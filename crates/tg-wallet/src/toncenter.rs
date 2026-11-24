//! Toncenter HTTP API client behind a trait + tdesktop `gram/api/*` request builders and parsers.
//!
//! Port origin: `Telegram/SourceFiles/gram/api/gram_api_{request,account,emulate,nft,rates,stream}.cpp`.
//!
//! Endpoint URLs are always supplied via [`ToncenterConfig`]; tests run strictly against a local
//! in-process fake HTTP server and fixture JSON payloads.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use url::Url;

use crate::WalletError;

/// Boxed async future helper for object-safe [`ToncenterApi`] trait methods.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Toncenter HTTP client configuration.
#[derive(Debug, Clone)]
pub struct ToncenterConfig {
    /// Base URL (e.g. `http://127.0.0.1:12345` in tests, or configured provider endpoint).
    pub base_url: String,
    /// Optional `X-API-Key` header value.
    pub api_key: Option<String>,
    /// Per-request HTTP timeout.
    pub timeout: Duration,
}

impl ToncenterConfig {
    /// Creates a config for the given `base_url` with a 15-second default timeout.
    #[must_use]
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            api_key: None,
            timeout: Duration::from_secs(15),
        }
    }
}

/// Abstract HTTP request descriptor (`Gram::HttpRequest` in `gram_api_request.h`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GramHttpRequest {
    /// `true` for POST, `false` for GET.
    pub post: bool,
    /// Relative endpoint path (e.g. `/api/v2/getAddressInformation`).
    pub endpoint: String,
    /// Percent-encoded query string without leading `?`.
    pub query: String,
    /// Optional JSON request body for POST requests.
    pub payload: Vec<u8>,
}

/// Provider API error (`Gram::ApiError` in `gram_api_request.h`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GramApiError {
    /// Provider status or RPC error code.
    pub code: i32,
    /// Human-readable diagnostic message.
    pub message: String,
}

/// On-chain account lifecycle status (`Wallet::AccountStatus`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum AccountState {
    /// Account has never been deployed or received funds.
    #[default]
    Uninitialized,
    /// Contract is deployed and active.
    Active,
    /// Contract is frozen due to storage fees.
    Frozen,
}

/// Account balance & state from `/api/v2/getAddressInformation`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddressInfo {
    /// Balance in nanoTON.
    pub balance_nano: u64,
    /// Contract state (`uninitialized`, `active`, `frozen`).
    pub state: AccountState,
    /// Logical time of the last transaction (`0` if never used).
    pub last_tx_lt: u64,
    /// Hash of the last transaction (base64 or hex), if any.
    pub last_tx_hash: String,
    /// Whether the account has never transacted (`lt == 0`).
    pub never_used: bool,
}

/// Single transaction summary returned by `/api/v2/getTransactions`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TonTransaction {
    /// Logical time of the transaction.
    pub lt: u64,
    /// Transaction hash (base64 or hex).
    pub hash: String,
    /// Unix timestamp (seconds).
    pub utime: i64,
    /// Total fee paid in nanoTON.
    pub fee_nano: u64,
    /// `true` for incoming transfer, `false` for outgoing transfer.
    pub incoming: bool,
    /// Counterparty address string.
    pub counterparty: String,
    /// Magnitude of transferred TON in nanoTON.
    pub amount_nano: u64,
    /// Optional decoded text comment.
    pub comment: String,
}

/// Result of submitting a signed external BOC via `/api/v2/sendBoc` or JSON-RPC `sendBoc`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendBocResult {
    /// Whether the node accepted the message.
    pub ok: bool,
    /// Optional message hash returned by `sendBocReturnHash`.
    pub hash: Option<String>,
}

/// Result of calling `/api/v2/runGetMethod`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetMethodResult {
    /// TVM exit code (`0` = success).
    pub exit_code: i32,
    /// Raw JSON stack items returned by toncenter.
    pub stack: Vec<Value>,
}

impl GetMethodResult {
    /// Extracts the first integer stack entry (e.g. `seqno` from `["num", "0x5"]`).
    #[must_use]
    pub fn first_u64(&self) -> Option<u64> {
        let pair = self.stack.first()?.as_array()?;
        if pair.len() < 2 || pair[0].as_str() != Some("num") {
            return None;
        }
        let s = pair[1].as_str()?;
        if let Some(hex_str) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
            u64::from_str_radix(hex_str, 16).ok()
        } else {
            s.parse::<u64>().ok()
        }
    }
}

/// Fee estimation result from `/api/emulate/v1/emulateTrace` (`Gram::EmulatedTrace`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmulatedTrace {
    /// Emulated total fee in nanoTON.
    pub fee_nano: i64,
    /// Root account raw address from the trace.
    pub account: String,
}

/// NFT classification matching `Gram::NftKind` (`gram_api_nft.h`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum NftKind {
    /// Standard TON NFT.
    #[default]
    Generic,
    /// Telegram collectible gift.
    TelegramGift,
    /// Fragment anonymous phone number (`0:0e41dc1d...`).
    TelegramNumber,
    /// Fragment collectible username (`0:80d78a35...`).
    TelegramUsername,
}

/// Parsed NFT collectible item (`Gram::NftItem`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct NftItem {
    /// Canonical lowercase raw address `workchain:64-hex`.
    pub address: String,
    /// Collection canonical raw address (empty if none).
    pub collection: String,
    /// Owner canonical raw address (`real_owner` or `owner_address`).
    pub owner: String,
    /// NFT index within collection.
    pub index: String,
    /// Item display name.
    pub name: String,
    /// Collection display name.
    pub collection_name: String,
    /// Off-chain metadata URI (`content.uri`), if present.
    #[serde(default)]
    pub content_uri: String,
    /// Domain name (`content.domain`, e.g. `"example.t.me"` or `"example.ton"`), if present.
    #[serde(default)]
    pub domain: String,
    /// Classified Fragment slug/key (e.g. username or anonymous number digits).
    #[serde(default)]
    pub key: String,
    /// Whether the NFT is currently listed for sale (`on_sale`).
    #[serde(default)]
    pub on_sale: bool,
    /// Classified Fragment / Telegram kind.
    pub kind: NftKind,
}

/// Page of NFTs (`Gram::NftPage`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct NftPage {
    /// Items on this page.
    pub list: Vec<NftItem>,
    /// Whether more items may exist.
    pub has_next: bool,
}

/// Fiat conversion table (`Gram::CurrencyRates` in `gram_api_rates.cpp`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CurrencyRates {
    /// Map of uppercase ISO currency code -> rate per 1 USD.
    pub values: HashMap<String, f64>,
}

impl CurrencyRates {
    /// Builds a validated [`CurrencyRates`] map ignoring non-finite or non-positive entries.
    #[must_use]
    pub fn from_entries(entries: &[(&str, f64)]) -> Option<Self> {
        let mut values = HashMap::new();
        for &(code, rate) in entries {
            if rate.is_finite() && rate > 0.0 {
                values.insert(code.to_ascii_uppercase(), rate);
            }
        }
        if values.is_empty() {
            None
        } else {
            Some(Self { values })
        }
    }

    /// Computes fiat price per 1 TON (`Gram::ComputePerGram` in `gram_api_rates.cpp`).
    #[must_use]
    pub fn compute_per_ton(&self, currency_code: &str, usd_per_ton: f64) -> Option<f64> {
        if !usd_per_ton.is_finite() || usd_per_ton <= 0.0 {
            return None;
        }
        let rate = *self.values.get(&currency_code.to_ascii_uppercase())?;
        let product = rate * usd_per_ton;
        (product.is_finite() && product > 0.0).then_some(product)
    }
}

/// Parsed WebSocket streaming endpoint (`Gram::StreamEndpoint` in `gram_api_stream.h`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamEndpoint {
    /// Hostname.
    pub host: String,
    /// Encoded path + optional `?query`.
    pub request_target: String,
    /// Port (defaults to 443).
    pub port: u16,
}

/// Streaming URL returned by `toncenter.getStreamingUrl` (`wallet_api.cpp:109-153`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamingUrlInfo {
    /// Raw streaming URL returned by Telegram (`wss://...` or `https://...`).
    pub url: String,
    /// Expiration timestamp or relative TTL in seconds (`expires`).
    pub expires: i32,
    /// Parsed WebSocket endpoint if `url` is valid.
    pub endpoint: Option<StreamEndpoint>,
}

/// Streaming event kind (`Gram::StreamEventKind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StreamEventKind {
    /// Unrecognized frame.
    #[default]
    Unknown,
    /// `account_state_change` notification.
    AccountState,
    /// `transactions` notification.
    Transactions,
    /// `trace_invalidated` notification.
    TraceInvalidated,
}

/// Parsed streaming frame (`Gram::StreamEvent`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StreamEvent {
    /// Event kind.
    pub kind: StreamEventKind,
    /// Deduplicated account strings referenced by the frame.
    pub accounts: Vec<String>,
}

/// Parses a provider JSON error body (`Gram::ParseApiError` in `gram_api_request.cpp`).
#[must_use]
pub fn parse_api_error(json_bytes: &[u8]) -> Option<GramApiError> {
    let val: Value = serde_json::from_slice(json_bytes).ok()?;
    let obj = val.as_object()?;
    let msg = obj.get("error")?.as_str()?;
    Some(GramApiError {
        code: obj.get("code").and_then(Value::as_i64).unwrap_or(0) as i32,
        message: msg.to_owned(),
    })
}

/// Builds the GET `/api/v2/getAddressInformation` request (`Gram::AddressInformationRequest`).
#[must_use]
pub fn address_information_request(address: &str) -> GramHttpRequest {
    let encoded: String = url::form_urlencoded::byte_serialize(address.as_bytes()).collect();
    GramHttpRequest {
        post: false,
        endpoint: "/api/v2/getAddressInformation".to_owned(),
        query: format!("address={encoded}"),
        payload: Vec::new(),
    }
}

fn parse_non_negative_u64(val: &Value) -> Option<u64> {
    if let Some(s) = val.as_str() {
        s.parse::<u64>().ok()
    } else if let Some(u) = val.as_u64() {
        Some(u)
    } else if let Some(f) = val.as_f64() {
        if f.is_finite() && f >= 0.0 && f <= ((1_u64 << 53) as f64) && f.fract() == 0.0 {
            Some(f as u64)
        } else {
            None
        }
    } else {
        None
    }
}

/// Parses `/api/v2/getAddressInformation` JSON (`Gram::ParseAddressFunds`).
#[must_use]
pub fn parse_address_info(json_bytes: &[u8]) -> Option<AddressInfo> {
    let val: Value = serde_json::from_slice(json_bytes).ok()?;
    let obj = val.as_object()?;
    if obj.get("ok").and_then(Value::as_bool) != Some(true) {
        return None;
    }
    let result = obj.get("result")?.as_object()?;
    let balance_nano = parse_non_negative_u64(result.get("balance")?)?;
    let state = match result.get("state").and_then(Value::as_str) {
        Some("active") => AccountState::Active,
        Some("frozen") => AccountState::Frozen,
        _ => AccountState::Uninitialized,
    };
    let last = result.get("last_transaction_id").and_then(Value::as_object);
    let last_tx_lt = last
        .and_then(|l| l.get("lt"))
        .and_then(parse_non_negative_u64)
        .unwrap_or(0);
    let last_tx_hash = last
        .and_then(|l| l.get("hash"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();

    Some(AddressInfo {
        balance_nano,
        state,
        last_tx_lt,
        last_tx_hash,
        never_used: last_tx_lt == 0,
    })
}

/// Builds POST `/api/emulate/v1/emulateTrace` request (`Gram::EmulateTraceRequest`).
#[must_use]
pub fn emulate_trace_request(boc_base64: &str) -> GramHttpRequest {
    let body = json!({
        "boc": boc_base64,
        "ignore_chksig": true,
        "include_code_data": false,
        "include_address_book": false,
        "include_metadata": false,
        "with_actions": true,
        "mc_block_seqno": Value::Null,
    });
    GramHttpRequest {
        post: true,
        endpoint: "/api/emulate/v1/emulateTrace".to_owned(),
        query: String::new(),
        payload: serde_json::to_vec(&body).unwrap_or_default(),
    }
}

/// Parses `/api/emulate/v1/emulateTrace` JSON (`Gram::ParseEmulatedTrace`).
#[must_use]
pub fn parse_emulated_trace(json_bytes: &[u8]) -> Option<EmulatedTrace> {
    let val: Value = serde_json::from_slice(json_bytes).ok()?;
    let obj = val.as_object()?;
    let tx_hash = obj.get("trace")?.as_object()?.get("tx_hash")?.as_str()?;
    if tx_hash.is_empty() {
        return None;
    }
    let root = obj
        .get("transactions")?
        .as_object()?
        .get(tx_hash)?
        .as_object()?;
    let account = root.get("account")?.as_str()?;
    if account.is_empty() {
        return None;
    }
    let fee_nano: i64 = root.get("total_fees")?.as_str()?.parse().ok()?;
    if fee_nano < 0 {
        return None;
    }
    let desc = root.get("description")?.as_object()?;
    if desc.get("aborted")?.as_bool()? {
        return None;
    }
    let compute = desc.get("compute_ph")?.as_object()?;
    let compute_ok =
        compute.get("success")?.as_bool()? && matches!(compute.get("exit_code")?.as_i64()?, 0 | 1);
    let action = desc.get("action")?.as_object()?;
    let action_ok = action.get("success")?.as_bool()? && action.get("result_code")?.as_i64()? == 0;
    if !compute_ok || !action_ok {
        return None;
    }
    Some(EmulatedTrace {
        fee_nano,
        account: account.to_owned(),
    })
}

fn extract_fragment_slug(uri: &str, segment: &str) -> String {
    let prefix = format!("https://nft.fragment.com/{segment}/");
    let Some(rest) = uri.strip_prefix(&prefix) else {
        return String::new();
    };
    let slug = rest.strip_suffix(".json").unwrap_or(rest);
    if slug.is_empty()
        || !slug
            .chars()
            .all(|ch| ch == '.' || ch == '_' || ch == '-' || ch.is_ascii_alphanumeric())
    {
        return String::new();
    }
    slug.to_owned()
}

/// Classifies an [`NftItem`] by known Fragment collection addresses (`Gram::ClassifyNftKind`).
pub fn classify_nft_kind(item: &mut NftItem) {
    const FRAGMENT_NUMBERS: &str =
        "0:0e41dc1dc3c9067ed24248580e12b3359818d83dee0304fabcf80845eafafdb2";
    const FRAGMENT_USERNAMES: &str =
        "0:80d78a35f955a14b679faa887ff4cd5bfc0f43b4a4eea2a7e6927f3701b273c2";

    let coll = item.collection.to_ascii_lowercase();
    if coll == FRAGMENT_NUMBERS {
        item.kind = NftKind::TelegramNumber;
        if item.key.is_empty() {
            item.key = extract_fragment_slug(&item.content_uri, "number");
        }
        if item.key.is_empty() && item.name.starts_with('+') {
            item.key = item.name.chars().filter(char::is_ascii_digit).collect();
        }
    } else if coll == FRAGMENT_USERNAMES {
        item.kind = NftKind::TelegramUsername;
        if item.key.is_empty() {
            item.key = extract_fragment_slug(&item.content_uri, "username");
        }
        if item.key.is_empty() && !item.domain.is_empty() {
            item.key = item
                .domain
                .strip_suffix(".t.me")
                .unwrap_or(&item.domain)
                .to_owned();
        }
        if item.key.is_empty() && !item.name.is_empty() {
            item.key = item.name.trim_start_matches('@').to_owned();
        }
    } else if item
        .collection_name
        .to_ascii_lowercase()
        .contains("telegram gift")
        || item
            .content_uri
            .starts_with("https://nft.fragment.com/gift/")
    {
        item.kind = NftKind::TelegramGift;
        if item.key.is_empty() {
            item.key = extract_fragment_slug(&item.content_uri, "gift");
        }
    }
}

/// Parses Toncenter v3 `/api/v3/nft/items` JSON (`Gram::ParseNftItems`).
#[must_use]
pub fn parse_nft_items(json_bytes: &[u8], limit: usize) -> Option<NftPage> {
    if parse_api_error(json_bytes).is_some() {
        return None;
    }
    let val: Value = serde_json::from_slice(json_bytes).ok()?;
    let items = val.as_object()?.get("nft_items")?.as_array()?;
    let mut list = Vec::with_capacity(items.len());
    for entry in items {
        let obj = entry.as_object()?;
        let address = obj
            .get("address")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_ascii_lowercase();
        if address.is_empty() {
            continue;
        }
        let collection = obj
            .get("collection_address")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_ascii_lowercase();
        let owner = obj
            .get("real_owner")
            .and_then(Value::as_str)
            .or_else(|| obj.get("owner_address").and_then(Value::as_str))
            .unwrap_or("")
            .to_ascii_lowercase();
        let index = obj
            .get("index")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let content = obj.get("content").and_then(Value::as_object);
        let content_uri = content
            .and_then(|c| c.get("uri"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let domain = content
            .and_then(|c| c.get("domain"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let name = content
            .and_then(|c| c.get("name"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let collection_name = obj
            .get("collection")
            .and_then(|c| c.get("name"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let on_sale = obj.get("on_sale").and_then(Value::as_bool).unwrap_or(false);
        let mut item = NftItem {
            address,
            collection,
            owner,
            index,
            name,
            collection_name,
            content_uri,
            domain,
            key: String::new(),
            on_sale,
            kind: NftKind::Generic,
        };
        classify_nft_kind(&mut item);
        list.push(item);
    }
    let has_next = limit > 0 && list.len() >= limit;
    Some(NftPage { list, has_next })
}

/// Parses a `wss://` or `https://` Toncenter streaming URL (`Gram::ParseStreamEndpoint`).
#[must_use]
pub fn parse_stream_endpoint(url_str: &str) -> Option<StreamEndpoint> {
    let parsed = Url::parse(url_str).ok()?;
    if parsed.scheme() != "wss" && parsed.scheme() != "https" {
        return None;
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return None;
    }
    let host = parsed.host_str()?.to_owned();
    if host.is_empty() {
        return None;
    }
    let port = parsed.port().unwrap_or(443);
    let mut request_target = if parsed.path().is_empty() {
        "/".to_owned()
    } else {
        parsed.path().to_owned()
    };
    if let Some(q) = parsed.query() {
        request_target.push('?');
        request_target.push_str(q);
    }
    Some(StreamEndpoint {
        host,
        request_target,
        port,
    })
}

/// Parses a JSON streaming frame (`Gram::ParseStreamEvent`).
#[must_use]
pub fn parse_stream_event(frame: &[u8]) -> StreamEvent {
    let Ok(Value::Object(obj)) = serde_json::from_slice::<Value>(frame) else {
        return StreamEvent::default();
    };
    let Some(kind) = obj.get("type").and_then(Value::as_str) else {
        return StreamEvent::default();
    };
    match kind {
        "account_state_change" => {
            let Some(account) = obj.get("account").and_then(Value::as_str) else {
                return StreamEvent::default();
            };
            let has_balance = obj
                .get("state")
                .and_then(Value::as_object)
                .and_then(|s| s.get("balance"))
                .and_then(Value::as_str)
                .is_some();
            if account.is_empty() || !has_balance {
                return StreamEvent::default();
            }
            StreamEvent {
                kind: StreamEventKind::AccountState,
                accounts: vec![account.to_owned()],
            }
        }
        "transactions" => {
            if obj
                .get("trace_external_hash_norm")
                .and_then(Value::as_str)
                .is_none()
            {
                return StreamEvent::default();
            }
            let Some(txs) = obj.get("transactions").and_then(Value::as_array) else {
                return StreamEvent::default();
            };
            let mut accounts = Vec::new();
            for tx in txs {
                if let Some(acc) = tx.get("account").and_then(Value::as_str)
                    && !acc.is_empty()
                    && !accounts.iter().any(|existing| existing == acc)
                {
                    accounts.push(acc.to_owned());
                }
            }
            StreamEvent {
                kind: StreamEventKind::Transactions,
                accounts,
            }
        }
        "trace_invalidated" => {
            if obj
                .get("trace_external_hash_norm")
                .and_then(Value::as_str)
                .is_none()
            {
                return StreamEvent::default();
            }
            StreamEvent {
                kind: StreamEventKind::TraceInvalidated,
                accounts: Vec::new(),
            }
        }
        _ => StreamEvent::default(),
    }
}

/// Builds a streaming subscribe payload (`Gram::StreamSubscribeMessage`).
#[must_use]
pub fn stream_subscribe_message(account: &str, id: u32) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "operation": "subscribe",
        "id": format!("sub-{id}"),
        "types": ["account_state_change", "transactions"],
        "addresses": [account],
        "min_finality": "pending",
    }))
    .unwrap_or_default()
}

/// Builds a streaming ping payload (`Gram::StreamPingMessage`).
#[must_use]
pub fn stream_ping_message(id: u32) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "operation": "ping",
        "id": format!("ping-{id}"),
    }))
    .unwrap_or_default()
}

/// Async Toncenter client trait so callers and tests can swap fake / in-process providers.
pub trait ToncenterApi: Send + Sync {
    /// Fetches account balance, state, and last transaction ID (`/api/v2/getAddressInformation`).
    fn get_address_information<'a>(
        &'a self,
        address: &'a str,
    ) -> BoxFuture<'a, Result<AddressInfo, WalletError>>;

    /// Fetches recent transactions for `address` (`/api/v2/getTransactions`).
    fn get_transactions<'a>(
        &'a self,
        address: &'a str,
        limit: usize,
        lt: Option<u64>,
        hash: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Vec<TonTransaction>, WalletError>>;

    /// Broadcasts a signed external BOC (`/api/v2/sendBoc`).
    fn send_boc<'a>(
        &'a self,
        boc_base64: &'a str,
    ) -> BoxFuture<'a, Result<SendBocResult, WalletError>>;

    /// Executes a TVM get-method such as `seqno` (`/api/v2/runGetMethod`).
    fn run_get_method<'a>(
        &'a self,
        address: &'a str,
        method: &'a str,
        stack: &'a [Value],
    ) -> BoxFuture<'a, Result<GetMethodResult, WalletError>>;

    /// Emulates a trace for fee estimation (`/api/emulate/v1/emulateTrace`).
    fn emulate_trace<'a>(
        &'a self,
        boc_base64: &'a str,
    ) -> BoxFuture<'a, Result<EmulatedTrace, WalletError>>;

    /// Emulates an external message BOC and returns full fees, per-account TON/Jetton/NFT
    /// balance deltas, classified actions, and aborted/bounced phase warnings
    /// (`/api/emulate/v1/emulateTrace`).
    fn emulate_message<'a>(
        &'a self,
        request: &'a crate::emulate::EmulateMessageRequest,
    ) -> BoxFuture<'a, Result<crate::emulate::EmulationResult, WalletError>> {
        Box::pin(async move {
            let trace = self.emulate_trace(&request.boc_base64).await?;
            Ok(crate::emulate::EmulationResult::from_basic_trace(trace))
        })
    }

    /// Fetches NFT collectibles owned by `owner` (`/api/v3/nft/items`).
    fn get_nft_items<'a>(
        &'a self,
        _owner: &'a str,
        _limit: usize,
        _offset: usize,
    ) -> BoxFuture<'a, Result<NftPage, WalletError>> {
        Box::pin(async { Ok(NftPage::default()) })
    }

    /// Fetches owned TEP-74 Jetton wallets for `owner` (`/api/v3/jetton/wallets`).
    fn get_jetton_wallets<'a>(
        &'a self,
        _owner: &'a str,
        _limit: usize,
        _offset: usize,
    ) -> BoxFuture<'a, Result<crate::jettons::JettonWalletsPage, WalletError>> {
        Box::pin(async { Ok(crate::jettons::JettonWalletsPage::default()) })
    }

    /// Fetches TEP-74 Jetton master metadata for `master_address` (`/api/v3/jetton/masters`).
    fn get_jetton_masters<'a>(
        &'a self,
        _master_address: &'a str,
    ) -> BoxFuture<'a, Result<Vec<crate::jettons::JettonMasterInfo>, WalletError>> {
        Box::pin(async { Ok(Vec::new()) })
    }
}

/// Low-level transport executing [`GramHttpRequest`] descriptors against Toncenter.
///
/// Mirrors tdesktop's `Wallet::Api` / `Gram::HttpRequest` boundary so `tg-wallet` can run
/// identically over Telegram's native MTProto proxy (`toncenter.performApiRequest` /
/// `toncenter.getStreamingUrl` on `shift_dc_id(webfile_dc_id, TONCENTER_DC_SHIFT)`) or over
/// direct HTTP (`HttpToncenterTransport`).
pub trait ToncenterTransport: Send + Sync {
    /// Executes a [`GramHttpRequest`] and returns the raw UTF-8 JSON response body.
    fn execute<'a>(
        &'a self,
        request: GramHttpRequest,
    ) -> BoxFuture<'a, Result<Vec<u8>, WalletError>>;

    /// Requests a WebSocket streaming URL (`toncenter.getStreamingUrl`).
    fn get_streaming_url<'a>(&'a self) -> BoxFuture<'a, Result<StreamingUrlInfo, WalletError>> {
        Box::pin(async {
            Err(WalletError::Toncenter(
                "streaming URL requires MTProto toncenter transport".to_owned(),
            ))
        })
    }
}

impl<T: ToncenterTransport + ?Sized> ToncenterTransport for std::sync::Arc<T> {
    fn execute<'a>(
        &'a self,
        request: GramHttpRequest,
    ) -> BoxFuture<'a, Result<Vec<u8>, WalletError>> {
        (**self).execute(request)
    }

    fn get_streaming_url<'a>(&'a self) -> BoxFuture<'a, Result<StreamingUrlInfo, WalletError>> {
        (**self).get_streaming_url()
    }
}

/// Transport selection policy matching tdesktop (`wallet_api.cpp` + `wallet_engine.cpp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ToncenterTransportPolicy {
    /// Default tdesktop behavior: use Telegram's native MTProto `toncenter.*` proxy unless
    /// an explicit HTTP `toncenter_base_url` override is configured.
    #[default]
    AutoPreferMtproto,
    /// Always use Telegram's native MTProto `toncenter.*` proxy, ignoring any HTTP base URL.
    ForceMtproto,
    /// Always use direct HTTP (`HttpToncenterTransport`), failing if `toncenter_base_url` is empty.
    ForceHttp,
}

/// Active transport kind chosen by [`crate::mtproto_api::select_toncenter_transport`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToncenterTransportKind {
    /// Routed through Telegram MTProto `toncenter.performApiRequest` on shifted webfile DC.
    Mtproto,
    /// Direct HTTP requests to a configured Toncenter base URL.
    Http,
}

/// Direct HTTP [`ToncenterTransport`] targeting a configured `base_url`.
#[derive(Debug, Clone)]
pub struct HttpToncenterTransport {
    config: ToncenterConfig,
    http: reqwest::Client,
}

impl HttpToncenterTransport {
    /// Creates a new HTTP transport targeting `config.base_url`.
    pub fn new(config: ToncenterConfig) -> Result<Self, WalletError> {
        if config.base_url.trim().is_empty() {
            return Err(WalletError::Toncenter(
                "toncenter base_url must be configured".to_owned(),
            ));
        }
        let http = reqwest::Client::builder()
            .timeout(config.timeout)
            .build()
            .map_err(|e| WalletError::Toncenter(format!("HTTP client build failed: {e}")))?;
        Ok(Self { config, http })
    }

    /// Returns the underlying HTTP client configuration.
    #[must_use]
    pub const fn config(&self) -> &ToncenterConfig {
        &self.config
    }

    fn url(&self, endpoint: &str, query: &str) -> String {
        let base = self.config.base_url.trim_end_matches('/');
        if query.is_empty() {
            format!("{base}{endpoint}")
        } else {
            format!("{base}{endpoint}?{query}")
        }
    }

    async fn execute_http(&self, req: GramHttpRequest) -> Result<Vec<u8>, WalletError> {
        let full_url = self.url(&req.endpoint, &req.query);
        let mut builder = if req.post {
            self.http
                .post(&full_url)
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(req.payload)
        } else {
            self.http.get(&full_url)
        };
        if let Some(key) = &self.config.api_key
            && !key.is_empty()
        {
            builder = builder.header("X-API-Key", key);
        }
        let resp = builder
            .send()
            .await
            .map_err(|e| WalletError::Toncenter(format!("request failed: {e}")))?;
        let status = resp.status();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| WalletError::Toncenter(format!("response read failed: {e}")))?
            .to_vec();
        if !status.is_success() {
            if let Some(err) = parse_api_error(&bytes) {
                return Err(WalletError::Toncenter(format!(
                    "HTTP {}: {}",
                    status.as_u16(),
                    err.message
                )));
            }
            return Err(WalletError::Toncenter(format!("HTTP {}", status.as_u16())));
        }
        Ok(bytes)
    }
}

impl ToncenterTransport for HttpToncenterTransport {
    fn execute<'a>(
        &'a self,
        request: GramHttpRequest,
    ) -> BoxFuture<'a, Result<Vec<u8>, WalletError>> {
        Box::pin(self.execute_http(request))
    }
}

/// Transport-agnostic [`ToncenterApi`] client working identically over MTProto
/// ([`crate::mtproto_api::MtprotoToncenterTransport`]) or HTTP ([`HttpToncenterTransport`]).
#[derive(Debug, Clone)]
pub struct ToncenterClient<T> {
    transport: T,
}

impl<T: ToncenterTransport> ToncenterClient<T> {
    /// Wraps a [`ToncenterTransport`] implementation.
    #[must_use]
    pub const fn new(transport: T) -> Self {
        Self { transport }
    }

    /// Returns a shared reference to the underlying transport.
    #[must_use]
    pub const fn transport(&self) -> &T {
        &self.transport
    }

    /// Executes a raw [`GramHttpRequest`] through the active transport.
    pub async fn execute_raw(&self, request: GramHttpRequest) -> Result<Vec<u8>, WalletError> {
        self.transport.execute(request).await
    }

    /// Requests a WebSocket streaming URL through the active transport.
    pub async fn get_streaming_url(&self) -> Result<StreamingUrlInfo, WalletError> {
        self.transport.get_streaming_url().await
    }
}

/// Reqwest-based [`ToncenterApi`] targeting a configured `base_url`.
#[derive(Debug, Clone)]
pub struct HttpToncenterClient {
    inner: ToncenterClient<HttpToncenterTransport>,
}

impl HttpToncenterClient {
    /// Creates a new HTTP client targeting `config.base_url`.
    pub fn new(config: ToncenterConfig) -> Result<Self, WalletError> {
        Ok(Self {
            inner: ToncenterClient::new(HttpToncenterTransport::new(config)?),
        })
    }
}

fn parse_transactions_response(json_bytes: &[u8]) -> Result<Vec<TonTransaction>, WalletError> {
    let val: Value = serde_json::from_slice(json_bytes)
        .map_err(|e| WalletError::Toncenter(format!("invalid transactions JSON: {e}")))?;
    let obj = val
        .as_object()
        .ok_or_else(|| WalletError::Toncenter("expected JSON object".to_owned()))?;
    if obj.get("ok").and_then(Value::as_bool) != Some(true) {
        let msg = obj
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("toncenter error");
        return Err(WalletError::Toncenter(msg.to_owned()));
    }
    let arr = obj
        .get("result")
        .and_then(Value::as_array)
        .ok_or_else(|| WalletError::Toncenter("missing result array".to_owned()))?;

    let mut out = Vec::with_capacity(arr.len());
    for entry in arr {
        let Some(tx) = entry.as_object() else {
            continue;
        };
        let tx_id = tx.get("transaction_id").and_then(Value::as_object);
        let lt = tx_id
            .and_then(|id| id.get("lt"))
            .and_then(parse_non_negative_u64)
            .unwrap_or(0);
        let hash = tx_id
            .and_then(|id| id.get("hash"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let utime = tx.get("utime").and_then(Value::as_i64).unwrap_or(0);
        let fee_nano = tx.get("fee").and_then(parse_non_negative_u64).unwrap_or(0);

        let out_msgs = tx.get("out_msgs").and_then(Value::as_array);
        let in_msg = tx.get("in_msg").and_then(Value::as_object);

        if let Some(outs) = out_msgs
            && let Some(first_out) = outs.first().and_then(Value::as_object)
        {
            let counterparty = first_out
                .get("destination")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            let amount_nano = first_out
                .get("value")
                .and_then(parse_non_negative_u64)
                .unwrap_or(0);
            let comment = first_out
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            out.push(TonTransaction {
                lt,
                hash,
                utime,
                fee_nano,
                incoming: false,
                counterparty,
                amount_nano,
                comment,
            });
        } else if let Some(inm) = in_msg {
            let counterparty = inm
                .get("source")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            let amount_nano = inm
                .get("value")
                .and_then(parse_non_negative_u64)
                .unwrap_or(0);
            let comment = inm
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            out.push(TonTransaction {
                lt,
                hash,
                utime,
                fee_nano,
                incoming: true,
                counterparty,
                amount_nano,
                comment,
            });
        }
    }
    Ok(out)
}

impl<T: ToncenterTransport> ToncenterApi for ToncenterClient<T> {
    fn get_address_information<'a>(
        &'a self,
        address: &'a str,
    ) -> BoxFuture<'a, Result<AddressInfo, WalletError>> {
        Box::pin(async move {
            let bytes = self
                .transport
                .execute(address_information_request(address))
                .await?;
            parse_address_info(&bytes).ok_or_else(|| {
                parse_api_error(&bytes).map_or_else(
                    || WalletError::Toncenter("failed to parse address information".to_owned()),
                    |e| WalletError::Toncenter(e.message),
                )
            })
        })
    }

    fn get_transactions<'a>(
        &'a self,
        address: &'a str,
        limit: usize,
        lt: Option<u64>,
        hash: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Vec<TonTransaction>, WalletError>> {
        Box::pin(async move {
            let query = {
                let mut serializer = url::form_urlencoded::Serializer::new(String::new());
                serializer.append_pair("address", address);
                serializer.append_pair("limit", &limit.to_string());
                if let Some(lt_val) = lt {
                    serializer.append_pair("lt", &lt_val.to_string());
                }
                if let Some(h) = hash {
                    serializer.append_pair("hash", h);
                }
                serializer.finish()
            };
            let req = GramHttpRequest {
                post: false,
                endpoint: "/api/v2/getTransactions".to_owned(),
                query,
                payload: Vec::new(),
            };
            let bytes = self.transport.execute(req).await?;
            parse_transactions_response(&bytes)
        })
    }

    fn send_boc<'a>(
        &'a self,
        boc_base64: &'a str,
    ) -> BoxFuture<'a, Result<SendBocResult, WalletError>> {
        Box::pin(async move {
            let payload = serde_json::to_vec(&json!({ "boc": boc_base64 }))
                .map_err(|e| WalletError::Toncenter(format!("encode sendBoc: {e}")))?;
            let req = GramHttpRequest {
                post: true,
                endpoint: "/api/v2/sendBoc".to_owned(),
                query: String::new(),
                payload,
            };
            let bytes = self.transport.execute(req).await?;
            let val: Value = serde_json::from_slice(&bytes)
                .map_err(|e| WalletError::Toncenter(format!("decode sendBoc: {e}")))?;
            let ok = val.get("ok").and_then(Value::as_bool).unwrap_or(false);
            if !ok {
                let msg = val
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("sendBoc rejected");
                return Err(WalletError::Toncenter(msg.to_owned()));
            }
            let hash = val
                .get("result")
                .and_then(|r| r.get("hash"))
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            Ok(SendBocResult { ok, hash })
        })
    }

    fn run_get_method<'a>(
        &'a self,
        address: &'a str,
        method: &'a str,
        stack: &'a [Value],
    ) -> BoxFuture<'a, Result<GetMethodResult, WalletError>> {
        Box::pin(async move {
            let payload = serde_json::to_vec(&json!({
                "address": address,
                "method": method,
                "stack": stack,
            }))
            .map_err(|e| WalletError::Toncenter(format!("encode runGetMethod: {e}")))?;
            let req = GramHttpRequest {
                post: true,
                endpoint: "/api/v2/runGetMethod".to_owned(),
                query: String::new(),
                payload,
            };
            let bytes = self.transport.execute(req).await?;
            let val: Value = serde_json::from_slice(&bytes)
                .map_err(|e| WalletError::Toncenter(format!("decode runGetMethod: {e}")))?;
            if val.get("ok").and_then(Value::as_bool) != Some(true) {
                let msg = val
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("runGetMethod failed");
                return Err(WalletError::Toncenter(msg.to_owned()));
            }
            let result = val
                .get("result")
                .and_then(Value::as_object)
                .ok_or_else(|| WalletError::Toncenter("missing result object".to_owned()))?;
            let exit_code = result
                .get("exit_code")
                .and_then(Value::as_i64)
                .unwrap_or(-1) as i32;
            let stack_out = result
                .get("stack")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            Ok(GetMethodResult {
                exit_code,
                stack: stack_out,
            })
        })
    }

    fn emulate_trace<'a>(
        &'a self,
        boc_base64: &'a str,
    ) -> BoxFuture<'a, Result<EmulatedTrace, WalletError>> {
        Box::pin(async move {
            let bytes = self
                .transport
                .execute(emulate_trace_request(boc_base64))
                .await?;
            parse_emulated_trace(&bytes).ok_or_else(|| {
                parse_api_error(&bytes).map_or_else(
                    || WalletError::Toncenter("emulateTrace returned invalid trace".to_owned()),
                    |e| WalletError::Toncenter(e.message),
                )
            })
        })
    }

    fn emulate_message<'a>(
        &'a self,
        request: &'a crate::emulate::EmulateMessageRequest,
    ) -> BoxFuture<'a, Result<crate::emulate::EmulationResult, WalletError>> {
        Box::pin(async move {
            let bytes = self.transport.execute(request.to_http_request()).await?;
            crate::emulate::parse_emulation_response(&bytes)
        })
    }

    fn get_nft_items<'a>(
        &'a self,
        owner: &'a str,
        limit: usize,
        offset: usize,
    ) -> BoxFuture<'a, Result<NftPage, WalletError>> {
        Box::pin(async move {
            let req = crate::jettons::nft_items_by_owner_request(owner, limit, offset);
            let bytes = self.transport.execute(req).await?;
            parse_nft_items(&bytes, limit).ok_or_else(|| {
                parse_api_error(&bytes).map_or_else(
                    || WalletError::Toncenter("failed to parse /api/v3/nft/items".to_owned()),
                    |e| WalletError::Toncenter(e.message),
                )
            })
        })
    }

    fn get_jetton_wallets<'a>(
        &'a self,
        owner: &'a str,
        limit: usize,
        offset: usize,
    ) -> BoxFuture<'a, Result<crate::jettons::JettonWalletsPage, WalletError>> {
        Box::pin(async move {
            let req = crate::jettons::jetton_wallets_by_owner_request(owner, limit, offset);
            let bytes = self.transport.execute(req).await?;
            crate::jettons::parse_jetton_wallets(&bytes, limit).ok_or_else(|| {
                parse_api_error(&bytes).map_or_else(
                    || WalletError::Toncenter("failed to parse /api/v3/jetton/wallets".to_owned()),
                    |e| WalletError::Toncenter(e.message),
                )
            })
        })
    }

    fn get_jetton_masters<'a>(
        &'a self,
        master_address: &'a str,
    ) -> BoxFuture<'a, Result<Vec<crate::jettons::JettonMasterInfo>, WalletError>> {
        Box::pin(async move {
            let req = crate::jettons::jetton_masters_request(master_address);
            let bytes = self.transport.execute(req).await?;
            crate::jettons::parse_jetton_masters(&bytes).ok_or_else(|| {
                parse_api_error(&bytes).map_or_else(
                    || WalletError::Toncenter("failed to parse /api/v3/jetton/masters".to_owned()),
                    |e| WalletError::Toncenter(e.message),
                )
            })
        })
    }
}

impl ToncenterApi for HttpToncenterClient {
    fn get_address_information<'a>(
        &'a self,
        address: &'a str,
    ) -> BoxFuture<'a, Result<AddressInfo, WalletError>> {
        self.inner.get_address_information(address)
    }

    fn get_transactions<'a>(
        &'a self,
        address: &'a str,
        limit: usize,
        lt: Option<u64>,
        hash: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Vec<TonTransaction>, WalletError>> {
        self.inner.get_transactions(address, limit, lt, hash)
    }

    fn send_boc<'a>(
        &'a self,
        boc_base64: &'a str,
    ) -> BoxFuture<'a, Result<SendBocResult, WalletError>> {
        self.inner.send_boc(boc_base64)
    }

    fn run_get_method<'a>(
        &'a self,
        address: &'a str,
        method: &'a str,
        stack: &'a [Value],
    ) -> BoxFuture<'a, Result<GetMethodResult, WalletError>> {
        self.inner.run_get_method(address, method, stack)
    }

    fn emulate_trace<'a>(
        &'a self,
        boc_base64: &'a str,
    ) -> BoxFuture<'a, Result<EmulatedTrace, WalletError>> {
        self.inner.emulate_trace(boc_base64)
    }

    fn emulate_message<'a>(
        &'a self,
        request: &'a crate::emulate::EmulateMessageRequest,
    ) -> BoxFuture<'a, Result<crate::emulate::EmulationResult, WalletError>> {
        self.inner.emulate_message(request)
    }

    fn get_nft_items<'a>(
        &'a self,
        owner: &'a str,
        limit: usize,
        offset: usize,
    ) -> BoxFuture<'a, Result<NftPage, WalletError>> {
        self.inner.get_nft_items(owner, limit, offset)
    }

    fn get_jetton_wallets<'a>(
        &'a self,
        owner: &'a str,
        limit: usize,
        offset: usize,
    ) -> BoxFuture<'a, Result<crate::jettons::JettonWalletsPage, WalletError>> {
        self.inner.get_jetton_wallets(owner, limit, offset)
    }

    fn get_jetton_masters<'a>(
        &'a self,
        master_address: &'a str,
    ) -> BoxFuture<'a, Result<Vec<crate::jettons::JettonMasterInfo>, WalletError>> {
        self.inner.get_jetton_masters(master_address)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    async fn spawn_fake_toncenter_server() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut buf = vec![0_u8; 8192];
                    let n = stream.read(&mut buf).await.unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]);
                    let first_line = req.lines().next().unwrap_or("");
                    let body = if first_line.contains("/api/v2/getAddressInformation") {
                        json!({
                            "ok": true,
                            "result": {
                                "balance": "2500000000",
                                "state": "active",
                                "last_transaction_id": {
                                    "lt": "42000001",
                                    "hash": "abc123"
                                }
                            }
                        })
                        .to_string()
                    } else if first_line.contains("/api/v2/getTransactions") {
                        json!({
                            "ok": true,
                            "result": [
                                {
                                    "transaction_id": { "lt": "42000001", "hash": "tx1" },
                                    "utime": 1700000000,
                                    "fee": "5000000",
                                    "in_msg": {
                                        "source": "UQCdqXGvONLwOr3zCNX5FjapflorB6ZsOdcdfLrjsDLt3AF4",
                                        "value": "1500000000",
                                        "message": "Coffee"
                                    },
                                    "out_msgs": []
                                }
                            ]
                        })
                        .to_string()
                    } else if first_line.contains("/api/v2/sendBoc") {
                        json!({
                            "ok": true,
                            "result": { "@type": "ok", "hash": "sent_hash_1" }
                        })
                        .to_string()
                    } else if first_line.contains("/api/v2/runGetMethod") {
                        json!({
                            "ok": true,
                            "result": { "exit_code": 0, "stack": [["num", "0x7"]] }
                        })
                        .to_string()
                    } else if first_line.contains("/api/emulate/v1/emulateTrace") {
                        json!({
                            "mc_block_seqno": 17,
                            "trace": { "tx_hash": "root" },
                            "transactions": {
                                "root": {
                                    "account": "0:1111111111111111111111111111111111111111111111111111111111111111",
                                    "total_fees": "4200000",
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
                    } else if first_line.contains("/api/v3/nft/items") {
                        json!({
                            "nft_items": [
                                {
                                    "address": "0:1111111111111111111111111111111111111111111111111111111111111111",
                                    "index": "1",
                                    "collection_address": "0:80D78A35F955A14B679FAA887FF4CD5BFC0F43B4A4EEA2A7E6927F3701B273C2",
                                    "owner_address": "0:2222222222222222222222222222222222222222222222222222222222222222",
                                    "content": { "uri": "https://nft.fragment.com/username/demo_handle.json" },
                                    "collection": { "name": "Telegram Usernames" }
                                }
                            ]
                        })
                        .to_string()
                    } else if first_line.contains("/api/v3/jetton/wallets") {
                        json!({
                            "jetton_wallets": [
                                {
                                    "address": "0:3333333333333333333333333333333333333333333333333333333333333333",
                                    "balance": "50000000",
                                    "owner": "0:2222222222222222222222222222222222222222222222222222222222222222",
                                    "jetton": "0:4444444444444444444444444444444444444444444444444444444444444444",
                                    "last_transaction_lt": "100"
                                }
                            ]
                        })
                        .to_string()
                    } else if first_line.contains("/api/v3/jetton/masters") {
                        json!({
                            "jetton_masters": [
                                {
                                    "address": "0:4444444444444444444444444444444444444444444444444444444444444444",
                                    "total_supply": "1000000000",
                                    "mintable": false,
                                    "jetton_content": {
                                        "decimals": "6",
                                        "name": "Demo USDt",
                                        "symbol": "USDt"
                                    }
                                }
                            ]
                        })
                        .to_string()
                    } else {
                        json!({ "ok": false, "error": "unknown endpoint" }).to_string()
                    };
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = stream.write_all(resp.as_bytes()).await;
                });
            }
        });

        format!("http://{addr}")
    }

    #[tokio::test]
    async fn http_toncenter_client_against_local_fake_server() {
        let base_url = spawn_fake_toncenter_server().await;
        let client = HttpToncenterClient::new(ToncenterConfig::new(base_url)).unwrap();

        let info = client
            .get_address_information("UQCdqXGvONLwOr3zCNX5FjapflorB6ZsOdcdfLrjsDLt3AF4")
            .await
            .unwrap();
        assert_eq!(info.balance_nano, 2_500_000_000);
        assert_eq!(info.state, AccountState::Active);
        assert!(!info.never_used);

        let txs = client
            .get_transactions(
                "UQCdqXGvONLwOr3zCNX5FjapflorB6ZsOdcdfLrjsDLt3AF4",
                10,
                None,
                None,
            )
            .await
            .unwrap();
        assert_eq!(txs.len(), 1);
        assert!(txs[0].incoming);
        assert_eq!(txs[0].amount_nano, 1_500_000_000);
        assert_eq!(txs[0].comment, "Coffee");

        let seq = client
            .run_get_method(
                "UQCdqXGvONLwOr3zCNX5FjapflorB6ZsOdcdfLrjsDLt3AF4",
                "seqno",
                &[],
            )
            .await
            .unwrap();
        assert_eq!(seq.exit_code, 0);
        assert_eq!(seq.first_u64(), Some(7));

        let emul = client
            .emulate_trace("te6cckEBAQEADAAAFAAAAABIZWxsbyGVgYQo")
            .await
            .unwrap();
        assert_eq!(emul.fee_nano, 4_200_000);

        let sent = client
            .send_boc("te6cckEBAQEADAAAFAAAAABIZWxsbyGVgYQo")
            .await
            .unwrap();
        assert!(sent.ok);
        assert_eq!(sent.hash.as_deref(), Some("sent_hash_1"));

        let nfts = client
            .get_nft_items(
                "0:2222222222222222222222222222222222222222222222222222222222222222",
                10,
                0,
            )
            .await
            .unwrap();
        assert_eq!(nfts.list.len(), 1);
        assert_eq!(nfts.list[0].kind, NftKind::TelegramUsername);
        assert_eq!(nfts.list[0].key, "demo_handle");

        let jw = client
            .get_jetton_wallets(
                "0:2222222222222222222222222222222222222222222222222222222222222222",
                10,
                0,
            )
            .await
            .unwrap();
        assert_eq!(jw.wallets.len(), 1);
        assert_eq!(jw.wallets[0].balance, 50_000_000);

        let masters = client
            .get_jetton_masters(
                "0:4444444444444444444444444444444444444444444444444444444444444444",
            )
            .await
            .unwrap();
        assert_eq!(masters.len(), 1);
        assert_eq!(masters[0].symbol, "USDt");
        assert_eq!(masters[0].decimals, 6);
    }

    #[test]
    fn gram_parsers_rates_stream_and_errors() {
        let err = parse_api_error(br#"{"error":"timeout: context deadline exceeded"}"#).unwrap();
        assert_eq!(err.message, "timeout: context deadline exceeded");
        assert!(parse_api_error(br#"{"balance":"1"}"#).is_none());

        let rates =
            CurrencyRates::from_entries(&[("usd", 1.0), ("eur", 0.92), ("bad", -1.0)]).unwrap();
        assert!((rates.compute_per_ton("EUR", 5.0).unwrap() - 4.6).abs() < 1e-9);
        assert!(rates.compute_per_ton("BAD", 5.0).is_none());

        let ep = parse_stream_endpoint("wss://example.invalid:8443/ws?token=abc").unwrap();
        assert_eq!(ep.host, "example.invalid");
        assert_eq!(ep.port, 8443);
        assert_eq!(ep.request_target, "/ws?token=abc");

        let ev = parse_stream_event(
            br#"{"type":"account_state_change","account":"0:11","state":{"balance":"100"}}"#,
        );
        assert_eq!(ev.kind, StreamEventKind::AccountState);
        assert_eq!(ev.accounts, vec!["0:11"]);

        let sub = stream_subscribe_message("0:11", 3);
        let sub_val: Value = serde_json::from_slice(&sub).unwrap();
        assert_eq!(sub_val["id"], "sub-3");
        let ping = stream_ping_message(9);
        let ping_val: Value = serde_json::from_slice(&ping).unwrap();
        assert_eq!(ping_val["id"], "ping-9");
    }
}
