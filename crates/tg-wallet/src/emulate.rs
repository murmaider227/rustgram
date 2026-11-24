//! TON transaction emulation request/response model, gas & fee breakdown,
//! per-account TON/Jetton/NFT balance delta summaries, aborted/bounced phase warnings,
//! and TON Connect emulation action classification.
//!
//! Port origin: `Telegram/SourceFiles/gram/api/gram_api_emulate.h`,
//! `Telegram/SourceFiles/gram/api/gram_api_emulate.cpp`,
//! `Telegram/SourceFiles/wallet/wallet_ton_connect_emulation.h`,
//! `Telegram/SourceFiles/wallet/wallet_ton_connect_emulation.cpp`.
//!
//! All network tests run strictly against local mock HTTP transports and canned JSON fixtures.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::WalletError;
use crate::address::canonical_address;
use crate::boc::{deserialize_boc_base64, nft_transfer_new_owner};
use crate::jettons::parse_jetton_transfer_body;
use crate::toncenter::{EmulatedTrace, GramHttpRequest, parse_api_error};

/// Relative Toncenter endpoint path for transaction trace emulation (`kEmulateTraceEndpoint`).
pub const EMULATE_TRACE_ENDPOINT: &str = "/api/emulate/v1/emulateTrace";

/// Excess gas refund opcode (`0xd53276db`, `kExcessOpcode` in `wallet_ton_connect_emulation.cpp`).
pub const EXCESS_OPCODE: u32 = 0xD532_76DB;

/// Request parameters for Toncenter `POST /api/emulate/v1/emulateTrace`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmulateMessageRequest {
    /// Standard padded base64 external message BOC.
    pub boc_base64: String,
    /// Whether the emulator should bypass Ed25519 signature checks (`true` for pre-sign quotes).
    pub ignore_chksig: bool,
    /// Whether to include contract code and data cells in the response.
    pub include_code_data: bool,
    /// Whether to include the Toncenter address book in the response.
    pub include_address_book: bool,
    /// Whether to include token/NFT metadata in the response.
    pub include_metadata: bool,
    /// Whether to include classified high-level actions in the response.
    pub with_actions: bool,
    /// Optional masterchain block seqno to emulate against (`None` = latest).
    pub mc_block_seqno: Option<u64>,
}

impl EmulateMessageRequest {
    /// Builds a request with tdesktop's default flags (`Gram::EmulateTraceRequest`).
    #[must_use]
    pub fn new(boc_base64: impl Into<String>) -> Self {
        Self {
            boc_base64: boc_base64.into(),
            ignore_chksig: true,
            include_code_data: false,
            include_address_book: false,
            include_metadata: false,
            with_actions: true,
            mc_block_seqno: None,
        }
    }

    /// Enables or disables metadata inclusion in the emulation response.
    #[must_use]
    pub const fn with_metadata(mut self, include: bool) -> Self {
        self.include_metadata = include;
        self
    }

    /// Sets `ignore_chksig`.
    #[must_use]
    pub const fn with_ignore_chksig(mut self, ignore: bool) -> Self {
        self.ignore_chksig = ignore;
        self
    }

    /// Pins emulation to a specific masterchain block sequence number.
    #[must_use]
    pub const fn with_mc_block_seqno(mut self, seqno: Option<u64>) -> Self {
        self.mc_block_seqno = seqno;
        self
    }

    /// Serializes this request into a [`GramHttpRequest`] targeting `POST /api/emulate/v1/emulateTrace`.
    #[must_use]
    pub fn to_http_request(&self) -> GramHttpRequest {
        let body = json!({
            "boc": self.boc_base64,
            "ignore_chksig": self.ignore_chksig,
            "include_code_data": self.include_code_data,
            "include_address_book": self.include_address_book,
            "include_metadata": self.include_metadata,
            "with_actions": self.with_actions,
            "mc_block_seqno": self.mc_block_seqno,
        });
        GramHttpRequest {
            post: true,
            endpoint: EMULATE_TRACE_ENDPOINT.to_owned(),
            query: String::new(),
            payload: serde_json::to_vec(&body).unwrap_or_default(),
        }
    }
}

/// Warning emitted while inspecting an emulated transaction trace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EmulationWarning {
    /// A transaction in the trace had `description.aborted == true`.
    TransactionAborted {
        /// Hash of the aborted transaction.
        tx_hash: String,
        /// Canonical account address of the aborted transaction.
        account: String,
    },
    /// A transaction's compute phase failed (`success == false` or `exit_code` not in `{0, 1}`).
    ComputePhaseFailed {
        /// Hash of the failing transaction.
        tx_hash: String,
        /// Canonical account address.
        account: String,
        /// TVM exit code reported by `compute_ph.exit_code`.
        exit_code: i32,
    },
    /// A transaction's action phase failed (`success == false` or `result_code != 0`).
    ActionPhaseFailed {
        /// Hash of the failing transaction.
        tx_hash: String,
        /// Canonical account address.
        account: String,
        /// Action phase result code reported by `action.result_code`.
        result_code: i32,
    },
    /// A message bounced during trace execution.
    MessageBounced {
        /// Hash of the transaction where the bounce was observed.
        tx_hash: String,
        /// Canonical account address of the transaction.
        account: String,
        /// Bounce source address (if present).
        source: Option<String>,
        /// Bounce destination address (if present).
        destination: Option<String>,
        /// Attached TON value of the bounced message in nanoTON.
        value_nano: u64,
    },
    /// A high-level classified action reported `success == false`.
    ActionFailed {
        /// Toncenter action type string (e.g. `"jetton_transfer"`).
        action_type: String,
        /// Optional trace or action identifier.
        trace_id: Option<String>,
    },
    /// The emulator reported `is_incomplete == true` (trace was truncated or unresolved).
    TraceIncomplete,
}

/// Net Jetton balance delta for a single account and Jetton master.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JettonDelta {
    /// Canonical lowercase raw address of the Jetton master contract.
    pub jetton_master: String,
    /// Optional canonical raw address of the account's Jetton wallet contract.
    pub jetton_wallet: Option<String>,
    /// Net change in smallest Jetton units (positive = received, negative = sent/burned).
    pub delta: i128,
    /// Optional ticker symbol resolved from response metadata.
    pub symbol: Option<String>,
    /// Optional decimal precision resolved from response metadata.
    pub decimals: Option<u8>,
}

/// Net NFT ownership delta for a single account and NFT item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NftDelta {
    /// Canonical lowercase raw address of the NFT item contract.
    pub nft_address: String,
    /// Optional canonical raw address of the NFT collection contract.
    pub collection_address: Option<String>,
    /// Ownership delta (`+1` received, `-1` transferred away).
    pub delta: i8,
    /// Counterparty canonical raw address (previous owner when received, new owner when sent).
    pub counterparty: Option<String>,
}

/// Net TON, Jetton, and NFT balance changes for a single account across an emulated trace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountBalanceDelta {
    /// Canonical lowercase raw account address (`workchain:64-hex`).
    pub account: String,
    /// Net TON balance change in nanoTON (negative = spent including fees, positive = received).
    pub ton_delta_nano: i128,
    /// Deterministic sorted list of Jetton balance changes for this account.
    pub jetton_deltas: Vec<JettonDelta>,
    /// Deterministic sorted list of NFT ownership changes for this account.
    pub nft_deltas: Vec<NftDelta>,
}

/// Parsed single transaction inside an emulated trace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmulatedTransaction {
    /// Transaction hash key from Toncenter's `transactions` map.
    pub hash: String,
    /// Canonical lowercase raw account address.
    pub account: String,
    /// Total transaction fee in nanoTON (`total_fees`).
    pub total_fees_nano: u64,
    /// Compute phase gas fee in nanoTON (`description.compute_ph.gas_fees`).
    pub gas_fees_nano: u64,
    /// Storage phase fee in nanoTON (`description.storage_ph.storage_fees_collected`).
    pub storage_fees_nano: u64,
    /// Forward fees paid in action phase (`description.action.total_fwd_fees`).
    pub fwd_fees_nano: u64,
    /// Whether `description.aborted` was `true`.
    pub aborted: bool,
    /// Whether `description.destroyed` was `true`.
    pub destroyed: bool,
    /// Compute phase success flag (`None` when skipped or absent).
    pub compute_success: Option<bool>,
    /// Compute phase TVM exit code (`None` when skipped or absent).
    pub compute_exit_code: Option<i32>,
    /// Action phase success flag (`None` when absent).
    pub action_success: Option<bool>,
    /// Action phase result code (`None` when absent).
    pub action_result_code: Option<i32>,
    /// Whether `in_msg.bounced` was `true`.
    pub bounced_in: bool,
    /// Whether any outgoing message in `out_msgs` had `bounced == true`.
    pub bounced_out: bool,
    /// Account TON balance before this transaction (if reported).
    pub balance_before: Option<u64>,
    /// Account TON balance after this transaction (if reported).
    pub balance_after: Option<u64>,
}

/// Raw action descriptor matching `wallet_engine::SendEmulationAction`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendEmulationAction {
    /// Action type string (e.g. `"ton_transfer"`, `"call_contract"`, `"contract_deploy"`).
    pub kind: String,
    /// Whether the action succeeded.
    pub succeeded: bool,
    /// Raw JSON string of the action's `details` object.
    pub details_json: String,
}

/// Raw send-emulation input matching `wallet_engine::SendEmulation`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SendEmulation {
    /// Whether the emulated trace succeeded overall.
    pub trace_succeeded: bool,
    /// Whether the emulated trace was incomplete.
    pub is_incomplete: bool,
    /// Classified actions produced by the emulator.
    pub actions: Vec<SendEmulationAction>,
}

/// Full parsed transaction emulation summary returned by [`parse_emulation_response`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmulationResult {
    /// Masterchain block sequence number used for emulation, if reported.
    pub mc_block_seqno: Option<u64>,
    /// Root transaction hash (`trace.tx_hash`).
    pub root_tx_hash: String,
    /// Canonical lowercase raw address of the root transaction's account.
    pub root_account: String,
    /// Root transaction's own fee in nanoTON (`Gram::EmulatedTrace::feeNano`).
    pub root_fee_nano: u64,
    /// Sum of `total_fees` across all transactions in the trace (nanoTON).
    pub total_fees_nano: u64,
    /// Sum of compute `gas_fees` across all transactions in the trace (nanoTON).
    pub total_gas_fees_nano: u64,
    /// Sum of `storage_fees_collected` across all transactions in the trace (nanoTON).
    pub total_storage_fees_nano: u64,
    /// Sum of action `total_fwd_fees` across all transactions in the trace (nanoTON).
    pub total_fwd_fees_nano: u64,
    /// Whether the emulator marked the trace incomplete.
    pub is_incomplete: bool,
    /// Whether the root and traced transactions completed without abort/phase failure.
    pub trace_succeeded: bool,
    /// Ordered transactions in the trace.
    pub transactions: Vec<EmulatedTransaction>,
    /// Per-account TON/Jetton/NFT balance deltas, sorted by canonical account address.
    pub account_deltas: Vec<AccountBalanceDelta>,
    /// Deterministic list of phase failures, aborts, bounces, and truncation warnings.
    pub warnings: Vec<EmulationWarning>,
    /// Raw classified actions for TON Connect preview rendering.
    pub raw_actions: Vec<SendEmulationAction>,
}

impl EmulationResult {
    /// Constructs a minimal [`EmulationResult`] from a basic [`EmulatedTrace`].
    #[must_use]
    pub fn from_basic_trace(trace: EmulatedTrace) -> Self {
        let root_fee = u64::try_from(trace.fee_nano).unwrap_or(0);
        let root_account = normalize_addr(&trace.account);
        Self {
            mc_block_seqno: None,
            root_tx_hash: String::new(),
            root_account: root_account.clone(),
            root_fee_nano: root_fee,
            total_fees_nano: root_fee,
            total_gas_fees_nano: 0,
            total_storage_fees_nano: 0,
            total_fwd_fees_nano: 0,
            is_incomplete: false,
            trace_succeeded: true,
            transactions: Vec::new(),
            account_deltas: if root_account.is_empty() {
                Vec::new()
            } else {
                vec![AccountBalanceDelta {
                    account: root_account,
                    ton_delta_nano: -i128::from(root_fee),
                    jetton_deltas: Vec::new(),
                    nft_deltas: Vec::new(),
                }]
            },
            warnings: Vec::new(),
            raw_actions: Vec::new(),
        }
    }

    /// Looks up the [`AccountBalanceDelta`] for any raw or user-friendly TON address.
    #[must_use]
    pub fn delta_for(&self, address: &str) -> Option<&AccountBalanceDelta> {
        let canon = normalize_addr(address);
        self.account_deltas.iter().find(|d| d.account == canon)
    }

    /// Returns `true` if any warning was produced during emulation.
    #[must_use]
    pub fn has_warnings(&self) -> bool {
        !self.warnings.is_empty()
    }

    /// Converts this emulation result into tdesktop's [`TonConnectEmulation`] preview model.
    #[must_use]
    pub fn to_ton_connect_emulation(
        &self,
        wallet_address: &str,
        request_total_nano: i64,
    ) -> TonConnectEmulation {
        let raw = SendEmulation {
            trace_succeeded: self.trace_succeeded,
            is_incomplete: self.is_incomplete,
            actions: self.raw_actions.clone(),
        };
        parse_ton_connect_emulation(&raw, wallet_address, request_total_nano)
    }
}

// ============================================================================
// Direct port of `wallet_ton_connect_emulation.h` / `.cpp`
// ============================================================================

/// High-level TON Connect action kind (`Wallet::TonConnectActionKind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum TonConnectActionKind {
    /// Outgoing TON transfer from the user's wallet.
    Withdraw,
    /// Incoming TON transfer into the user's wallet.
    Deposit,
    /// Third-party TON transfer between other addresses.
    Transfer,
    /// Excess gas refund (`opcode == 0xd53276db`).
    Excess,
    /// Smart contract call.
    CallContract,
    /// Smart contract deployment.
    DeployContract,
    /// Unclassified or unsupported action.
    #[default]
    Unknown,
}

/// Direction of an emulated action relative to the user's wallet (`Wallet::TonConnectActionSide`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum TonConnectActionSide {
    /// Neither source nor destination matches the wallet.
    #[default]
    None,
    /// Destination matches the wallet.
    Incoming,
    /// Source matches the wallet.
    Outgoing,
}

/// Single classified action shown in a TON Connect confirmation preview (`Wallet::TonConnectAction`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TonConnectAction {
    /// Classified action kind.
    pub kind: TonConnectActionKind,
    /// Direction relative to the wallet.
    pub side: TonConnectActionSide,
    /// Optional TON amount in nanoTON.
    pub amount_nano: Option<i64>,
    /// Canonical counterparty address.
    pub counterparty: String,
}

/// Overall trust status of a TON Connect emulation preview (`Wallet::TonConnectEmulationStatus`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum TonConnectEmulationStatus {
    /// Trace succeeded completely and produced non-empty valid actions.
    Shown,
    /// Trace failed and at least one non-skipped action explicitly failed.
    Failed,
    /// Trace failed/aborted without an individual non-skipped failed action.
    Aborted,
    /// Trace succeeded but the emulator flagged `is_incomplete`.
    Incomplete,
    /// No displayable actions or overflow/invalid wallet address.
    #[default]
    Empty,
}

/// Parsed TON Connect preview (`Wallet::TonConnectEmulation`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TonConnectEmulation {
    /// Displayable classified actions.
    pub actions: Vec<TonConnectAction>,
    /// Net TON change (`credited - request_total_nano`) in nanoTON.
    pub net_nano: i64,
    /// Emulation trust status.
    pub status: TonConnectEmulationStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SourceKind {
    Transfer,
    CallContract,
    ContractDeploy,
    Skipped,
    Other,
}

fn source_kind_of(kind: &str) -> SourceKind {
    match kind {
        "ton_transfer" | "extra_currency_transfer" => SourceKind::Transfer,
        "call_contract" => SourceKind::CallContract,
        "contract_deploy" => SourceKind::ContractDeploy,
        "unknown" => SourceKind::Skipped,
        _ => SourceKind::Other,
    }
}

fn details_address(details: &serde_json::Map<String, Value>, key: &str) -> String {
    details
        .get(key)
        .and_then(Value::as_str)
        .and_then(canonical_address)
        .unwrap_or_default()
}

fn details_nano(details: &serde_json::Map<String, Value>) -> Option<i64> {
    let text = details.get("value")?.as_str()?;
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse::<i64>().ok()
}

fn details_opcode(details: &serde_json::Map<String, Value>) -> Option<u32> {
    let value = details.get("opcode")?;
    if let Some(text) = value.as_str() {
        let digits = text
            .strip_prefix("0x")
            .or_else(|| text.strip_prefix("0X"))?;
        if digits.is_empty() || digits.len() > 8 || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        return u32::from_str_radix(digits, 16).ok();
    }
    if let Some(u) = value.as_u64() {
        return u32::try_from(u).ok();
    }
    if let Some(i) = value.as_i64() {
        return i32::try_from(i).ok().map(|v| v as u32);
    }
    if let Some(num) = value.as_f64() {
        if num.fract() != 0.0 || num < f64::from(i32::MIN) || num > f64::from(u32::MAX) {
            return None;
        }
        return Some(if num < 0.0 {
            (num as i32) as u32
        } else {
            num as u32
        });
    }
    None
}

fn side_of(source: &str, destination: &str, own: &str) -> TonConnectActionSide {
    if !source.is_empty() && source == own {
        TonConnectActionSide::Outgoing
    } else if !destination.is_empty() && destination == own {
        TonConnectActionSide::Incoming
    } else {
        TonConnectActionSide::None
    }
}

fn action_kind_of(
    kind: SourceKind,
    side: TonConnectActionSide,
    details: &serde_json::Map<String, Value>,
) -> TonConnectActionKind {
    match kind {
        SourceKind::Transfer => match side {
            TonConnectActionSide::Outgoing => TonConnectActionKind::Withdraw,
            TonConnectActionSide::Incoming => TonConnectActionKind::Deposit,
            TonConnectActionSide::None => TonConnectActionKind::Transfer,
        },
        SourceKind::CallContract => {
            if details_opcode(details) == Some(EXCESS_OPCODE) {
                TonConnectActionKind::Excess
            } else {
                TonConnectActionKind::CallContract
            }
        }
        SourceKind::ContractDeploy => TonConnectActionKind::DeployContract,
        SourceKind::Skipped | SourceKind::Other => TonConnectActionKind::Unknown,
    }
}

fn trust_status(emulation: &SendEmulation) -> TonConnectEmulationStatus {
    if !emulation.trace_succeeded {
        let failed = emulation
            .actions
            .iter()
            .any(|action| !action.succeeded && source_kind_of(&action.kind) != SourceKind::Skipped);
        return if failed {
            TonConnectEmulationStatus::Failed
        } else {
            TonConnectEmulationStatus::Aborted
        };
    }
    if emulation.is_incomplete {
        TonConnectEmulationStatus::Incomplete
    } else {
        TonConnectEmulationStatus::Shown
    }
}

/// Classifies raw emulator actions relative to `wallet_address` and computes `net_nano`
/// (`Wallet::ParseTonConnectEmulation` in `wallet_ton_connect_emulation.cpp:220-257`).
#[must_use]
pub fn parse_ton_connect_emulation(
    emulation: &SendEmulation,
    wallet_address: &str,
    request_total_nano: i64,
) -> TonConnectEmulation {
    let mut result = TonConnectEmulation {
        actions: Vec::new(),
        net_nano: 0,
        status: trust_status(emulation),
    };
    let own = canonical_address(wallet_address).unwrap_or_default();
    let mut valid = !own.is_empty();
    let mut credited = 0_i64;

    if valid {
        for raw_action in &emulation.actions {
            let kind = source_kind_of(&raw_action.kind);
            if kind == SourceKind::Skipped {
                continue;
            }
            let details_val: Value =
                serde_json::from_str(&raw_action.details_json).unwrap_or(Value::Null);
            let empty_map = serde_json::Map::new();
            let details = details_val.as_object().unwrap_or(&empty_map);
            let source = details_address(details, "source");
            let destination = details_address(details, "destination");

            if kind == SourceKind::ContractDeploy {
                result.actions.push(TonConnectAction {
                    kind: TonConnectActionKind::DeployContract,
                    side: TonConnectActionSide::None,
                    amount_nano: None,
                    counterparty: destination,
                });
                continue;
            }

            let side = side_of(&source, &destination, &own);
            let amount = details_nano(details);
            let basic = matches!(kind, SourceKind::Transfer | SourceKind::CallContract);
            let is_credited = basic && amount.is_some() && destination == own;
            if is_credited && let Some(amt) = amount {
                if let Some(next) = credited.checked_add(amt) {
                    credited = next;
                } else {
                    valid = false;
                }
            }
            let counterparty = if side == TonConnectActionSide::Incoming {
                source
            } else {
                destination
            };
            result.actions.push(TonConnectAction {
                kind: action_kind_of(kind, side, details),
                side,
                amount_nano: amount,
                counterparty,
            });
        }
    }

    if let Some(net) = credited.checked_sub(request_total_nano) {
        if valid {
            result.net_nano = net;
        }
    } else {
        valid = false;
    }

    if result.status == TonConnectEmulationStatus::Shown && (!valid || result.actions.is_empty()) {
        result.status = TonConnectEmulationStatus::Empty;
    }
    result
}

// ============================================================================
// Full Toncenter emulation response parser
// ============================================================================

fn normalize_addr(addr: &str) -> String {
    let trimmed = addr.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    canonical_address(trimmed).unwrap_or_else(|| trimmed.to_ascii_lowercase())
}

fn parse_u64_field(val: &Value) -> Option<u64> {
    if let Some(s) = val.as_str() {
        s.trim().parse::<u64>().ok()
    } else if let Some(u) = val.as_u64() {
        Some(u)
    } else if let Some(i) = val.as_i64() {
        u64::try_from(i).ok()
    } else {
        None
    }
}

fn parse_u128_field(val: &Value) -> Option<u128> {
    if let Some(s) = val.as_str() {
        s.trim().parse::<u128>().ok()
    } else {
        val.as_u64().map(u128::from)
    }
}

fn parse_i32_field(val: &Value) -> Option<i32> {
    val.as_i64().and_then(|v| i32::try_from(v).ok())
}

fn collect_trace_order(node: &Value, out: &mut Vec<String>, seen: &mut HashSet<String>) {
    let Some(obj) = node.as_object() else {
        return;
    };
    if let Some(hash) = obj.get("tx_hash").and_then(Value::as_str)
        && !hash.is_empty()
        && seen.insert(hash.to_owned())
    {
        out.push(hash.to_owned());
    }
    if let Some(children) = obj.get("children").and_then(Value::as_array) {
        for child in children {
            collect_trace_order(child, out, seen);
        }
    }
}

struct TokenMeta {
    symbol: Option<String>,
    decimals: Option<u8>,
}

fn extract_metadata(obj: &serde_json::Map<String, Value>) -> HashMap<String, TokenMeta> {
    let mut map = HashMap::new();
    let Some(meta_obj) = obj.get("metadata").and_then(Value::as_object) else {
        return map;
    };
    for (addr, entry) in meta_obj {
        let canon = normalize_addr(addr);
        if canon.is_empty() {
            continue;
        }
        let Some(token_info) = entry
            .get("token_info")
            .and_then(Value::as_array)
            .and_then(|arr| arr.first())
            .and_then(Value::as_object)
        else {
            continue;
        };
        let symbol = token_info
            .get("symbol")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(ToOwned::to_owned);
        let decimals = token_info
            .get("extra")
            .and_then(Value::as_object)
            .and_then(|e| e.get("decimals"))
            .or_else(|| token_info.get("decimals"))
            .and_then(|v| {
                v.as_str()
                    .and_then(|s| s.parse::<u8>().ok())
                    .or_else(|| v.as_u64().and_then(|n| u8::try_from(n).ok()))
            });
        map.insert(canon, TokenMeta { symbol, decimals });
    }
    map
}

#[derive(Default)]
struct AccountAccumulator {
    first_balance_before: Option<u64>,
    last_balance_after: Option<u64>,
    fallback_ton_delta: i128,
    has_state_balances: bool,
    jettons: BTreeMap<(String, Option<String>), i128>,
    nfts: BTreeMap<String, (Option<String>, i8, Option<String>)>,
}

impl AccountAccumulator {
    fn add_jetton(&mut self, master: String, wallet: Option<String>, delta: i128) {
        if master.is_empty() || delta == 0 {
            return;
        }
        let entry = self.jettons.entry((master, wallet)).or_insert(0);
        *entry = entry.saturating_add(delta);
    }

    fn add_nft(
        &mut self,
        nft_address: String,
        collection: Option<String>,
        delta: i8,
        counterparty: Option<String>,
    ) {
        if nft_address.is_empty() || delta == 0 {
            return;
        }
        let entry =
            self.nfts
                .entry(nft_address)
                .or_insert((collection.clone(), 0, counterparty.clone()));
        if entry.0.is_none() && collection.is_some() {
            entry.0 = collection;
        }
        entry.1 = entry.1.saturating_add(delta);
        if counterparty.is_some() {
            entry.2 = counterparty;
        }
    }
}

/// Parses a full Toncenter `/api/emulate/v1/emulateTrace` JSON response into an [`EmulationResult`].
///
/// Deterministically extracts:
/// - root and aggregate fees (`total_fees`, `gas_fees`, `storage_fees`, `fwd_fees`),
/// - per-account TON, Jetton, and NFT balance deltas,
/// - aborted transactions, failed compute/action phases, bounced messages, and incomplete trace warnings,
/// - classified actions compatible with [`parse_ton_connect_emulation`].
pub fn parse_emulation_response(json_bytes: &[u8]) -> Result<EmulationResult, WalletError> {
    if let Some(api_err) = parse_api_error(json_bytes) {
        return Err(WalletError::Toncenter(api_err.message));
    }
    let val: Value = serde_json::from_slice(json_bytes)
        .map_err(|e| WalletError::Toncenter(format!("invalid emulate JSON: {e}")))?;
    let obj = val.as_object().ok_or_else(|| {
        WalletError::Toncenter("emulate response is not a JSON object".to_owned())
    })?;

    let mc_block_seqno = obj.get("mc_block_seqno").and_then(parse_u64_field);
    let is_incomplete = obj
        .get("is_incomplete")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let trace_val = obj
        .get("trace")
        .ok_or_else(|| WalletError::Toncenter("emulate response missing trace".to_owned()))?;
    let root_tx_hash = trace_val
        .as_object()
        .and_then(|t| t.get("tx_hash"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| WalletError::Toncenter("emulate response missing trace.tx_hash".to_owned()))?
        .to_owned();

    let tx_map = obj
        .get("transactions")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            WalletError::Toncenter("emulate response missing transactions map".to_owned())
        })?;
    if !tx_map.contains_key(&root_tx_hash) {
        return Err(WalletError::Toncenter(format!(
            "emulate response missing root transaction {root_tx_hash}"
        )));
    }

    // Build deterministic transaction iteration order.
    let mut order = Vec::with_capacity(tx_map.len());
    let mut seen = HashSet::with_capacity(tx_map.len());
    if let Some(explicit_order) = obj.get("transactions_order").and_then(Value::as_array) {
        for item in explicit_order {
            if let Some(h) = item.as_str()
                && tx_map.contains_key(h)
                && seen.insert(h.to_owned())
            {
                order.push(h.to_owned());
            }
        }
    }
    collect_trace_order(trace_val, &mut order, &mut seen);
    let mut remaining: Vec<&String> = tx_map
        .keys()
        .filter(|k| !seen.contains(k.as_str()))
        .collect();
    remaining.sort();
    for k in remaining {
        order.push(k.clone());
    }

    let token_meta = extract_metadata(obj);
    let mut accounts: BTreeMap<String, AccountAccumulator> = BTreeMap::new();
    let mut transactions = Vec::with_capacity(order.len());
    let mut warnings = Vec::new();
    let mut bounce_keys: HashSet<(Option<String>, Option<String>, u64)> = HashSet::new();

    let mut total_fees_nano = 0_u64;
    let mut total_gas_fees_nano = 0_u64;
    let mut total_storage_fees_nano = 0_u64;
    let mut total_fwd_fees_nano = 0_u64;
    let mut root_fee_nano = 0_u64;
    let mut root_account = String::new();
    let mut trace_succeeded = true;

    for tx_hash in &order {
        let Some(tx_obj) = tx_map.get(tx_hash).and_then(Value::as_object) else {
            continue;
        };
        let raw_account = tx_obj
            .get("account")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let account = normalize_addr(raw_account);
        if tx_hash == &root_tx_hash {
            if account.is_empty() {
                return Err(WalletError::Toncenter(
                    "root transaction missing account".to_owned(),
                ));
            }
            root_account = account.clone();
        }

        let tx_total_fees = tx_obj
            .get("total_fees")
            .and_then(parse_u64_field)
            .ok_or_else(|| {
                WalletError::Toncenter(format!("transaction {tx_hash} missing valid total_fees"))
            })?;

        let desc = tx_obj.get("description").and_then(Value::as_object);
        let aborted = desc
            .and_then(|d| d.get("aborted"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let destroyed = desc
            .and_then(|d| d.get("destroyed"))
            .and_then(Value::as_bool)
            .unwrap_or(false);

        let storage_fees = desc
            .and_then(|d| d.get("storage_ph"))
            .and_then(Value::as_object)
            .and_then(|s| s.get("storage_fees_collected"))
            .and_then(parse_u64_field)
            .unwrap_or(0);

        let compute_ph = desc
            .and_then(|d| d.get("compute_ph"))
            .and_then(Value::as_object);
        let compute_skipped = compute_ph
            .and_then(|c| c.get("skipped"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let compute_success = if compute_skipped {
            None
        } else {
            compute_ph
                .and_then(|c| c.get("success"))
                .and_then(Value::as_bool)
        };
        let compute_exit_code = if compute_skipped {
            None
        } else {
            compute_ph
                .and_then(|c| c.get("exit_code"))
                .and_then(parse_i32_field)
        };
        let gas_fees = compute_ph
            .and_then(|c| c.get("gas_fees"))
            .and_then(parse_u64_field)
            .unwrap_or(0);

        let action_ph = desc
            .and_then(|d| d.get("action"))
            .and_then(Value::as_object);
        let action_success = action_ph
            .and_then(|a| a.get("success"))
            .and_then(Value::as_bool);
        let action_result_code = action_ph
            .and_then(|a| a.get("result_code"))
            .and_then(parse_i32_field);
        let fwd_fees = action_ph
            .and_then(|a| a.get("total_fwd_fees"))
            .and_then(parse_u64_field)
            .unwrap_or(0);

        // Phase diagnostics & warnings.
        if aborted {
            trace_succeeded = false;
            warnings.push(EmulationWarning::TransactionAborted {
                tx_hash: tx_hash.clone(),
                account: account.clone(),
            });
        }
        if !compute_skipped {
            let compute_failed = match (compute_success, compute_exit_code) {
                (Some(true), Some(0 | 1)) => false,
                (Some(false), _) => true,
                (_, Some(code)) if code != 0 && code != 1 => true,
                _ => tx_hash == &root_tx_hash,
            };
            if compute_failed {
                trace_succeeded = false;
                warnings.push(EmulationWarning::ComputePhaseFailed {
                    tx_hash: tx_hash.clone(),
                    account: account.clone(),
                    exit_code: compute_exit_code.unwrap_or(-1),
                });
            }
        }
        if action_ph.is_some() || tx_hash == &root_tx_hash {
            let action_failed = match (action_success, action_result_code) {
                (Some(true), Some(0)) => false,
                (Some(false), _) => true,
                (_, Some(code)) if code != 0 => true,
                _ => tx_hash == &root_tx_hash,
            };
            if action_failed {
                trace_succeeded = false;
                warnings.push(EmulationWarning::ActionPhaseFailed {
                    tx_hash: tx_hash.clone(),
                    account: account.clone(),
                    result_code: action_result_code.unwrap_or(-1),
                });
            }
        }

        // Inspect incoming and outgoing messages for bounces & fallback TON delta.
        let in_msg = tx_obj.get("in_msg").and_then(Value::as_object);
        let bounced_in = in_msg
            .and_then(|m| m.get("bounced"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let in_value = in_msg
            .and_then(|m| m.get("value"))
            .and_then(parse_u64_field)
            .unwrap_or(0);
        if bounced_in {
            let source = in_msg
                .and_then(|m| m.get("source"))
                .and_then(Value::as_str)
                .map(normalize_addr)
                .filter(|s| !s.is_empty());
            let destination = in_msg
                .and_then(|m| m.get("destination"))
                .and_then(Value::as_str)
                .map(normalize_addr)
                .filter(|s| !s.is_empty());
            let key = (source.clone(), destination.clone(), in_value);
            if bounce_keys.insert(key) {
                warnings.push(EmulationWarning::MessageBounced {
                    tx_hash: tx_hash.clone(),
                    account: account.clone(),
                    source,
                    destination,
                    value_nano: in_value,
                });
            }
        }

        let mut out_value_sum = 0_u128;
        let mut bounced_out = false;
        if let Some(out_msgs) = tx_obj.get("out_msgs").and_then(Value::as_array) {
            for msg in out_msgs {
                let Some(m) = msg.as_object() else {
                    continue;
                };
                let val = m.get("value").and_then(parse_u64_field).unwrap_or(0);
                out_value_sum = out_value_sum.saturating_add(u128::from(val));
                if m.get("bounced").and_then(Value::as_bool) == Some(true) {
                    bounced_out = true;
                    let source = m
                        .get("source")
                        .and_then(Value::as_str)
                        .map(normalize_addr)
                        .filter(|s| !s.is_empty());
                    let destination = m
                        .get("destination")
                        .and_then(Value::as_str)
                        .map(normalize_addr)
                        .filter(|s| !s.is_empty());
                    let key = (source.clone(), destination.clone(), val);
                    if bounce_keys.insert(key) {
                        warnings.push(EmulationWarning::MessageBounced {
                            tx_hash: tx_hash.clone(),
                            account: account.clone(),
                            source,
                            destination,
                            value_nano: val,
                        });
                    }
                }
            }
        }

        let balance_before = tx_obj
            .get("account_state_before")
            .and_then(Value::as_object)
            .and_then(|s| s.get("balance"))
            .and_then(parse_u64_field);
        let balance_after = tx_obj
            .get("account_state_after")
            .and_then(Value::as_object)
            .and_then(|s| s.get("balance"))
            .and_then(parse_u64_field);

        if !account.is_empty() {
            let acc = accounts.entry(account.clone()).or_default();
            if let (Some(before), Some(after)) = (balance_before, balance_after) {
                if !acc.has_state_balances {
                    acc.first_balance_before = Some(before);
                    acc.has_state_balances = true;
                }
                acc.last_balance_after = Some(after);
            }
            let tx_fallback = i128::from(in_value)
                .saturating_sub(out_value_sum as i128)
                .saturating_sub(i128::from(tx_total_fees));
            acc.fallback_ton_delta = acc.fallback_ton_delta.saturating_add(tx_fallback);
        }

        if tx_hash == &root_tx_hash {
            root_fee_nano = tx_total_fees;
        }
        total_fees_nano = total_fees_nano.saturating_add(tx_total_fees);
        total_gas_fees_nano = total_gas_fees_nano.saturating_add(gas_fees);
        total_storage_fees_nano = total_storage_fees_nano.saturating_add(storage_fees);
        total_fwd_fees_nano = total_fwd_fees_nano.saturating_add(fwd_fees);

        transactions.push(EmulatedTransaction {
            hash: tx_hash.clone(),
            account,
            total_fees_nano: tx_total_fees,
            gas_fees_nano: gas_fees,
            storage_fees_nano: storage_fees,
            fwd_fees_nano: fwd_fees,
            aborted,
            destroyed,
            compute_success,
            compute_exit_code,
            action_success,
            action_result_code,
            bounced_in,
            bounced_out,
            balance_before,
            balance_after,
        });
    }

    // Parse classified `actions` array (Jetton, NFT, TON transfers, contract calls).
    let mut raw_actions = Vec::new();
    let mut saw_jetton_action = false;
    let mut saw_nft_action = false;

    if let Some(actions) = obj.get("actions").and_then(Value::as_array) {
        for act in actions {
            let Some(act_obj) = act.as_object() else {
                continue;
            };
            let action_type = act_obj
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_owned();
            let succeeded = act_obj
                .get("success")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            let details_val = act_obj.get("details").cloned().unwrap_or_else(|| json!({}));
            let details_json = serde_json::to_string(&details_val).unwrap_or_default();

            raw_actions.push(SendEmulationAction {
                kind: action_type.clone(),
                succeeded,
                details_json,
            });

            if !succeeded {
                trace_succeeded = false;
                let trace_id = act_obj
                    .get("action_id")
                    .or_else(|| act_obj.get("trace_id"))
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned);
                warnings.push(EmulationWarning::ActionFailed {
                    action_type: action_type.clone(),
                    trace_id,
                });
                continue;
            }

            let Some(details) = details_val.as_object() else {
                continue;
            };
            apply_classified_action(
                &action_type,
                details,
                &mut accounts,
                &mut saw_jetton_action,
                &mut saw_nft_action,
            );
        }
    }

    // Fallback: inspect raw message BOC bodies if `actions` did not report Jetton/NFT transfers.
    if !saw_jetton_action || !saw_nft_action {
        extract_deltas_from_raw_messages(
            tx_map,
            &order,
            &mut accounts,
            !saw_jetton_action,
            !saw_nft_action,
        );
    }

    if is_incomplete {
        warnings.push(EmulationWarning::TraceIncomplete);
    }

    let account_deltas = accounts
        .into_iter()
        .map(|(account, acc)| {
            let ton_delta_nano = if acc.has_state_balances {
                match (acc.first_balance_before, acc.last_balance_after) {
                    (Some(before), Some(after)) => i128::from(after) - i128::from(before),
                    _ => acc.fallback_ton_delta,
                }
            } else {
                acc.fallback_ton_delta
            };
            let jetton_deltas = acc
                .jettons
                .into_iter()
                .filter(|(_, delta)| *delta != 0)
                .map(|((jetton_master, jetton_wallet), delta)| {
                    let meta = token_meta.get(&jetton_master);
                    JettonDelta {
                        jetton_master,
                        jetton_wallet,
                        delta,
                        symbol: meta.and_then(|m| m.symbol.clone()),
                        decimals: meta.and_then(|m| m.decimals),
                    }
                })
                .collect();
            let nft_deltas = acc
                .nfts
                .into_iter()
                .filter(|(_, (_, delta, _))| *delta != 0)
                .map(
                    |(nft_address, (collection_address, delta, counterparty))| NftDelta {
                        nft_address,
                        collection_address,
                        delta,
                        counterparty,
                    },
                )
                .collect();
            AccountBalanceDelta {
                account,
                ton_delta_nano,
                jetton_deltas,
                nft_deltas,
            }
        })
        .collect();

    Ok(EmulationResult {
        mc_block_seqno,
        root_tx_hash,
        root_account,
        root_fee_nano,
        total_fees_nano,
        total_gas_fees_nano,
        total_storage_fees_nano,
        total_fwd_fees_nano,
        is_incomplete,
        trace_succeeded,
        transactions,
        account_deltas,
        warnings,
        raw_actions,
    })
}

fn opt_addr(details: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<String> {
    for &key in keys {
        if let Some(s) = details.get(key).and_then(Value::as_str) {
            let norm = normalize_addr(s);
            if !norm.is_empty() {
                return Some(norm);
            }
        }
    }
    None
}

fn apply_classified_action(
    action_type: &str,
    details: &serde_json::Map<String, Value>,
    accounts: &mut BTreeMap<String, AccountAccumulator>,
    saw_jetton: &mut bool,
    saw_nft: &mut bool,
) {
    match action_type {
        "jetton_transfer" => {
            let Some(master) = opt_addr(details, &["asset", "jetton_master", "jetton"]) else {
                return;
            };
            let Some(amount) = details.get("amount").and_then(parse_u128_field) else {
                return;
            };
            let amount_i128 = i128::try_from(amount).unwrap_or(i128::MAX);
            *saw_jetton = true;
            if let Some(sender) = opt_addr(details, &["sender", "source"]) {
                let sender_wallet = opt_addr(details, &["sender_jetton_wallet", "sender_wallet"]);
                accounts.entry(sender).or_default().add_jetton(
                    master.clone(),
                    sender_wallet,
                    -amount_i128,
                );
            }
            if let Some(receiver) = opt_addr(details, &["receiver", "destination"]) {
                let receiver_wallet =
                    opt_addr(details, &["receiver_jetton_wallet", "receiver_wallet"]);
                accounts.entry(receiver).or_default().add_jetton(
                    master,
                    receiver_wallet,
                    amount_i128,
                );
            }
        }
        "jetton_burn" => {
            let Some(master) = opt_addr(details, &["asset", "jetton_master", "jetton"]) else {
                return;
            };
            let Some(amount) = details.get("amount").and_then(parse_u128_field) else {
                return;
            };
            let amount_i128 = i128::try_from(amount).unwrap_or(i128::MAX);
            *saw_jetton = true;
            if let Some(owner) = opt_addr(details, &["owner", "sender"]) {
                let wallet = opt_addr(details, &["owner_jetton_wallet", "jetton_wallet"]);
                accounts
                    .entry(owner)
                    .or_default()
                    .add_jetton(master, wallet, -amount_i128);
            }
        }
        "jetton_mint" => {
            let Some(master) = opt_addr(details, &["asset", "jetton_master", "jetton"]) else {
                return;
            };
            let Some(amount) = details.get("amount").and_then(parse_u128_field) else {
                return;
            };
            let amount_i128 = i128::try_from(amount).unwrap_or(i128::MAX);
            *saw_jetton = true;
            if let Some(receiver) = opt_addr(details, &["receiver", "owner"]) {
                let wallet = opt_addr(details, &["receiver_jetton_wallet", "jetton_wallet"]);
                accounts
                    .entry(receiver)
                    .or_default()
                    .add_jetton(master, wallet, amount_i128);
            }
        }
        "jetton_swap" => {
            let Some(user) = opt_addr(details, &["sender", "user", "owner"]) else {
                return;
            };
            let mut applied = false;
            if let (Some(asset_in), Some(amt_in)) = (
                opt_addr(details, &["asset_in"]),
                details.get("amount_in").and_then(parse_u128_field),
            ) {
                let v = i128::try_from(amt_in).unwrap_or(i128::MAX);
                accounts
                    .entry(user.clone())
                    .or_default()
                    .add_jetton(asset_in, None, -v);
                applied = true;
            }
            if let (Some(asset_out), Some(amt_out)) = (
                opt_addr(details, &["asset_out"]),
                details.get("amount_out").and_then(parse_u128_field),
            ) {
                let v = i128::try_from(amt_out).unwrap_or(i128::MAX);
                accounts
                    .entry(user)
                    .or_default()
                    .add_jetton(asset_out, None, v);
                applied = true;
            }
            if applied {
                *saw_jetton = true;
            }
        }
        "nft_transfer" => {
            let Some(nft_item) = opt_addr(details, &["nft_item", "nft_address", "item"]) else {
                return;
            };
            let collection = opt_addr(details, &["nft_collection", "collection"]);
            let old_owner = opt_addr(details, &["old_owner", "previous_owner", "sender"]);
            let new_owner = opt_addr(details, &["new_owner", "receiver", "destination"]);
            *saw_nft = true;
            if let Some(ref prev) = old_owner {
                accounts.entry(prev.clone()).or_default().add_nft(
                    nft_item.clone(),
                    collection.clone(),
                    -1,
                    new_owner.clone(),
                );
            }
            if let Some(next) = new_owner {
                accounts
                    .entry(next)
                    .or_default()
                    .add_nft(nft_item, collection, 1, old_owner);
            }
        }
        "nft_mint" => {
            let Some(nft_item) = opt_addr(details, &["nft_item", "nft_address", "item"]) else {
                return;
            };
            let collection = opt_addr(details, &["nft_collection", "collection"]);
            let owner = opt_addr(details, &["owner", "new_owner", "receiver"]);
            *saw_nft = true;
            if let Some(owner) = owner {
                accounts
                    .entry(owner)
                    .or_default()
                    .add_nft(nft_item, collection, 1, None);
            }
        }
        _ => {}
    }
}

fn extract_deltas_from_raw_messages(
    tx_map: &serde_json::Map<String, Value>,
    order: &[String],
    accounts: &mut BTreeMap<String, AccountAccumulator>,
    parse_jettons: bool,
    parse_nfts: bool,
) {
    for tx_hash in order {
        let Some(tx_obj) = tx_map.get(tx_hash).and_then(Value::as_object) else {
            continue;
        };
        if tx_obj
            .get("description")
            .and_then(|d| d.get("aborted"))
            .and_then(Value::as_bool)
            == Some(true)
        {
            continue;
        }
        let Some(out_msgs) = tx_obj.get("out_msgs").and_then(Value::as_array) else {
            continue;
        };
        let sender = tx_obj
            .get("account")
            .and_then(Value::as_str)
            .map(normalize_addr)
            .unwrap_or_default();
        for msg in out_msgs {
            let Some(m) = msg.as_object() else {
                continue;
            };
            let Some(body_b64) = m
                .get("message_content")
                .and_then(|c| c.get("body"))
                .and_then(Value::as_str)
            else {
                continue;
            };
            let dest = m
                .get("destination")
                .and_then(Value::as_str)
                .map(normalize_addr)
                .unwrap_or_default();

            if parse_jettons
                && let Ok(cell) = deserialize_boc_base64(body_b64)
                && let Ok(decoded) = parse_jetton_transfer_body(&cell)
            {
                let amt = i128::try_from(decoded.jetton_amount).unwrap_or(i128::MAX);
                let receiver = decoded.destination.to_raw();
                if !sender.is_empty() {
                    accounts.entry(sender.clone()).or_default().add_jetton(
                        dest.clone(),
                        Some(dest.clone()),
                        -amt,
                    );
                }
                accounts
                    .entry(receiver)
                    .or_default()
                    .add_jetton(dest.clone(), None, amt);
            }

            if parse_nfts && let Some(new_owner) = nft_transfer_new_owner(body_b64) {
                let new_owner_norm = normalize_addr(&new_owner);
                if !dest.is_empty() {
                    if !sender.is_empty() {
                        accounts.entry(sender.clone()).or_default().add_nft(
                            dest.clone(),
                            None,
                            -1,
                            Some(new_owner_norm.clone()),
                        );
                    }
                    accounts.entry(new_owner_norm).or_default().add_nft(
                        dest,
                        None,
                        1,
                        (!sender.is_empty()).then(|| sender.clone()),
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACC_WALLET: &str = "0:1111111111111111111111111111111111111111111111111111111111111111";
    const ACC_PEER: &str = "0:2222222222222222222222222222222222222222222222222222222222222222";
    const ACC_JETTON_MASTER: &str =
        "0:3333333333333333333333333333333333333333333333333333333333333333";
    const ACC_NFT_ITEM: &str = "0:4444444444444444444444444444444444444444444444444444444444444444";

    #[test]
    fn emulate_request_matches_tdesktop_shape() {
        let req = EmulateMessageRequest::new("AAAA").to_http_request();
        assert!(req.post);
        assert_eq!(req.endpoint, "/api/emulate/v1/emulateTrace");
        assert!(req.query.is_empty());
        let val: Value = serde_json::from_slice(&req.payload).unwrap();
        let obj = val.as_object().unwrap();
        assert_eq!(obj.len(), 7);
        assert_eq!(obj["boc"], "AAAA");
        assert_eq!(obj["ignore_chksig"], true);
        assert_eq!(obj["include_code_data"], false);
        assert_eq!(obj["include_address_book"], false);
        assert_eq!(obj["include_metadata"], false);
        assert_eq!(obj["with_actions"], true);
        assert!(obj["mc_block_seqno"].is_null());
    }

    #[test]
    fn parse_ton_connect_emulation_classifies_actions_and_net_nano() {
        let emulation = SendEmulation {
            trace_succeeded: true,
            is_incomplete: false,
            actions: vec![
                SendEmulationAction {
                    kind: "ton_transfer".to_owned(),
                    succeeded: true,
                    details_json: json!({
                        "source": ACC_WALLET,
                        "destination": ACC_PEER,
                        "value": "1000000000"
                    })
                    .to_string(),
                },
                SendEmulationAction {
                    kind: "call_contract".to_owned(),
                    succeeded: true,
                    details_json: json!({
                        "source": ACC_PEER,
                        "destination": ACC_WALLET,
                        "value": "45000000",
                        "opcode": "0xd53276db"
                    })
                    .to_string(),
                },
                SendEmulationAction {
                    kind: "contract_deploy".to_owned(),
                    succeeded: true,
                    details_json: json!({
                        "destination": ACC_NFT_ITEM
                    })
                    .to_string(),
                },
                SendEmulationAction {
                    kind: "unknown".to_owned(),
                    succeeded: true,
                    details_json: "{}".to_owned(),
                },
            ],
        };

        let parsed = parse_ton_connect_emulation(&emulation, ACC_WALLET, 1_000_000_000);
        assert_eq!(parsed.status, TonConnectEmulationStatus::Shown);
        assert_eq!(parsed.actions.len(), 3);
        assert_eq!(parsed.actions[0].kind, TonConnectActionKind::Withdraw);
        assert_eq!(parsed.actions[0].side, TonConnectActionSide::Outgoing);
        assert_eq!(parsed.actions[0].counterparty, ACC_PEER);
        assert_eq!(parsed.actions[1].kind, TonConnectActionKind::Excess);
        assert_eq!(parsed.actions[1].side, TonConnectActionSide::Incoming);
        assert_eq!(parsed.actions[1].amount_nano, Some(45_000_000));
        assert_eq!(parsed.actions[2].kind, TonConnectActionKind::DeployContract);
        assert_eq!(parsed.actions[2].counterparty, ACC_NFT_ITEM);
        // credited (45_000_000) - request_total (1_000_000_000) = -955_000_000
        assert_eq!(parsed.net_nano, -955_000_000);
    }

    #[test]
    fn parse_emulation_response_extracts_fees_deltas_and_warnings() {
        let body = json!({
            "mc_block_seqno": 42000,
            "is_incomplete": false,
            "trace": {
                "tx_hash": "root",
                "children": [{ "tx_hash": "child", "children": [] }]
            },
            "transactions_order": ["root", "child"],
            "transactions": {
                "root": {
                    "account": ACC_WALLET,
                    "total_fees": "4500000",
                    "description": {
                        "type": "ord",
                        "aborted": false,
                        "compute_ph": { "success": true, "exit_code": 0, "gas_fees": "3000000" },
                        "storage_ph": { "storage_fees_collected": "500000" },
                        "action": { "success": true, "result_code": 0, "total_fwd_fees": "1000000" }
                    },
                    "account_state_before": { "balance": "5000000000" },
                    "account_state_after": { "balance": "3995500000" },
                    "out_msgs": [{
                        "source": ACC_WALLET,
                        "destination": ACC_PEER,
                        "value": "1000000000",
                        "bounced": false
                    }]
                },
                "child": {
                    "account": ACC_PEER,
                    "total_fees": "1200000",
                    "description": {
                        "type": "ord",
                        "aborted": false,
                        "compute_ph": { "success": true, "exit_code": 0, "gas_fees": "1000000" },
                        "storage_ph": { "storage_fees_collected": "200000" },
                        "action": { "success": true, "result_code": 0, "total_fwd_fees": "0" }
                    },
                    "account_state_before": { "balance": "2000000000" },
                    "account_state_after": { "balance": "2998800000" },
                    "in_msg": {
                        "source": ACC_WALLET,
                        "destination": ACC_PEER,
                        "value": "1000000000",
                        "bounced": false
                    },
                    "out_msgs": []
                }
            },
            "actions": [
                {
                    "type": "jetton_transfer",
                    "success": true,
                    "details": {
                        "sender": ACC_WALLET,
                        "receiver": ACC_PEER,
                        "asset": ACC_JETTON_MASTER,
                        "amount": "25000000"
                    }
                },
                {
                    "type": "nft_transfer",
                    "success": true,
                    "details": {
                        "old_owner": ACC_WALLET,
                        "new_owner": ACC_PEER,
                        "nft_item": ACC_NFT_ITEM
                    }
                }
            ],
            "metadata": {
                "0:3333333333333333333333333333333333333333333333333333333333333333": {
                    "token_info": [{ "symbol": "USDt", "extra": { "decimals": "6" } }]
                }
            }
        });

        let parsed = parse_emulation_response(&serde_json::to_vec(&body).unwrap()).unwrap();
        assert!(parsed.trace_succeeded);
        assert!(!parsed.has_warnings());
        assert_eq!(parsed.root_fee_nano, 4_500_000);
        assert_eq!(parsed.total_fees_nano, 5_700_000);
        assert_eq!(parsed.total_gas_fees_nano, 4_000_000);
        assert_eq!(parsed.total_storage_fees_nano, 700_000);
        assert_eq!(parsed.total_fwd_fees_nano, 1_000_000);

        let w = parsed.delta_for(ACC_WALLET).unwrap();
        assert_eq!(w.ton_delta_nano, -1_004_500_000);
        assert_eq!(w.jetton_deltas.len(), 1);
        assert_eq!(w.jetton_deltas[0].delta, -25_000_000);
        assert_eq!(w.jetton_deltas[0].symbol.as_deref(), Some("USDt"));
        assert_eq!(w.jetton_deltas[0].decimals, Some(6));
        assert_eq!(w.nft_deltas.len(), 1);
        assert_eq!(w.nft_deltas[0].delta, -1);

        let p = parsed.delta_for(ACC_PEER).unwrap();
        assert_eq!(p.ton_delta_nano, 998_800_000);
        assert_eq!(p.jetton_deltas[0].delta, 25_000_000);
        assert_eq!(p.nft_deltas[0].delta, 1);
    }
}
