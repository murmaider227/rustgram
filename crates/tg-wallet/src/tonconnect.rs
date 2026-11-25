//! TON Connect 2.0 bridge protocol, NaCl `crypto_box` session encryption, link parsing,
//! TON Proof signing, and connect / `sendTransaction` / `signData` / `disconnect` flows.
//!
//! Port origin: `Telegram/SourceFiles/wallet/wallet_ton_connect.h`,
//! `Telegram/SourceFiles/wallet/wallet_ton_connect_link.cpp`,
//! `Telegram/SourceFiles/wallet/wallet_ton_connect_request.cpp`.
//!
//! All bridge and manifest endpoints are configured externally; tests run strictly against a
//! local in-process fake HTTP/SSE bridge server.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use crypto_box::aead::Aead;
use crypto_box::{PublicKey as BoxPublicKey, SalsaBox, SecretKey as BoxSecretKey};
use ed25519_dalek::{Signature, Signer, VerifyingKey};
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use url::Url;
use zeroize::Zeroizing;

use crate::WalletError;
use crate::address::{TonAddress, canonical_address};
use crate::boc::{deserialize_boc_base64, serialize_boc_base64, text_comment_from_boc};
use crate::cell::CellBuilder;
use crate::contract::WalletContract;
use crate::mnemonic::TonKeypair;
use crate::toncenter::BoxFuture;

/// Maximum query string length accepted by [`parse_ton_connect_link`] (`kMaxQueryLength`).
pub const MAX_TON_CONNECT_QUERY_LEN: usize = 8192;
/// NaCl `crypto_box` nonce size (24 bytes).
pub const NACL_NONCE_SIZE: usize = 24;

/// TON Connect link kind (`Wallet::TonConnectLinkKind` in `wallet_ton_connect_link.h`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum TonConnectLinkKind {
    /// Bare wallet link with no dApp parameters.
    #[default]
    Bare,
    /// Link referencing a pending dApp session by `id`.
    OpenPending,
    /// Full v2 connect link with `v=2&id=<64-hex>&r=<json>`.
    Connect,
}

/// Parsed TON Connect v2 link (`Wallet::TonConnectLink`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TonConnectLink {
    /// Link kind.
    pub kind: TonConnectLinkKind,
    /// Remote dApp X25519 public key (64 hex chars).
    pub client_id: String,
    /// Manifest URL requested by the dApp (`https://...`).
    pub manifest_url: String,
    /// Optional `ton_proof` challenge payload requested by the dApp.
    pub proof_payload: Option<String>,
    /// Optional return strategy (`ret=...`).
    pub ret: String,
    /// Optional UUID trace ID (`trace_id=...`).
    pub trace_id: Option<String>,
}

/// dApp manifest fetched from `manifestUrl` (`Wallet::TonConnectManifest`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DappManifest {
    /// Canonical dApp site URL.
    pub url: String,
    /// Display name of the dApp.
    pub name: String,
    /// Icon URL (if provided).
    #[serde(rename = "iconUrl", default)]
    pub icon_url: String,
}

impl DappManifest {
    /// Extracts the hostname from `self.url` (`Wallet::TonConnectHost`).
    #[must_use]
    pub fn host(&self) -> String {
        Url::parse(&self.url)
            .ok()
            .and_then(|u| u.host_str().map(ToOwned::to_owned))
            .unwrap_or_default()
    }
}

/// Lifecycle state of a TON Connect session (`Wallet::TonConnectSessionStatus`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum TonConnectSessionStatus {
    /// Awaiting user approval.
    #[default]
    Pending,
    /// Connected and exchanging encrypted bridge messages.
    Active,
    /// Disconnect in progress.
    Closing,
    /// Disconnected.
    Closed,
}

/// Error codes defined by the TON Connect 2.0 specification (`Wallet::TonConnectError`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(i32)]
pub enum TonConnectErrorCode {
    /// Generic/unknown error (`0`).
    Unknown = 0,
    /// Malformed request (`1`).
    BadRequest = 1,
    /// Manifest could not be fetched (`2`).
    AppManifestNotFound = 2,
    /// Manifest JSON was invalid (`3`).
    AppManifestContentError = 3,
    /// Unknown app (`100`).
    UnknownApp = 100,
    /// User rejected the request (`300`).
    UserDeclined = 300,
    /// Unsupported RPC method (`400`).
    MethodNotSupported = 400,
}

/// Single outgoing message inside a TON Connect `sendTransaction` request (`Wallet::TonConnectMessage`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TonConnectTxMessage {
    /// Destination address string (raw or user-friendly).
    pub address: String,
    /// Amount in nanoTON as decimal string or parsed `u64`.
    pub amount_nano: u64,
    /// Optional base64 payload BOC.
    pub payload_boc: Option<String>,
    /// Decoded plain text comment if `payload_boc` is a zero-opcode comment.
    pub decoded_comment: Option<String>,
    /// Optional base64 `StateInit` BOC.
    pub state_init_boc: Option<String>,
}

/// Parsed `sendTransaction` payload (`Wallet::TonConnectTransfer`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TonConnectSendTxRequest {
    /// JSON-RPC request ID.
    pub request_id: String,
    /// Expiration unix timestamp (seconds).
    pub valid_until: Option<u64>,
    /// Target chain (`"-239"` mainnet, `"-3"` testnet).
    pub network: Option<String>,
    /// Optional sender address assertion.
    pub from: Option<String>,
    /// Messages requested by the dApp.
    pub messages: Vec<TonConnectTxMessage>,
}

impl TonConnectSendTxRequest {
    /// Total nanoTON summed across all messages.
    #[must_use]
    pub fn total_nano(&self) -> u64 {
        self.messages
            .iter()
            .fold(0_u64, |acc, m| acc.saturating_add(m.amount_nano))
    }
}

/// Parsed `signData` payload (`Wallet::TonConnectSignData`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TonConnectSignDataRequest {
    /// JSON-RPC request ID.
    pub request_id: String,
    /// Payload kind: `"text"`, `"binary"`, or `"cell"`.
    pub kind: String,
    /// UTF-8 text, base64 bytes, or base64 BOC cell.
    pub payload: String,
    /// Optional TL-B schema for cell signing.
    pub schema: Option<String>,
}

impl TonConnectSignDataRequest {
    /// Converts this request into a validated typed [`SignDataPayload`].
    pub fn to_sign_data_payload(&self) -> Result<SignDataPayload, WalletError> {
        match self.kind.as_str() {
            "text" => Ok(SignDataPayload::Text {
                text: self.payload.clone(),
                network: None,
                from: None,
            }),
            "binary" => {
                STANDARD.decode(self.payload.trim()).map_err(|e| {
                    WalletError::TonConnect(format!("invalid signData binary base64: {e}"))
                })?;
                Ok(SignDataPayload::Binary {
                    bytes: self.payload.clone(),
                    network: None,
                    from: None,
                })
            }
            "cell" => {
                let schema = self
                    .schema
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| {
                        WalletError::TonConnect("signData cell payload missing schema".to_owned())
                    })?
                    .to_owned();
                deserialize_boc_base64(&self.payload)?;
                Ok(SignDataPayload::Cell {
                    schema,
                    cell: self.payload.clone(),
                    network: None,
                    from: None,
                })
            }
            other => Err(WalletError::TonConnect(format!(
                "unsupported signData payload type: {other}"
            ))),
        }
    }
}

/// Typed TON Connect 2.0 `signData` payload (`text`, `binary`, or `cell`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum SignDataPayload {
    /// Plain UTF-8 text signed with the `txt` off-chain prefix.
    Text {
        /// UTF-8 text message to sign.
        text: String,
        /// Optional target network (`"-239"` mainnet, `"-3"` testnet).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        network: Option<String>,
        /// Optional expected signer address.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<String>,
    },
    /// Arbitrary base64-encoded binary payload signed with the `bin` off-chain prefix.
    Binary {
        /// Standard base64-encoded payload bytes.
        bytes: String,
        /// Optional target network.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        network: Option<String>,
        /// Optional expected signer address.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<String>,
    },
    /// TVM cell payload wrapped with prefix `0x75569022` and `crc32(schema)` for on-chain checks.
    Cell {
        /// TL-B schema string describing `cell`.
        schema: String,
        /// Standard base64-encoded BOC cell.
        cell: String,
        /// Optional target network.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        network: Option<String>,
        /// Optional expected signer address.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<String>,
    },
}

/// Signed TON Connect 2.0 `signData` response payload (`SignDataResponseSuccess.result`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignDataResult {
    /// Base64-encoded 64-byte Ed25519 signature.
    pub signature: String,
    /// Signer wallet address in canonical raw format (`workchain:64-hex`).
    pub address: String,
    /// Unix timestamp (seconds) bound into the signature.
    pub timestamp: u64,
    /// dApp domain bound into the signature.
    pub domain: String,
    /// Signed payload (`text`, `binary`, or `cell`).
    pub payload: SignDataPayload,
}

/// Decrypted incoming dApp request (`Wallet::TonConnectAppRequest`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TonConnectAppRequest {
    /// `sendTransaction` request.
    SendTransaction(TonConnectSendTxRequest),
    /// `signData` request.
    SignData(TonConnectSignDataRequest),
    /// `disconnect` notification from dApp.
    Disconnect {
        /// Request ID.
        request_id: String,
    },
    /// Unsupported method.
    Unsupported {
        /// Request ID.
        request_id: String,
        /// Method name.
        method: String,
    },
}

/// NaCl `crypto_box` (`X25519` + `XSalsa20-Poly1305`) session keypair for TON Connect 2.0.
#[derive(Clone)]
pub struct TonConnectKeypair {
    secret: Zeroizing<[u8; 32]>,
    public: [u8; 32],
}

impl std::fmt::Debug for TonConnectKeypair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TonConnectKeypair")
            .field("client_id", &self.client_id())
            .finish_non_exhaustive()
    }
}

impl TonConnectKeypair {
    /// Generates a fresh random X25519 keypair.
    #[must_use]
    pub fn generate() -> Self {
        let sk = BoxSecretKey::generate(&mut OsRng);
        let pk = sk.public_key();
        Self {
            secret: Zeroizing::new(sk.to_bytes()),
            public: *pk.as_bytes(),
        }
    }

    /// Constructs a keypair from a 32-byte X25519 secret scalar.
    #[must_use]
    pub fn from_secret_bytes(secret: [u8; 32]) -> Self {
        let sk = BoxSecretKey::from(secret);
        let pk = sk.public_key();
        Self {
            secret: Zeroizing::new(secret),
            public: *pk.as_bytes(),
        }
    }

    /// 32-byte X25519 public key.
    #[must_use]
    pub const fn public_key_bytes(&self) -> [u8; 32] {
        self.public
    }

    /// 32-byte X25519 secret key.
    #[must_use]
    pub fn secret_key_bytes(&self) -> &[u8; 32] {
        &self.secret
    }

    /// 64-character lowercase hex `client_id` used on the TON Connect HTTP bridge.
    #[must_use]
    pub fn client_id(&self) -> String {
        hex::encode(self.public)
    }
}

/// Active or pending TON Connect 2.0 session with NaCl box session encryption.
#[derive(Debug, Clone)]
pub struct TonConnectSession {
    /// Local wallet X25519 keypair.
    pub wallet_keypair: TonConnectKeypair,
    /// Remote dApp 32-byte X25519 public key.
    pub dapp_public_key: [u8; 32],
    /// Verified or pending dApp manifest.
    pub manifest: DappManifest,
    /// Session status.
    pub status: TonConnectSessionStatus,
    /// Last SSE bridge event ID processed.
    pub last_event_id: Option<String>,
}

impl TonConnectSession {
    /// Creates a new session paired with a dApp's 64-hex `client_id`.
    pub fn new(
        wallet_keypair: TonConnectKeypair,
        dapp_client_id: &str,
        manifest: DappManifest,
    ) -> Result<Self, WalletError> {
        let dapp_public_key = parse_client_id(dapp_client_id)?;
        Ok(Self {
            wallet_keypair,
            dapp_public_key,
            manifest,
            status: TonConnectSessionStatus::Pending,
            last_event_id: None,
        })
    }

    /// Local wallet `client_id` (64 hex chars).
    #[must_use]
    pub fn wallet_client_id(&self) -> String {
        self.wallet_keypair.client_id()
    }

    /// Remote dApp `client_id` (64 hex chars).
    #[must_use]
    pub fn dapp_client_id(&self) -> String {
        hex::encode(self.dapp_public_key)
    }

    /// Encrypts `plaintext` using TON Connect's NaCl box framing (`base64(nonce_24 || ciphertext)`).
    pub fn encrypt_message(&self, plaintext: &[u8]) -> Result<String, WalletError> {
        let mut nonce = [0_u8; NACL_NONCE_SIZE];
        OsRng.fill_bytes(&mut nonce);
        self.encrypt_message_with_nonce(plaintext, &nonce)
    }

    /// Deterministic NaCl box encryption with explicit 24-byte nonce (used by KATs).
    pub fn encrypt_message_with_nonce(
        &self,
        plaintext: &[u8],
        nonce: &[u8; NACL_NONCE_SIZE],
    ) -> Result<String, WalletError> {
        let box_cipher = SalsaBox::new(
            &BoxPublicKey::from(self.dapp_public_key),
            &BoxSecretKey::from(*self.wallet_keypair.secret_key_bytes()),
        );
        let ct = box_cipher
            .encrypt(nonce.into(), plaintext)
            .map_err(|_| WalletError::TonConnect("NaCl box encrypt failed".to_owned()))?;
        let mut combined = Vec::with_capacity(NACL_NONCE_SIZE + ct.len());
        combined.extend_from_slice(nonce);
        combined.extend_from_slice(&ct);
        Ok(STANDARD.encode(combined))
    }

    /// Decrypts a base64-encoded TON Connect bridge payload (`nonce_24 || ciphertext`).
    pub fn decrypt_message(&self, encoded_base64: &str) -> Result<Vec<u8>, WalletError> {
        let raw = STANDARD
            .decode(encoded_base64.trim())
            .map_err(|e| WalletError::TonConnect(format!("invalid base64 payload: {e}")))?;
        if raw.len() <= NACL_NONCE_SIZE {
            return Err(WalletError::TonConnect(
                "encrypted payload too short".to_owned(),
            ));
        }
        let (nonce_bytes, ct) = raw.split_at(NACL_NONCE_SIZE);
        let box_cipher = SalsaBox::new(
            &BoxPublicKey::from(self.dapp_public_key),
            &BoxSecretKey::from(*self.wallet_keypair.secret_key_bytes()),
        );
        box_cipher
            .decrypt(nonce_bytes.into(), ct)
            .map_err(|_| WalletError::TonConnect("NaCl box decrypt/auth failed".to_owned()))
    }

    /// Builds an encrypted `connect` event JSON response (including optional `ton_proof`).
    pub fn build_connect_event(
        &self,
        event_id: u64,
        contract: &WalletContract,
        keypair: &TonKeypair,
        proof_payload: Option<&str>,
        timestamp: u64,
    ) -> Result<String, WalletError> {
        let addr = contract.address()?;
        let init_cell = contract.state_init_cell()?;
        let state_init = serialize_boc_base64(&init_cell)?;
        let network = if contract.testnet { "-3" } else { "-239" };

        let mut items = vec![json!({
            "name": "ton_addr",
            "address": addr.to_raw(),
            "network": network,
            "walletStateInit": state_init,
            "publicKey": hex::encode(contract.public_key),
        })];

        if let Some(payload) = proof_payload {
            let domain = self.manifest.host();
            if !ton_connect_proof_domain_allowed(&domain, "") {
                return Err(WalletError::TonConnect(format!(
                    "TON Proof domain '{domain}' is not allowed"
                )));
            }
            let sig = sign_ton_proof(keypair, &addr, &domain, timestamp, payload);
            items.push(json!({
                "name": "ton_proof",
                "proof": {
                    "timestamp": timestamp,
                    "domain": {
                        "lengthBytes": domain.len(),
                        "value": domain,
                    },
                    "payload": payload,
                    "signature": STANDARD.encode(sig),
                }
            }));
        }

        let event = json!({
            "event": "connect",
            "id": event_id,
            "payload": {
                "items": items,
                "device": {
                    "platform": "windows",
                    "appName": "Rustgram",
                    "appVersion": "0.1.0",
                    "maxProtocolVersion": 2,
                    "features": [
                        "SendTransaction",
                        { "name": "SendTransaction", "maxMessages": contract.version.max_messages() },
                        { "name": "SignData", "types": ["text", "binary", "cell"] }
                    ],
                }
            }
        });
        let bytes = serde_json::to_vec(&event)
            .map_err(|e| WalletError::TonConnect(format!("serialize connect event: {e}")))?;
        self.encrypt_message(&bytes)
    }

    /// Builds an encrypted `connect_error` event response.
    pub fn build_connect_error(
        &self,
        event_id: u64,
        code: TonConnectErrorCode,
        message: &str,
    ) -> Result<String, WalletError> {
        let event = json!({
            "event": "connect_error",
            "id": event_id,
            "payload": {
                "code": code as i32,
                "message": message,
            }
        });
        let bytes = serde_json::to_vec(&event)
            .map_err(|e| WalletError::TonConnect(format!("serialize connect_error: {e}")))?;
        self.encrypt_message(&bytes)
    }

    /// Builds an encrypted `sendTransaction` success response carrying the signed BOC base64.
    pub fn build_tx_success_response(
        &self,
        request_id: &str,
        signed_boc_base64: &str,
    ) -> Result<String, WalletError> {
        let resp = json!({
            "id": request_id,
            "result": signed_boc_base64,
        });
        let bytes = serde_json::to_vec(&resp)
            .map_err(|e| WalletError::TonConnect(format!("serialize tx response: {e}")))?;
        self.encrypt_message(&bytes)
    }

    /// Builds an encrypted `signData` success response carrying the [`SignDataResult`].
    pub fn build_sign_data_success_response(
        &self,
        request_id: &str,
        result: &SignDataResult,
    ) -> Result<String, WalletError> {
        let resp = json!({
            "id": request_id,
            "result": result,
        });
        let bytes = serde_json::to_vec(&resp)
            .map_err(|e| WalletError::TonConnect(format!("serialize signData response: {e}")))?;
        self.encrypt_message(&bytes)
    }

    /// Builds an encrypted RPC error response for a declined or invalid request.
    pub fn build_error_response(
        &self,
        request_id: &str,
        code: TonConnectErrorCode,
        message: &str,
    ) -> Result<String, WalletError> {
        let resp = json!({
            "id": request_id,
            "error": {
                "code": code as i32,
                "message": message,
            }
        });
        let bytes = serde_json::to_vec(&resp)
            .map_err(|e| WalletError::TonConnect(format!("serialize error response: {e}")))?;
        self.encrypt_message(&bytes)
    }

    /// Builds an encrypted `disconnect` event.
    pub fn build_disconnect_event(&self, event_id: u64) -> Result<String, WalletError> {
        let event = json!({
            "event": "disconnect",
            "id": event_id,
            "payload": {}
        });
        let bytes = serde_json::to_vec(&event)
            .map_err(|e| WalletError::TonConnect(format!("serialize disconnect event: {e}")))?;
        self.encrypt_message(&bytes)
    }

    /// Decrypts and parses an incoming encrypted dApp JSON-RPC request.
    pub fn decrypt_app_request(
        &self,
        encoded_base64: &str,
    ) -> Result<TonConnectAppRequest, WalletError> {
        let plain = self.decrypt_message(encoded_base64)?;
        parse_app_request_json(&plain)
    }
}

/// Parses a decrypted TON Connect JSON-RPC request (`sendTransaction`, `signData`, `disconnect`).
pub fn parse_app_request_json(json_bytes: &[u8]) -> Result<TonConnectAppRequest, WalletError> {
    let val: Value = serde_json::from_slice(json_bytes)
        .map_err(|e| WalletError::TonConnect(format!("invalid request JSON: {e}")))?;
    let obj = val
        .as_object()
        .ok_or_else(|| WalletError::TonConnect("expected JSON object".to_owned()))?;
    let request_id = obj
        .get("id")
        .and_then(|v| {
            v.as_str()
                .map(ToOwned::to_owned)
                .or_else(|| v.as_u64().map(|n| n.to_string()))
        })
        .ok_or_else(|| WalletError::TonConnect("missing request id".to_owned()))?;
    let method = obj
        .get("method")
        .and_then(Value::as_str)
        .ok_or_else(|| WalletError::TonConnect("missing request method".to_owned()))?;

    match method {
        "disconnect" => Ok(TonConnectAppRequest::Disconnect { request_id }),
        "sendTransaction" => {
            let param_str = obj
                .get("params")
                .and_then(Value::as_array)
                .and_then(|arr| arr.first())
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    WalletError::TonConnect("sendTransaction missing params[0]".to_owned())
                })?;
            let tx_val: Value = serde_json::from_str(param_str)
                .map_err(|e| WalletError::TonConnect(format!("invalid tx params JSON: {e}")))?;
            let tx_obj = tx_val
                .as_object()
                .ok_or_else(|| WalletError::TonConnect("tx params not an object".to_owned()))?;
            let valid_until = tx_obj.get("valid_until").and_then(Value::as_u64);
            let network = tx_obj
                .get("network")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            let from = tx_obj
                .get("from")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            let raw_msgs = tx_obj
                .get("messages")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    WalletError::TonConnect("sendTransaction missing messages".to_owned())
                })?;
            let mut messages = Vec::with_capacity(raw_msgs.len());
            for item in raw_msgs {
                let m = item
                    .as_object()
                    .ok_or_else(|| WalletError::TonConnect("message is not object".to_owned()))?;
                let address = m
                    .get("address")
                    .and_then(Value::as_str)
                    .ok_or_else(|| WalletError::TonConnect("message missing address".to_owned()))?
                    .to_owned();
                let amount_nano = m
                    .get("amount")
                    .and_then(|v| {
                        v.as_str()
                            .and_then(|s| s.parse::<u64>().ok())
                            .or_else(|| v.as_u64())
                    })
                    .ok_or_else(|| WalletError::TonConnect("message missing amount".to_owned()))?;
                let payload_boc = m
                    .get("payload")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(ToOwned::to_owned);
                let decoded_comment = payload_boc.as_deref().and_then(text_comment_from_boc);
                let state_init_boc = m
                    .get("stateInit")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(ToOwned::to_owned);
                messages.push(TonConnectTxMessage {
                    address,
                    amount_nano,
                    payload_boc,
                    decoded_comment,
                    state_init_boc,
                });
            }
            Ok(TonConnectAppRequest::SendTransaction(
                TonConnectSendTxRequest {
                    request_id,
                    valid_until,
                    network,
                    from,
                    messages,
                },
            ))
        }
        "signData" => {
            let param_str = obj
                .get("params")
                .and_then(Value::as_array)
                .and_then(|arr| arr.first())
                .and_then(Value::as_str)
                .ok_or_else(|| WalletError::TonConnect("signData missing params[0]".to_owned()))?;
            let sd_val: Value = serde_json::from_str(param_str)
                .map_err(|e| WalletError::TonConnect(format!("invalid signData JSON: {e}")))?;
            let kind = sd_val
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("text")
                .to_owned();
            let payload = sd_val
                .get("text")
                .or_else(|| sd_val.get("bytes"))
                .or_else(|| sd_val.get("cell"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            let schema = sd_val
                .get("schema")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            Ok(TonConnectAppRequest::SignData(TonConnectSignDataRequest {
                request_id,
                kind,
                payload,
                schema,
            }))
        }
        other => Ok(TonConnectAppRequest::Unsupported {
            request_id,
            method: other.to_owned(),
        }),
    }
}

/// Computes the TON Connect 2.0 `ton_proof` Ed25519 signature per the official spec.
#[must_use]
pub fn sign_ton_proof(
    keypair: &TonKeypair,
    address: &TonAddress,
    domain: &str,
    timestamp: u64,
    payload: &str,
) -> [u8; 64] {
    let mut message = Vec::new();
    message.extend_from_slice(b"ton-proof-item-v2/");
    message.extend_from_slice(&i32::from(address.workchain).to_be_bytes());
    message.extend_from_slice(&address.hash);
    message.extend_from_slice(&(domain.len() as u32).to_le_bytes());
    message.extend_from_slice(domain.as_bytes());
    message.extend_from_slice(&timestamp.to_le_bytes());
    message.extend_from_slice(payload.as_bytes());
    let msg_hash = Sha256::digest(&message);

    let mut full = Vec::with_capacity(2 + 11 + 32);
    full.extend_from_slice(&[0xFF, 0xFF]);
    full.extend_from_slice(b"ton-connect");
    full.extend_from_slice(&msg_hash);
    let full_hash = Sha256::digest(&full);

    keypair.signing_key().sign(&full_hash).to_bytes()
}

/// Magic 32-bit prefix stored at the start of a TON Connect `signData` `cell` envelope (`0x75569022`).
pub const SIGN_DATA_CELL_PREFIX: u32 = 0x7556_9022;

const SIGN_DATA_OFFCHAIN_PREFIX: &[u8] = b"ton-connect/sign-data/";
const MAX_DNS_ENCODED_LEN: usize = 126;

/// Computes standard IEEE 802.3 CRC-32 (reflected polynomial `0xEDB88320`) used by TON Connect
/// `signData` cell schema hashing.
#[must_use]
pub fn crc32_ieee(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFF_u32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            if (crc & 1) != 0 {
                crc = (crc >> 1) ^ 0xEDB8_8320;
            } else {
                crc >>= 1;
            }
        }
    }
    !crc
}

/// Encodes a domain name into TON DNS label format (TEP-81): reversed dot-separated labels,
/// each terminated by a `0x00` byte (e.g. `"example.com"` -> `b"com\0example\0"`).
pub fn encode_dns_name(domain: &str) -> Result<Vec<u8>, WalletError> {
    let trimmed = domain.trim().trim_end_matches('.');
    if trimmed.is_empty() {
        return Err(WalletError::TonConnect(
            "signData domain cannot be empty".to_owned(),
        ));
    }
    let labels: Vec<&str> = trimmed.split('.').collect();
    let mut out = Vec::with_capacity(trimmed.len() + 1);
    for label in labels.into_iter().rev() {
        if label.is_empty() {
            return Err(WalletError::TonConnect(format!(
                "invalid empty DNS label in domain: {domain}"
            )));
        }
        if label.bytes().any(|b| b <= 0x20 || b >= 0x7F) {
            return Err(WalletError::TonConnect(format!(
                "invalid character in domain: {domain}"
            )));
        }
        out.extend_from_slice(label.as_bytes());
        out.push(0);
    }
    if out.len() > MAX_DNS_ENCODED_LEN {
        return Err(WalletError::TonConnect(format!(
            "encoded DNS domain length {} exceeds {MAX_DNS_ENCODED_LEN}",
            out.len()
        )));
    }
    Ok(out)
}

/// Computes the 32-byte digest signed by TON Connect 2.0 `signData` for `text`, `binary`, or
/// `cell` payloads, binding `address`, `domain`, and `timestamp`.
pub fn sign_data_hash(
    address: &TonAddress,
    domain: &str,
    timestamp: u64,
    payload: &SignDataPayload,
) -> Result<[u8; 32], WalletError> {
    if domain.is_empty() {
        return Err(WalletError::TonConnect(
            "signData domain cannot be empty".to_owned(),
        ));
    }
    match payload {
        SignDataPayload::Text { text, .. } => Ok(text_binary_sign_data_hash(
            address,
            domain,
            timestamp,
            b"txt",
            text.as_bytes(),
        )),
        SignDataPayload::Binary { bytes, .. } => {
            let decoded = STANDARD
                .decode(bytes.trim())
                .map_err(|e| WalletError::TonConnect(format!("invalid binary base64: {e}")))?;
            Ok(text_binary_sign_data_hash(
                address, domain, timestamp, b"bin", &decoded,
            ))
        }
        SignDataPayload::Cell { schema, cell, .. } => {
            if schema.is_empty() {
                return Err(WalletError::TonConnect(
                    "signData cell schema cannot be empty".to_owned(),
                ));
            }
            let schema_hash = crc32_ieee(schema.as_bytes());
            let dns_bytes = encode_dns_name(domain)?;
            let mut domain_builder = CellBuilder::new();
            domain_builder.store_snake_bytes(&dns_bytes)?;
            let domain_cell = Arc::new(domain_builder.end_cell()?);

            let payload_cell = deserialize_boc_base64(cell)?;

            let mut root = CellBuilder::new();
            root.store_uint(u64::from(SIGN_DATA_CELL_PREFIX), 32)?;
            root.store_uint(u64::from(schema_hash), 32)?;
            root.store_uint(timestamp, 64)?;
            root.store_address(address)?;
            root.store_ref(domain_cell)?;
            root.store_ref(payload_cell)?;
            Ok(root.end_cell()?.hash())
        }
    }
}

fn text_binary_sign_data_hash(
    address: &TonAddress,
    domain: &str,
    timestamp: u64,
    type_prefix: &[u8; 3],
    content: &[u8],
) -> [u8; 32] {
    let mut message = Vec::with_capacity(
        2 + SIGN_DATA_OFFCHAIN_PREFIX.len() + 4 + 32 + 4 + domain.len() + 8 + 3 + 4 + content.len(),
    );
    message.extend_from_slice(&[0xFF, 0xFF]);
    message.extend_from_slice(SIGN_DATA_OFFCHAIN_PREFIX);
    message.extend_from_slice(&i32::from(address.workchain).to_be_bytes());
    message.extend_from_slice(&address.hash);
    message.extend_from_slice(&(domain.len() as u32).to_be_bytes());
    message.extend_from_slice(domain.as_bytes());
    message.extend_from_slice(&timestamp.to_be_bytes());
    message.extend_from_slice(type_prefix);
    message.extend_from_slice(&(content.len() as u32).to_be_bytes());
    message.extend_from_slice(content);

    let digest = Sha256::digest(&message);
    let mut out = [0_u8; 32];
    out.copy_from_slice(&digest);
    out
}

/// Signs a TON Connect 2.0 `signData` payload (`text`, `binary`, or `cell`) with the wallet's
/// Ed25519 keypair, binding `address`, `domain`, and `timestamp`.
pub fn sign_ton_connect_data(
    keypair: &TonKeypair,
    address: &TonAddress,
    domain: &str,
    timestamp: u64,
    payload: &SignDataPayload,
) -> Result<SignDataResult, WalletError> {
    let hash = sign_data_hash(address, domain, timestamp, payload)?;
    let sig = keypair.signing_key().sign(&hash).to_bytes();
    Ok(SignDataResult {
        signature: STANDARD.encode(sig),
        address: address.to_raw(),
        timestamp,
        domain: domain.to_owned(),
        payload: payload.clone(),
    })
}

/// Verifies a [`SignDataResult`] Ed25519 signature against `public_key` and its bound fields.
pub fn verify_ton_connect_data(
    public_key: &[u8; 32],
    result: &SignDataResult,
) -> Result<(), WalletError> {
    let parsed_addr = TonAddress::parse(&result.address)?.address;
    verify_ton_connect_data_against(
        public_key,
        &parsed_addr,
        &result.domain,
        result.timestamp,
        &result.payload,
        &result.signature,
    )
}

/// Verifies a base64 Ed25519 `signData` signature against explicit expected `public_key`,
/// `address`, `domain`, `timestamp`, and `payload`.
pub fn verify_ton_connect_data_against(
    public_key: &[u8; 32],
    address: &TonAddress,
    domain: &str,
    timestamp: u64,
    payload: &SignDataPayload,
    signature_base64: &str,
) -> Result<(), WalletError> {
    let hash = sign_data_hash(address, domain, timestamp, payload)?;
    let sig_bytes = STANDARD
        .decode(signature_base64.trim())
        .map_err(|e| WalletError::TonConnect(format!("invalid signData signature base64: {e}")))?;
    let signature = Signature::from_slice(&sig_bytes)
        .map_err(|e| WalletError::TonConnect(format!("invalid Ed25519 signature length: {e}")))?;
    let verifying = VerifyingKey::from_bytes(public_key)
        .map_err(|e| WalletError::TonConnect(format!("invalid Ed25519 public key: {e}")))?;
    verifying
        .verify_strict(&hash, &signature)
        .map_err(|_| WalletError::TonConnect("signData signature verification failed".to_owned()))
}

/// Validates a TON Connect JSON-RPC request ID (`Wallet::TonConnectRequestIdValid` in
/// `wallet_ton_connect.cpp:376-383`): non-empty, `<= 100` bytes, printable ASCII `0x20..=0x7E`.
#[must_use]
pub fn ton_connect_request_id_valid(id: &str) -> bool {
    !id.is_empty() && id.len() <= 100 && id.bytes().all(|b| (0x20..=0x7E).contains(&b))
}

/// Checks whether a dApp domain is allowed for TON Proof / signData
/// (`Wallet::TonConnectProofDomainAllowed` in `wallet_ton_connect.cpp:389-404`).
#[must_use]
pub fn ton_connect_proof_domain_allowed(domain: &str, ownership_domain: &str) -> bool {
    let normalize = |value: &str| value.to_ascii_lowercase().trim_end_matches('.').to_owned();
    let normalized = normalize(domain);
    let ownership = normalize(ownership_domain);
    !normalized.is_empty()
        && normalized != "telegram.org"
        && !normalized.ends_with(".telegram.org")
        && (ownership.is_empty() || normalized != ownership)
}

/// Extracts a normalized lowercase hostname (without trailing dots) from a strict `https://`
/// TON Connect manifest URL, returning `None` if the URL is invalid or non-HTTPS.
#[must_use]
pub fn manifest_url_host(manifest_url: &str) -> Option<String> {
    if !valid_https_url(manifest_url) {
        return None;
    }
    let parsed = Url::parse(manifest_url).ok()?;
    let host = parsed
        .host_str()?
        .to_ascii_lowercase()
        .trim_end_matches('.')
        .to_owned();
    (!host.is_empty()).then_some(host)
}

/// Returns `true` when `manifest.url` resolves to the exact same normalized hostname as
/// `manifest_url`, preventing a manifest hosted on an attacker domain from claiming another
/// site's hostname for TON Proof or `signData`.
#[must_use]
pub fn ton_connect_manifest_hosts_match(manifest_url: &str, manifest: &DappManifest) -> bool {
    let Some(url_host) = manifest_url_host(manifest_url) else {
        return false;
    };
    let manifest_host = manifest
        .host()
        .to_ascii_lowercase()
        .trim_end_matches('.')
        .to_owned();
    !manifest_host.is_empty() && manifest_host == url_host
}

// ============================================================================
// Persistent TON Connect claim store (`wallet_ton_connect_claims.h/.cpp`)
// ============================================================================

/// Maximum claim records retained in [`TonConnectClaimStore`] (`kTonConnectClaimMaxRecords`).
pub const TON_CONNECT_CLAIM_MAX_RECORDS: usize = 32;
/// Maximum serialized byte size of [`TonConnectClaimStore`] (`kTonConnectClaimMaxBytes`).
pub const TON_CONNECT_CLAIM_MAX_BYTES: usize = 1024 * 1024;
/// Retention window after `expires` in seconds (`kTonConnectClaimLifetime = 24h`).
pub const TON_CONNECT_CLAIM_LIFETIME: i64 = 24 * 3600;

const TON_CONNECT_CLAIM_FORMAT_VERSION: u32 = 1;
const CLAIM_TRACE_ID_MAX_LEN: usize = 256;
const CLAIM_ANSWER_MAX_BYTES: usize = 64 * 1024;
const CLAIM_OPERATION_ID_MAX_BYTES: usize = 256;
const CLAIM_SIGNED_BOC_MAX_LEN: usize = 24 * 1024;
const CLAIM_PUBLIC_KEY_SIZE: usize = 32;

/// Stored decision for a claimed TON Connect request (`Wallet::TonConnectClaimDecision`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u32)]
pub enum TonConnectClaimDecision {
    /// User confirmed sending a transaction; holds fallback `not_sent` error + optional `signed_boc`.
    Confirm = 1,
    /// Immediate answer (decline/error/signData) ready for delivery.
    Answer = 2,
}

/// Single persisted TON Connect request claim (`Wallet::TonConnectClaimRecord`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TonConnectClaimRecord {
    /// TON Connect session ID (non-zero).
    pub session_id: u64,
    /// Service notification message ID (`> 0`).
    pub msg_id: i32,
    /// JSON-RPC request ID.
    pub request_id: String,
    /// Optional trace ID.
    pub trace_id: String,
    /// Request expiration UNIX timestamp (`> 0`).
    pub expires: i64,
    /// Creation UNIX timestamp (`> 0`).
    pub created: i64,
    /// Canonical lowercase raw wallet address.
    pub address: String,
    /// 32-byte wallet Ed25519 public key.
    pub public_key: Vec<u8>,
    /// Claim decision kind.
    pub decision: TonConnectClaimDecision,
    /// Encrypted bridge answer payload.
    pub answer: Vec<u8>,
    /// Encrypted fallback "not sent" payload (required for `Confirm`).
    pub not_sent: Vec<u8>,
    /// Optional send operation identifier.
    pub operation_id: String,
    /// Optional signed BOC base64 once broadcast started.
    pub signed_boc: String,
}

impl TonConnectClaimRecord {
    /// Returns `true` if this claim matches `(session_id, msg_id)` (`TonConnectClaimMatches`).
    #[must_use]
    pub const fn matches(&self, session_id: u64, msg_id: i32) -> bool {
        self.session_id == session_id && self.msg_id == msg_id
    }

    /// Validates all field invariants matching `ValidRecord` in `wallet_ton_connect_claims.cpp:25-52`.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        if self.session_id == 0
            || self.msg_id <= 0
            || !ton_connect_request_id_valid(&self.request_id)
            || self.trace_id.len() > CLAIM_TRACE_ID_MAX_LEN
            || self.expires <= 0
            || self.created <= 0
            || canonical_address(&self.address).as_deref() != Some(self.address.as_str())
            || self.public_key.len() != CLAIM_PUBLIC_KEY_SIZE
            || self.answer.len() > CLAIM_ANSWER_MAX_BYTES
            || self.not_sent.len() > CLAIM_ANSWER_MAX_BYTES
            || self.operation_id.len() > CLAIM_OPERATION_ID_MAX_BYTES
            || self.signed_boc.len() > CLAIM_SIGNED_BOC_MAX_LEN
            || (!self.signed_boc.is_empty() && self.operation_id.is_empty())
        {
            return false;
        }
        match self.decision {
            TonConnectClaimDecision::Confirm => !self.not_sent.is_empty(),
            TonConnectClaimDecision::Answer => {
                !self.answer.is_empty()
                    && self.not_sent.is_empty()
                    && self.operation_id.is_empty()
                    && self.signed_boc.is_empty()
            }
        }
    }
}

/// Collection of persisted TON Connect claims (`Wallet::TonConnectClaimStore`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TonConnectClaimStore {
    /// Validated claim records with unique `(session_id, msg_id)` keys.
    pub records: Vec<TonConnectClaimRecord>,
}

impl TonConnectClaimStore {
    /// Validates record count, per-record invariants, and `(session_id, msg_id)` uniqueness.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        if self.records.len() > TON_CONNECT_CLAIM_MAX_RECORDS {
            return false;
        }
        for (i, rec) in self.records.iter().enumerate() {
            if !rec.is_valid()
                || self.records[..i]
                    .iter()
                    .any(|other| other.matches(rec.session_id, rec.msg_id))
            {
                return false;
            }
        }
        true
    }

    /// Removes claims whose `expires + TON_CONNECT_CLAIM_LIFETIME < now_unix`.
    pub fn prune_expired(&mut self, now_unix: i64) {
        self.records
            .retain(|r| now_unix <= r.expires.saturating_add(TON_CONNECT_CLAIM_LIFETIME));
    }

    /// Finds a claim by `(session_id, msg_id)`.
    #[must_use]
    pub fn find(&self, session_id: u64, msg_id: i32) -> Option<&TonConnectClaimRecord> {
        self.records.iter().find(|r| r.matches(session_id, msg_id))
    }

    /// Serializes the claim store to versioned binary bytes (`WriteTonConnectClaims`).
    pub fn to_bytes(&self) -> Result<Vec<u8>, WalletError> {
        if !self.is_valid() {
            return Err(WalletError::TonConnect(
                "claim store failed validation".to_owned(),
            ));
        }
        let body = serde_json::to_vec(self)
            .map_err(|e| WalletError::TonConnect(format!("serialize claims: {e}")))?;
        let mut out = Vec::with_capacity(8 + body.len());
        out.extend_from_slice(&TON_CONNECT_CLAIM_FORMAT_VERSION.to_le_bytes());
        out.extend_from_slice(&(self.records.len() as u32).to_le_bytes());
        out.extend_from_slice(&body);
        if out.len() > TON_CONNECT_CLAIM_MAX_BYTES {
            return Err(WalletError::TonConnect(
                "serialized claims exceed maximum size".to_owned(),
            ));
        }
        Ok(out)
    }

    /// Deserializes and validates a claim store from bytes (`ReadTonConnectClaims`).
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, WalletError> {
        if bytes.is_empty() {
            return Ok(Self::default());
        }
        if bytes.len() < 8 || bytes.len() > TON_CONNECT_CLAIM_MAX_BYTES {
            return Err(WalletError::TonConnect(
                "invalid claim store byte length".to_owned(),
            ));
        }
        let version = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        let count = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize;
        if version == 0
            || version > TON_CONNECT_CLAIM_FORMAT_VERSION
            || count > TON_CONNECT_CLAIM_MAX_RECORDS
        {
            return Err(WalletError::TonConnect(format!(
                "unsupported claim store version {version} or count {count}"
            )));
        }
        let store: Self = serde_json::from_slice(&bytes[8..])
            .map_err(|e| WalletError::TonConnect(format!("decode claims: {e}")))?;
        if store.records.len() != count || !store.is_valid() {
            return Err(WalletError::TonConnect(
                "claim store failed validation on decode".to_owned(),
            ));
        }
        Ok(store)
    }
}

/// Validates a 64-hex-character TON Connect `client_id` and decodes it to 32 bytes.
pub fn parse_client_id(id: &str) -> Result<[u8; 32], WalletError> {
    if id.len() != 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(WalletError::TonConnect(format!(
            "invalid 64-hex client_id: {id}"
        )));
    }
    let decoded = hex::decode(id)
        .map_err(|e| WalletError::TonConnect(format!("client_id hex decode: {e}")))?;
    let mut out = [0_u8; 32];
    out.copy_from_slice(&decoded);
    Ok(out)
}

/// Returns `true` if `url_str` is a strict `https://` URL with non-empty host and no userinfo
/// (`Wallet::ValidHttpsUrl` in `wallet_ton_connect_link.cpp:174-180`).
#[must_use]
pub fn valid_https_url(url_str: &str) -> bool {
    let Ok(parsed) = Url::parse(url_str) else {
        return false;
    };
    parsed.scheme() == "https"
        && parsed.host_str().is_some_and(|h| !h.is_empty())
        && parsed.username().is_empty()
        && parsed.password().is_none()
}

/// Decodes a Telegram Mini App `tonconnect-...` start parameter back into a query string
/// (`Wallet::TonConnectStartParamQuery` in `wallet_ton_connect_link.cpp:98-110`).
#[must_use]
pub fn ton_connect_start_param_query(startapp: &str) -> Option<String> {
    let rest = startapp.strip_prefix("tonconnect-")?;
    // Exact replacement order from tdesktop: `--` -> `%`, `__` -> `=`, `-` -> `&`.
    let out = rest.replace("--", "%").replace("__", "=").replace('-', "&");
    Some(out)
}

/// Encodes a query string into a Telegram Mini App `tonconnect-...` start parameter
/// (`Wallet::TonConnectStartParam` in `wallet_ton_connect_link.cpp:112-138`).
#[must_use]
pub fn ton_connect_start_param(query: &str) -> String {
    let without_frag = query.split('#').next().unwrap_or(query);
    let normalized = without_frag.replace('+', "%20");
    let mut parts = Vec::new();
    for pair in normalized.split('&').filter(|p| !p.is_empty()) {
        // Mirror `QUrl::toPercentEncoding(chunk, "=", "-._~")` across `%`-split chunks:
        // keep `=` and ASCII alphanumerics unencoded, and percent-encode `-`, `.`, `_`, `~`
        // (and any other non-alphanumeric bytes) so `-` and `__` in values never collide
        // with the startapp separators.
        let mut encoded_pair = String::with_capacity(pair.len());
        let mut chars = pair.as_bytes().iter().copied().peekable();
        while let Some(b) = chars.next() {
            if b == b'%' {
                encoded_pair.push('%');
                // Preserve two hex digits of an existing percent-escape verbatim.
                for _ in 0..2 {
                    if let Some(&next_b) = chars.peek()
                        && next_b.is_ascii_hexdigit()
                    {
                        encoded_pair.push(next_b as char);
                        chars.next();
                    }
                }
            } else if b.is_ascii_alphanumeric() || b == b'=' {
                encoded_pair.push(b as char);
            } else {
                encoded_pair.push_str(&format!("%{b:02X}"));
            }
        }
        if !encoded_pair.starts_with('%') {
            parts.push(encoded_pair);
        }
    }
    let joined = parts.join("-");
    let encoded = joined.replace('=', "__").replace('%', "--");
    format!("tonconnect-{encoded}")
}

/// Parses a TON Connect URL (`tc://?...`, `https://...?...`) or raw query string
/// (`Wallet::ParseTonConnectLink` in `wallet_ton_connect_link.cpp:140-172`).
pub fn parse_ton_connect_link(input: &str) -> Result<TonConnectLink, WalletError> {
    let trimmed = input.trim();
    let query = if let Some(q) = ton_connect_start_param_query(trimmed) {
        q
    } else if let Some((_, q)) = trimmed.split_once('?') {
        q.split('#').next().unwrap_or(q).to_owned()
    } else {
        trimmed.split('#').next().unwrap_or(trimmed).to_owned()
    };

    if query.len() > MAX_TON_CONNECT_QUERY_LEN {
        return Err(WalletError::TonConnect("query too long".to_owned()));
    }

    let mut version: Option<String> = None;
    let mut client_id: Option<String> = None;
    let mut request_json: Option<String> = None;
    let mut ret = String::new();
    let mut trace_id: Option<String> = None;

    for (k, v) in url::form_urlencoded::parse(query.as_bytes()) {
        match k.as_ref() {
            "v" if version.is_none() => version = Some(v.into_owned()),
            "id" if client_id.is_none() => client_id = Some(v.into_owned()),
            "r" if request_json.is_none() => request_json = Some(v.into_owned()),
            "ret" if ret.is_empty() => ret = v.into_owned(),
            "trace_id" if trace_id.is_none() => trace_id = Some(v.into_owned()),
            _ => {}
        }
    }

    if let Some(ref v) = version
        && v != "2"
    {
        return Err(WalletError::TonConnect(format!(
            "unsupported TON Connect version: {v}"
        )));
    }
    if let Some(ref id) = client_id {
        parse_client_id(id)?;
    }
    if request_json.is_some() && (version.is_none() || client_id.is_none()) {
        return Err(WalletError::TonConnect(
            "connect link with `r` requires `v=2` and `id`".to_owned(),
        ));
    }

    let mut link = TonConnectLink {
        kind: if request_json.is_some() {
            TonConnectLinkKind::Connect
        } else if client_id.is_some() {
            TonConnectLinkKind::OpenPending
        } else {
            TonConnectLinkKind::Bare
        },
        client_id: client_id.unwrap_or_default(),
        manifest_url: String::new(),
        proof_payload: None,
        ret,
        trace_id,
    };

    if let Some(r_str) = request_json {
        let r_val: Value = serde_json::from_str(&r_str)
            .map_err(|e| WalletError::TonConnect(format!("invalid connect request JSON: {e}")))?;
        let obj = r_val
            .as_object()
            .ok_or_else(|| WalletError::TonConnect("connect request not an object".to_owned()))?;
        let manifest_url = obj
            .get("manifestUrl")
            .and_then(Value::as_str)
            .ok_or_else(|| WalletError::TonConnect("missing manifestUrl".to_owned()))?;
        if !valid_https_url(manifest_url) {
            return Err(WalletError::TonConnect(format!(
                "manifestUrl must be valid https URL: {manifest_url}"
            )));
        }
        let items = obj
            .get("items")
            .and_then(Value::as_array)
            .ok_or_else(|| WalletError::TonConnect("missing connect items array".to_owned()))?;
        let mut has_addr = false;
        for item in items {
            let name = item.get("name").and_then(Value::as_str).unwrap_or("");
            if name == "ton_addr" {
                has_addr = true;
            } else if name == "ton_proof" && link.proof_payload.is_none() {
                let payload = item.get("payload").and_then(Value::as_str).ok_or_else(|| {
                    WalletError::TonConnect("ton_proof item missing string payload".to_owned())
                })?;
                link.proof_payload = Some(payload.to_owned());
            }
        }
        if !has_addr {
            return Err(WalletError::TonConnect(
                "connect request must include ton_addr item".to_owned(),
            ));
        }
        link.manifest_url = manifest_url.to_owned();
    }

    Ok(link)
}

// ============================================================================
// HTTP Bridge protocol client (configurable endpoint, tested via local fake)
// ============================================================================

/// Incoming SSE bridge event envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BridgeEvent {
    /// SSE event ID (for `last_event_id` resumption).
    pub id: Option<String>,
    /// Sender dApp `client_id`.
    pub from: String,
    /// Base64-encoded NaCl-box encrypted payload.
    pub message: String,
}

/// Configuration for [`HttpTonConnectBridge`].
#[derive(Debug, Clone)]
pub struct TonConnectBridgeConfig {
    /// Base URL of the TON Connect HTTP bridge (e.g. `http://127.0.0.1:<port>/bridge` in tests).
    pub bridge_url: String,
    /// Default message TTL in seconds (`300` per TON Connect spec).
    pub default_ttl_secs: u32,
    /// HTTP timeout.
    pub timeout: Duration,
}

impl TonConnectBridgeConfig {
    /// Creates a bridge config for `bridge_url`.
    #[must_use]
    pub fn new(bridge_url: impl Into<String>) -> Self {
        Self {
            bridge_url: bridge_url.into(),
            default_ttl_secs: 300,
            timeout: Duration::from_secs(15),
        }
    }
}

/// Trait abstracting the TON Connect HTTP bridge + manifest fetcher.
pub trait TonConnectBridge: Send + Sync {
    /// Sends an encrypted base64 payload from `from_client_id` to `to_client_id`.
    fn send_message<'a>(
        &'a self,
        from_client_id: &'a str,
        to_client_id: &'a str,
        ttl_secs: u32,
        encrypted_base64: &'a str,
    ) -> BoxFuture<'a, Result<(), WalletError>>;

    /// Polls queued SSE bridge events for `client_id` (optionally resuming from `last_event_id`).
    fn poll_events<'a>(
        &'a self,
        client_id: &'a str,
        last_event_id: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Vec<BridgeEvent>, WalletError>>;

    /// Fetches and parses a dApp's `tonconnect-manifest.json`.
    fn fetch_manifest<'a>(
        &'a self,
        manifest_url: &'a str,
    ) -> BoxFuture<'a, Result<DappManifest, WalletError>>;
}

/// Parses standard SSE `id:` / `data:` frames returned by a TON Connect bridge `/events` endpoint.
#[must_use]
pub fn parse_sse_events(body: &str) -> Vec<BridgeEvent> {
    let mut events = Vec::new();
    let mut current_id: Option<String> = None;
    let mut current_data = Vec::<String>::new();

    let flush = |id: &mut Option<String>, data: &mut Vec<String>, out: &mut Vec<BridgeEvent>| {
        if data.is_empty() {
            return;
        }
        let joined = data.join("\n");
        data.clear();
        if let Ok(val) = serde_json::from_str::<Value>(&joined)
            && let (Some(from), Some(message)) = (
                val.get("from").and_then(Value::as_str),
                val.get("message").and_then(Value::as_str),
            )
        {
            out.push(BridgeEvent {
                id: id.take(),
                from: from.to_owned(),
                message: message.to_owned(),
            });
        }
    };

    for line in body.lines() {
        let trimmed = line.trim_end_matches('\r');
        if trimmed.is_empty() {
            flush(&mut current_id, &mut current_data, &mut events);
        } else if let Some(rest) = trimmed.strip_prefix("id:") {
            current_id = Some(rest.trim().to_owned());
        } else if let Some(rest) = trimmed.strip_prefix("data:") {
            current_data.push(rest.trim().to_owned());
        }
    }
    flush(&mut current_id, &mut current_data, &mut events);
    events
}

/// Reqwest-based [`TonConnectBridge`] implementation targeting a configured bridge URL.
#[derive(Debug, Clone)]
pub struct HttpTonConnectBridge {
    config: TonConnectBridgeConfig,
    http: reqwest::Client,
}

impl HttpTonConnectBridge {
    /// Creates a new HTTP bridge client.
    pub fn new(config: TonConnectBridgeConfig) -> Result<Self, WalletError> {
        if config.bridge_url.trim().is_empty() {
            return Err(WalletError::TonConnect(
                "bridge_url must be configured".to_owned(),
            ));
        }
        let http = reqwest::Client::builder()
            .timeout(config.timeout)
            .build()
            .map_err(|e| WalletError::TonConnect(format!("bridge HTTP build: {e}")))?;
        Ok(Self { config, http })
    }
}

impl TonConnectBridge for HttpTonConnectBridge {
    fn send_message<'a>(
        &'a self,
        from_client_id: &'a str,
        to_client_id: &'a str,
        ttl_secs: u32,
        encrypted_base64: &'a str,
    ) -> BoxFuture<'a, Result<(), WalletError>> {
        Box::pin(async move {
            let base = self.config.bridge_url.trim_end_matches('/');
            let url = format!(
                "{base}/message?client_id={from_client_id}&to={to_client_id}&ttl={ttl_secs}"
            );
            let resp = self
                .http
                .post(&url)
                .header(reqwest::header::CONTENT_TYPE, "text/plain")
                .body(encrypted_base64.to_owned())
                .send()
                .await
                .map_err(|e| WalletError::TonConnect(format!("bridge send failed: {e}")))?;
            if !resp.status().is_success() {
                return Err(WalletError::TonConnect(format!(
                    "bridge send returned HTTP {}",
                    resp.status().as_u16()
                )));
            }
            Ok(())
        })
    }

    fn poll_events<'a>(
        &'a self,
        client_id: &'a str,
        last_event_id: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Vec<BridgeEvent>, WalletError>> {
        Box::pin(async move {
            let base = self.config.bridge_url.trim_end_matches('/');
            let url = match last_event_id {
                Some(last) => format!("{base}/events?client_id={client_id}&last_event_id={last}"),
                None => format!("{base}/events?client_id={client_id}"),
            };
            let resp = self
                .http
                .get(&url)
                .header(reqwest::header::ACCEPT, "text/event-stream")
                .send()
                .await
                .map_err(|e| WalletError::TonConnect(format!("bridge poll failed: {e}")))?;
            if !resp.status().is_success() {
                return Err(WalletError::TonConnect(format!(
                    "bridge poll returned HTTP {}",
                    resp.status().as_u16()
                )));
            }
            let text = resp
                .text()
                .await
                .map_err(|e| WalletError::TonConnect(format!("bridge read failed: {e}")))?;
            Ok(parse_sse_events(&text))
        })
    }

    fn fetch_manifest<'a>(
        &'a self,
        manifest_url: &'a str,
    ) -> BoxFuture<'a, Result<DappManifest, WalletError>> {
        Box::pin(async move {
            let resp = self
                .http
                .get(manifest_url)
                .send()
                .await
                .map_err(|e| WalletError::TonConnect(format!("manifest fetch failed: {e}")))?;
            if !resp.status().is_success() {
                return Err(WalletError::TonConnect(format!(
                    "manifest fetch returned HTTP {}",
                    resp.status().as_u16()
                )));
            }
            resp.json::<DappManifest>()
                .await
                .map_err(|e| WalletError::TonConnect(format!("manifest JSON invalid: {e}")))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::{TransferMessage, WalletVersion};
    use crate::mnemonic::mnemonic_to_keypair;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[test]
    fn nacl_box_official_alice_bob_kat() {
        // Canonical NaCl Alice/Bob keys from Daniel J. Bernstein's "Cryptography in NaCl".
        let alice_sk: [u8; 32] =
            hex::decode("77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a")
                .unwrap()
                .try_into()
                .unwrap();
        let alice_pk_expected = "8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a";
        let bob_sk: [u8; 32] =
            hex::decode("5dab087e624a8a4b79e17f8b83800ee66f3bb1292618b6fd1c2f8b27ff88e0eb")
                .unwrap()
                .try_into()
                .unwrap();
        let bob_pk_expected = "de9edb7d7b7dc1b4d35b61c2ece435373f8343c85b78674dadfc7e146f882b4f";

        let alice = TonConnectKeypair::from_secret_bytes(alice_sk);
        let bob = TonConnectKeypair::from_secret_bytes(bob_sk);
        assert_eq!(alice.client_id(), alice_pk_expected);
        assert_eq!(bob.client_id(), bob_pk_expected);

        let manifest = DappManifest {
            url: "https://demo.example".to_owned(),
            name: "Demo dApp".to_owned(),
            icon_url: String::new(),
        };
        let alice_sess = TonConnectSession::new(alice, bob_pk_expected, manifest.clone()).unwrap();
        let bob_sess = TonConnectSession::new(bob, alice_pk_expected, manifest).unwrap();

        let nonce = [0x42_u8; NACL_NONCE_SIZE];
        let enc = alice_sess
            .encrypt_message_with_nonce(b"hello ton connect", &nonce)
            .unwrap();
        let dec = bob_sess.decrypt_message(&enc).unwrap();
        assert_eq!(dec, b"hello ton connect");
    }

    #[test]
    fn ton_connect_link_and_start_param_roundtrip() {
        let dapp_id = "11".repeat(32);
        let raw_query = format!(
            "v=2&id={dapp_id}&r=%7B%22manifestUrl%22%3A%22https%3A%2F%2Fdemo.example%2Fmanifest.json%22%2C%22items%22%3A%5B%7B%22name%22%3A%22ton_addr%22%7D%2C%7B%22name%22%3A%22ton_proof%22%2C%22payload%22%3A%22nonce-123%22%7D%5D%7D"
        );
        let link = parse_ton_connect_link(&format!("tc://?{raw_query}")).unwrap();
        assert_eq!(link.kind, TonConnectLinkKind::Connect);
        assert_eq!(link.client_id, dapp_id);
        assert_eq!(link.manifest_url, "https://demo.example/manifest.json");
        assert_eq!(link.proof_payload.as_deref(), Some("nonce-123"));

        // Startapp encoding round-trip (wallet_ton_connect_link.cpp:98-138).
        let start = ton_connect_start_param(&raw_query);
        assert!(start.starts_with("tonconnect-"));
        let decoded = parse_ton_connect_link(&start).unwrap();
        assert_eq!(decoded, link);

        // Non-HTTPS manifest rejected.
        assert!(!valid_https_url("http://insecure.example/manifest.json"));
        assert!(valid_https_url("https://secure.example/manifest.json"));
    }

    #[tokio::test]
    async fn ton_connect_full_flow_against_local_fake_bridge() {
        let wallet_kp = TonConnectKeypair::generate();
        let dapp_kp = TonConnectKeypair::generate();
        let manifest = DappManifest {
            url: "https://demo.example".to_owned(),
            name: "Demo Swap".to_owned(),
            icon_url: "https://demo.example/icon.png".to_owned(),
        };
        let wallet_session =
            TonConnectSession::new(wallet_kp.clone(), &dapp_kp.client_id(), manifest.clone())
                .unwrap();
        let dapp_session =
            TonConnectSession::new(dapp_kp.clone(), &wallet_kp.client_id(), manifest.clone())
                .unwrap();

        // Prepare an encrypted sendTransaction request from the fake dApp.
        let tx_req_json = json!({
            "id": "42",
            "method": "sendTransaction",
            "params": [
                json!({
                    "valid_until": 1800000000,
                    "network": "-239",
                    "messages": [{
                        "address": "UQCdqXGvONLwOr3zCNX5FjapflorB6ZsOdcdfLrjsDLt3AF4",
                        "amount": "500000000",
                        "payload": "te6cckEBAQEADAAAFAAAAABIZWxsbyGVgYQo"
                    }]
                }).to_string()
            ]
        });
        let encrypted_req = dapp_session
            .encrypt_message(&serde_json::to_vec(&tx_req_json).unwrap())
            .unwrap();

        let sent_by_wallet: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let sent_clone = Arc::clone(&sent_by_wallet);
        let sse_payload = format!(
            "id: 1\ndata: {}\n\n",
            json!({ "from": dapp_kp.client_id(), "message": encrypted_req })
        );

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let sent_clone = Arc::clone(&sent_clone);
                let sse_payload = sse_payload.clone();
                let manifest_json = serde_json::to_string(&manifest).unwrap();
                tokio::spawn(async move {
                    let mut buf = vec![0_u8; 16384];
                    let n = stream.read(&mut buf).await.unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]);
                    let first_line = req.lines().next().unwrap_or("");
                    if first_line.starts_with("POST /bridge/message") {
                        let body = req.split("\r\n\r\n").nth(1).unwrap_or("").to_owned();
                        sent_clone.lock().unwrap().push(body);
                        let resp =
                            "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nOK";
                        let _ = stream.write_all(resp.as_bytes()).await;
                    } else if first_line.starts_with("GET /bridge/events") {
                        let resp = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            sse_payload.len(),
                            sse_payload
                        );
                        let _ = stream.write_all(resp.as_bytes()).await;
                    } else if first_line.starts_with("GET /manifest.json") {
                        let resp = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            manifest_json.len(),
                            manifest_json
                        );
                        let _ = stream.write_all(resp.as_bytes()).await;
                    }
                });
            }
        });

        let bridge =
            HttpTonConnectBridge::new(TonConnectBridgeConfig::new(format!("http://{addr}/bridge")))
                .unwrap();

        // 1. Fetch manifest.
        let fetched = bridge
            .fetch_manifest(&format!("http://{addr}/manifest.json"))
            .await
            .unwrap();
        assert_eq!(fetched.name, "Demo Swap");

        // 2. Send connect event with ton_proof.
        let words: Vec<String> = [
            "hospital", "stove", "relief", "fringe", "tongue", "always", "charge", "angry", "urge",
            "sentence", "again", "match", "nerve", "inquiry", "senior", "coconut", "label",
            "tumble", "carry", "category", "beauty", "bean", "road", "solution",
        ]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
        let ton_kp = mnemonic_to_keypair(&words, "").unwrap();
        let contract =
            WalletContract::new(WalletVersion::V5R1, 0, ton_kp.public_key_bytes(), false);

        let connect_enc = wallet_session
            .build_connect_event(
                1,
                &contract,
                &ton_kp,
                Some("proof-challenge"),
                1_700_000_000,
            )
            .unwrap();
        bridge
            .send_message(
                &wallet_session.wallet_client_id(),
                &wallet_session.dapp_client_id(),
                300,
                &connect_enc,
            )
            .await
            .unwrap();

        // 3. Poll incoming sendTransaction SSE event and decrypt it.
        let events = bridge
            .poll_events(&wallet_session.wallet_client_id(), None)
            .await
            .unwrap();
        assert_eq!(events.len(), 1);
        let app_req = wallet_session
            .decrypt_app_request(&events[0].message)
            .unwrap();
        let TonConnectAppRequest::SendTransaction(tx) = app_req else {
            panic!("expected sendTransaction");
        };
        assert_eq!(tx.request_id, "42");
        assert_eq!(tx.total_nano(), 500_000_000);
        assert_eq!(tx.messages[0].decoded_comment.as_deref(), Some("Hello!"));

        // 4. Sign transfer and reply, then disconnect.
        let dest = TonAddress::parse(&tx.messages[0].address).unwrap().address;
        let msg = TransferMessage::simple(dest, tx.messages[0].amount_nano, false, Some("Hello!"))
            .unwrap();
        let signed = contract
            .create_signed_transfer(&ton_kp, 1, 1_800_000_000, &[msg])
            .unwrap();
        let tx_resp_enc = wallet_session
            .build_tx_success_response(&tx.request_id, &signed.boc_base64)
            .unwrap();
        bridge
            .send_message(
                &wallet_session.wallet_client_id(),
                &wallet_session.dapp_client_id(),
                300,
                &tx_resp_enc,
            )
            .await
            .unwrap();

        let disc_enc = wallet_session.build_disconnect_event(2).unwrap();
        bridge
            .send_message(
                &wallet_session.wallet_client_id(),
                &wallet_session.dapp_client_id(),
                300,
                &disc_enc,
            )
            .await
            .unwrap();

        // Verify the fake dApp can decrypt all 3 messages sent by the wallet.
        let recorded = sent_by_wallet.lock().unwrap().clone();
        assert_eq!(recorded.len(), 3);
        let connect_dec: Value =
            serde_json::from_slice(&dapp_session.decrypt_message(&recorded[0]).unwrap()).unwrap();
        assert_eq!(connect_dec["event"], "connect");
        assert_eq!(connect_dec["payload"]["items"][1]["name"], "ton_proof");

        let tx_dec: Value =
            serde_json::from_slice(&dapp_session.decrypt_message(&recorded[1]).unwrap()).unwrap();
        assert_eq!(tx_dec["id"], "42");
        assert_eq!(tx_dec["result"], signed.boc_base64);

        let disc_dec: Value =
            serde_json::from_slice(&dapp_session.decrypt_message(&recorded[2]).unwrap()).unwrap();
        assert_eq!(disc_dec["event"], "disconnect");
    }
}
