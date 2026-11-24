//! Port of `mtproto/mtproto_response.cpp` and `mtproto/mtp_instance.cpp` (error classification).

use tg_crypto::CryptoError;
use tg_tl::DeserializeError;
use tg_transport::Error as TransportError;
use thiserror::Error;

use crate::dc_options::DcId;

/// Migration type parsed from `*_MIGRATE_X` RPC errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrateKind {
    /// `FILE_MIGRATE_X`
    File,
    /// `PHONE_MIGRATE_X`
    Phone,
    /// `NETWORK_MIGRATE_X`
    Network,
    /// `USER_MIGRATE_X`
    User,
}

/// Structured Telegram RPC error (`rpc_error#2144ca19`), matching `ARCHITECTURE.md` contract.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("RPC error {code}: {name}{}", if description.is_empty() { String::new() } else { format!(": {description}") })]
pub struct RpcError {
    /// Numeric error code (e.g. 303, 400, 401, 420, 500, or negative).
    pub code: i32,
    /// Normalized error type / name (e.g. `FLOOD_WAIT_42`, `PHONE_MIGRATE_4`, `AUTH_KEY_UNREGISTERED`).
    pub name: String,
    /// Trailing numeric parameter extracted from `NAME_<N>` errors (e.g. `42` for `FLOOD_WAIT_42`).
    pub value: Option<u32>,
    /// Optional human-readable description following `": "`.
    pub description: String,
}

impl RpcError {
    /// Parses a wire `rpc_error(code, raw_message)` following `MTP::Error::Error(const MTPrpcError&)`.
    pub fn parse(code: i32, raw_message: &str) -> Self {
        let (name, description) = parse_error_parts(code, raw_message);
        let value = extract_trailing_number(&name);
        Self {
            code,
            name,
            value,
            description,
        }
    }

    /// Creates a local synthetic error (`Error::Local` in `mtproto_response.cpp`).
    pub fn local(kind: &str, description: impl Into<String>) -> Self {
        Self {
            code: 0,
            name: format!("CLIENT_{kind}"),
            value: None,
            description: description.into(),
        }
    }

    /// Parses `(FILE|PHONE|NETWORK|USER)_MIGRATE_<dc>` into `(MigrateKind, DcId)`.
    pub fn migrate_dc(&self) -> Option<(MigrateKind, DcId)> {
        let (kind, rest) = if let Some(r) = self.name.strip_prefix("FILE_MIGRATE_") {
            (MigrateKind::File, r)
        } else if let Some(r) = self.name.strip_prefix("PHONE_MIGRATE_") {
            (MigrateKind::Phone, r)
        } else if let Some(r) = self.name.strip_prefix("NETWORK_MIGRATE_") {
            (MigrateKind::Network, r)
        } else {
            let r = self.name.strip_prefix("USER_MIGRATE_")?;
            (MigrateKind::User, r)
        };
        let dc_id: DcId = rest.parse().ok()?;
        if dc_id > 0 { Some((kind, dc_id)) } else { None }
    }

    /// Returns wait seconds if this is `FLOOD_WAIT_<N>`.
    pub fn flood_wait_secs(&self) -> Option<u32> {
        self.name
            .strip_prefix("FLOOD_WAIT_")
            .and_then(|s| s.parse().ok())
    }

    /// Returns wait seconds if this is `FLOOD_PREMIUM_WAIT_<N>`.
    pub fn flood_premium_wait_secs(&self) -> Option<u32> {
        self.name
            .strip_prefix("FLOOD_PREMIUM_WAIT_")
            .and_then(|s| s.parse().ok())
    }

    /// Returns wait seconds if this is `SLOWMODE_WAIT_<N>`.
    pub fn slowmode_wait_secs(&self) -> Option<u32> {
        self.name
            .strip_prefix("SLOWMODE_WAIT_")
            .and_then(|s| s.parse().ok())
    }

    /// Matches `MTP::IsFloodError`.
    pub fn is_flood(&self) -> bool {
        self.flood_wait_secs().is_some() || self.flood_premium_wait_secs().is_some()
    }

    /// Matches `MTP::IsTemporaryError`.
    pub fn is_temporary(&self) -> bool {
        self.code < 0 || self.code >= 500 || self.is_flood()
    }

    /// Matches `MTP::IsDefaultHandledError`.
    pub fn is_default_handled(&self) -> bool {
        self.is_temporary()
    }

    /// Matches `MTP::IgnoreError`.
    pub fn is_ignored(&self) -> bool {
        self.code == 406
    }

    /// Matches `IsDestroyedTemporaryKeyError` (`401 AUTH_KEY_PERM_EMPTY`).
    pub fn is_destroyed_temp_key(&self) -> bool {
        self.code == 401 && self.name == "AUTH_KEY_PERM_EMPTY"
    }

    /// Matches `CONNECTION_NOT_INITED` / `CONNECTION_LAYER_INVALID`.
    pub fn is_connection_not_inited(&self) -> bool {
        self.name == "CONNECTION_NOT_INITED" || self.name == "CONNECTION_LAYER_INVALID"
    }

    /// Matches `AUTH_KEY_DUPLICATED` (406), requiring key regeneration on that DC.
    pub fn is_auth_key_duplicated(&self) -> bool {
        self.name == "AUTH_KEY_DUPLICATED"
    }

    /// Matches `401` (excluding `AUTH_KEY_PERM_EMPTY`) used to trigger guest DC auth export/import
    /// or main-DC session logout (`Instance::Private::onErrorDefault` in `mtproto/mtp_instance.cpp`).
    pub fn is_unauthorized(&self) -> bool {
        self.code == 401 && self.name != "AUTH_KEY_PERM_EMPTY"
    }

    /// Matches `400 FILE_ID_INVALID` on guest DCs.
    pub fn is_bad_guest_dc(&self) -> bool {
        self.code == 400 && self.name == "FILE_ID_INVALID"
    }

    /// Matches `MSG_WAIT_TIMEOUT` / `MSG_WAIT_FAILED`.
    pub fn is_msg_wait(&self) -> bool {
        self.name == "MSG_WAIT_TIMEOUT" || self.name == "MSG_WAIT_FAILED"
    }

    /// Matches `FROZEN_METHOD_INVALID`.
    pub fn is_frozen_method(&self) -> bool {
        self.name == "FROZEN_METHOD_INVALID"
    }
}

fn parse_error_parts(code: i32, raw: &str) -> (String, String) {
    // Port of `^([A-Z0-9_]+)(: .*)?$` from mtproto_response.cpp.
    let (prefix, rest) = match raw.split_once(": ") {
        Some((p, d)) => (p, Some(d)),
        None => (raw, None),
    };
    let valid_prefix = !prefix.is_empty()
        && prefix
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_');
    if valid_prefix {
        (prefix.to_string(), rest.unwrap_or("").to_string())
    } else if !(0..500).contains(&code) {
        ("INTERNAL_SERVER_ERROR".to_string(), raw.to_string())
    } else {
        (
            "CLIENT_BAD_RPC_ERROR".to_string(),
            format!("Bad rpc error received, text = '{raw}'"),
        )
    }
}

fn extract_trailing_number(name: &str) -> Option<u32> {
    let (_, suffix) = name.rsplit_once('_')?;
    if !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit()) {
        suffix.parse().ok()
    } else {
        None
    }
}

/// Top-level error returned by `Client::invoke` / `Sender` (matches `docs/ARCHITECTURE.md`).
#[derive(Debug, Error)]
pub enum InvocationError {
    /// Server returned an `rpc_error`.
    #[error("{0}")]
    Rpc(#[from] RpcError),
    /// Request was dropped because the session or client shut down before a reply arrived.
    #[error("request dropped before completion")]
    Dropped,
    /// Underlying network / framing transport error.
    #[error("transport error: {0}")]
    Transport(#[from] TransportError),
    /// Failed to deserialize the response or a protocol message.
    #[error("deserialization error: {0}")]
    Deserialize(#[from] DeserializeError),
    /// Cryptographic or auth key creation error.
    #[error("crypto error: {0}")]
    Crypto(#[from] CryptoError),
    /// Request was explicitly cancelled by the caller.
    #[error("request cancelled")]
    Cancelled,
    /// Requested DC ID is unknown or has no endpoints.
    #[error("invalid or unknown DC ID: {0}")]
    InvalidDc(DcId),
    /// Protocol error during handshake or session framing.
    #[error("protocol error: {0}")]
    Protocol(String),
    /// Operation timed out.
    #[error("request timed out")]
    Timeout,
}
