//! TON Jetton (TEP-74) and NFT Collectible (TEP-62) transfer BOC builders, amount codecs,
//! and Toncenter v3 `/api/v3/nft/items`, `/api/v3/jetton/wallets`, `/api/v3/jetton/masters`
//! request builders & JSON parsers.
//!
//! Port origin: `Telegram/SourceFiles/gram/api/gram_api_nft.cpp`,
//! `Telegram/SourceFiles/wallet/wallet_collectibles.cpp`,
//! and official TON TEP-62 / TEP-74 smart contract message layouts.
//!
//! Never contacts real TON endpoints in tests; all HTTP tests run against local fakes.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::WalletError;
use crate::address::TonAddress;
use crate::cell::{Cell, CellBuilder};
use crate::contract::{DEFAULT_SEND_MODE, TransferMessage};
use crate::toncenter::{GramHttpRequest, parse_api_error};

/// TEP-74 Jetton `transfer` internal message opcode (`0x0f8a7ea5`).
pub const OP_JETTON_TRANSFER: u32 = 0x0F8A_7EA5;
/// TEP-74 Jetton `internal_transfer` opcode (`0x178d4519`).
pub const OP_JETTON_INTERNAL_TRANSFER: u32 = 0x178D_4519;
/// TEP-74 Jetton `burn` opcode (`0x595f07bc`).
pub const OP_JETTON_BURN: u32 = 0x595F_07BC;
/// TEP-62 NFT `transfer` internal message opcode (`0x5fcc3d14`).
pub const OP_NFT_TRANSFER: u32 = 0x5FCC_3D14;
/// TEP-62 NFT `ownership_assigned` opcode (`0x05138d91`).
pub const OP_NFT_OWNERSHIP_ASSIGNED: u32 = 0x0513_8D91;

/// Default attached TON gas for a TEP-74 Jetton transfer (`0.05 TON`).
pub const DEFAULT_JETTON_GAS_NANO: u64 = 50_000_000;
/// Default forward TON amount for a TEP-74 Jetton transfer (`1 nanoTON` to deliver notification).
pub const DEFAULT_JETTON_FORWARD_NANO: u64 = 1;
/// Default attached TON gas for a TEP-62 NFT transfer (`0.05 TON`).
pub const DEFAULT_NFT_GAS_NANO: u64 = 50_000_000;
/// Default forward TON amount for a TEP-62 NFT transfer (`1 nanoTON` to deliver notification).
pub const DEFAULT_NFT_FORWARD_NANO: u64 = 1;

/// Metadata for a TEP-74 Jetton master contract parsed from `/api/v3/jetton/masters`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct JettonMasterInfo {
    /// Canonical lowercase raw address (`workchain:64-hex`).
    pub address: String,
    /// User-friendly bounceable address (from `address_book`, if present).
    pub friendly_address: String,
    /// Human-readable token name (e.g. `"Tether USD"`).
    pub name: String,
    /// Short ticker symbol (e.g. `"USD₮"`, `"NOT"`).
    pub symbol: String,
    /// Number of decimal places (e.g. `6` for USDT, `9` default).
    pub decimals: u8,
    /// Off-chain metadata JSON URI (`jetton_content.uri`), if present.
    pub uri: String,
    /// Optional token icon URL.
    pub image_url: String,
    /// Raw decimal total supply string.
    pub total_supply: String,
    /// Whether additional supply can be minted by the admin.
    pub mintable: bool,
}

/// Owned TEP-74 Jetton wallet balance parsed from `/api/v3/jetton/wallets`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct JettonWalletBalance {
    /// Canonical lowercase raw address of the owner's jetton wallet contract.
    pub wallet_address: String,
    /// Canonical lowercase raw address of the jetton master contract.
    pub jetton_master: String,
    /// Canonical lowercase raw address of the wallet owner.
    pub owner: String,
    /// Raw balance in smallest jetton units.
    pub balance: u128,
    /// Logical time of the last transaction on this jetton wallet.
    pub last_transaction_lt: u64,
    /// Ticker symbol (resolved from metadata / master info, or fallback).
    pub symbol: String,
    /// Display name (resolved from metadata / master info, or fallback).
    pub name: String,
    /// Number of decimal places (`9` by default until master metadata overrides).
    pub decimals: u8,
    /// User-friendly address of the jetton wallet contract (from `address_book`, if present).
    pub user_friendly_wallet: String,
    /// User-friendly address of the jetton master contract (from `address_book`, if present).
    pub user_friendly_master: String,
}

impl JettonWalletBalance {
    /// Formats [`Self::balance`] using [`Self::decimals`] as a human-readable decimal string.
    #[must_use]
    pub fn formatted_balance(&self) -> String {
        format_jetton_amount(self.balance, self.decimals)
    }

    /// Returns the best available ticker symbol for UI display.
    #[must_use]
    pub fn display_symbol(&self) -> String {
        if !self.symbol.trim().is_empty() {
            return self.symbol.clone();
        }
        if !self.name.trim().is_empty() {
            return self.name.clone();
        }
        short_raw_address(&self.jetton_master)
    }

    /// Applies master metadata (symbol, name, decimals, friendly master address) to this balance.
    pub fn apply_master_info(&mut self, master: &JettonMasterInfo) {
        if !master.symbol.is_empty() {
            self.symbol = master.symbol.clone();
        }
        if !master.name.is_empty() {
            self.name = master.name.clone();
        }
        self.decimals = master.decimals;
        if self.user_friendly_master.is_empty() && !master.friendly_address.is_empty() {
            self.user_friendly_master = master.friendly_address.clone();
        }
    }
}

/// Paginated list of jetton wallets returned by `/api/v3/jetton/wallets`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct JettonWalletsPage {
    /// Jetton wallets on this page.
    pub wallets: Vec<JettonWalletBalance>,
    /// Whether more pages may exist.
    pub has_next: bool,
}

/// Parameters required to build a TEP-74 Jetton `transfer#0f8a7ea5` internal message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JettonTransferParams {
    /// Address of the sender's Jetton wallet contract (destination of the TON internal message).
    pub jetton_wallet: TonAddress,
    /// Recipient TON owner address receiving the Jettons.
    pub destination: TonAddress,
    /// Address receiving excess TON gas refunds (typically the sender's TON wallet).
    pub response_destination: TonAddress,
    /// Jetton transfer amount in smallest units.
    pub jetton_amount: u128,
    /// Arbitrary 64-bit query identifier (`0` if unused).
    pub query_id: u64,
    /// TON gas attached to the internal message sent to `jetton_wallet` (in nanoTON).
    pub attached_ton_nano: u64,
    /// TON amount forwarded to `destination` alongside `transfer_notification` (in nanoTON).
    pub forward_ton_nano: u64,
    /// Optional UTF-8 text comment stored in `forward_payload`.
    pub forward_comment: Option<String>,
}

/// Decoded TEP-74 Jetton transfer payload (for verification & unit tests).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedJettonTransfer {
    /// 64-bit query ID.
    pub query_id: u64,
    /// Jetton amount in smallest units.
    pub jetton_amount: u128,
    /// Recipient owner address.
    pub destination: TonAddress,
    /// Excess gas refund destination address.
    pub response_destination: Option<TonAddress>,
    /// Forward TON notification amount in nanoTON.
    pub forward_ton_nano: u128,
    /// Decoded UTF-8 comment from `forward_payload`, if present.
    pub forward_comment: Option<String>,
}

/// Parameters required to build a TEP-62 NFT `transfer#5fcc3d14` internal message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NftTransferParams {
    /// Address of the NFT item contract (destination of the TON internal message).
    pub nft_item: TonAddress,
    /// Recipient TON owner address receiving the NFT.
    pub new_owner: TonAddress,
    /// Address receiving excess TON gas refunds (typically the sender's TON wallet).
    pub response_destination: TonAddress,
    /// Arbitrary 64-bit query identifier (`0` if unused).
    pub query_id: u64,
    /// TON gas attached to the internal message sent to `nft_item` (in nanoTON).
    pub attached_ton_nano: u64,
    /// TON amount forwarded to `new_owner` alongside `ownership_assigned` (in nanoTON).
    pub forward_ton_nano: u64,
    /// Optional UTF-8 text comment stored in `forward_payload`.
    pub forward_comment: Option<String>,
}

/// Decoded TEP-62 NFT transfer payload (for verification & unit tests).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedNftTransfer {
    /// 64-bit query ID.
    pub query_id: u64,
    /// New owner address.
    pub new_owner: TonAddress,
    /// Excess gas refund destination address.
    pub response_destination: Option<TonAddress>,
    /// Forward TON notification amount in nanoTON.
    pub forward_ton_nano: u128,
    /// Decoded UTF-8 comment from `forward_payload`, if present.
    pub forward_comment: Option<String>,
}

// ============================================================================
// Decimal amount formatting & parsing for arbitrary jetton decimals (0..=18)
// ============================================================================

/// Formats a raw `u128` jetton amount with `decimals` fractional digits, trimming trailing zeros.
#[must_use]
pub fn format_jetton_amount(raw: u128, decimals: u8) -> String {
    let dec = usize::from(decimals.min(18));
    if dec == 0 {
        return raw.to_string();
    }
    let scale = 10_u128.pow(dec as u32);
    let whole = raw / scale;
    let frac = raw % scale;
    if frac == 0 {
        return whole.to_string();
    }
    let frac_str = format!("{frac:0>dec$}");
    let trimmed = frac_str.trim_end_matches('0');
    format!("{whole}.{trimmed}")
}

/// Parses a decimal jetton string (e.g. `"12.50"` with `decimals = 6` -> `12_500_000`) into raw units.
pub fn parse_jetton_amount(input: &str, decimals: u8) -> Result<u128, WalletError> {
    let dec = usize::from(decimals.min(18));
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(WalletError::InvalidAmount("empty jetton amount".to_owned()));
    }
    if trimmed.contains(',') || trimmed.starts_with('-') || trimmed.starts_with('+') {
        return Err(WalletError::InvalidAmount(format!(
            "unsupported jetton amount syntax: {trimmed}"
        )));
    }
    let mut parts = trimmed.split('.');
    let whole_str = parts.next().unwrap_or("");
    let frac_str = parts.next();
    if parts.next().is_some() {
        return Err(WalletError::InvalidAmount(format!(
            "multiple decimal points: {trimmed}"
        )));
    }
    if whole_str.is_empty() || !whole_str.bytes().all(|b| b.is_ascii_digit()) {
        return Err(WalletError::InvalidAmount(format!(
            "invalid integer part: {trimmed}"
        )));
    }
    let whole: u128 = whole_str
        .parse()
        .map_err(|_| WalletError::InvalidAmount(format!("jetton amount overflow: {trimmed}")))?;

    let scale = 10_u128.pow(dec as u32);
    let frac: u128 = match frac_str {
        None => 0,
        Some(f) => {
            if dec == 0 {
                return Err(WalletError::InvalidAmount(format!(
                    "token does not support fractional digits: {trimmed}"
                )));
            }
            if f.is_empty() || f.len() > dec || !f.bytes().all(|b| b.is_ascii_digit()) {
                return Err(WalletError::InvalidAmount(format!(
                    "invalid fractional part (max {dec} digits): {trimmed}"
                )));
            }
            let frac_scale = 10_u128.pow((dec - f.len()) as u32);
            let parsed: u128 = f.parse().map_err(|_| {
                WalletError::InvalidAmount(format!("jetton fraction overflow: {trimmed}"))
            })?;
            parsed.checked_mul(frac_scale).ok_or_else(|| {
                WalletError::InvalidAmount(format!("jetton fraction overflow: {trimmed}"))
            })?
        }
    };

    whole
        .checked_mul(scale)
        .and_then(|v| v.checked_add(frac))
        .ok_or_else(|| WalletError::InvalidAmount(format!("jetton amount overflow: {trimmed}")))
}

// ============================================================================
// TEP-74 Jetton & TEP-62 NFT Transfer Cell / Message Builders
// ============================================================================

/// Builds a TEP-74 Jetton `transfer#0f8a7ea5` internal message body [`Cell`].
///
/// TL-B layout:
/// `transfer#0f8a7ea5 query_id:uint64 amount:(VarUInteger 16) destination:MsgAddress`
/// `response_destination:MsgAddress custom_payload:(Maybe ^Cell)`
/// `forward_ton_amount:(VarUInteger 16) forward_payload:(Either Cell ^Cell)`
pub fn build_jetton_transfer_body(params: &JettonTransferParams) -> Result<Arc<Cell>, WalletError> {
    if params.jetton_amount == 0 {
        return Err(WalletError::InvalidAmount(
            "jetton transfer amount must be greater than zero".to_owned(),
        ));
    }
    let mut b = CellBuilder::new();
    b.store_uint(u64::from(OP_JETTON_TRANSFER), 32)?;
    b.store_uint(params.query_id, 64)?;
    b.store_coins(params.jetton_amount)?;
    b.store_address(&params.destination)?;
    b.store_address(&params.response_destination)?;
    b.store_bit(false)?; // custom_payload: Nothing
    b.store_coins(u128::from(params.forward_ton_nano))?;
    store_forward_comment(&mut b, params.forward_comment.as_deref())?;
    Ok(Arc::new(b.end_cell()?))
}

/// Builds a wallet [`TransferMessage`] that invokes TEP-74 `transfer#0f8a7ea5` on `jetton_wallet`.
pub fn build_jetton_transfer_message(
    params: &JettonTransferParams,
) -> Result<TransferMessage, WalletError> {
    if params.attached_ton_nano == 0 {
        return Err(WalletError::InvalidAmount(
            "jetton transfer requires attached TON gas".to_owned(),
        ));
    }
    let body = build_jetton_transfer_body(params)?;
    Ok(TransferMessage {
        destination: params.jetton_wallet,
        amount_nano: params.attached_ton_nano,
        bounce: true,
        send_mode: DEFAULT_SEND_MODE,
        body: Some(body),
        state_init: None,
    })
}

/// Decodes a TEP-74 Jetton `transfer#0f8a7ea5` body [`Cell`] back into [`DecodedJettonTransfer`].
pub fn parse_jetton_transfer_body(cell: &Cell) -> Result<DecodedJettonTransfer, WalletError> {
    let mut s = cell.begin_parse();
    let op = s.load_uint(32)? as u32;
    if op != OP_JETTON_TRANSFER {
        return Err(WalletError::Contract(format!(
            "unexpected jetton transfer opcode 0x{op:08x}"
        )));
    }
    let query_id = s.load_uint(64)?;
    let jetton_amount = s.load_coins()?;
    let destination = s
        .load_address()?
        .ok_or_else(|| WalletError::Contract("missing jetton destination address".to_owned()))?;
    let response_destination = s.load_address()?;
    let _custom_payload = s.load_maybe_ref()?;
    let forward_ton_nano = s.load_coins()?;
    let forward_comment = load_forward_comment(&mut s)?;
    Ok(DecodedJettonTransfer {
        query_id,
        jetton_amount,
        destination,
        response_destination,
        forward_ton_nano,
        forward_comment,
    })
}

/// Builds a TEP-62 NFT `transfer#5fcc3d14` internal message body [`Cell`].
///
/// TL-B layout:
/// `transfer#5fcc3d14 query_id:uint64 new_owner:MsgAddress response_destination:MsgAddress`
/// `custom_payload:(Maybe ^Cell) forward_amount:(VarUInteger 16) forward_payload:(Either Cell ^Cell)`
pub fn build_nft_transfer_body(params: &NftTransferParams) -> Result<Arc<Cell>, WalletError> {
    let mut b = CellBuilder::new();
    b.store_uint(u64::from(OP_NFT_TRANSFER), 32)?;
    b.store_uint(params.query_id, 64)?;
    b.store_address(&params.new_owner)?;
    b.store_address(&params.response_destination)?;
    b.store_bit(false)?; // custom_payload: Nothing
    b.store_coins(u128::from(params.forward_ton_nano))?;
    store_forward_comment(&mut b, params.forward_comment.as_deref())?;
    Ok(Arc::new(b.end_cell()?))
}

/// Builds a wallet [`TransferMessage`] that invokes TEP-62 `transfer#5fcc3d14` on `nft_item`.
pub fn build_nft_transfer_message(
    params: &NftTransferParams,
) -> Result<TransferMessage, WalletError> {
    if params.attached_ton_nano == 0 {
        return Err(WalletError::InvalidAmount(
            "NFT transfer requires attached TON gas".to_owned(),
        ));
    }
    let body = build_nft_transfer_body(params)?;
    Ok(TransferMessage {
        destination: params.nft_item,
        amount_nano: params.attached_ton_nano,
        bounce: true,
        send_mode: DEFAULT_SEND_MODE,
        body: Some(body),
        state_init: None,
    })
}

/// Decodes a TEP-62 NFT `transfer#5fcc3d14` body [`Cell`] back into [`DecodedNftTransfer`].
pub fn parse_nft_transfer_body(cell: &Cell) -> Result<DecodedNftTransfer, WalletError> {
    let mut s = cell.begin_parse();
    let op = s.load_uint(32)? as u32;
    if op != OP_NFT_TRANSFER {
        return Err(WalletError::Contract(format!(
            "unexpected NFT transfer opcode 0x{op:08x}"
        )));
    }
    let query_id = s.load_uint(64)?;
    let new_owner = s
        .load_address()?
        .ok_or_else(|| WalletError::Contract("missing NFT new_owner address".to_owned()))?;
    let response_destination = s.load_address()?;
    let _custom_payload = s.load_maybe_ref()?;
    let forward_ton_nano = s.load_coins()?;
    let forward_comment = load_forward_comment(&mut s)?;
    Ok(DecodedNftTransfer {
        query_id,
        new_owner,
        response_destination,
        forward_ton_nano,
        forward_comment,
    })
}

fn store_forward_comment(b: &mut CellBuilder, comment: Option<&str>) -> Result<(), WalletError> {
    match comment {
        Some(text) if !text.is_empty() => {
            let mut comment_builder = CellBuilder::new();
            comment_builder.store_text_comment(text)?;
            let comment_cell = Arc::new(comment_builder.end_cell()?);
            b.store_bit(true)?; // Either::Right (^Cell)
            b.store_ref(comment_cell)?;
        }
        _ => {
            b.store_bit(false)?; // Either::Left (empty inline payload)
        }
    }
    Ok(())
}

fn load_forward_comment(s: &mut crate::cell::CellSlice<'_>) -> Result<Option<String>, WalletError> {
    let is_ref = s.load_bit()?;
    if !is_ref {
        if s.remaining_bits() >= 32 {
            let op = s.load_uint(32)?;
            if op == 0 {
                let bytes = read_snake_bytes_from_slice(s)?;
                return Ok(String::from_utf8(bytes).ok());
            }
        }
        return Ok(None);
    }
    let payload_cell = s.load_ref()?;
    let mut ps = payload_cell.begin_parse();
    if ps.remaining_bits() < 32 {
        return Ok(None);
    }
    let op = ps.load_uint(32)?;
    if op != 0 {
        return Ok(None);
    }
    let bytes = read_snake_bytes_from_slice(&mut ps)?;
    Ok(String::from_utf8(bytes).ok())
}

fn read_snake_bytes_from_slice(
    slice: &mut crate::cell::CellSlice<'_>,
) -> Result<Vec<u8>, WalletError> {
    let mut out = Vec::new();
    let here_bytes = slice.remaining_bits() / 8;
    if here_bytes > 0 {
        out.extend_from_slice(&slice.load_bytes(here_bytes)?);
    }
    let mut next_cell = if slice.remaining_refs() > 0 {
        Some(Arc::clone(slice.load_ref()?))
    } else {
        None
    };
    let mut depth_guard = 0_usize;
    while let Some(cell) = next_cell {
        depth_guard += 1;
        if depth_guard > 64 {
            break;
        }
        let mut child_slice = cell.begin_parse();
        let n = child_slice.remaining_bits() / 8;
        if n > 0 {
            out.extend_from_slice(&child_slice.load_bytes(n)?);
        }
        next_cell = if child_slice.remaining_refs() > 0 {
            Some(Arc::clone(child_slice.load_ref()?))
        } else {
            None
        };
    }
    Ok(out)
}

// ============================================================================
// Toncenter v3 Request Builders & JSON Parsers (NFTs & Jettons)
// ============================================================================

/// Builds GET `/api/v3/nft/items` for all NFTs owned by `owner` (`owner_address=...`).
#[must_use]
pub fn nft_items_by_owner_request(owner: &str, limit: usize, offset: usize) -> GramHttpRequest {
    let mut ser = url::form_urlencoded::Serializer::new(String::new());
    ser.append_pair("owner_address", owner);
    ser.append_pair("limit", &limit.max(1).to_string());
    ser.append_pair("offset", &offset.to_string());
    GramHttpRequest {
        post: false,
        endpoint: "/api/v3/nft/items".to_owned(),
        query: ser.finish(),
        payload: Vec::new(),
    }
}

/// Builds GET `/api/v3/nft/items?address=...` (`Gram::NftItemByAddressRequest`).
#[must_use]
pub fn nft_item_by_address_request(item_address: &str) -> GramHttpRequest {
    let mut ser = url::form_urlencoded::Serializer::new(String::new());
    ser.append_pair("address", &item_address.to_ascii_uppercase());
    GramHttpRequest {
        post: false,
        endpoint: "/api/v3/nft/items".to_owned(),
        query: ser.finish(),
        payload: Vec::new(),
    }
}

/// Builds GET `/api/v3/jetton/wallets` for all non-zero Jetton wallets of `owner`.
#[must_use]
pub fn jetton_wallets_by_owner_request(
    owner: &str,
    limit: usize,
    offset: usize,
) -> GramHttpRequest {
    let mut ser = url::form_urlencoded::Serializer::new(String::new());
    ser.append_pair("owner_address", owner);
    ser.append_pair("limit", &limit.max(1).to_string());
    ser.append_pair("offset", &offset.to_string());
    ser.append_pair("exclude_zero_balance", "true");
    GramHttpRequest {
        post: false,
        endpoint: "/api/v3/jetton/wallets".to_owned(),
        query: ser.finish(),
        payload: Vec::new(),
    }
}

/// Builds GET `/api/v3/jetton/masters?address=...` for Jetton master contract metadata.
#[must_use]
pub fn jetton_masters_request(master_address: &str) -> GramHttpRequest {
    let mut ser = url::form_urlencoded::Serializer::new(String::new());
    ser.append_pair("address", master_address);
    GramHttpRequest {
        post: false,
        endpoint: "/api/v3/jetton/masters".to_owned(),
        query: ser.finish(),
        payload: Vec::new(),
    }
}

/// Normalizes a raw `workchain:64-hex` TON address to canonical lowercase, matching
/// `CanonicalRawAddress` in `gram_api_nft.cpp:60-76`.
#[must_use]
pub fn canonical_raw_address(text: &str) -> Option<String> {
    let trimmed = text.trim();
    let (wc_str, hash_str) = trimmed.split_once(':')?;
    let wc: i32 = wc_str.parse().ok()?;
    if hash_str.len() != 64 || !hash_str.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Some(format!("{wc}:{}", hash_str.to_ascii_lowercase()))
}

fn lookup_case_insensitive<'a>(
    map: &'a serde_json::Map<String, Value>,
    key: &str,
) -> Option<&'a Value> {
    map.get(key)
        .or_else(|| map.get(&key.to_ascii_uppercase()))
        .or_else(|| map.get(&key.to_ascii_lowercase()))
        .or_else(|| {
            map.iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(key))
                .map(|(_, v)| v)
        })
}

fn extract_metadata_fields(
    metadata_map: Option<&serde_json::Map<String, Value>>,
    master_raw: &str,
) -> (String, String, Option<u8>, String) {
    let Some(meta_map) = metadata_map else {
        return (String::new(), String::new(), None, String::new());
    };
    let Some(entry) = lookup_case_insensitive(meta_map, master_raw).and_then(Value::as_object)
    else {
        return (String::new(), String::new(), None, String::new());
    };
    let Some(tokens) = entry.get("token_info").and_then(Value::as_array) else {
        return (String::new(), String::new(), None, String::new());
    };
    for token in tokens {
        let Some(obj) = token.as_object() else {
            continue;
        };
        let symbol = obj
            .get("symbol")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let name = obj
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let image = obj
            .get("image")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let decimals = obj
            .get("extra")
            .and_then(|e| e.get("decimals"))
            .or_else(|| obj.get("decimals"))
            .and_then(|v| {
                v.as_u64()
                    .or_else(|| v.as_str().and_then(|s| s.parse::<u64>().ok()))
            })
            .and_then(|d| u8::try_from(d).ok());
        if !symbol.is_empty() || !name.is_empty() || decimals.is_some() {
            return (symbol, name, decimals, image);
        }
    }
    (String::new(), String::new(), None, String::new())
}

/// Parses Toncenter v3 `/api/v3/jetton/wallets` JSON response into [`JettonWalletsPage`].
#[must_use]
pub fn parse_jetton_wallets(json_bytes: &[u8], limit: usize) -> Option<JettonWalletsPage> {
    if parse_api_error(json_bytes).is_some() {
        return None;
    }
    let val: Value = serde_json::from_slice(json_bytes).ok()?;
    let root = val.as_object()?;
    let arr = root.get("jetton_wallets")?.as_array()?;
    let address_book = root.get("address_book").and_then(Value::as_object);
    let metadata = root.get("metadata").and_then(Value::as_object);

    let mut wallets = Vec::with_capacity(arr.len());
    for entry in arr {
        let obj = entry.as_object()?;
        let wallet_raw = obj
            .get("address")
            .and_then(Value::as_str)
            .and_then(canonical_raw_address)?;
        let jetton_master = obj
            .get("jetton")
            .and_then(Value::as_str)
            .and_then(canonical_raw_address)
            .unwrap_or_default();
        let owner = obj
            .get("owner")
            .and_then(Value::as_str)
            .and_then(canonical_raw_address)
            .unwrap_or_default();
        let balance = match obj.get("balance") {
            Some(Value::String(s)) => s.parse::<u128>().ok()?,
            Some(Value::Number(n)) => u128::from(n.as_u64()?),
            _ => return None,
        };
        let last_transaction_lt = obj
            .get("last_transaction_lt")
            .and_then(|v| {
                v.as_u64()
                    .or_else(|| v.as_str().and_then(|s| s.parse::<u64>().ok()))
            })
            .unwrap_or(0);

        let user_friendly_wallet = address_book
            .and_then(|book| lookup_case_insensitive(book, &wallet_raw))
            .and_then(|e| e.get("user_friendly"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let user_friendly_master = address_book
            .and_then(|book| lookup_case_insensitive(book, &jetton_master))
            .and_then(|e| e.get("user_friendly"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let (symbol, name, decimals_opt, _image) =
            extract_metadata_fields(metadata, &jetton_master);

        wallets.push(JettonWalletBalance {
            wallet_address: wallet_raw,
            jetton_master,
            owner,
            balance,
            last_transaction_lt,
            symbol,
            name,
            decimals: decimals_opt.unwrap_or(9),
            user_friendly_wallet,
            user_friendly_master,
        });
    }

    let has_next = limit > 0 && wallets.len() >= limit;
    Some(JettonWalletsPage { wallets, has_next })
}

/// Parses Toncenter v3 `/api/v3/jetton/masters` JSON response into `Vec<JettonMasterInfo>`.
#[must_use]
pub fn parse_jetton_masters(json_bytes: &[u8]) -> Option<Vec<JettonMasterInfo>> {
    if parse_api_error(json_bytes).is_some() {
        return None;
    }
    let val: Value = serde_json::from_slice(json_bytes).ok()?;
    let root = val.as_object()?;
    let arr = root.get("jetton_masters")?.as_array()?;
    let address_book = root.get("address_book").and_then(Value::as_object);
    let metadata = root.get("metadata").and_then(Value::as_object);

    let mut out = Vec::with_capacity(arr.len());
    for entry in arr {
        let obj = entry.as_object()?;
        let address = obj
            .get("address")
            .and_then(Value::as_str)
            .and_then(canonical_raw_address)?;
        let friendly_address = address_book
            .and_then(|book| lookup_case_insensitive(book, &address))
            .and_then(|e| e.get("user_friendly"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();

        let content = obj.get("jetton_content").and_then(Value::as_object);
        let uri = content
            .and_then(|c| c.get("uri"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let content_name = content
            .and_then(|c| c.get("name"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let content_symbol = content
            .and_then(|c| c.get("symbol"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let content_image = content
            .and_then(|c| c.get("image"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let content_decimals = content
            .and_then(|c| c.get("decimals"))
            .and_then(|v| {
                v.as_u64()
                    .or_else(|| v.as_str().and_then(|s| s.parse::<u64>().ok()))
            })
            .and_then(|d| u8::try_from(d).ok());

        let (meta_symbol, meta_name, meta_decimals, meta_image) =
            extract_metadata_fields(metadata, &address);

        let name = if !content_name.is_empty() {
            content_name
        } else {
            meta_name
        };
        let symbol = if !content_symbol.is_empty() {
            content_symbol
        } else {
            meta_symbol
        };
        let image_url = if !content_image.is_empty() {
            content_image
        } else {
            meta_image
        };
        let decimals = content_decimals.or(meta_decimals).unwrap_or(9);

        let total_supply = obj
            .get("total_supply")
            .and_then(|v| {
                v.as_str()
                    .map(ToOwned::to_owned)
                    .or_else(|| v.as_u64().map(|n| n.to_string()))
            })
            .unwrap_or_default();
        let mintable = obj
            .get("mintable")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        out.push(JettonMasterInfo {
            address,
            friendly_address,
            name,
            symbol,
            decimals,
            uri,
            image_url,
            total_supply,
            mintable,
        });
    }
    Some(out)
}

fn short_raw_address(raw: &str) -> String {
    if raw.len() > 14 {
        format!("{}...{}", &raw[..8], &raw[raw.len() - 4..])
    } else {
        raw.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::boc::{deserialize_boc_base64, serialize_boc_base64};
    use crate::contract::{WalletContract, WalletVersion};
    use crate::mnemonic::TonKeypair;

    #[test]
    fn jetton_amount_format_and_parse_roundtrip() {
        assert_eq!(format_jetton_amount(0, 6), "0");
        assert_eq!(format_jetton_amount(1_500_000, 6), "1.5");
        assert_eq!(format_jetton_amount(12_345_678, 6), "12.345678");
        assert_eq!(format_jetton_amount(42, 0), "42");

        assert_eq!(parse_jetton_amount("1.5", 6).unwrap(), 1_500_000);
        assert_eq!(parse_jetton_amount("12.345678", 6).unwrap(), 12_345_678);
        assert_eq!(parse_jetton_amount("42", 0).unwrap(), 42);
        assert!(parse_jetton_amount("1.1234567", 6).is_err());
        assert!(parse_jetton_amount("1.5", 0).is_err());
        assert!(parse_jetton_amount("-1", 6).is_err());
    }

    #[test]
    fn tep74_jetton_transfer_body_and_signed_boc_roundtrip() {
        let jw = TonAddress::new(0, [0x11; 32]);
        let dest = TonAddress::new(0, [0x22; 32]);
        let resp = TonAddress::new(0, [0x33; 32]);
        let params = JettonTransferParams {
            jetton_wallet: jw,
            destination: dest,
            response_destination: resp,
            jetton_amount: 25_750_000, // 25.75 USDT (6 decimals)
            query_id: 9001,
            attached_ton_nano: DEFAULT_JETTON_GAS_NANO,
            forward_ton_nano: DEFAULT_JETTON_FORWARD_NANO,
            forward_comment: Some("Invoice #42".to_owned()),
        };

        let body = build_jetton_transfer_body(&params).unwrap();
        let boc_b64 = serialize_boc_base64(&body).unwrap();
        let decoded_cell = deserialize_boc_base64(&boc_b64).unwrap();
        let decoded = parse_jetton_transfer_body(&decoded_cell).unwrap();
        assert_eq!(decoded.query_id, 9001);
        assert_eq!(decoded.jetton_amount, 25_750_000);
        assert_eq!(decoded.destination, dest);
        assert_eq!(decoded.response_destination, Some(resp));
        assert_eq!(
            decoded.forward_ton_nano,
            u128::from(DEFAULT_JETTON_FORWARD_NANO)
        );
        assert_eq!(decoded.forward_comment.as_deref(), Some("Invoice #42"));

        // Sign with a synthetic test keypair inside WalletContract V5R1.
        let kp = TonKeypair::from_seed([0xA5; 32]);
        let contract = WalletContract::new(WalletVersion::V5R1, 0, kp.public_key_bytes(), false);
        let msg = build_jetton_transfer_message(&params).unwrap();
        assert_eq!(msg.destination, jw);
        assert!(msg.bounce);
        let signed = contract
            .create_signed_transfer(&kp, 4, 1_800_000_000, std::slice::from_ref(&msg))
            .unwrap();
        contract
            .verify_signed_transfer(&signed, 4, 1_800_000_000, std::slice::from_ref(&msg))
            .unwrap();
        assert!(signed.boc_base64.starts_with("te6cc"));
    }

    #[test]
    fn tep62_nft_transfer_body_and_signed_boc_roundtrip() {
        let nft = TonAddress::new(0, [0x44; 32]);
        let new_owner = TonAddress::new(0, [0x55; 32]);
        let resp = TonAddress::new(0, [0x66; 32]);
        let params = NftTransferParams {
            nft_item: nft,
            new_owner,
            response_destination: resp,
            query_id: 777,
            attached_ton_nano: DEFAULT_NFT_GAS_NANO,
            forward_ton_nano: DEFAULT_NFT_FORWARD_NANO,
            forward_comment: Some("Gift transfer".to_owned()),
        };

        let body = build_nft_transfer_body(&params).unwrap();
        let boc_b64 = serialize_boc_base64(&body).unwrap();
        let decoded_cell = deserialize_boc_base64(&boc_b64).unwrap();
        let decoded = parse_nft_transfer_body(&decoded_cell).unwrap();
        assert_eq!(decoded.query_id, 777);
        assert_eq!(decoded.new_owner, new_owner);
        assert_eq!(decoded.response_destination, Some(resp));
        assert_eq!(
            decoded.forward_ton_nano,
            u128::from(DEFAULT_NFT_FORWARD_NANO)
        );
        assert_eq!(decoded.forward_comment.as_deref(), Some("Gift transfer"));

        let kp = TonKeypair::from_seed([0x5A; 32]);
        let contract = WalletContract::new(WalletVersion::V4R2, 0, kp.public_key_bytes(), false);
        let msg = build_nft_transfer_message(&params).unwrap();
        assert_eq!(msg.destination, nft);
        assert!(msg.bounce);
        let signed = contract
            .create_signed_transfer(&kp, 2, 1_800_000_000, std::slice::from_ref(&msg))
            .unwrap();
        contract
            .verify_signed_transfer(&signed, 2, 1_800_000_000, std::slice::from_ref(&msg))
            .unwrap();
    }

    #[test]
    fn parse_toncenter_v3_jetton_wallets_and_masters() {
        let wallets_json = br#"{
            "jetton_wallets": [
                {
                    "address": "0:037F2244CC8F2BE9E795E3943ECA82D78B0F4A599BA6650CE8A3C0AD507D3481",
                    "balance": "1995027351808",
                    "owner": "0:9DA971AF38D2F03ABDF308D5F91636A97E5A2B07A66C39D71D7CBAE3B032EDDC",
                    "jetton": "0:B113A994B5024A16719F69139328EB759596C38A25F59028B146FECDC3621DFE",
                    "last_transaction_lt": "49451091000001"
                }
            ],
            "address_book": {
                "0:037F2244CC8F2BE9E795E3943ECA82D78B0F4A599BA6650CE8A3C0AD507D3481": {
                    "user_friendly": "EQADfyJEzI8r6eeV45Q-yoLXiw9KWZumZQzoo8CtUH00gWhh"
                },
                "0:B113A994B5024A16719F69139328EB759596C38A25F59028B146FECDC3621DFE": {
                    "user_friendly": "EQCxE6mUtQJKFnGfaROTKOt1lZbDiiX1kCixRv7Nw2Id_sDs"
                }
            },
            "metadata": {
                "0:B113A994B5024A16719F69139328EB759596C38A25F59028B146FECDC3621DFE": {
                    "is_indexed": true,
                    "token_info": [
                        {
                            "type": "jetton_masters",
                            "name": "Tether USD",
                            "symbol": "USDt",
                            "extra": { "decimals": "6" }
                        }
                    ]
                }
            }
        }"#;

        let page = parse_jetton_wallets(wallets_json, 20).unwrap();
        assert!(!page.has_next);
        assert_eq!(page.wallets.len(), 1);
        let w = &page.wallets[0];
        assert_eq!(
            w.wallet_address,
            "0:037f2244cc8f2be9e795e3943eca82d78b0f4a599ba6650ce8a3c0ad507d3481"
        );
        assert_eq!(w.balance, 1_995_027_351_808);
        assert_eq!(w.symbol, "USDt");
        assert_eq!(w.decimals, 6);
        assert_eq!(w.formatted_balance(), "1995027.351808");

        let masters_json = br#"{
            "jetton_masters": [
                {
                    "address": "0:B113A994B5024A16719F69139328EB759596C38A25F59028B146FECDC3621DFE",
                    "total_supply": "1429976002510000",
                    "mintable": true,
                    "jetton_content": {
                        "decimals": "6",
                        "name": "Tether USD",
                        "symbol": "USDt",
                        "uri": "https://tether.to/usdt-ton.json"
                    }
                }
            ],
            "address_book": {
                "0:B113A994B5024A16719F69139328EB759596C38A25F59028B146FECDC3621DFE": {
                    "user_friendly": "EQCxE6mUtQJKFnGfaROTKOt1lZbDiiX1kCixRv7Nw2Id_sDs"
                }
            }
        }"#;
        let masters = parse_jetton_masters(masters_json).unwrap();
        assert_eq!(masters.len(), 1);
        assert_eq!(masters[0].decimals, 6);
        assert_eq!(masters[0].symbol, "USDt");
        assert!(masters[0].mintable);
    }
}
